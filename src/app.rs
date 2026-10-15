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
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use uuid::Uuid;

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
    /// Open RDP, SSH/Telnet and VNC sessions as tabs inside the window (X11/XWayland).
    embed_sessions: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { dark: false, quick_protocol: Protocol::Rdp, sidebar_collapsed: false, log_hidden: false, auto_lock_minutes: 15, embed_sessions: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sel {
    Folder(Uuid),
    Connection(Uuid),
}

#[derive(Clone, PartialEq)]
enum Editor {
    Folder(Folder),
    Connection(Connection),
}

enum Action {
    Select(Sel),
    Connect(Uuid),
    QuickConnect,
    NewFolder(Option<Uuid>),
    NewConnection(Option<Uuid>, Protocol),
    Duplicate(Uuid),
    AskDelete(Sel),
    Wake(Uuid),
    CheckPort(Uuid),
    SetExpanded(Uuid, bool),
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
    name: String,
    started: Instant,
    display: String,
    pid: u32,
    stdin: Option<std::process::ChildStdin>,
    /// Last lines of output, shown with prompts (certificate details).
    recent: VecDeque<String>,
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

/// A session shown as a tab; its client draws into the X11 window `host`.
struct SessionTab {
    session: u64,
    name: String,
    icon: &'static str,
    host: u32,
    /// TigerVNC opens its own window, which is moved into the tab once it appears.
    adopt: Option<(u32, Instant)>,
    /// Last geometry given to the host (physical pixels) and whether it was visible.
    placed: Option<(i32, i32, u32, u32, bool)>,
    last_fit: Instant,
}

/// Asks for credentials right before connecting.
struct AskCredentials {
    connection: Uuid,
    cred: Credential,
    save: bool,
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
}

/// Result of a native file dialog; `None` when it was cancelled.
enum Picked {
    Database(PathBuf),
    Rdp(Vec<PathBuf>),
}

#[derive(Default)]
struct MasterDialog {
    pw1: String,
    pw2: String,
    error: String,
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
    embed_checked: bool,
    tabs: Vec<SessionTab>,
    /// Session shown in the central area; `None` shows the connection properties.
    active_tab: Option<u64>,
    /// Central area of the active session tab (points), set while drawing.
    session_rect: Option<egui::Rect>,
    /// Session that has the keyboard focus.
    session_focus: Option<u64>,
    /// Connections to open at start (`--connect`).
    pending_connect: Vec<String>,
    prompts: VecDeque<PromptDialog>,

    log: VecDeque<LogLine>,
    sessions: BTreeMap<u64, Session>,
    next_session: u64,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    bg_tx: Sender<(Level, String)>,
    bg_rx: Receiver<(Level, String)>,
    ctx: egui::Context,

    show_credentials: bool,
    cred_selected: Option<Uuid>,
    ask_credentials: Option<AskCredentials>,
    confirm_delete: Option<Sel>,
    import_dialog: Option<ImportDialog>,
    rdp_import: Option<RdpImportDialog>,
    master_dialog: Option<MasterDialog>,
    show_tools: bool,
    show_about: bool,
}

/// "YYYY-MM-DD HH:MM UTC" without pulling in a date library.
/// "[12:35:21:300] [4377:0000111f] [ERROR][com.freerdp.crypto] - [fn]: text" → "text"
fn strip_log_prefix(line: &str) -> String {
    match line.find("]: ") {
        Some(i) if line.starts_with('[') => line[i + 3..].to_string(),
        _ => line.to_string(),
    }
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
        let (events_tx, events_rx) = channel();
        let (bg_tx, bg_rx) = channel();
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
            embed_checked: false,
            tabs: Vec::new(),
            active_tab: None,
            session_rect: None,
            session_focus: None,
            pending_connect: connect,
            prompts: VecDeque::new(),
            log: VecDeque::new(),
            sessions: BTreeMap::new(),
            next_session: 1,
            events_tx,
            events_rx,
            bg_tx,
            bg_rx,
            ctx: cc.egui_ctx.clone(),
            show_credentials: false,
            cred_selected: None,
            ask_credentials: None,
            confirm_delete: None,
            import_dialog: None,
            rdp_import: None,
            master_dialog: None,
            show_tools: false,
            show_about: false,
        };
        if !locked {
            app.try_unlock(None);
        }
        // FreeRDP's configuration lives in temporary per-start directories below this one;
        // remove leftovers (and the store of 0.4.0, which FreeRDP could not read back).
        let freerdp_config = app.store.dir().join("freerdp");
        std::fs::remove_dir_all(&freerdp_config).ok();
        app.tools.freerdp_config = Some(freerdp_config);
        if app.tools.freerdp.is_none() {
            app.push(Level::Warn, "FreeRDP not found – RDP connections need it installed (Options → External Programs).");
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

    /// Resolves credentials; `Inherit` walks up the folder tree.
    fn resolve_credential(&self, conn: &Connection) -> Option<Credential> {
        match conn.credential {
            CredentialRef::None => None,
            CredentialRef::Id(id) => self.credential(id).cloned(),
            CredentialRef::Inherit => {
                let mut cur = conn.parent;
                for _ in 0..256 {
                    let f = self.folder(cur?)?;
                    if let Some(c) = f.credential.and_then(|id| self.credential(id)) {
                        return Some(c.clone());
                    }
                    cur = f.parent;
                }
                None
            }
        }
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
            Action::Connect(id) => {
                self.select(Some(Sel::Connection(id)));
                self.connect(id);
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
                    }
                }
            }
            Action::AskDelete(s) => self.confirm_delete = Some(s),
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
                    let tx = self.bg_tx.clone();
                    let port = if c.protocol == Protocol::Web && c.port == 0 && c.host.starts_with("http://") { 80 } else { c.effective_port() };
                    let host = if c.protocol == Protocol::Web {
                        c.host.split("://").last().unwrap_or("").split(['/', ':']).next().unwrap_or("").to_string()
                    } else {
                        c.host.clone()
                    };
                    self.push(Level::Info, format!("Checking {host}:{port} …"));
                    let ctx = self.ctx.clone();
                    std::thread::spawn(move || {
                        let msg = match net::check_port(&host, port, Duration::from_secs(3)) {
                            Ok(t) => (Level::Info, format!("{host}:{port} is reachable ({} ms).", t.as_millis())),
                            Err(e) => (Level::Warn, format!("{host}:{port} is not reachable: {e}")),
                        };
                        tx.send(msg).ok();
                        ctx.request_repaint();
                    });
                }
            }
            Action::SetExpanded(id, open) => self.set_expanded(id, open),
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
            self.ask_credentials = Some(AskCredentials { connection: id, cred: Credential { password: String::new(), ..base }, save: false });
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
            Protocol::Ssh | Protocol::Telnet => self.tools.xterm.is_some(),
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
                    Session { name: conn.name.clone(), started: Instant::now(), display: plan.display.clone(), pid, stdin: spawned.stdin, recent: VecDeque::new() },
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
            self.ask_credentials = Some(AskCredentials { connection: Uuid::nil(), cred: Credential::new(&c.host), save: false });
            self.quick_pending = Some(c);
        } else {
            self.launch(&c, None);
        }
    }

    fn poll_events(&mut self) {
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
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
                Event::Exited { session, code } => {
                    if let Some(s) = self.sessions.remove(&session) {
                        let dur = s.started.elapsed().as_secs();
                        let lvl = if code.unwrap_or(0) == 0 { Level::Info } else { Level::Warn };
                        let code = code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
                        self.push(lvl, format!("[{session}] {} ended (exit code {code}, {}m {}s).", s.name, dur / 60, dur % 60));
                    }
                    self.remove_tab(session);
                    self.prompts.retain(|p| p.session != session);
                }
            }
        }
        while let Ok((lvl, msg)) = self.bg_rx.try_recv() {
            self.push(lvl, msg);
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
    fn ui_menu(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Import Windows RDP-Manager Backup or Database …").clicked() {
                        self.pick(Pick::Database);
                        ui.close_menu();
                    }
                    if ui.button("Import .rdp Files …").clicked() {
                        self.rdp_import = Some(RdpImportDialog { files: Vec::new(), folder: self.current_folder() });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let parent = self.current_folder();
                    if ui.button("🗀 New Folder").clicked() {
                        actions.push(Action::NewFolder(parent));
                        ui.close_menu();
                    }
                    new_connection_menu(ui, parent, actions);
                    ui.separator();
                    if ui.button("🔑 Manage Credentials …").clicked() {
                        self.show_credentials = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Options", |ui| {
                    ui.checkbox(&mut self.s.dark, "Dark Theme");
                    let embed_hint = if self.embedder.is_some() { "Sessions open as tabs inside this window" } else { "Needs X11 or XWayland" };
                    ui.checkbox(&mut self.s.embed_sessions, "Open Sessions in Tabs").on_hover_text(embed_hint);
                    let mut shown = !self.s.sidebar_collapsed;
                    if ui.checkbox(&mut shown, "Show Sidebar (Ctrl+B)").changed() {
                        self.s.sidebar_collapsed = !shown;
                    }
                    let mut log = !self.s.log_hidden;
                    if ui.checkbox(&mut log, "Show Log (Ctrl+L)").changed() {
                        self.set_log_hidden(!log);
                    }
                    ui.separator();
                    let has_master = self.store.has_master_password().unwrap_or(false);
                    if ui.add_enabled(has_master, egui::Button::new("🔒 Lock Now (Ctrl+Shift+L)")).on_disabled_hover_text("Set a master password first").clicked() {
                        self.lock_now("Locked.");
                        ui.close_menu();
                    }
                    if ui.button("Master Password …").clicked() {
                        self.master_dialog = Some(MasterDialog::default());
                        ui.close_menu();
                    }
                    if ui.button("External Programs …").clicked() {
                        self.show_tools = true;
                        ui.close_menu();
                    }
                    if ui.button("Re-detect External Programs").clicked() {
                        self.tools = Tools::detect();
                        self.tools.freerdp_config = Some(self.store.dir().join("freerdp"));
                        self.push(Level::Info, "External programs re-detected.");
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About RC Manager").clicked() {
                        self.show_about = true;
                        ui.close_menu();
                    }
                });
            });
        });

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.allocate_ui(egui::vec2(300.0, 56.0), |ui| theme::header(ui, "Remote Connection Manager"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Connect").clicked() {
                        actions.push(Action::QuickConnect);
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut self.quick_host).hint_text("host[:port]").desired_width(220.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        actions.push(Action::QuickConnect);
                    }
                    egui::ComboBox::from_id_salt("quick_proto").selected_text(self.s.quick_protocol.label()).show_ui(ui, |ui| {
                        for p in [Protocol::Rdp, Protocol::Ssh, Protocol::Telnet, Protocol::Vnc, Protocol::Web] {
                            ui.selectable_value(&mut self.s.quick_protocol, p, p.label());
                        }
                    });
                    ui.label("Quick connect:");
                });
            });
        });
    }

    fn connection_row(&self, ui: &mut egui::Ui, c: &Connection, label: String, actions: &mut Vec<Action>) {
        let selected = self.selection == Some(Sel::Connection(c.id));
        let r = ui.selectable_label(selected, format!("{} {label}", c.protocol.icon()));
        let r = if c.description.is_empty() { r.on_hover_text(&c.host) } else { r.on_hover_text(format!("{}\n\n{}", c.host, c.description)) };
        if r.clicked() {
            actions.push(Action::Select(Sel::Connection(c.id)));
        }
        if r.double_clicked() {
            actions.push(Action::Connect(c.id));
        }
        r.context_menu(|ui| {
            if ui.button("▶ Connect").clicked() {
                actions.push(Action::Connect(c.id));
                ui.close_menu();
            }
            if ui.button("Duplicate").clicked() {
                actions.push(Action::Duplicate(c.id));
                ui.close_menu();
            }
            if ui.button("Check Reachability").clicked() {
                actions.push(Action::CheckPort(c.id));
                ui.close_menu();
            }
            if ui.add_enabled(!c.mac.trim().is_empty(), egui::Button::new("Wake-on-LAN")).clicked() {
                actions.push(Action::Wake(c.id));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("🗑 Delete …").clicked() {
                actions.push(Action::AskDelete(Sel::Connection(c.id)));
                ui.close_menu();
            }
        });
    }

    fn folder_tree(&self, ui: &mut egui::Ui, parent: Option<Uuid>, actions: &mut Vec<Action>) {
        for f in self.children_folders(parent) {
            let id = ui.make_persistent_id(("folder", f.id));
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, f.expanded);
            let was_open = state.is_open();
            let selected = self.selection == Some(Sel::Folder(f.id));
            state
                .show_header(ui, |ui| {
                    let r = ui.selectable_label(selected, format!("{} {}", if f.expanded { "🗁" } else { "🗀" }, f.name));
                    if r.clicked() {
                        actions.push(Action::Select(Sel::Folder(f.id)));
                    }
                    r.context_menu(|ui| {
                        if ui.button("🗀 New Subfolder").clicked() {
                            actions.push(Action::NewFolder(Some(f.id)));
                            ui.close_menu();
                        }
                        new_connection_menu(ui, Some(f.id), actions);
                        ui.separator();
                        if ui.button("🗑 Delete Folder …").clicked() {
                            actions.push(Action::AskDelete(Sel::Folder(f.id)));
                            ui.close_menu();
                        }
                    });
                })
                .body(|ui| self.folder_tree(ui, Some(f.id), actions));
            let now_open = egui::collapsing_header::CollapsingState::load(ui.ctx(), id).map(|s| s.is_open()).unwrap_or(was_open);
            if now_open != f.expanded {
                actions.push(Action::SetExpanded(f.id, now_open));
            }
        }
        for c in self.children_connections(parent) {
            self.connection_row(ui, c, c.name.clone(), actions);
        }
    }

    fn ui_tree(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::B)) {
            self.s.sidebar_collapsed = !self.s.sidebar_collapsed;
            self.sidebar_peek = false;
        }
        if !self.s.sidebar_collapsed {
            egui::SidePanel::left("tree").resizable(true).default_width(320.0).min_width(220.0).show(ctx, |ui| {
                self.tree_contents(ui, actions, false);
            });
            return;
        }

        // Narrow strip with a vertical caption. Hovering it shows the sidebar as an overlay,
        // clicking it (or the pin in the overlay) docks the sidebar again.
        let strip = egui::SidePanel::left("tree_collapsed").resizable(false).exact_width(28.0).show(ctx, |ui| {
            let rect = ui.max_rect().expand2(egui::vec2(8.0, 0.0));
            let resp = ui.interact(rect, ui.id().with("expand"), egui::Sense::click());
            let visuals = ui.visuals();
            let active = resp.hovered() || self.sidebar_peek;
            let color = if active { visuals.strong_text_color() } else { visuals.text_color() };
            if active {
                ui.painter().rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
            }
            let x = rect.center().x;
            ui.painter().text(egui::pos2(x, rect.top() + 16.0), Align2::CENTER_CENTER, "»", egui::FontId::proportional(18.0), color);
            let galley = ui.painter().layout_no_wrap("Connections".into(), egui::FontId::proportional(14.0), color);
            // Rotated 90° clockwise: the text runs top to bottom, its height extends to the left of `pos`.
            let pos = egui::pos2(x + galley.size().y / 2.0, rect.top() + 34.0);
            ui.painter().add(egui::epaint::TextShape::new(pos, galley, color).with_angle(std::f32::consts::FRAC_PI_2));
            if resp.clicked() {
                self.s.sidebar_collapsed = false;
                self.sidebar_peek = false;
            }
            if resp.hovered() {
                self.sidebar_peek = true;
            }
            rect
        });
        if !self.s.sidebar_collapsed || !self.sidebar_peek {
            return;
        }

        let strip_rect = strip.inner;
        let width = 320.0;
        let area = egui::Area::new(egui::Id::new("tree_peek"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(strip_rect.right(), strip_rect.top()))
            .show(ctx, |ui| {
                let frame = egui::Frame::window(ui.style()).rounding(0.0).inner_margin(egui::Margin::symmetric(8.0, 6.0));
                frame.show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_height(strip_rect.height() - 18.0);
                    self.tree_contents(ui, actions, true);
                });
            });

        // Keep the overlay while the pointer is over the strip, the overlay itself or one of
        // its menus (context menu, combo box); hide it as soon as the pointer moves away.
        let overlay_rect = area.response.rect;
        let keep = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| {
            strip_rect.contains(p)
                || overlay_rect.contains(p)
                || ctx.layer_id_at(p).is_some_and(|l| l.order == egui::Order::Foreground && l.id != area.response.layer_id.id)
        });
        let menu_open = ctx.memory(|m| m.any_popup_open());
        if !keep && !menu_open {
            self.sidebar_peek = false;
        }
    }

    /// Header, search, toolbar and tree; used by the docked panel and the hover overlay.
    fn tree_contents(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>, overlay: bool) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong("Connections");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if overlay {
                    if ui.small_button("📌").on_hover_text("Dock sidebar (Ctrl+B)").clicked() {
                        self.s.sidebar_collapsed = false;
                        self.sidebar_peek = false;
                    }
                } else if ui.small_button("«").on_hover_text("Collapse sidebar (Ctrl+B)").clicked() {
                    self.s.sidebar_collapsed = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("🔍 Search …").desired_width(ui.available_width() - 30.0));
            if !self.filter.is_empty() && ui.small_button("✖").clicked() {
                self.filter.clear();
            }
        });
        ui.horizontal(|ui| {
            let parent = self.current_folder();
            if ui.button("🗀 +").on_hover_text("New folder").clicked() {
                actions.push(Action::NewFolder(parent));
            }
            if ui.button("🖥 +").on_hover_text("New RDP connection").clicked() {
                actions.push(Action::NewConnection(parent, Protocol::Rdp));
            }
            if ui.button("⌨ +").on_hover_text("New SSH connection").clicked() {
                actions.push(Action::NewConnection(parent, Protocol::Ssh));
            }
            if ui.button("🔑").on_hover_text("Manage credentials").clicked() {
                self.show_credentials = true;
            }
        });
        ui.add_space(4.0);
        theme::list_frame(ui, |ui| {
            ui.set_min_height(ui.available_height() - 6.0);
            egui::ScrollArea::vertical().id_salt(("tree_scroll", overlay)).auto_shrink([false; 2]).show(ui, |ui| {
                let q = self.filter.trim().to_lowercase();
                if q.is_empty() {
                    self.folder_tree(ui, None, actions);
                    if self.data.folders.is_empty() && self.data.connections.is_empty() {
                        ui.add_space(20.0);
                        ui.label(
                            RichText::new("No connections yet.\n\nCreate one via “Edit” or import a backup or database of the Windows RDP-Manager via “File”.")
                                .weak(),
                        );
                    }
                } else {
                    let mut hits: Vec<&Connection> = self
                        .data
                        .connections
                        .iter()
                        .filter(|c| [&c.name, &c.host, &c.description].iter().any(|s| s.to_lowercase().contains(&q)))
                        .collect();
                    hits.sort_by_key(|c| c.name.to_lowercase());
                    for c in hits {
                        let path = self.folder_path(c.parent);
                        let label = if path.is_empty() { c.name.clone() } else { format!("{}   ({path})", c.name) };
                        self.connection_row(ui, c, label, actions);
                    }
                }
            });
        });
    }

    fn credential_combo(ui: &mut egui::Ui, id: &str, value: &mut CredentialRef, creds: &[Credential], allow_inherit: bool, inherited: Option<&Credential>) {
        let text = match value {
            CredentialRef::None => "(none)".to_string(),
            CredentialRef::Inherit => format!("Inherit from folder → {}", inherited.map(|c| c.display()).unwrap_or_else(|| "nothing".into())),
            CredentialRef::Id(id) => creds.iter().find(|c| c.id == *id).map(|c| c.display()).unwrap_or_else(|| "(deleted)".into()),
        };
        egui::ComboBox::from_id_salt(id).selected_text(text).width(320.0).show_ui(ui, |ui| {
            ui.selectable_value(value, CredentialRef::None, "(none)");
            if allow_inherit {
                ui.selectable_value(value, CredentialRef::Inherit, "Inherit from folder");
            }
            let mut sorted: Vec<&Credential> = creds.iter().collect();
            sorted.sort_by_key(|c| c.name.to_lowercase());
            for c in sorted {
                ui.selectable_value(value, CredentialRef::Id(c.id), c.display());
            }
        });
    }

    fn folder_combo(ui: &mut egui::Ui, id: &str, value: &mut Option<Uuid>, choices: &[(Option<Uuid>, String)]) {
        let text = choices.iter().find(|(i, _)| i == value).map(|(_, n)| n.trim().to_string()).unwrap_or_default();
        egui::ComboBox::from_id_salt(id).selected_text(text).width(320.0).show_ui(ui, |ui| {
            for (i, n) in choices {
                ui.selectable_value(value, *i, n);
            }
        });
    }

    fn ui_editor(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.session_rect = None;
            if !self.tabs.is_empty() {
                self.tab_strip(ui);
                ui.add_space(2.0);
            }
            match self.active_tab {
                Some(id) if self.tabs.iter().any(|t| t.session == id) => self.session_area(ui, id),
                _ => self.editor_contents(ui, actions),
            }
        });
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        egui::ScrollArea::horizontal().id_salt("session_tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let props = RichText::new("🗐 Properties");
                let sel = self.active_tab.is_none();
                if ui.add(egui::SelectableLabel::new(sel, if sel { props.strong() } else { props })).clicked() {
                    self.active_tab = None;
                }
                for t in &self.tabs {
                    ui.separator();
                    let sel = self.active_tab == Some(t.session);
                    let text = RichText::new(format!("{} {}", t.icon, t.name));
                    let r = ui.add(egui::SelectableLabel::new(sel, if sel { text.strong() } else { text }));
                    if r.clicked() {
                        self.active_tab = Some(t.session);
                        self.session_focus = None;
                    }
                    if r.middle_clicked() {
                        close = Some(t.session);
                    }
                    if ui.small_button("✖").on_hover_text("Close session").clicked() {
                        close = Some(t.session);
                    }
                }
            });
        });
        if let Some(id) = close {
            self.stop_session(id);
        }
    }

    /// Reserves the area the active session's window is placed over.
    fn session_area(&mut self, ui: &mut egui::Ui, id: u64) {
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, egui::Color32::from_gray(if self.s.dark { 16 } else { 32 }));
        let waiting = self.tabs.iter().find(|t| t.session == id).map(|t| t.adopt.is_some()).unwrap_or(false);
        let text = if waiting { "Waiting for the session window …" } else { "Connecting …" };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, egui::FontId::proportional(14.0), egui::Color32::from_gray(160));
        self.session_rect = Some(rect);
    }

    fn editor_contents(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        {
            let Some(mut editor) = self.editor.clone() else {
                ui.centered_and_justified(|ui| ui.label(RichText::new("Select a connection or folder.\nDouble-click connects.").weak()));
                return;
            };
            let dirty = self.editor_dirty();
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                match &mut editor {
                    Editor::Folder(f) => {
                        ui.heading(RichText::new(format!("🗀 {}", f.name)).color(theme::accent_blue(ui)));
                        ui.add_space(8.0);
                        let choices = self.folder_choices(Some(f.id));
                        egui::Grid::new("folder_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(320.0));
                            ui.end_row();
                            ui.label("Parent folder");
                            Self::folder_combo(ui, "folder_parent", &mut f.parent, &choices);
                            ui.end_row();
                            ui.label("Credentials for\nconnections inside");
                            let mut r = f.credential.map(CredentialRef::Id).unwrap_or_default();
                            Self::credential_combo(ui, "folder_cred", &mut r, &self.data.credentials, false, None);
                            f.credential = match r {
                                CredentialRef::Id(id) => Some(id),
                                _ => None,
                            };
                            ui.end_row();
                            ui.label("Sort order");
                            ui.add(egui::DragValue::new(&mut f.sorting));
                            ui.end_row();
                            ui.label("Description");
                            ui.add(egui::TextEdit::multiline(&mut f.description).desired_width(320.0).desired_rows(4));
                            ui.end_row();
                        });
                    }
                    Editor::Connection(c) => {
                        ui.horizontal(|ui| {
                            ui.heading(RichText::new(format!("{} {}", c.protocol.icon(), c.name)).color(theme::accent_blue(ui)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new("▶ Connect").strong())).clicked() {
                                    actions.push(Action::Connect(c.id));
                                }
                            });
                        });
                        if let Some(t) = &c.last_connected {
                            ui.label(RichText::new(format!("Last connected: {t}")).weak());
                        }
                        ui.add_space(8.0);
                        let choices = self.folder_choices(None);
                        let inherited = {
                            let mut probe = c.clone();
                            probe.credential = CredentialRef::Inherit;
                            self.resolve_credential(&probe)
                        };
                        egui::Grid::new("conn_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(320.0));
                            ui.end_row();
                            ui.label("Protocol");
                            egui::ComboBox::from_id_salt("conn_proto").selected_text(c.protocol.label()).width(320.0).show_ui(ui, |ui| {
                                for p in Protocol::ALL {
                                    ui.selectable_value(&mut c.protocol, p, p.label());
                                }
                            });
                            ui.end_row();
                            ui.label(if c.protocol == Protocol::Web { "URL" } else { "Host" });
                            ui.add(egui::TextEdit::singleline(&mut c.host).desired_width(320.0));
                            ui.end_row();
                            if c.protocol != Protocol::External {
                                ui.label("Port");
                                ui.horizontal(|ui| {
                                    ui.add(egui::DragValue::new(&mut c.port).range(0..=65535));
                                    ui.label(RichText::new(format!("0 = default ({})", c.protocol.default_port())).weak());
                                });
                                ui.end_row();
                            }
                            ui.label("Folder");
                            Self::folder_combo(ui, "conn_parent", &mut c.parent, &choices);
                            ui.end_row();
                            ui.label("Credentials");
                            Self::credential_combo(ui, "conn_cred", &mut c.credential, &self.data.credentials, true, inherited.as_ref());
                            ui.end_row();
                            ui.label("MAC (Wake-on-LAN)");
                            ui.add(egui::TextEdit::singleline(&mut c.mac).hint_text("00:11:22:33:44:55").desired_width(320.0));
                            ui.end_row();
                            ui.label("Description");
                            ui.add(egui::TextEdit::multiline(&mut c.description).desired_width(320.0).desired_rows(3));
                            ui.end_row();
                        });
                        ui.add_space(8.0);
                        Self::protocol_options(ui, c);
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(dirty, egui::Button::new("💾 Save")).clicked() {
                        actions.push(Action::Select(match &editor {
                            Editor::Folder(f) => Sel::Folder(f.id),
                            Editor::Connection(c) => Sel::Connection(c.id),
                        }));
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                        editor = self.editor_original.clone().unwrap_or(editor.clone());
                    }
                    if dirty {
                        ui.label(RichText::new("unsaved – saved automatically when switching").weak());
                    }
                });
            });
            self.editor = Some(editor);
        }
    }

    fn protocol_options(ui: &mut egui::Ui, c: &mut Connection) {
        match c.protocol {
            Protocol::Rdp => {
                let o = &mut c.options.rdp;
                ui.collapsing("Display", |ui| {
                    egui::Grid::new("rdp_disp").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Mode");
                        egui::ComboBox::from_id_salt("rdp_mode").selected_text(o.window_mode.label()).show_ui(ui, |ui| {
                            for m in [WindowMode::Tab, WindowMode::Window, WindowMode::Fullscreen, WindowMode::MultiMonitor] {
                                ui.selectable_value(&mut o.window_mode, m, m.label());
                            }
                        });
                        ui.end_row();
                        ui.label("Resolution");
                        ui.add(egui::TextEdit::singleline(&mut o.resolution).hint_text("e.g. 1920x1080").desired_width(120.0));
                        ui.end_row();
                        ui.label("Colour depth");
                        let depth = if o.color_depth == 0 { "Default".into() } else { format!("{} bit", o.color_depth) };
                        egui::ComboBox::from_id_salt("rdp_bpp").selected_text(depth).show_ui(ui, |ui| {
                            ui.selectable_value(&mut o.color_depth, 0, "Default");
                            for b in [8u8, 15, 16, 24, 32] {
                                ui.selectable_value(&mut o.color_depth, b, format!("{b} bit"));
                            }
                        });
                        ui.end_row();
                    });
                    ui.checkbox(&mut o.dynamic_resolution, "Adapt the remote resolution to the window size")
                        .on_hover_text("Needs server support (Windows 8.1 / Server 2012 R2 or newer). When off, the desktop is scaled to the window.");
                });
                ui.collapsing("Local Resources", |ui| {
                    ui.checkbox(&mut o.clipboard, "Clipboard");
                    ui.checkbox(&mut o.sound, "Play audio locally");
                    ui.checkbox(&mut o.microphone, "Microphone");
                    ui.checkbox(&mut o.printers, "Printers");
                    ui.checkbox(&mut o.smartcards, "Smart cards");
                    ui.checkbox(&mut o.home_drive, "Share home folder as drive “home”");
                    ui.label("Additional drives (one per line, name=/path):");
                    let mut drives = o.drives.join("\n");
                    if ui.add(egui::TextEdit::multiline(&mut drives).desired_rows(2).desired_width(320.0)).changed() {
                        o.drives = drives.lines().map(|l| l.to_string()).collect();
                    }
                });
                ui.collapsing("Advanced", |ui| {
                    ui.checkbox(&mut o.console, "Admin / console session");
                    ui.checkbox(&mut o.ignore_certificate, "Do not verify certificate (insecure)");
                    egui::Grid::new("rdp_adv").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("RD Gateway");
                        ui.add(egui::TextEdit::singleline(&mut o.gateway).desired_width(240.0));
                        ui.end_row();
                        ui.label("Gateway user");
                        ui.add(egui::TextEdit::singleline(&mut o.gateway_user).hint_text("empty = same as connection").desired_width(240.0));
                        ui.end_row();
                        ui.label("Gateway domain");
                        ui.add(egui::TextEdit::singleline(&mut o.gateway_domain).desired_width(240.0));
                        ui.end_row();
                        ui.label("Keyboard layout");
                        ui.add(egui::TextEdit::singleline(&mut o.keyboard_layout).hint_text("e.g. 00000409").desired_width(240.0));
                        ui.end_row();
                        ui.label("RemoteApp / program");
                        ui.add(egui::TextEdit::singleline(&mut o.remote_app).desired_width(240.0));
                        ui.end_row();
                        ui.label("Extra arguments");
                        ui.add(egui::TextEdit::singleline(&mut o.extra_args).hint_text("FreeRDP options").desired_width(240.0));
                        ui.end_row();
                    });
                });
            }
            Protocol::Ssh => {
                let o = &mut c.options.ssh;
                ui.collapsing("SSH", |ui| {
                    ui.checkbox(&mut o.x11_forwarding, "X11 forwarding (-X)");
                    ui.checkbox(&mut o.compression, "Compression (-C)");
                    egui::Grid::new("ssh_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Identity file");
                        ui.add(egui::TextEdit::singleline(&mut o.identity_file).hint_text("~/.ssh/id_ed25519").desired_width(240.0));
                        ui.end_row();
                        ui.label("Remote command");
                        ui.add(egui::TextEdit::singleline(&mut o.remote_command).desired_width(240.0));
                        ui.end_row();
                        ui.label("Extra arguments");
                        ui.add(egui::TextEdit::singleline(&mut o.extra_args).hint_text("e.g. -L 8080:localhost:80").desired_width(240.0));
                        ui.end_row();
                    });
                });
            }
            Protocol::Vnc => {
                let o = &mut c.options.vnc;
                ui.collapsing("VNC", |ui| {
                    ui.checkbox(&mut o.view_only, "View only");
                    ui.checkbox(&mut o.fullscreen, "Full screen");
                    ui.checkbox(&mut o.shared, "Shared session");
                    ui.horizontal(|ui| {
                        ui.label("Extra arguments");
                        ui.add(egui::TextEdit::singleline(&mut o.extra_args).desired_width(240.0));
                    });
                });
            }
            Protocol::External => {
                let o = &mut c.options.external;
                ui.collapsing("Program", |ui| {
                    egui::Grid::new("ext_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Command line");
                        ui.add(egui::TextEdit::singleline(&mut o.command).hint_text("remmina -c rdp://{U}@{T}").desired_width(320.0));
                        ui.end_row();
                        ui.label("Working directory");
                        ui.add(egui::TextEdit::singleline(&mut o.working_dir).desired_width(320.0));
                        ui.end_row();
                    });
                    ui.label(RichText::new("Placeholders: {T} host, {PORT} port, {N} name, {U} user, {D} domain, {P} password").weak());
                });
            }
            Protocol::Telnet | Protocol::Web => {}
        }
    }

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

    fn modal(title: &str) -> egui::Window<'_> {
        egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0])
    }

    fn ui_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(sel) = self.confirm_delete {
            let text = match sel {
                Sel::Connection(id) => format!("Delete connection “{}”?", self.connection(id).map(|c| c.name.as_str()).unwrap_or("")),
                Sel::Folder(id) => {
                    let (f, c) = self.descendants(id);
                    format!(
                        "Delete folder “{}” with {} subfolder(s) and {} connection(s)?",
                        self.folder(id).map(|f| f.name.as_str()).unwrap_or(""),
                        f.len() - 1,
                        c.len()
                    )
                }
            };
            let mut close = false;
            Self::modal("Delete").show(ctx, |ui| {
                ui.label(text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        self.delete(sel);
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.confirm_delete = None;
            }
        }

        if let Some(mut ask) = self.ask_credentials.take() {
            let mut keep = true;
            let mut go = false;
            Self::modal("Log On").show(ctx, |ui| {
                egui::Grid::new("ask_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("User");
                    ui.text_edit_singleline(&mut ask.cred.username);
                    ui.end_row();
                    ui.label("Domain");
                    ui.text_edit_singleline(&mut ask.cred.domain);
                    ui.end_row();
                    ui.label("Password");
                    let r = ui.add(egui::TextEdit::singleline(&mut ask.cred.password).password(true));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    ui.end_row();
                });
                if !ask.connection.is_nil() {
                    ui.checkbox(&mut ask.save, "Save as credentials and assign to this connection");
                }
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() {
                        go = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                        self.quick_pending = None;
                    }
                });
            });
            if go {
                self.finish_ask(ask);
            } else if keep {
                self.ask_credentials = Some(ask);
            }
        }

        self.ui_prompt(ctx);
        self.ui_credentials(ctx);
        self.ui_import(ctx);
        self.ui_rdp_import(ctx);
        self.ui_master(ctx);

        if self.show_tools {
            let mut open = true;
            egui::Window::new("External Programs").open(&mut open).collapsible(false).show(ctx, |ui| {
                egui::Grid::new("tools").num_columns(2).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                    for (k, v) in self.tools.summary() {
                        ui.label(k);
                        ui.label(v);
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Debian/Ubuntu: sudo apt install freerdp3-x11 tigervnc-viewer sshpass\n\
                         Fedora:        sudo dnf install freerdp tigervnc sshpass\n\
                         Arch:          sudo pacman -S freerdp tigervnc sshpass",
                    )
                    .monospace(),
                );
            });
            self.show_tools = open;
        }
        if self.show_about {
            let mut open = true;
            egui::Window::new("About RC Manager")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading(format!("RC Manager {}", env!("CARGO_PKG_VERSION")));
                        ui.label("Connection manager for RDP, SSH, Telnet, VNC, web and external programs.");
                        ui.label("Reads databases of the Windows RDP-Manager (Cinspiration, DB version 12).");
                        ui.label(RichText::new(format!("Data: {}", Store::default_dir().display())).weak());
                        ui.add_space(10.0);
                        ui.separator();
                        ui.label(RichText::new(env!("CARGO_PKG_AUTHORS")).strong());
                    });
                });
            self.show_about = open;
        }
    }

    /// Shows a client's question (certificate check) and writes the answer to its stdin.
    fn ui_prompt(&mut self, ctx: &egui::Context) {
        let Some(p) = self.prompts.front() else { return };
        let certificate = p.question.contains("certificate");
        let mut answer = None;
        Self::modal(if certificate { "Certificate Check" } else { "Question" }).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.label(RichText::new(format!("{}:", p.name)).strong());
            if p.changed {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    RichText::new("The certificate of this server has CHANGED since it was trusted. This can mean that someone intercepts the connection (man in the middle). Only continue if you know that the server got a new certificate.").strong(),
                );
            } else if certificate {
                ui.label("The identity of the server could not be verified. Check the details below; trust the certificate only if it really belongs to this server.");
            }
            theme::list_frame(ui, |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for line in &p.details {
                        ui.label(RichText::new(line).monospace().small());
                    }
                });
            });
            ui.label(RichText::new(&p.question).weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if certificate {
                    if ui.button("Trust once").clicked() {
                        answer = Some("Y");
                    }
                    let remember = ui.add_enabled(p.certificate.is_some(), egui::Button::new("Trust always"));
                    if remember.on_hover_text("Remember the fingerprint of this certificate for this server").clicked() {
                        answer = Some("always");
                    }
                    if ui.button("Cancel connection").clicked() {
                        answer = Some("N");
                    }
                } else {
                    if ui.button("Yes").clicked() {
                        answer = Some("Y");
                    }
                    if ui.button("No").clicked() {
                        answer = Some("N");
                    }
                }
            });
        });
        if let Some(mut a) = answer {
            let p = self.prompts.pop_front().unwrap();
            // "T" accepts the certificate for this connection only, so FreeRDP never writes its
            // own store; RC Manager passes trusted fingerprints itself.
            let temporary = p.question.contains("Y/T/N");
            if a == "always" {
                // Accept now and remember the fingerprint in RC Manager's own store.
                a = "Y";
                if let Some((host_port, fp)) = &p.certificate {
                    match self.store.trust_certificate(host_port, fp) {
                        Ok(()) => self.push(Level::Info, format!("Certificate of {host_port} is trusted from now on.")),
                        Err(e) => self.push(Level::Warn, format!("Cannot remember the certificate: {e:#}")),
                    }
                }
            }
            if a == "Y" && temporary {
                a = "T";
            }
            let ok = self.sessions.get_mut(&p.session).and_then(|s| s.stdin.as_mut()).map(|stdin| {
                use std::io::Write;
                stdin.write_all(format!("{a}\n").as_bytes()).and_then(|_| stdin.flush()).is_ok()
            });
            if ok != Some(true) {
                self.push(Level::Warn, format!("{}: the answer could not be sent", p.name));
            }
        }
    }

    fn finish_ask(&mut self, ask: AskCredentials) {
        if ask.connection.is_nil() {
            if let Some(c) = self.quick_pending.take() {
                self.launch(&c, Some(&ask.cred));
            }
            return;
        }
        if ask.save {
            let mut cred = ask.cred.clone();
            cred.id = Uuid::new_v4();
            if cred.name.is_empty() {
                cred.name = cred.username.clone();
            }
            if self.result(self.store.save_credential(&cred)).is_some() {
                self.data.credentials.push(cred.clone());
                if let Some(mut c) = self.connection(ask.connection).cloned() {
                    c.credential = CredentialRef::Id(cred.id);
                    self.result(self.store.save_connection(&c));
                    if let Some(x) = self.data.connections.iter_mut().find(|x| x.id == c.id) {
                        *x = c.clone();
                    }
                    if matches!(&self.editor, Some(Editor::Connection(e)) if e.id == c.id) {
                        self.editor = Some(Editor::Connection(c));
                        self.editor_original = self.editor.clone();
                    }
                }
            }
        }
        if let Some(c) = self.connection(ask.connection).cloned() {
            self.launch(&c, Some(&ask.cred));
        }
    }

    fn ui_credentials(&mut self, ctx: &egui::Context) {
        if !self.show_credentials {
            return;
        }
        let mut open = true;
        let mut save: Option<Credential> = None;
        let mut delete: Option<Uuid> = None;
        egui::Window::new("Credentials").open(&mut open).default_width(580.0).show(ctx, |ui| {
            if ui.button("➕ New").clicked() {
                let c = Credential::new("New Credentials");
                self.cred_selected = Some(c.id);
                save = Some(c);
            }
            ui.add_space(4.0);
            ui.columns(2, |cols| {
                theme::list_frame(&mut cols[0], |ui| {
                    egui::ScrollArea::vertical().id_salt("cred_list").max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                        let mut sorted: Vec<&Credential> = self.data.credentials.iter().collect();
                        sorted.sort_by_key(|c| c.name.to_lowercase());
                        for c in sorted {
                            if ui.selectable_label(self.cred_selected == Some(c.id), c.display()).clicked() {
                                self.cred_selected = Some(c.id);
                            }
                        }
                    });
                });
                let ui = &mut cols[1];
                if let Some(mut c) = self.cred_selected.and_then(|id| self.credential(id)).cloned() {
                    let before = c.clone();
                    egui::Grid::new("cred_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut c.name);
                        ui.end_row();
                        ui.label("User");
                        ui.text_edit_singleline(&mut c.username);
                        ui.end_row();
                        ui.label("Domain");
                        ui.text_edit_singleline(&mut c.domain);
                        ui.end_row();
                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(&mut c.password).password(true));
                        ui.end_row();
                    });
                    let users = self.data.connections.iter().filter(|x| x.credential == CredentialRef::Id(c.id)).count()
                        + self.data.folders.iter().filter(|x| x.credential == Some(c.id)).count();
                    ui.label(RichText::new(format!("Used by {users} connection(s)/folder(s)")).weak());
                    if c != before {
                        save = Some(c.clone());
                    }
                    if ui.button("🗑 Delete").clicked() {
                        delete = Some(c.id);
                    }
                }
            });
        });
        if let Some(c) = save {
            if self.result(self.store.save_credential(&c)).is_some() {
                match self.data.credentials.iter_mut().find(|x| x.id == c.id) {
                    Some(x) => *x = c,
                    None => self.data.credentials.push(c),
                }
            }
        }
        if let Some(id) = delete {
            if self.result(self.store.delete_credential(id)).is_some() {
                self.data.credentials.retain(|c| c.id != id);
                for c in &mut self.data.connections {
                    if c.credential == CredentialRef::Id(id) {
                        c.credential = CredentialRef::None;
                    }
                }
                for f in &mut self.data.folders {
                    if f.credential == Some(id) {
                        f.credential = None;
                    }
                }
                self.cred_selected = None;
                let sel = self.selection;
                self.editor_original = self.editor.clone();
                self.select(sel);
            }
        }
        self.show_credentials = open;
    }

    /// Opens a native file dialog without blocking the UI; the answer arrives in `poll_picker`.
    fn pick(&mut self, kind: Pick) {
        if self.picker.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.picker = Some(rx);
        let ctx = self.ctx.clone();
        let home = dirs::home_dir();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new();
            if let Some(h) = &home {
                dialog = dialog.set_directory(h);
            }
            let picked = match kind {
                Pick::Database => dialog
                    .set_title("Select an RDP-Manager backup (.zip) or database (SqLiteDatabase.db3)")
                    .add_filter("RDP-Manager backup or database", &["zip", "db3"])
                    .add_filter("RDP-Manager backup", &["zip"])
                    .add_filter("RDP-Manager database", &["db3"])
                    .add_filter("All files", &["*"])
                    .pick_file()
                    .map(Picked::Database),
                Pick::RdpFiles => dialog
                    .set_title("Select .rdp files")
                    .add_filter("Remote Desktop files", &["rdp"])
                    .pick_files()
                    .map(Picked::Rdp),
                Pick::RdpFolder => dialog.set_title("Select a folder with .rdp files").pick_folder().map(|dir| {
                    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                        .map(|rd| {
                            rd.flatten()
                                .map(|e| e.path())
                                .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rdp")))
                                .collect()
                        })
                        .unwrap_or_default();
                    files.sort();
                    Picked::Rdp(files)
                }),
            };
            tx.send(picked).ok();
            ctx.request_repaint();
        });
    }

    fn poll_picker(&mut self) {
        let Some(rx) = &self.picker else { return };
        let picked = match rx.try_recv() {
            Ok(p) => p,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        self.picker = None;
        match picked {
            Some(Picked::Database(path)) => {
                let app_password = self.import_dialog.take().map(|d| d.app_password).unwrap_or_default();
                let backup = backup::is_zip(&path).then(|| backup::inspect(&path).map_err(|e| format!("{e:#}")));
                self.import_dialog = Some(ImportDialog { path, app_password, backup });
            }
            Some(Picked::Rdp(files)) => {
                if files.is_empty() {
                    self.push(Level::Warn, "The selected folder contains no .rdp files.");
                }
                if let Some(d) = &mut self.rdp_import {
                    d.files = files;
                }
            }
            None => {}
        }
    }

    fn file_label(ui: &mut egui::Ui, path: &std::path::Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        ui.vertical(|ui| {
            // Leave room for a button on the right; long folders are shortened with "…".
            ui.set_max_width((ui.available_width() - 100.0).max(120.0));
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
            ui.add(egui::Label::new(RichText::new(dir).weak().small()).truncate());
        });
    }

    fn ui_import(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.import_dialog.take() else { return };
        let mut keep = true;
        let mut run = false;
        let mut change = false;
        let busy = self.picker.is_some();
        Self::modal("Import Windows RDP-Manager").show(ctx, |ui| {
            ui.set_max_width(480.0);
            theme::list_frame(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🗄");
                    Self::file_label(ui, &d.path);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!busy, egui::Button::new("Change …")).clicked() {
                            change = true;
                        }
                    });
                });
            });
            match &d.backup {
                Some(Ok(info)) => {
                    let mut text = format!("Backup of RDP-Manager {}", if info.version.is_empty() { "?" } else { &info.version });
                    if !info.date.is_empty() {
                        text += &format!(" from {}", info.date);
                    }
                    ui.label(text);
                    if !info.comment.is_empty() {
                        ui.label(RichText::new(format!("“{}”", info.comment)).italics());
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {
                    ui.label(RichText::new("A “DisableShowPw” file next to it is picked up automatically.").weak());
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Application password (if set):");
                ui.add(egui::TextEdit::singleline(&mut d.app_password).password(true));
            });
            ui.label(RichText::new("Entries with the same ID are overwritten; importing twice is harmless.").weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let importable = !matches!(d.backup, Some(Err(_)));
                if ui.add_enabled(!busy && importable, egui::Button::new("Import")).clicked() {
                    run = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if change {
            self.import_dialog = Some(d);
            self.pick(Pick::Database);
            return;
        }
        if run {
            let pw = Some(d.app_password.as_str()).filter(|p| !p.is_empty());
            match legacy_import::import(&d.path, pw) {
                Ok(r) => {
                    if self.result(self.store.save_all(&r.data)).is_some() {
                        let msg = format!(
                            "Import finished: {} folders, {} connections, {} credentials ({} deleted entries skipped).",
                            r.data.folders.len(),
                            r.data.connections.len(),
                            r.data.credentials.len(),
                            r.skipped_deleted
                        );
                        self.push(Level::Info, msg);
                        for w in r.warnings {
                            self.push(Level::Warn, w);
                        }
                        // Take over the layout of the original: an auto-hidden connection tree
                        // corresponds to the collapsed sidebar with hover overlay.
                        if let Some(collapsed) = r.backup.as_ref().and_then(|b| b.sidebar_autohide) {
                            self.s.sidebar_collapsed = collapsed;
                            self.push(
                                Level::Info,
                                if collapsed { "Sidebar collapsed as in the backup (auto-hide)." } else { "Sidebar docked as in the backup." },
                            );
                        }
                        if let Some(data) = self.result(self.store.load()) {
                            self.data = data;
                        }
                        self.editor_original = self.editor.clone();
                        self.select(None);
                        keep = false;
                    }
                }
                Err(e) => self.push(Level::Error, format!("Import failed: {e:#}")),
            }
        }
        if keep {
            self.import_dialog = Some(d);
        }
    }

    fn ui_rdp_import(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.rdp_import.take() else { return };
        let mut keep = true;
        let mut run = false;
        let mut pick = None;
        let busy = self.picker.is_some();
        let choices = self.folder_choices(None);
        Self::modal("Import .rdp Files").show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new("📄 Choose files …")).clicked() {
                    pick = Some(Pick::RdpFiles);
                }
                if ui.add_enabled(!busy, egui::Button::new("🗀 Choose folder …")).clicked() {
                    pick = Some(Pick::RdpFolder);
                }
                if busy {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
            theme::list_frame(ui, |ui| {
                egui::ScrollArea::vertical().max_height(180.0).auto_shrink([false, true]).show(ui, |ui| {
                    if d.files.is_empty() {
                        ui.label(RichText::new("No files selected.").weak());
                    }
                    for f in &d.files {
                        ui.horizontal(|ui| {
                            ui.label("🖥");
                            Self::file_label(ui, f);
                        });
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Target folder");
                Self::folder_combo(ui, "rdp_import_folder", &mut d.folder, &choices);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = if d.files.is_empty() { "Import".to_string() } else { format!("Import {} file(s)", d.files.len()) };
                if ui.add_enabled(!busy && !d.files.is_empty(), egui::Button::new(label)).clicked() {
                    run = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if let Some(kind) = pick {
            self.rdp_import = Some(d);
            self.pick(kind);
            return;
        }
        if run {
            let files = std::mem::take(&mut d.files);
            let mut ok = 0;
            for f in files {
                match rdpfile::parse_file(&f, d.folder) {
                    Ok(r) => {
                        for w in &r.warnings {
                            self.push(Level::Warn, w.clone());
                        }
                        let mut conn = r.connection;
                        if !r.username.is_empty() {
                            let existing = self.data.credentials.iter().find(|c| c.username == r.username && c.domain == r.domain).map(|c| c.id);
                            let id = match existing {
                                Some(id) => id,
                                None => {
                                    let name = if r.domain.is_empty() { r.username.clone() } else { format!("{}\\{}", r.domain, r.username) };
                                    let mut c = Credential::new(&name);
                                    c.username = r.username.clone();
                                    c.domain = r.domain.clone();
                                    let id = c.id;
                                    if self.result(self.store.save_credential(&c)).is_some() {
                                        self.data.credentials.push(c);
                                    }
                                    id
                                }
                            };
                            conn.credential = CredentialRef::Id(id);
                        }
                        if self.result(self.store.save_connection(&conn)).is_some() {
                            self.data.connections.push(conn);
                            ok += 1;
                        }
                    }
                    Err(e) => self.push(Level::Warn, format!("{}: {e:#}", f.display())),
                }
            }
            self.push(Level::Info, format!("{ok} .rdp file(s) imported."));
            keep = false;
        }
        if keep {
            self.rdp_import = Some(d);
        }
    }

    fn remove_tab(&mut self, session: u64) {
        if let Some(pos) = self.tabs.iter().position(|t| t.session == session) {
            let tab = self.tabs.remove(pos);
            if let Some(emb) = &self.embedder {
                emb.destroy(tab.host);
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
        if let Some(s) = self.sessions.get(&session) {
            #[cfg(unix)]
            {
                std::process::Command::new("kill").arg(s.pid.to_string()).status().ok();
            }
            #[cfg(not(unix))]
            {
                std::process::Command::new("taskkill").args(["/PID", &s.pid.to_string()]).status().ok();
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
        let covered = rect.is_some_and(|r| {
            ctx.memory(|m| {
                m.areas()
                    .visible_layer_ids()
                    .into_iter()
                    .filter(|l| !matches!(l.order, egui::Order::Background | egui::Order::Tooltip))
                    .any(|l| m.area_rect(l.id).is_some_and(|a| a.intersects(r)))
            })
        });
        let mut logs = Vec::new();
        let mut focus_change = None;
        for tab in &mut self.tabs {
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
            let visible = Some(tab.session) == self.active_tab && rect.is_some() && !covered;
            let geom = match rect.filter(|_| visible) {
                Some(r) => (
                    (r.min.x * ppp).round() as i32,
                    (r.min.y * ppp).round() as i32,
                    (r.width() * ppp).round().max(1.0) as u32,
                    (r.height() * ppp).round().max(1.0) as u32,
                    true,
                ),
                None => tab.placed.map(|(x, y, w, h, _)| (x, y, w, h, false)).unwrap_or((0, 0, 1, 1, false)),
            };
            // Clients create their window a moment after starting: fit them again now and then.
            let refit = visible && tab.last_fit.elapsed() > Duration::from_millis(500);
            if tab.placed != Some(geom) || refit {
                if let Err(e) = emb.place(tab.host, geom.0, geom.1, geom.2, geom.3, geom.4) {
                    logs.push((Level::Warn, format!("{}: {e:#}", tab.name)));
                }
                tab.placed = Some(geom);
                tab.last_fit = Instant::now();
            }
            // Keyboard: a click into the session gives it the focus.
            if visible && self.session_focus != Some(tab.session) {
                if let Some(p) = emb.pointer() {
                    let inside = p.x >= geom.0 && p.y >= geom.1 && p.x < geom.0 + geom.2 as i32 && p.y < geom.1 + geom.3 as i32;
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
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        theme::apply_style(ctx, self.s.dark);
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
        self.ui_dialogs(ctx);
        for a in actions {
            self.apply(a);
        }
        self.sync_embedded(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        launch::cleanup_secret_files();
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
}
