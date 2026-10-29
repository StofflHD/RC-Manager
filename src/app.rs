//! Main window: connection tree on the left, properties on the right, sessions and log at the bottom.

use crate::embed::Embedder;
use crate::launch::{self, EmbedTarget, Event, Tools};
use crate::legacy::{backup, import as legacy_import, rdpfile};
use crate::model::*;
use crate::net;
use crate::store::{Data, Store};
use crate::theme;
use egui::{Align2, RichText};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use uuid::Uuid;

mod dialogs;
mod editor;
mod tree;

/// Full text of the GPL-3.0, shown in the About window (CR are dropped when shown: Windows
/// checkouts may have CRLF line endings).
const LICENSE: &str = include_str!("../LICENSE");

/// Everything about the window that survives a restart (eframe storage).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    dark: bool,
    quick_protocol: Protocol,
    sidebar_collapsed: bool,
    log_hidden: bool,
    /// Minutes of inactivity before the app locks itself (only with a master password; 0 = never).
    auto_lock_minutes: u32,
    /// Open RDP, SSH and VNC sessions as tabs inside the window (X11/XWayland).
    embed_sessions: bool,
    /// Windows: RDP in tabs with the Remote Desktop ActiveX control (mstsc's) instead of FreeRDP.
    rdp_activex: bool,
    /// Updater: check GitHub for a new version at start (at most once a day).
    update_auto_check: bool,
    /// Proxy for the updater, e.g. `http://proxy:3128` (empty = none).
    update_proxy: String,
    /// Unix time of the last automatic check.
    update_last_check: i64,
    /// Version the user chose to skip.
    update_skip: String,
    /// Connections whose sessions were open at the last exit, offered for reopening at start.
    reopen: Vec<Uuid>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: false,
            quick_protocol: Protocol::Rdp,
            sidebar_collapsed: false,
            log_hidden: false,
            auto_lock_minutes: 15,
            embed_sessions: true,
            rdp_activex: true,
            update_auto_check: true,
            update_proxy: String::new(),
            update_last_check: 0,
            update_skip: String::new(),
            reopen: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sel {
    Folder(Uuid),
    Connection(Uuid),
}

// Folder and Connection differ a lot in size; Editor is short-lived UI state (one instance,
// cloned only on edit), so boxing the larger variant would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq)]
enum Editor {
    Folder(Folder),
    Connection(Connection),
}

enum Action {
    Select(Sel),
    Connect(Uuid),
    /// Close the connection's existing session(s), then connect again (in-place reconnect).
    Reconnect(Uuid),
    /// Type the stored password and Enter into the connection's open RDP tab (unlocks a locked
    /// remote screen without reconnecting).
    Unlock(Uuid),
    /// `Unlock` for one session tab (by its session id).
    UnlockTab(u64),
    QuickConnect,
    NewFolder(Option<Uuid>),
    NewConnection(Option<Uuid>, Protocol),
    Duplicate(Uuid),
    AskDelete(Sel),
    Wake(Uuid),
    CheckPort(Uuid),
    /// Opens the properties dialog of a folder or connection (the original's "Edit").
    Edit(Sel),
    SetExpanded(Uuid, bool),
    /// Move a folder or connection into a folder (`None` = top level), via drag and drop.
    Move(Sel, Option<Uuid>),
    /// Copy the recorded history of a terminal session (by session id) to the clipboard.
    CopySessionLog(u64),
    /// Reconnect a tab whose session ended on its own (by its session id): drop the ended tab
    /// and connect its connection again.
    ReconnectTab(u64),
    /// Close a tab (by its session id), stopping its session if it is still running.
    CloseTab(u64),
}

/// Drag-and-drop state for the connection tree, collected while it is drawn.
#[derive(Default)]
struct TreeDnd {
    /// Off while searching or in the hover overlay.
    enabled: bool,
    /// The entry currently being dragged (this frame).
    dragged: Option<Sel>,
    /// The entry dropped (this frame).
    released: Option<Sel>,
    /// Drop zones in draw order: a row's rectangle and the folder a drop there targets
    /// (a folder header targets itself, a connection row its parent folder; `None` = top level).
    zones: Vec<(egui::Rect, Option<Uuid>)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Info,
    Warn,
    Error,
}

struct LogLine {
    level: Level,
    text: String,
}

struct Session {
    /// The connection and credentials (without password) it was started with, to ask again
    /// when the client needs other credentials.
    conn: Connection,
    cred: Option<Credential>,
    name: String,
    started: Instant,
    display: String,
    pid: u32,
    stdin: Option<std::process::ChildStdin>,
    /// Last lines of output, shown with prompts (certificate details).
    recent: VecDeque<String>,
    /// Ended by RC Manager (tab closed): the client's shutdown messages are not logged.
    closing: bool,
    /// Terminal sessions: file the whole session is recorded to, for "copy/save history".
    log: Option<PathBuf>,
}

/// A question of a client waiting on stdin, e.g. FreeRDP's certificate check.
struct PromptDialog {
    session: u64,
    name: String,
    question: String,
    details: Vec<String>,
    /// "host:port" and SHA-256 fingerprint of the certificate (from FreeRDP's details).
    certificate: Option<(String, String)>,
    /// FreeRDP reported that the certificate differs from the one trusted before.
    changed: bool,
}

/// Reads "Certificate details for 127.0.0.1:3390 (RDP-Server):" (or FreeRDP's
/// "!!!Certificate for 127.0.0.1:3390 (RDP-Server) has changed!!!") and the first
/// "Thumbprint: 56:30:…", which is the one of the certificate presented now.
fn certificate_of(details: &[String]) -> Option<(String, String)> {
    let host_port = details.iter().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("Certificate details for ").or_else(|| l.strip_prefix("!!!Certificate for "))
    })?;
    let host_port = host_port.split([' ', ':']).collect::<Vec<_>>();
    let host_port = format!("{}:{}", host_port.first()?, host_port.get(1)?.trim_end_matches(':'));
    let thumb = details.iter().find_map(|l| l.trim().strip_prefix("Thumbprint:"))?;
    let fp: String = thumb.trim().chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_lowercase();
    (fp.len() == 64).then_some((host_port, fp))
}

enum UpdateState {
    Checking,
    UpToDate,
    Available(crate::update::Release),
    Installing,
    Installed(PathBuf),
    Error(String),
}

struct UpdateDlg {
    state: UpdateState,
    /// Opened from the menu (an automatic check only shows a found update).
    manual: bool,
}

/// "HH:MM:SS UTC" of now.
fn clock() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) % 86_400;
    format!("{:02}:{:02}:{:02} UTC", s / 3600, s / 60 % 60, s % 60)
}

/// The continuous reachability check of one connection: a TCP connect to its port every two
/// seconds and, if available, the system's `ping`, until the dialog is closed.
struct Reachability {
    name: String,
    host: String,
    port: u16,
    lines: Vec<(Level, String)>,
    rx: Receiver<(Level, String)>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ping: Option<std::process::Child>,
}

impl Reachability {
    fn start(name: String, host: String, port: u16, ctx: egui::Context) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (tx, rx) = channel();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        {
            let (tx, stop, ctx, host) = (tx.clone(), stop.clone(), ctx.clone(), host.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let msg = match net::check_port(&host, port, Duration::from_secs(3)) {
                        Ok(t) => (Level::Info, format!("{} port {port}: reachable ({} ms)", clock(), t.as_millis())),
                        Err(e) => (Level::Warn, format!("{} port {port}: not reachable – {e}", clock())),
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    for _ in 0..20 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            });
        }
        // ICMP via the system's ping (it has the needed privileges); optional.
        let ping_exe = if cfg!(windows) { "ping.exe" } else { "ping" };
        let ping = launch::find_in_path(ping_exe).and_then(|p| {
            let mut cmd = std::process::Command::new(p);
            // Windows' ping stops after four packets unless -t is given.
            if cfg!(windows) {
                cmd.arg("-t");
            }
            launch::hide_console(&mut cmd)
                .arg(&host)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .ok()
        });
        let mut ping = ping;
        match ping.as_mut() {
            Some(child) => {
                for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as _)]
                    .into_iter()
                    .flatten()
                {
                    let (tx, ctx) = (tx.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                            if tx.send((Level::Info, format!("ping: {line}"))).is_err() {
                                break;
                            }
                            ctx.request_repaint();
                        }
                    });
                }
            }
            None => tx.send((Level::Info, "ping is not installed – only the port is checked.".into())).unwrap_or(()),
        }
        Self { name, host, port, lines: Vec::new(), rx, stop, ping }
    }

    fn poll(&mut self) {
        while let Ok(l) = self.rx.try_recv() {
            self.lines.push(l);
        }
        if self.lines.len() > 500 {
            self.lines.drain(..self.lines.len() - 500);
        }
    }

}

/// Closing the dialog (or the app) ends the checks and the ping process.
impl Drop for Reachability {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(mut p) = self.ping.take() {
            p.kill().ok();
            p.wait().ok();
        }
    }
}

/// Geometry last given to a host window: (x, y, width, height, visible) and the cut-out holes.
type Placement = ((i32, i32, u32, u32, bool), Vec<[i32; 4]>);

/// An unlock in progress (see `Action::Unlock`): the session first gets the keyboard focus.
struct Unlock {
    session: u64,
    password: String,
    started: Instant,
    /// When the session was given the focus.
    focused: Option<Instant>,
}

/// A session shown as a tab; its client draws into the window `host` (X11 or Win32).
struct SessionTab {
    session: u64,
    name: String,
    icon: &'static str,
    host: crate::embed::WinId,
    /// TigerVNC opens its own window, which is moved into the tab once it appears.
    adopt: Option<(u32, Instant)>,
    /// Last geometry given to the host (physical pixels) and whether it was visible.
    placed: Option<Placement>,
    last_fit: Instant,
    /// The host's size changed at this time; the client is told once the size has settled.
    resized: Option<Instant>,
    /// Last time the client was told its size (also repeated while a new session connects).
    size_told: Instant,
    /// When the tab was opened; the periodic refit only runs for a short while after this.
    opened: Instant,
    /// Set when the session ended on its own (host closed it, client crashed): the embedded
    /// window is gone and the tab shows a Reconnect button instead. Only RDP and VNC tabs are
    /// kept this way; a terminal tab closes when its session ends.
    ended: Option<EndedTab>,
}

/// State of a tab whose session ended on its own, kept open for reconnecting.
struct EndedTab {
    /// The connection to reconnect, if it still exists.
    conn: Option<Uuid>,
    /// Short reason shown in the tab (e.g. "exit code 1").
    reason: String,
}

/// Asks for credentials right before connecting.
struct AskCredentials {
    connection: Uuid,
    cred: Credential,
    save: bool,
    /// Why the dialog is shown again (e.g. the server requires a user name and password).
    note: Option<String>,
}

struct ImportDialog {
    path: PathBuf,
    app_password: String,
    /// Metadata when the file is a backup ZIP (or why it cannot be imported).
    backup: Option<Result<backup::BackupInfo, String>>,
}

struct RdpImportDialog {
    files: Vec<PathBuf>,
    folder: Option<Uuid>,
}

/// Which native file dialog to show.
#[derive(Clone, Copy)]
enum Pick {
    Database,
    RdpFiles,
    RdpFolder,
    /// Save dialog for exporting RC Manager's own database to a JSON file.
    ExportDb,
    /// Open dialog for importing an RC Manager export.
    ImportDb,
}

/// Result of a native file dialog; `None` when it was cancelled.
enum Picked {
    Database(PathBuf),
    Rdp(Vec<PathBuf>),
    ExportTo(PathBuf),
    ImportDb(PathBuf),
}

#[derive(Default)]
struct MasterDialog {
    pw1: String,
    pw2: String,
    error: String,
}

/// Column the credential list is sorted by.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum CredSort {
    #[default]
    Name,
    User,
    Domain,
    Used,
}

pub struct App {
    s: Settings,
    store: Store,
    data: Data,
    tools: Tools,
    locked: bool,
    unlock_input: String,
    unlock_error: String,

    selection: Option<Sel>,
    editor: Option<Editor>,
    editor_original: Option<Editor>,
    filter: String,
    quick_host: String,
    quick_pending: Option<Connection>,
    /// Collapsed sidebar temporarily shown as an overlay while hovered.
    sidebar_peek: bool,
    /// Warnings/errors logged while the log panel was hidden.
    log_unseen: usize,
    last_activity: Instant,
    /// Answer of a native file dialog running on its own thread.
    picker: Option<Receiver<Option<Picked>>>,

    /// X11 embedding of sessions; `None` without X11 or when switched off.
    embedder: Option<Embedder>,
    /// RDP sessions running in the ActiveX control (Windows), by session id.
    ax_sessions: HashMap<u64, crate::rdpax::RdpAx>,
    embed_checked: bool,
    tabs: Vec<SessionTab>,
    /// Session shown in the central area; `None` shows the connection properties.
    active_tab: Option<u64>,
    /// The properties dialog for the selected entry is open.
    props_open: bool,
    /// Updater: running check/download and its dialog.
    update_rx: Option<Receiver<crate::update::Msg>>,
    update: Option<UpdateDlg>,
    /// "Updater Proxy" dialog (edited value).
    update_proxy_edit: Option<String>,
    /// Windows: update script to run when RC Manager exits ("Later" in the update dialog).
    pending_update: Option<PathBuf>,
    /// AppImage to start once RC Manager has exited (restart after an update).
    restart_exe: Option<PathBuf>,
    /// Running reachability check (dialog).
    reach: Option<Reachability>,
    /// Central area of the active session tab (points), set while drawing.
    session_rect: Option<egui::Rect>,
    /// Session that has the keyboard focus.
    session_focus: Option<u64>,
    /// Last time the focus was handed back to a session (see `has_focus`).
    focus_checked: Instant,
    unlock: Option<Unlock>,
    /// Session name and result of the password being typed by `Unlock`.
    typing: Option<(String, crate::embed::Typed)>,
    /// Connections to open at start (`--connect`).
    pending_connect: Vec<String>,
    /// Sessions that were open at the last exit, offered for reopening once unlocked.
    reopen_offer: Vec<Uuid>,
    /// Which of the offered connections are ticked in the reopen dialog (None = not yet shown).
    reopen_sel: Option<HashSet<Uuid>>,
    prompts: VecDeque<PromptDialog>,

    log: VecDeque<LogLine>,
    sessions: BTreeMap<u64, Session>,
    next_session: u64,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    ctx: egui::Context,

    show_credentials: bool,
    cred_selected: Option<Uuid>,
    /// Working copy of the selected (or new) credentials in the credential manager.
    cred_edit: Option<Credential>,
    /// Credentials being created in a connection's properties (for that connection's id).
    new_cred: Option<(Uuid, Credential)>,
    /// `cred_edit` is a new set that is not stored yet.
    cred_new: bool,
    cred_filter: String,
    /// Filter text in the "assign to a connection" combo of the credential detail.
    cred_assign_filter: String,
    cred_show_password: bool,
    cred_confirm_delete: Option<Uuid>,
    /// Column the credential list is sorted by, and whether descending.
    cred_sort: CredSort,
    cred_sort_desc: bool,
    /// Show only credentials that no connection or folder uses (to find orphans).
    cred_only_unused: bool,
    /// Credentials ticked for a bulk delete.
    cred_checked: HashSet<Uuid>,
    /// A bulk delete of `cred_checked` is awaiting confirmation.
    cred_confirm_bulk: bool,
    ask_credentials: Option<AskCredentials>,
    confirm_delete: Option<Sel>,
    import_dialog: Option<ImportDialog>,
    rdp_import: Option<RdpImportDialog>,
    master_dialog: Option<MasterDialog>,
    show_tools: bool,
    show_about: bool,
    /// Warning shown before exporting the database (the file holds passwords in clear text).
    export_confirm: bool,
}

/// "YYYY-MM-DD HH:MM UTC" without pulling in a date library.
/// "[12:35:21:300] [4377:0000111f] [ERROR][com.freerdp.crypto] - [fn]: text" → "text"
fn strip_log_prefix(line: &str) -> String {
    match line.find("]: ") {
        Some(i) if line.starts_with('[') => line[i + 3..].to_string(),
        _ => line.to_string(),
    }
}

/// Strips ANSI escape sequences and control bytes from a recorded terminal line, so the plain
/// text of a `script` typescript can be scanned and shown.
fn clean_terminal_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                // CSI "ESC [ … <letter>": skip up to and including the final letter.
                Some('[') => {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if n.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                // Other escapes (e.g. "ESC ) 0"): drop the single following byte.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if c == '\r' || ((c as u32) < 0x20 && c != '\t') {
            continue;
        }
        out.push(c);
    }
    out
}

/// Known endings of an SSH/terminal session that never came up – a network or ssh error rather
/// than a normal logout. Kept specific so a clean logout ("Connection to host closed.") is not
/// mistaken for a failure.
const SSH_FAILURE_SIGNS: &[&str] = &[
    "ssh: connect to host",
    "connection refused",
    "could not resolve hostname",
    "name or service not known",
    "no address associated with hostname",
    "connection timed out",
    "operation timed out",
    "no route to host",
    "network is unreachable",
    "host key verification failed",
    "remote host identification has changed",
    "permission denied (",
    "permission denied, please try again",
    "too many authentication failures",
    "ssh_exchange_identification",
];

/// Reads the recorded log of a just-ended terminal (SSH) session and, if its last lines show the
/// connection could not be established, returns that error line to show in the tab. Returns None
/// for a session that connected normally (so its tab closes as before).
fn terminal_failure_reason(log: &std::path::Path) -> Option<String> {
    let data = std::fs::read(log).ok()?;
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<String> = text
        .lines()
        .map(clean_terminal_line)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with("Script started") && !l.starts_with("Script done"))
        .collect();
    // Only the last lines matter: when an ssh connect fails, ssh is the terminal's only process,
    // so its error is the final output before the window closes. Scanning deeper would match the
    // same words printed earlier in a session that then logged out cleanly.
    let hit = lines.iter().rev().take(3).find(|l| {
        let low = l.to_lowercase();
        SSH_FAILURE_SIGNS.iter().any(|s| low.contains(s))
    })?;
    // One tidy line for the tab.
    let mut reason = hit.clone();
    if reason.chars().count() > 200 {
        reason = reason.chars().take(199).collect::<String>() + "…";
    }
    Some(reason)
}

fn now_string() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, store: Store, tools: Tools, connect: Vec<String>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default();
        // Captured before `s` moves into the app; the sessions open at the last exit.
        let reopen_offer = s.reopen.clone();
        let (events_tx, events_rx) = channel();
        let locked = store.has_master_password().unwrap_or(false);
        let mut app = Self {
            s,
            store,
            data: Data::default(),
            tools,
            locked,
            unlock_input: String::new(),
            unlock_error: String::new(),
            selection: None,
            editor: None,
            editor_original: None,
            filter: String::new(),
            quick_host: String::new(),
            quick_pending: None,
            sidebar_peek: false,
            log_unseen: 0,
            last_activity: Instant::now(),
            picker: None,
            embedder: None,
            ax_sessions: HashMap::new(),
            embed_checked: false,
            tabs: Vec::new(),
            active_tab: None,
            props_open: false,
            update_rx: None,
            update: None,
            update_proxy_edit: None,
            restart_exe: None,
            pending_update: None,
            reach: None,
            session_rect: None,
            session_focus: None,
            focus_checked: Instant::now(),
            unlock: None,
            typing: None,
            pending_connect: connect,
            reopen_offer,
            reopen_sel: None,
            prompts: VecDeque::new(),
            log: VecDeque::new(),
            sessions: BTreeMap::new(),
            next_session: 1,
            events_tx,
            events_rx,
            ctx: cc.egui_ctx.clone(),
            show_credentials: false,
            cred_selected: None,
            cred_edit: None,
            new_cred: None,
            cred_new: false,
            cred_filter: String::new(),
            cred_assign_filter: String::new(),
            cred_show_password: false,
            cred_confirm_delete: None,
            cred_sort: CredSort::default(),
            cred_sort_desc: false,
            cred_only_unused: false,
            cred_checked: HashSet::new(),
            cred_confirm_bulk: false,
            ask_credentials: None,
            confirm_delete: None,
            import_dialog: None,
            rdp_import: None,
            master_dialog: None,
            show_tools: false,
            show_about: false,
            export_confirm: false,
        };
        if !locked {
            app.try_unlock(None);
        }
        // FreeRDP's configuration lives in temporary per-start directories below this one;
        // remove leftovers (and the store of 0.4.0, which FreeRDP could not read back).
        let freerdp_config = app.store.dir().join("freerdp");
        std::fs::remove_dir_all(&freerdp_config).ok();
        app.tools.freerdp_config = Some(freerdp_config);
        // Automatic update check (at most once a day, in the background).
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        if app.s.update_auto_check && std::env::var_os("RC_MANAGER_NO_UPDATE").is_none() && now - app.s.update_last_check >= 86_400 {
            app.s.update_last_check = now;
            app.start_update_check(false);
        }
        if app.tools.freerdp.is_none() {
            app.push(Level::Warn, "FreeRDP not found – RDP connections need it installed (Options → External Programs).");
        }
        let missing = launch::bundle_problems();
        if !missing.is_empty() {
            app.push(
                Level::Warn,
                format!("The bundled clients cannot start: this system lacks {} (Options → External Programs).", missing.join(", ")),
            );
        }
        app
    }

    fn push(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        if level != Level::Info {
            eprintln!("{text}");
            if self.s.log_hidden {
                self.log_unseen += 1;
            }
        }
        self.log.push_back(LogLine { level, text });
        while self.log.len() > 500 {
            self.log.pop_front();
        }
    }

    fn try_unlock(&mut self, pw: Option<&str>) {
        match self.store.unlock(pw).and_then(|_| self.store.load()) {
            Ok(data) => {
                self.data = data;
                self.locked = false;
                self.last_activity = Instant::now();
                self.unlock_input.clear();
                self.unlock_error.clear();
                let n = self.data.connections.len();
                self.push(Level::Info, format!("{n} connection(s) loaded."));
            }
            Err(e) => self.unlock_error = e.to_string(),
        }
    }

    fn result<T>(&mut self, r: anyhow::Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.push(Level::Error, format!("Error: {e:#}"));
                None
            }
        }
    }

    // ---------------------------------------------------------------- data

    fn folder(&self, id: Uuid) -> Option<&Folder> {
        self.data.folders.iter().find(|f| f.id == id)
    }

    fn connection(&self, id: Uuid) -> Option<&Connection> {
        self.data.connections.iter().find(|c| c.id == id)
    }

    fn credential(&self, id: Uuid) -> Option<&Credential> {
        self.data.credentials.iter().find(|c| c.id == id)
    }

    /// Starts `Action::Unlock` for a connection's session tab (`session`, or its first open one).
    fn start_unlock(&mut self, id: Uuid, session: Option<u64>) {
        let Some(conn) = self.connection(id).cloned() else { return };
        let password = self.resolve_credential(&conn).map(|c| c.password).filter(|p| !p.is_empty());
        let session = session.filter(|s| self.tabs.iter().any(|t| t.session == *s && t.ended.is_none())).or_else(|| self.unlock_tab(id));
        match (session, password) {
            (Some(session), Some(password)) => {
                self.active_tab = Some(session);
                self.unlock = Some(Unlock { session, password, started: Instant::now(), focused: None });
            }
            _ => self.push(Level::Warn, format!("{}: unlocking needs an open session tab and a stored password.", conn.name)),
        }
    }

    /// The open (embedded, running) session tab of a connection.
    fn unlock_tab(&self, conn: Uuid) -> Option<u64> {
        self.tabs
            .iter()
            .find(|t| t.ended.is_none() && self.sessions.get(&t.session).is_some_and(|s| s.conn.id == conn))
            .map(|t| t.session)
    }

    /// Resolves credentials; `Inherit` walks up the folder tree.
    fn resolve_credential(&self, conn: &Connection) -> Option<Credential> {
        match conn.credential {
            CredentialRef::None => None,
            CredentialRef::Id(id) => self.credential(id).cloned(),
            CredentialRef::Inherit => self.inherited_credential(conn.parent),
        }
    }

    /// Resolves the credential a folder chain provides, starting at `folder` and walking up while
    /// a folder inherits. Used for `CredentialRef::Inherit` on both connections and folders.
    fn inherited_credential(&self, folder: Option<Uuid>) -> Option<Credential> {
        let mut cur = folder;
        for _ in 0..256 {
            let f = self.folder(cur?)?;
            match f.credential {
                CredentialRef::Id(id) => return self.credential(id).cloned(),
                CredentialRef::Inherit => cur = f.parent,
                CredentialRef::None => return None,
            }
        }
        None
    }

    fn folder_path(&self, mut id: Option<Uuid>) -> String {
        let mut parts = Vec::new();
        for _ in 0..64 {
            let Some(f) = id.and_then(|i| self.folder(i)) else { break };
            parts.push(f.name.clone());
            id = f.parent;
        }
        parts.reverse();
        parts.join(" / ")
    }

    fn children_folders(&self, parent: Option<Uuid>) -> Vec<&Folder> {
        let mut v: Vec<&Folder> = self.data.folders.iter().filter(|f| f.parent == parent).collect();
        v.sort_by(|a, b| a.sorting.cmp(&b.sorting).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }

    fn children_connections(&self, parent: Option<Uuid>) -> Vec<&Connection> {
        let mut v: Vec<&Connection> = self.data.connections.iter().filter(|c| c.parent == parent).collect();
        v.sort_by(|a, b| a.sorting.cmp(&b.sorting).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }

    /// All folders for drop-downs, indented by depth; `exclude` hides a subtree.
    fn folder_choices(&self, exclude: Option<Uuid>) -> Vec<(Option<Uuid>, String)> {
        fn walk(app: &App, parent: Option<Uuid>, depth: usize, exclude: Option<Uuid>, out: &mut Vec<(Option<Uuid>, String)>) {
            for f in app.children_folders(parent) {
                if Some(f.id) == exclude {
                    continue;
                }
                out.push((Some(f.id), format!("{}{}", "   ".repeat(depth), f.name)));
                walk(app, Some(f.id), depth + 1, exclude, out);
            }
        }
        let mut out = vec![(None, "(top level)".to_string())];
        walk(self, None, 1, exclude, &mut out);
        out
    }

    fn descendants(&self, id: Uuid) -> (Vec<Uuid>, Vec<Uuid>) {
        let mut folders = vec![id];
        let mut i = 0;
        while i < folders.len() {
            let cur = folders[i];
            folders.extend(self.data.folders.iter().filter(|f| f.parent == Some(cur)).map(|f| f.id));
            i += 1;
        }
        let conns = self.data.connections.iter().filter(|c| c.parent.map(|p| folders.contains(&p)).unwrap_or(false)).map(|c| c.id).collect();
        (folders, conns)
    }

    fn current_folder(&self) -> Option<Uuid> {
        match self.selection {
            Some(Sel::Folder(id)) => Some(id),
            Some(Sel::Connection(id)) => self.connection(id).and_then(|c| c.parent),
            None => None,
        }
    }

    // ------------------------------------------------------------- editing

    fn editor_dirty(&self) -> bool {
        self.editor != self.editor_original
    }

    fn save_editor(&mut self) {
        let Some(ed) = self.editor.clone() else { return };
        if !self.editor_dirty() {
            return;
        }
        match ed.clone() {
            Editor::Folder(f) => {
                if self.result(self.store.save_folder(&f)).is_some() {
                    match self.data.folders.iter_mut().find(|x| x.id == f.id) {
                        Some(x) => *x = f,
                        None => self.data.folders.push(f),
                    }
                }
            }
            Editor::Connection(c) => {
                if self.result(self.store.save_connection(&c)).is_some() {
                    match self.data.connections.iter_mut().find(|x| x.id == c.id) {
                        Some(x) => *x = c,
                        None => self.data.connections.push(c),
                    }
                }
            }
        }
        self.editor_original = Some(ed);
    }

    fn select(&mut self, sel: Option<Sel>) {
        if self.editor_dirty() {
            self.save_editor();
        }
        self.selection = sel;
        self.editor = match sel {
            Some(Sel::Folder(id)) => self.folder(id).cloned().map(Editor::Folder),
            Some(Sel::Connection(id)) => self.connection(id).cloned().map(Editor::Connection),
            None => None,
        };
        self.editor_original = self.editor.clone();
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Select(s) => self.select(Some(s)),
            Action::Edit(s) => {
                self.select(Some(s));
                self.props_open = self.editor.is_some();
            }
            Action::Connect(id) => {
                self.select(Some(Sel::Connection(id)));
                self.connect(id);
            }
            Action::Reconnect(id) => {
                // Close the running session(s) for this connection first (cleanly, so the
                // server-initiated takeover is not logged as an error), then connect again.
                let open: Vec<u64> = self.sessions.iter().filter(|(_, s)| s.conn.id == id).map(|(sid, _)| *sid).collect();
                for sid in open {
                    self.stop_session(sid);
                }
                self.select(Some(Sel::Connection(id)));
                self.connect(id);
            }
            Action::ReconnectTab(session) => {
                let conn = self.tabs.iter().find(|t| t.session == session).and_then(|t| t.ended.as_ref()).and_then(|e| e.conn);
                self.remove_tab(session);
                if let Some(id) = conn {
                    self.select(Some(Sel::Connection(id)));
                    self.connect(id);
                }
            }
            Action::CloseTab(session) => {
                if self.sessions.contains_key(&session) {
                    self.stop_session(session);
                } else {
                    self.remove_tab(session);
                }
            }
            Action::QuickConnect => self.quick_connect(),
            Action::NewFolder(parent) => {
                let f = Folder::new("New Folder", parent);
                if self.result(self.store.save_folder(&f)).is_some() {
                    let id = f.id;
                    self.data.folders.push(f);
                    if let Some(p) = parent {
                        self.set_expanded(p, true);
                    }
                    self.select(Some(Sel::Folder(id)));
                    self.props_open = true;
                }
            }
            Action::NewConnection(parent, protocol) => {
                let c = Connection::new("New Connection", protocol, parent);
                if self.result(self.store.save_connection(&c)).is_some() {
                    let id = c.id;
                    self.data.connections.push(c);
                    if let Some(p) = parent {
                        self.set_expanded(p, true);
                    }
                    self.select(Some(Sel::Connection(id)));
                    self.props_open = true;
                }
            }
            Action::Duplicate(id) => {
                if let Some(mut c) = self.connection(id).cloned() {
                    c.id = Uuid::new_v4();
                    c.name = format!("{} (Copy)", c.name);
                    c.last_connected = None;
                    if self.result(self.store.save_connection(&c)).is_some() {
                        let nid = c.id;
                        self.data.connections.push(c);
                        self.select(Some(Sel::Connection(nid)));
                        self.props_open = true;
                    }
                }
            }
            Action::AskDelete(s) => self.confirm_delete = Some(s),
            Action::Unlock(id) => self.start_unlock(id, None),
            Action::UnlockTab(session) => {
                if let Some(id) = self.sessions.get(&session).map(|s| s.conn.id) {
                    self.start_unlock(id, Some(session));
                }
            }
            Action::Wake(id) => {
                if let Some(c) = self.connection(id).cloned() {
                    match net::wake_on_lan(&c.mac) {
                        Ok(()) => self.push(Level::Info, format!("Wake-on-LAN packet sent to {} ({}).", c.name, c.mac)),
                        Err(e) => self.push(Level::Error, format!("Wake-on-LAN for {}: {e}", c.name)),
                    }
                }
            }
            Action::CheckPort(id) => {
                if let Some(c) = self.connection(id).cloned() {
                    let port = if c.protocol == Protocol::Web && c.port == 0 && c.host.starts_with("http://") { 80 } else { c.effective_port() };
                    let host = if c.protocol == Protocol::Web {
                        c.host.split("://").last().unwrap_or("").split(['/', ':']).next().unwrap_or("").to_string()
                    } else {
                        c.host.trim().to_string()
                    };
                    self.reach = Some(Reachability::start(c.name.clone(), host, port, self.ctx.clone()));
                }
            }
            Action::SetExpanded(id, open) => self.set_expanded(id, open),
            Action::Move(sel, parent) => self.move_item(sel, parent),
            Action::CopySessionLog(session) => {
                let Some(log) = self.sessions.get(&session).and_then(|s| s.log.clone()) else { return };
                match std::fs::read(&log) {
                    Ok(raw) => {
                        let text = launch::clean_typescript(&raw);
                        let lines = text.lines().count();
                        self.ctx.copy_text(text);
                        self.push(Level::Info, format!("Session history copied to the clipboard ({lines} line(s))."));
                    }
                    Err(e) => self.push(Level::Warn, format!("Cannot read the session history: {e}")),
                }
            }
        }
    }

    /// Moves a folder or connection into `parent` (`None` = top level), persists it and keeps
    /// the open editor in sync. Rejects moving a folder into itself or one of its descendants.
    fn move_item(&mut self, sel: Sel, parent: Option<Uuid>) {
        match sel {
            Sel::Connection(id) => {
                let Some(mut c) = self.connection(id).cloned() else { return };
                if c.parent == parent {
                    return;
                }
                c.parent = parent;
                if self.result(self.store.save_connection(&c)).is_some() {
                    if let Some(x) = self.data.connections.iter_mut().find(|x| x.id == id) {
                        *x = c;
                    }
                }
            }
            Sel::Folder(id) => {
                if parent == Some(id) {
                    return;
                }
                // A folder must not move into its own subtree.
                let (descendants, _) = self.descendants(id);
                if parent.is_some_and(|p| descendants.contains(&p)) {
                    return;
                }
                let Some(mut f) = self.folder(id).cloned() else { return };
                if f.parent == parent {
                    return;
                }
                f.parent = parent;
                if self.result(self.store.save_folder(&f)).is_some() {
                    if let Some(x) = self.data.folders.iter_mut().find(|x| x.id == id) {
                        *x = f;
                    }
                }
            }
        }
        if let Some(p) = parent {
            self.set_expanded(p, true);
        }
        // Reflect the new parent in the open properties editor.
        for ed in [&mut self.editor, &mut self.editor_original] {
            match (ed, sel) {
                (Some(Editor::Connection(e)), Sel::Connection(id)) if e.id == id => e.parent = parent,
                (Some(Editor::Folder(e)), Sel::Folder(id)) if e.id == id => e.parent = parent,
                _ => {}
            }
        }
    }

    fn set_expanded(&mut self, id: Uuid, open: bool) {
        let Some(f) = self.data.folders.iter_mut().find(|f| f.id == id) else { return };
        if f.expanded == open {
            return;
        }
        f.expanded = open;
        let f = f.clone();
        self.result(self.store.save_folder(&f));
        for ed in [&mut self.editor, &mut self.editor_original] {
            if let Some(Editor::Folder(e)) = ed {
                if e.id == id {
                    e.expanded = open;
                }
            }
        }
    }

    fn delete(&mut self, sel: Sel) {
        match sel {
            Sel::Connection(id) => {
                if self.result(self.store.delete_connection(id)).is_some() {
                    self.data.connections.retain(|c| c.id != id);
                }
            }
            Sel::Folder(id) => {
                let (folders, conns) = self.descendants(id);
                for c in &conns {
                    self.result(self.store.delete_connection(*c));
                }
                for f in &folders {
                    self.result(self.store.delete_folder(*f));
                }
                self.data.connections.retain(|c| !conns.contains(&c.id));
                self.data.folders.retain(|f| !folders.contains(&f.id));
            }
        }
        let gone = match self.selection {
            Some(Sel::Connection(id)) => self.connection(id).is_none(),
            Some(Sel::Folder(id)) => self.folder(id).is_none(),
            None => false,
        };
        if gone {
            self.editor_original = self.editor.clone();
            self.select(None);
        }
    }

    // ---------------------------------------------------------- connecting

    fn connect(&mut self, id: Uuid) {
        if self.editor_dirty() {
            self.save_editor();
        }
        let Some(conn) = self.connection(id).cloned() else { return };
        let cred = self.resolve_credential(&conn);
        let needs_password = matches!(conn.protocol, Protocol::Rdp | Protocol::Vnc);
        if needs_password && cred.as_ref().map(|c| c.password.is_empty()).unwrap_or(true) {
            let base = cred.unwrap_or_else(|| Credential::new(&conn.name));
            self.ask_credentials = Some(AskCredentials { connection: id, cred: Credential { password: String::new(), ..base }, save: false, note: None });
            return;
        }
        self.launch(&conn, cred.as_ref());
    }

    /// Creates the tab window for a session that should be embedded.
    fn embed_target(&mut self, conn: &Connection) -> Option<EmbedTarget> {
        if !self.s.embed_sessions {
            return None;
        }
        let emb = self.embedder.as_ref()?;
        let wanted = match conn.protocol {
            Protocol::Rdp => conn.options.rdp.window_mode == WindowMode::Tab,
            Protocol::Ssh => self.tools.ssh_embeddable(),
            Protocol::Vnc => self.tools.vncviewer.is_some() && emb.can_adopt(),
            Protocol::Web | Protocol::External => false,
        };
        if !wanted {
            return None;
        }
        let host = match emb.create_host() {
            Ok(h) => h,
            Err(e) => {
                self.push(Level::Warn, format!("Cannot create a tab window, opening a separate window: {e:#}"));
                return None;
            }
        };
        // Size of the session area: the last one drawn, or the central panel's.
        let ppp = self.ctx.pixels_per_point();
        let rect = self.session_rect.unwrap_or_else(|| self.ctx.available_rect());
        Some(EmbedTarget { host, width: (rect.width() * ppp) as u32, height: (rect.height() * ppp) as u32, dark: self.s.dark })
    }

    fn launch(&mut self, conn: &Connection, cred: Option<&Credential>) {
        let embed = self.embed_target(conn);
        // Windows: RDP in a tab with the ActiveX control; FreeRDP if it is off or not available.
        if let Some(t) = embed.as_ref().filter(|_| cfg!(windows) && self.s.rdp_activex && conn.protocol == Protocol::Rdp) {
            if !conn.options.rdp.remote_app.trim().is_empty() {
                self.push(Level::Info, format!("{}: RemoteApp runs with FreeRDP.", conn.name));
            } else {
                match crate::rdpax::RdpAx::connect(t.host, t.width, t.height, conn, cred) {
                    Ok(ax) => {
                        if !conn.options.rdp.extra_args.trim().is_empty() {
                            self.push(Level::Warn, format!("{}: the extra FreeRDP arguments are ignored by the Windows RDP client.", conn.name));
                        }
                        self.start_ax_session(conn, cred, t.host, ax);
                        return;
                    }
                    Err(e) => self.push(Level::Warn, format!("{}: Windows RDP client not available ({e:#}), using FreeRDP.", conn.name)),
                }
            }
        }
        let fingerprint = if conn.protocol == Protocol::Rdp {
            let host_port = format!("{}:{}", conn.host.trim(), conn.effective_port());
            self.store.trusted_fingerprint(&host_port).ok().flatten()
        } else {
            None
        };
        let plan = match launch::plan(conn, cred, &self.tools, embed.as_ref(), fingerprint.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                if let (Some(emb), Some(t)) = (&self.embedder, &embed) {
                    emb.destroy(t.host);
                }
                self.push(Level::Error, format!("{}: {e:#}", conn.name));
                return;
            }
        };
        for w in &plan.warnings {
            self.push(Level::Warn, format!("{}: {w}", conn.name));
        }
        let session = self.next_session;
        self.next_session += 1;
        let ctx = self.ctx.clone();
        match launch::spawn(&plan, session, self.events_tx.clone(), move || ctx.request_repaint()) {
            Ok(spawned) => {
                let pid = spawned.pid;
                self.push(Level::Info, format!("[{session}] {} started: {}", conn.name, plan.display));
                self.sessions.insert(
                    session,
                    Session {
                        conn: conn.clone(),
                        cred: cred.map(|c| Credential { password: String::new(), ..c.clone() }),
                        name: conn.name.clone(),
                        started: Instant::now(),
                        display: plan.display.clone(),
                        pid,
                        stdin: spawned.stdin,
                        recent: VecDeque::new(),
                        closing: false,
                        log: plan.session_log.clone(),
                    },
                );
                if let Some(t) = embed {
                    self.tabs.push(SessionTab {
                        session,
                        name: conn.name.clone(),
                        icon: conn.protocol.icon(),
                        host: t.host,
                        adopt: plan.adopt_window.then(|| (pid, Instant::now())),
                        placed: None,
                        last_fit: Instant::now(),
                        resized: None,
                        size_told: Instant::now(),
                        opened: Instant::now(),
                        ended: None,
                    });
                    self.active_tab = Some(session);
                    self.session_focus = None;
                }
                if let Some(mut c) = self.connection(conn.id).cloned() {
                    c.last_connected = Some(now_string());
                    self.result(self.store.save_connection(&c));
                    for ed in [&mut self.editor, &mut self.editor_original] {
                        if let Some(Editor::Connection(e)) = ed {
                            if e.id == c.id {
                                e.last_connected = c.last_connected.clone();
                            }
                        }
                    }
                    if let Some(x) = self.data.connections.iter_mut().find(|x| x.id == c.id) {
                        *x = c;
                    }
                }
            }
            Err(e) => {
                if let (Some(emb), Some(t)) = (&self.embedder, &embed) {
                    emb.destroy(t.host);
                }
                self.push(Level::Error, format!("{}: {e:#}", conn.name))
            }
        }
    }

    /// Registers an RDP session running in the ActiveX control and opens its tab.
    fn start_ax_session(&mut self, conn: &Connection, cred: Option<&Credential>, host: crate::embed::WinId, ax: crate::rdpax::RdpAx) {
        let session = self.next_session;
        self.next_session += 1;
        self.push(Level::Info, format!("[{session}] {} started with the Windows RDP client.", conn.name));
        self.sessions.insert(
            session,
            Session {
                conn: conn.clone(),
                cred: cred.map(|c| Credential { password: String::new(), ..c.clone() }),
                name: conn.name.clone(),
                started: Instant::now(),
                display: "Windows RDP client (ActiveX)".into(),
                pid: 0,
                stdin: None,
                recent: VecDeque::new(),
                closing: false,
                log: None,
            },
        );
        self.ax_sessions.insert(session, ax);
        self.tabs.push(SessionTab {
            session,
            name: conn.name.clone(),
            icon: conn.protocol.icon(),
            host,
            adopt: None,
            placed: None,
            last_fit: Instant::now(),
            resized: None,
            size_told: Instant::now(),
            opened: Instant::now(),
            ended: None,
        });
        self.active_tab = Some(session);
        self.session_focus = None;
        if let Some(mut c) = self.connection(conn.id).cloned() {
            c.last_connected = Some(now_string());
            self.result(self.store.save_connection(&c));
            if let Some(x) = self.data.connections.iter_mut().find(|x| x.id == c.id) {
                *x = c;
            }
        }
    }

    /// Reports ActiveX sessions that have ended, like a client process that exited.
    fn poll_ax_sessions(&mut self) {
        let notes: Vec<(u64, String)> = self.ax_sessions.iter().filter_map(|(s, ax)| ax.note().map(|n| (*s, n))).collect();
        for (session, note) in notes {
            let name = self.sessions.get(&session).map(|s| s.name.clone()).unwrap_or_default();
            self.push(Level::Warn, format!("[{session}] {name}: {note}"));
        }
        let ended: Vec<(u64, String)> = self.ax_sessions.iter().filter_map(|(s, ax)| ax.poll().map(|r| (*s, r))).collect();
        for (session, reason) in ended {
            // Removing it closes the control's window.
            if self.ax_sessions.remove(&session).is_some() {
                self.events_tx.send(Event::Exited { session, code: None, reason: Some(reason) }).ok();
            }
        }
    }

    fn quick_connect(&mut self) {
        let host = self.quick_host.trim().to_string();
        if host.is_empty() {
            return;
        }
        let mut c = Connection::new(&host, self.s.quick_protocol, None);
        if let Some((h, p)) = host.rsplit_once(':').filter(|(h, p)| !h.contains(':') && !h.contains('/') && p.parse::<u16>().is_ok()) {
            c.host = h.to_string();
            c.port = p.parse().unwrap();
        } else {
            c.host = host;
        }
        if matches!(c.protocol, Protocol::Rdp | Protocol::Vnc) {
            self.ask_credentials = Some(AskCredentials { connection: Uuid::nil(), cred: Credential::new(&c.host), save: false, note: None });
            self.quick_pending = Some(c);
        } else {
            self.launch(&c, None);
        }
    }

    fn poll_events(&mut self) {
        self.poll_ax_sessions();
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                Event::Prompt { session, text } if launch::is_credential_prompt(&text) => {
                    // FreeRDP would wait on its console forever: end this attempt and ask in
                    // RC Manager's logon dialog instead.
                    if let Some(s) = self.sessions.get(&session) {
                        let (conn, cred) = (s.conn.clone(), s.cred.clone());
                        self.push(Level::Warn, format!("{}: the server requires a user name and password ({}).", conn.name, text.trim()));
                        self.stop_session(session);
                        let stored = self.connection(conn.id).is_some();
                        if !stored {
                            self.quick_pending = Some(conn.clone());
                        }
                        self.ask_credentials = Some(AskCredentials {
                            connection: if stored { conn.id } else { Uuid::nil() },
                            cred: cred.unwrap_or_else(|| Credential::new(&conn.name)),
                            save: false,
                            note: Some("The server requires a user name and password for this connection.".into()),
                        });
                    }
                }
                Event::Prompt { session, text } => {
                    if let Some(s) = self.sessions.get(&session) {
                        // The details block starts with "Certificate details for host:port" (or
                        // "!!!Certificate for host:port … has changed!!!").
                        let start = s
                            .recent
                            .iter()
                            .rposition(|l| l.contains("Certificate details for") || l.contains("!!!Certificate for"))
                            .unwrap_or(s.recent.len().saturating_sub(12));
                        let details: Vec<String> = s.recent.iter().skip(start).map(|l| strip_log_prefix(l)).filter(|l| !l.trim().is_empty() && !l.contains("@@@@")).collect();
                        let certificate = certificate_of(&details);
                        // "Changed" only if RC Manager trusted a different certificate for this
                        // server before (FreeRDP's own store is not used and not reliable).
                        let changed = certificate.as_ref().is_some_and(|(hp, fp)| {
                            self.store.trusted_fingerprint(hp).ok().flatten().is_some_and(|known| &known != fp)
                        });
                        self.prompts.push_back(PromptDialog { session, name: s.name.clone(), question: text, details, certificate, changed });
                    }
                }
                Event::Output { session, line } => {
                    // FreeRDP reports being stopped as errors ("Caught signal 'Terminated'",
                    // ERRCONNECT_CONNECT_CANCELLED); for a closed tab that is no news.
                    if self.sessions.get(&session).is_some_and(|s| s.closing) {
                        continue;
                    }
                    if let Some(s) = self.sessions.get_mut(&session) {
                        s.recent.push_back(line.clone());
                        while s.recent.len() > 40 {
                            s.recent.pop_front();
                        }
                    }
                    let name = self.sessions.get(&session).map(|s| s.name.clone()).unwrap_or_default();
                    let lvl = if line.contains("ERROR") { Level::Error } else if line.contains("WARN") { Level::Warn } else { Level::Info };
                    self.push(lvl, format!("[{session}] {name}: {line}"));
                }
                Event::Exited { session, code, reason: why } => {
                    if let Some(s) = self.sessions.remove(&session) {
                        // A terminal (SSH) session that never connected: read the ssh error from its
                        // recorded log before it is deleted, so the tab can show why. If nothing was
                        // recorded but it ended within seconds, it still almost certainly failed to
                        // connect, so show a generic hint rather than letting the tab vanish.
                        let terminal_fail = if matches!(s.conn.protocol, Protocol::Ssh) && !s.closing {
                            s.log.as_deref().and_then(terminal_failure_reason).or_else(|| {
                                (s.started.elapsed() < Duration::from_secs(3)).then(|| "The SSH session could not be established.".to_string())
                            })
                        } else {
                            None
                        };
                        if let Some(log) = &s.log {
                            std::fs::remove_file(log).ok();
                        }
                        let dur = s.started.elapsed().as_secs();
                        if s.closing {
                            self.push(Level::Info, format!("[{session}] {} closed ({}m {}s).", s.name, dur / 60, dur % 60));
                            self.remove_tab(session);
                            self.prompts.retain(|p| p.session != session);
                            continue;
                        }
                        let code = code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
                        if let Some(reason) = &terminal_fail {
                            self.push(Level::Warn, format!("[{session}] {} could not connect: {reason}", s.name));
                        } else if let Some(why) = &why {
                            self.push(Level::Warn, format!("[{session}] {} ended: {why} ({}m {}s).", s.name, dur / 60, dur % 60));
                        } else {
                            self.push(Level::Warn, format!("[{session}] {} ended (exit code {code}, {}m {}s).", s.name, dur / 60, dur % 60));
                        }
                        // RDP/VNC drop on network loss or a server-side logoff, and a terminal (SSH)
                        // session that never connected ends at once: keep the tab with the reason and
                        // a Reconnect button instead of letting it vanish. A terminal session that was
                        // logged out of (no failure in its log) closes as before.
                        let reason = if matches!(s.conn.protocol, Protocol::Rdp | Protocol::Vnc) {
                            Some(why.unwrap_or_else(|| format!("exit code {code}")))
                        } else {
                            terminal_fail
                        };
                        let keep = reason.is_some() && self.tabs.iter().any(|t| t.session == session);
                        if keep {
                            let reason = reason.unwrap_or_default();
                            let host = self.tabs.iter().find(|t| t.session == session).map(|t| t.host);
                            if let Some(emb) = &self.embedder {
                                if let Some(host) = host.filter(|h| *h != 0) {
                                    emb.destroy(host);
                                }
                                if self.session_focus == Some(session) {
                                    emb.focus(None);
                                }
                            }
                            if self.session_focus == Some(session) {
                                self.session_focus = None;
                            }
                            if let Some(tab) = self.tabs.iter_mut().find(|t| t.session == session) {
                                tab.host = 0;
                                tab.adopt = None;
                                tab.placed = None;
                                tab.ended = Some(EndedTab { conn: Some(s.conn.id), reason });
                            }
                            self.prompts.retain(|p| p.session != session);
                            continue;
                        }
                    }
                    self.remove_tab(session);
                    self.prompts.retain(|p| p.session != session);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ UI

fn new_connection_menu(ui: &mut egui::Ui, parent: Option<Uuid>, actions: &mut Vec<Action>) {
    ui.menu_button("➕ New Connection", |ui| {
        for p in Protocol::ALL {
            if ui.button(format!("{} {}", p.icon(), p.label())).clicked() {
                actions.push(Action::NewConnection(parent, p));
                ui.close_menu();
            }
        }
    });
}

impl App {

    fn set_log_hidden(&mut self, hidden: bool) {
        self.s.log_hidden = hidden;
        self.log_unseen = 0;
    }

    fn sessions_summary(&self, ui: &mut egui::Ui) {
        ui.strong(format!("Active sessions: {}", self.sessions.len()));
        for (id, s) in &self.sessions {
            let secs = s.started.elapsed().as_secs();
            ui.label(format!("[{id}] {} ({}:{:02})", s.name, secs / 60, secs % 60)).on_hover_text(&s.display);
        }
    }

    fn ui_bottom(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::L)) {
            self.set_log_hidden(!self.s.log_hidden);
        }
        if !self.sessions.is_empty() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        if self.s.log_hidden {
            // Only a status line remains: sessions, unseen problems and the way back.
            egui::TopBottomPanel::bottom("status").resizable(false).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    self.sessions_summary(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("▲ Show log").on_hover_text("Ctrl+L").clicked() {
                            self.set_log_hidden(false);
                        }
                        if self.log_unseen > 0 {
                            let text = RichText::new(format!("⚠ {} new warning(s)", self.log_unseen)).color(ui.visuals().warn_fg_color);
                            if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text("Show log").clicked() {
                                self.set_log_hidden(false);
                            }
                        }
                    });
                });
            });
            return;
        }
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(150.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                self.sessions_summary(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("▼ Hide log").on_hover_text("Ctrl+L").clicked() {
                        self.set_log_hidden(true);
                    }
                    if ui.small_button("Clear log").clicked() {
                        self.log.clear();
                    }
                });
            });
            ui.add_space(2.0);
            theme::list_frame(ui, |ui| {
                ui.set_min_height(ui.available_height() - 6.0);
                egui::ScrollArea::vertical().auto_shrink([false; 2]).stick_to_bottom(true).show(ui, |ui| {
                    for l in &self.log {
                        let color = match l.level {
                            Level::Info => ui.visuals().text_color(),
                            Level::Warn => ui.visuals().warn_fg_color,
                            Level::Error => ui.visuals().error_fg_color,
                        };
                        ui.label(RichText::new(&l.text).monospace().color(color));
                    }
                });
            });
        });
    }

    fn start_update_check(&mut self, manual: bool) {
        if self.update_rx.is_some() {
            if manual {
                self.update.get_or_insert(UpdateDlg { state: UpdateState::Checking, manual });
            }
            return;
        }
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        crate::update::spawn_check(self.s.update_proxy.clone(), tx, move || ctx.request_repaint());
        self.update_rx = Some(rx);
        if manual {
            self.update = Some(UpdateDlg { state: UpdateState::Checking, manual });
        }
    }

    fn start_update_install(&mut self, rel: crate::update::Release) {
        let Some(target) = crate::update::update_target() else { return };
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        crate::update::spawn_install(rel, target, self.s.update_proxy.clone(), tx, move || ctx.request_repaint());
        self.update_rx = Some(rx);
        self.update = Some(UpdateDlg { state: UpdateState::Installing, manual: true });
    }

    fn poll_update(&mut self) {
        let Some(rx) = &self.update_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.update_rx = None;
        let manual = self.update.as_ref().is_some_and(|u| u.manual);
        let state = match msg {
            crate::update::Msg::Checked(Ok(Some(rel))) if manual || rel.version != self.s.update_skip => {
                self.push(Level::Info, format!("RC Manager {} is available.", rel.version));
                Some(UpdateState::Available(rel))
            }
            crate::update::Msg::Checked(Ok(_)) => manual.then_some(UpdateState::UpToDate),
            crate::update::Msg::Checked(Err(e)) => {
                if !manual {
                    self.push(Level::Warn, format!("Update check failed: {e}"));
                }
                manual.then_some(UpdateState::Error(e))
            }
            crate::update::Msg::Installed(Ok(p)) => {
                self.push(Level::Info, format!("Update installed: {}", p.display()));
                Some(UpdateState::Installed(p))
            }
            crate::update::Msg::Installed(Err(e)) => Some(UpdateState::Error(e)),
        };
        self.update = state.map(|state| UpdateDlg { state, manual });
    }

    fn open_url(&mut self, url: &str) {
        match &self.tools.opener {
            Some(o) => {
                if let Err(e) = std::process::Command::new(o).arg(url).spawn() {
                    self.push(Level::Warn, format!("Cannot open {url}: {e}"));
                }
            }
            None => self.push(Level::Warn, format!("No browser found – open {url} yourself.")),
        }
    }


    fn remove_tab(&mut self, session: u64) {
        if let Some(pos) = self.tabs.iter().position(|t| t.session == session) {
            let tab = self.tabs.remove(pos);
            if let Some(emb) = &self.embedder {
                // An ended tab already had its window destroyed (host set to 0).
                if tab.host != 0 {
                    emb.destroy(tab.host);
                }
                if self.session_focus == Some(session) {
                    emb.focus(None);
                }
            }
            if self.session_focus == Some(session) {
                self.session_focus = None;
            }
            if self.active_tab == Some(session) {
                self.active_tab = self.tabs.get(pos.saturating_sub(1)).or(self.tabs.first()).map(|t| t.session);
            }
        }
    }

    /// Ends a session (closing its tab follows when the process has exited).
    fn stop_session(&mut self, session: u64) {
        if let Some(ax) = self.ax_sessions.remove(&session) {
            if let Some(s) = self.sessions.get_mut(&session) {
                s.closing = true;
            }
            ax.disconnect();
            self.events_tx.send(Event::Exited { session, code: Some(0), reason: None }).ok();
            return;
        }
        if let Some(s) = self.sessions.get_mut(&session) {
            s.closing = true;
            #[cfg(unix)]
            {
                std::process::Command::new("kill").arg(s.pid.to_string()).status().ok();
            }
            #[cfg(not(unix))]
            {
                // Forced: PuTTY would otherwise ask whether to close the session.
                launch::hide_console(std::process::Command::new("taskkill").args(["/F", "/T", "/PID", &s.pid.to_string()])).status().ok();
            }
        }
    }

    /// Connects to the X server once RC Manager's window exists.
    fn init_embedding(&mut self, frame: &eframe::Frame) {
        if self.embed_checked {
            return;
        }
        self.embed_checked = true;
        let Some(main) = crate::embed::main_window(frame) else {
            self.push(Level::Info, "Sessions open in separate windows (tabs need X11 or XWayland).");
            return;
        };
        match Embedder::connect(main) {
            Ok(e) => self.embedder = Some(e),
            Err(e) => self.push(Level::Warn, format!("Sessions open in separate windows: {e:#}")),
        }
    }

    /// Places the session windows over their tab area each frame. X11 child windows are
    /// drawn above everything egui paints, so a session is hidden while a menu, popup,
    /// dialog or the sidebar overlay covers its area.
    fn sync_embedded(&mut self, ctx: &egui::Context) {
        let Some(emb) = &self.embedder else { return };
        if self.tabs.is_empty() {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let rect = if self.locked { None } else { self.session_rect };
        // Menus, dialogs and the sidebar overlay over the session: cut out of the session window
        // (their shadows stay hidden), or hide it without SHAPE. Tooltips are excluded: they fade
        // in and follow the hovered entry, so cutting a hole for them made the session flicker
        // (e.g. hovering a host in the collapsed-sidebar overlay) for little benefit. Within an
        // open menu they are cut out, though, so the hint of a menu entry is not cropped.
        let menu_open = ctx.is_context_menu_open() || ctx.memory(|m| m.any_popup_open());
        let covers: Vec<egui::Rect> = rect
            .map(|r| {
                ctx.memory(|m| {
                    m.areas()
                        .visible_layer_ids()
                        .into_iter()
                        .filter(|l| !matches!(l.order, egui::Order::Background | egui::Order::PanelResizeLine))
                        .filter(|l| l.order != egui::Order::Tooltip || menu_open)
                        .filter_map(|l| m.area_rect(l.id))
                        .map(|a| a.expand(1.0))
                        .filter(|a| a.intersects(r))
                        .collect()
                })
            })
            .unwrap_or_default();
        let shape = emb.can_shape();
        // An egui window (Order::Middle) over the session is a modal dialog (properties,
        // credentials, import, …). Cutting a SHAPE hole for a centred dialog made the session
        // shimmer around its edges every repaint, so hide the session entirely while one is
        // open instead. Menus and the sidebar overlay are Order::Foreground and still cut holes.
        let dialog_over = rect.is_some_and(|r| {
            ctx.memory(|m| {
                m.areas()
                    .visible_layer_ids()
                    .into_iter()
                    .filter(|l| l.order == egui::Order::Middle)
                    .filter_map(|l| m.area_rect(l.id))
                    .any(|a| a.intersects(r))
            })
        });
        let covered = (!covers.is_empty() && !shape) || dialog_over;
        let mut logs = Vec::new();
        let mut focus_change = None;
        for tab in &mut self.tabs {
            // An ended tab has no window to place; it shows its Reconnect button instead.
            if tab.ended.is_some() {
                continue;
            }
            // TigerVNC: move its window into the tab as soon as it appears.
            if let Some((pid, since)) = tab.adopt {
                if let Some(win) = emb.window_of_pid(pid) {
                    match emb.adopt(win, tab.host) {
                        Ok(()) => tab.adopt = None,
                        Err(e) => logs.push((Level::Warn, format!("{}: cannot move the VNC window into the tab: {e:#}", tab.name))),
                    }
                    tab.placed = None;
                } else if since.elapsed() > Duration::from_secs(30) {
                    tab.adopt = None;
                    logs.push((Level::Warn, format!("{}: the VNC window was not found; it stays a separate window.", tab.name)));
                }
            }
            let active = Some(tab.session) == self.active_tab;
            // A session can be shown when its area is known and nothing covers it (without SHAPE).
            let show = rect.is_some() && !covered;
            let visible = active && show;
            // The active tab fills the session area; the others stay mapped at full size but just
            // off-screen, so their client already connects and renders and is shown the instant
            // the tab is selected (instead of only starting to connect once it becomes active).
            let geom = match rect.filter(|_| show) {
                Some(r) => {
                    let (x, y) = ((r.min.x * ppp).round() as i32, (r.min.y * ppp).round() as i32);
                    let (w, h) = ((r.width() * ppp).round().max(1.0) as u32, (r.height() * ppp).round().max(1.0) as u32);
                    if active {
                        (x, y, w, h, true)
                    } else {
                        (-(w as i32) - 100, y, w, h, true)
                    }
                }
                None => tab.placed.as_ref().map(|((x, y, w, h, _), _)| (*x, *y, *w, *h, false)).unwrap_or((0, 0, 1, 1, false)),
            };
            // Only the active, on-screen tab needs cut-outs for menus/dialogs over it.
            let holes: Vec<[i32; 4]> = match rect.filter(|_| visible && shape) {
                Some(r) => covers
                    .iter()
                    .map(|a| {
                        let a = a.intersect(r);
                        let (x0, y0) = (((a.min.x - r.min.x) * ppp).floor() as i32, ((a.min.y - r.min.y) * ppp).floor() as i32);
                        let (x1, y1) = (((a.max.x - r.min.x) * ppp).ceil() as i32, ((a.max.y - r.min.y) * ppp).ceil() as i32);
                        [x0, y0, x1 - x0, y1 - y0]
                    })
                    .collect(),
                None => Vec::new(),
            };
            let state = (geom, holes);
            // Clients create their window a moment after starting: refit for a few seconds so a
            // late window still fills the tab. Afterwards only react to real geometry changes, so
            // a steady re-place no longer interrupts an in-progress selection in the client.
            let refit =
                geom.4 && tab.opened.elapsed() < Duration::from_secs(5) && tab.last_fit.elapsed() > Duration::from_millis(500);
            if tab.placed.as_ref() != Some(&state) || refit {
                if let Err(e) = emb.place(tab.host, geom.0, geom.1, geom.2, geom.3, geom.4, &state.1) {
                    logs.push((Level::Warn, format!("{}: {e:#}", tab.name)));
                }
                if tab.placed.as_ref().is_some_and(|((_, _, w, h, _), _)| (*w, *h) != (geom.2, geom.3)) {
                    tab.resized = Some(Instant::now());
                }
                tab.placed = Some(state.clone());
                tab.last_fit = Instant::now();
            }
            // A new (or reconnected) RDP session can only follow the tab size once its display
            // channel is up, so the size is also repeated while it connects; wfreerdp ignores it
            // when nothing changed.
            let connecting = geom.4 && tab.opened.elapsed() < Duration::from_secs(30) && tab.size_told.elapsed() > Duration::from_secs(2);
            if tab.resized.is_some_and(|t| t.elapsed() > Duration::from_millis(400)) || connecting {
                emb.resized(tab.host);
                if let Some(ax) = self.ax_sessions.get(&tab.session) {
                    ax.resize(geom.2, geom.3);
                }
                tab.resized = None;
                tab.size_told = Instant::now();
            }
            // Keyboard: a click into the session gives it the focus.
            if visible && self.session_focus != Some(tab.session) {
                if let Some(p) = emb.pointer() {
                    let inside = p.x >= geom.0 && p.y >= geom.1 && p.x < geom.0 + geom.2 as i32 && p.y < geom.1 + geom.3 as i32;
                    // A click into a cut-out area belongs to RC Manager.
                    let (hx, hy) = (p.x - geom.0, p.y - geom.1);
                    let in_hole = state.1.iter().any(|&[x, y, w, h]| hx >= x && hy >= y && hx < x + w && hy < y + h);
                    let inside = inside && !in_hole;
                    if p.pressed && inside {
                        focus_change = Some(Some((tab.session, tab.host)));
                    }
                }
            }
        }
        // A click or text input in RC Manager itself takes the keyboard back.
        let rc_click = ctx.input(|i| i.pointer.any_pressed()) || ctx.wants_keyboard_input();
        if self.session_focus.is_some() && rc_click {
            focus_change = Some(None);
        }
        // Back in the foreground (Alt+Tab, taskbar), Windows gives the focus to RC Manager's
        // window – possibly only after the first frame. As long as RC Manager is active, the
        // session that should have the focus gets it back (wfreerdp ignores mouse and keyboard
        // without it); retried at most every 300 ms.
        let active = emb.active();
        if active && focus_change.is_none() && self.focus_checked.elapsed() > Duration::from_millis(300) {
            if let Some(tab) = self.tabs.iter().find(|t| Some(t.session) == self.session_focus && t.ended.is_none()) {
                if !emb.has_focus(emb.client_of(tab.host)) {
                    focus_change = Some(Some((tab.session, tab.host)));
                    self.focus_checked = Instant::now();
                }
            }
        }
        match focus_change {
            Some(Some((session, host))) => {
                emb.focus(emb.client_of(host));
                self.session_focus = Some(session);
            }
            Some(None) => {
                emb.focus(None);
                self.session_focus = None;
            }
            None => {}
        }
        // Unlock: show the tab, give the session the keyboard, then type the password into it.
        let mut unlock_done = false;
        if let Some(u) = &mut self.unlock {
            match self.tabs.iter().find(|t| t.session == u.session && t.ended.is_none()) {
                None => unlock_done = true,
                Some(tab) => {
                    let client = emb.client_of(tab.host);
                    let shown = Some(tab.session) == self.active_tab && tab.placed.as_ref().is_some_and(|((.., visible), _)| *visible);
                    match (client.filter(|_| shown), u.focused) {
                        _ if u.started.elapsed() > Duration::from_secs(5) => {
                            logs.push((Level::Warn, format!("{}: the session did not get the keyboard, nothing was typed.", tab.name)));
                            unlock_done = true;
                        }
                        (Some(client), None) => {
                            emb.focus(Some(client));
                            self.session_focus = Some(tab.session);
                            u.focused = Some(Instant::now());
                        }
                        (Some(client), Some(t)) if t.elapsed() > Duration::from_millis(400) => {
                            match emb.type_text(client, std::mem::take(&mut u.password)) {
                                Ok(rx) => {
                                    logs.push((Level::Info, format!("{}: typing the password to unlock the session …", tab.name)));
                                    self.typing = Some((tab.name.clone(), rx));
                                }
                                Err(e) => logs.push((Level::Warn, format!("{}: cannot type the password: {e:#}", tab.name))),
                            }
                            unlock_done = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        if unlock_done {
            self.unlock = None;
        }
        let typed = self.typing.as_ref().and_then(|(name, rx)| match rx.try_recv() {
            Ok(r) => Some((name.clone(), r)),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some((name.clone(), Err("typing stopped".into()))),
        });
        if let Some((name, r)) = typed {
            self.typing = None;
            logs.push(match r {
                Ok(()) => (Level::Info, format!("{name}: password typed.")),
                Err(e) => (Level::Warn, format!("{name}: unlocking failed: {e}")),
            });
        }
        for (lvl, msg) in logs {
            self.push(lvl, msg);
        }
        // Poll the pointer and window state while a session is shown.
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    /// Drops all decrypted data and shows the unlock screen (only with a master password,
    /// otherwise the key file would unlock it again without asking).
    fn lock_now(&mut self, reason: &str) {
        if !self.store.has_master_password().unwrap_or(false) {
            return;
        }
        if self.editor_dirty() {
            self.save_editor();
        }
        self.store.lock();
        self.data = Data::default();
        self.selection = None;
        self.editor = None;
        self.editor_original = None;
        self.ask_credentials = None;
        self.quick_pending = None;
        self.show_credentials = false;
        self.cred_selected = None;
        self.cred_edit = None;
        self.cred_new = false;
        self.cred_confirm_delete = None;
        self.cred_show_password = false;
        self.master_dialog = None;
        self.import_dialog = None;
        self.rdp_import = None;
        self.confirm_delete = None;
        self.sidebar_peek = false;
        self.locked = true;
        self.push(Level::Info, reason);
    }

    fn check_auto_lock(&mut self, ctx: &egui::Context) {
        let input = ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving() || i.pointer.any_down());
        if input {
            self.last_activity = Instant::now();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::L)) {
            self.lock_now("Locked.");
            return;
        }
        let minutes = self.s.auto_lock_minutes;
        if minutes == 0 || !self.store.has_master_password().unwrap_or(false) {
            return;
        }
        let limit = Duration::from_secs(minutes as u64 * 60);
        if self.last_activity.elapsed() >= limit {
            self.lock_now(&format!("Locked after {minutes} min of inactivity."));
        } else {
            // Wake up in time even when nothing else repaints the window.
            ctx.request_repaint_after(limit - self.last_activity.elapsed() + Duration::from_millis(200));
        }
    }

    fn ui_master(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.master_dialog.take() else { return };
        let mut keep = true;
        let has = self.store.has_master_password().unwrap_or(false);
        Self::modal("Master Password").show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(if has {
                "A master password is set. It is asked for at start-up; without it the stored passwords cannot be decrypted."
            } else {
                "Without a master password the key is kept in a file only you can read. With a master password it is never stored."
            });
            egui::Grid::new("mp").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("New password");
                ui.add(egui::TextEdit::singleline(&mut d.pw1).password(true));
                ui.end_row();
                ui.label("Repeat");
                ui.add(egui::TextEdit::singleline(&mut d.pw2).password(true));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.label("Lock after");
                ui.add(egui::DragValue::new(&mut self.s.auto_lock_minutes).range(0..=1440).suffix(" min"));
                ui.label(RichText::new("of inactivity (0 = never; needs a master password)").weak());
            });
            if !d.error.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &d.error);
            }
            ui.horizontal(|ui| {
                if ui.button("Set").clicked() {
                    if d.pw1.is_empty() {
                        d.error = "The password must not be empty.".into();
                    } else if d.pw1 != d.pw2 {
                        d.error = "The passwords do not match.".into();
                    } else {
                        match self.store.set_master_password(Some(&d.pw1)) {
                            Ok(()) => {
                                self.push(Level::Info, "Master password set.");
                                keep = false;
                            }
                            Err(e) => d.error = e.to_string(),
                        }
                    }
                }
                if has && ui.button("Remove").clicked() {
                    match self.store.set_master_password(None) {
                        Ok(()) => {
                            self.push(Level::Info, "Master password removed.");
                            keep = false;
                        }
                        Err(e) => d.error = e.to_string(),
                    }
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if keep {
            self.master_dialog = Some(d);
        }
    }

    fn ui_locked(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.25);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), egui::Sense::hover());
                theme::paint_logo(ui.painter(), rect.min, 72.0);
                ui.add_space(10.0);
                ui.heading("RC Manager is locked");
                ui.add_space(10.0);
                let r = ui.add(egui::TextEdit::singleline(&mut self.unlock_input).password(true).hint_text("Master password").desired_width(260.0));
                r.request_focus();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Unlock").clicked() || enter {
                    let pw = self.unlock_input.clone();
                    self.try_unlock(Some(&pw));
                }
                if !self.unlock_error.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, &self.unlock_error);
                }
            });
        });
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Remember the connections with an open session (deduped, only stored ones), so they
        // can be offered for reopening next time. Recomputed on every save, including on exit.
        let mut reopen = Vec::new();
        for s in self.sessions.values() {
            let id = s.conn.id;
            if !id.is_nil() && self.connection(id).is_some() && !reopen.contains(&id) {
                reopen.push(id);
            }
        }
        self.s.reopen = reopen;
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        theme::apply(ctx, self.s.dark);
        self.init_embedding(frame);
        self.poll_events();
        self.poll_picker();
        if !self.locked {
            self.check_auto_lock(ctx);
        }
        if self.locked {
            self.ui_locked(ctx);
            // Never show a session over the unlock screen.
            self.sync_embedded(ctx);
            return;
        }
        if !self.pending_connect.is_empty() {
            for name in std::mem::take(&mut self.pending_connect) {
                match self.data.connections.iter().find(|c| c.name.eq_ignore_ascii_case(&name)).map(|c| c.id) {
                    Some(id) => self.apply(Action::Connect(id)),
                    None => self.push(Level::Warn, format!("--connect: no connection named \"{name}\"")),
                }
            }
        }
        let mut actions = Vec::new();
        self.ui_menu(ctx, &mut actions);
        self.ui_bottom(ctx);
        self.ui_tree(ctx, &mut actions);
        self.ui_editor(ctx, &mut actions);
        self.ui_properties(ctx, &mut actions);
        self.ui_reachability(ctx);
        self.poll_update();
        self.ui_update(ctx);
        self.ui_dialogs(ctx);
        for a in actions {
            self.apply(a);
        }
        self.sync_embedded(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Sessions in tabs end with RC Manager – on Windows their clients (wfreerdp, PuTTY,
        // TigerVNC) would keep running without a window; sessions in separate windows stay open.
        let tabbed: Vec<u64> =
            self.tabs.iter().filter(|t| t.ended.is_none() && self.sessions.contains_key(&t.session)).map(|t| t.session).collect();
        for session in tabbed {
            self.stop_session(session);
        }
        // The reachability check's `ping -t` would keep running, too.
        self.reach = None;
        launch::cleanup_secret_files();
        if let Some(exe) = self.restart_exe.take() {
            crate::update::finish_on_exit(&exe, true);
        } else if let Some(update) = self.pending_update.take() {
            // Windows: an update installed with "Later" is applied now, without a restart.
            crate::update::finish_on_exit(&update, false);
        }
        if !self.locked && self.editor_dirty() {
            self.save_editor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_from_freerdp_details() {
        let details: Vec<String> = [
            "Certificate details for 127.0.0.1:3390 (RDP-Server):",
            "\tCommon Name: runnervmk1d70",
            "\tThumbprint:  56:30:94:2d:aa:7c:67:ce:9e:54:ec:48:12:25:79:07:d2:dd:24:35:c9:91:65:24:9b:66:ba:f9:5f:1d:5c:05",
        ]
        .map(String::from)
        .to_vec();
        let (hp, fp) = certificate_of(&details).unwrap();
        assert_eq!(hp, "127.0.0.1:3390");
        assert_eq!(fp, "5630942daa7c67ce9e54ec4812257907d2dd2435c99165249b66baf95f1d5c05");
        assert!(certificate_of(&details[..2]).is_none());

        let changed: Vec<String> = [
            "!!!Certificate for 127.0.0.1:3390 (RDP-Server) has changed!!!",
            "New Certificate details:",
            "	Thumbprint:  f0:06:65:05:b2:1b:ca:ce:ba:b1:9d:72:e7:3d:9b:94:3d:89:18:b2:be:4e:71:16:5e:61:56:f1:04:e2:64:4e",
            "Old Certificate details:",
            "	Thumbprint:  7f:0e:34:eb:28:d9:7a:77:89:61:ed:0f:bd:76:f3:27:61:04:02:57:28:ec:05:87:54:b6:e7:c3:e0:45:df:88",
        ]
        .map(String::from)
        .to_vec();
        let (hp, fp) = certificate_of(&changed).unwrap();
        assert_eq!(hp, "127.0.0.1:3390");
        assert!(fp.starts_with("f0066505"));
    }

    #[test]
    fn terminal_control_sequences_are_stripped() {
        assert_eq!(clean_terminal_line("\u{1b}[0;31mred\u{1b}[0m text\r"), "red text");
        assert_eq!(clean_terminal_line("plain\ttab"), "plain\ttab");
    }

    fn reason_of(typescript: &str) -> Option<String> {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rcm-test-{}.log", Uuid::new_v4()));
        std::fs::write(&path, typescript).unwrap();
        let r = terminal_failure_reason(&path);
        std::fs::remove_file(&path).ok();
        r
    }

    #[test]
    fn ssh_connect_failures_are_detected() {
        // A failed connect shows the ssh error line.
        let r = reason_of("Script started on 2026-01-01\r\nssh: connect to host box port 22: Connection refused\r\n").unwrap();
        assert!(r.contains("Connection refused"), "{r}");
        assert!(reason_of("ssh: connect to host nope port 22: Name or service not known\n").unwrap().contains("Name or service not known"));
        assert!(reason_of("\u{1b}[31mPermission denied (publickey,password).\u{1b}[0m\n").is_some());
        // A normal logout is not a failure.
        assert!(reason_of("admin@box:~$ exit\r\nlogout\r\nConnection to box closed.\r\n").is_none());
        // A session that merely printed those words earlier, then logged out cleanly, is not one.
        assert!(reason_of("$ cat log\nConnection refused earlier\n$ exit\nlogout\nConnection to box closed.\n").is_none());
    }
}
