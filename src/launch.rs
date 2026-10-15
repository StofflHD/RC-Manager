//! Starts the actual clients (FreeRDP, ssh in a terminal, TigerVNC, browser …).
//!
//! The Windows original embeds ActiveX/PuTTY/VNC windows in tabs. On Linux the
//! established native clients are started in their own windows instead.

use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct Tools {
    /// Path and major version (2 or 3)
    pub freerdp: Option<(String, u32)>,
    /// Terminal program and the arguments before the actual command
    pub terminal: Option<(String, Vec<String>)>,
    pub sshpass: Option<String>,
    pub vncviewer: Option<String>,
    pub opener: Option<String>,
    /// xterm for SSH/Telnet sessions embedded in a tab (`-into`).
    pub xterm: Option<String>,
    pub home: String,
}

/// Where an embedded session is drawn: the X11 host window of its tab (see `embed.rs`).
#[derive(Debug, Clone, Copy)]
pub struct EmbedTarget {
    pub host: u32,
    /// Size of the tab in physical pixels.
    pub width: u32,
    pub height: u32,
    pub dark: bool,
}

pub fn find_in_path(name: &str) -> Option<String> {
    if name.contains('/') {
        return Path::new(name).is_file().then(|| name.to_string());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|p| p.join(name)).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
    })
}

/// Directory with the clients shipped inside the AppImage (exported by `AppRun`).
pub fn bundle_dir() -> Option<PathBuf> {
    std::env::var_os("RC_MANAGER_BUNDLE_DIR").map(PathBuf::from).filter(|d| d.is_dir())
}

fn in_bundle(name: &str) -> Option<String> {
    let p = bundle_dir()?.join(name);
    p.is_file().then(|| p.to_string_lossy().into_owned())
}

/// The dynamic loader refuses a binary built against newer libraries than the host has.
fn loader_failed(text: &str) -> bool {
    text.contains("error while loading shared libraries") || (text.contains("version `GLIBC") && text.contains("not found"))
}

/// Runs a program once (e.g. with `--version`) and returns its output, or `None` if it
/// cannot be started on this system.
fn probe(path: &str, arg: &str) -> Option<String> {
    let out = Command::new(path).arg(arg).stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (!loader_failed(&text)).then_some(text)
}

fn freerdp_at(path: String, name: &str) -> Option<(String, u32)> {
    let text = probe(&path, "--version")?;
    let major = text
        .split("version")
        .nth(1)
        .and_then(|v| v.trim().chars().next())
        .and_then(|c| c.to_digit(10))
        .unwrap_or(if name.contains('3') { 3 } else { 2 });
    Some((path, major))
}

pub fn is_bundled(path: &str) -> bool {
    bundle_dir().is_some_and(|d| Path::new(path).starts_with(d))
}

impl Tools {
    pub fn detect() -> Self {
        // Clients shipped in the AppImage come first, if they run on this system.
        const RDP: [&str; 6] = ["xfreerdp3", "sdl-freerdp3", "xfreerdp", "wlfreerdp3", "wlfreerdp", "sdl-freerdp"];
        let freerdp = RDP
            .iter()
            .find_map(|n| freerdp_at(in_bundle(n)?, n))
            .or_else(|| RDP.iter().find_map(|n| freerdp_at(find_in_path(n)?, n)));
        let bundled = |names: &[&str], arg: &str| names.iter().find_map(|n| in_bundle(n).filter(|p| probe(p, arg).is_some()));
        let vnc_names = ["vncviewer", "xtigervncviewer", "tigervnc"];

        let mut candidates: Vec<(String, Vec<&str>)> = Vec::new();
        if let Ok(t) = std::env::var("TERMINAL") {
            if !t.is_empty() {
                candidates.push((t, vec!["-e"]));
            }
        }
        for (n, a) in [
            ("ptyxis", vec!["--"]),
            ("gnome-terminal", vec!["--"]),
            ("konsole", vec!["-e"]),
            ("xfce4-terminal", vec!["-x"]),
            ("mate-terminal", vec!["-x"]),
            ("kgx", vec!["--"]),
            ("alacritty", vec!["-e"]),
            ("kitty", vec![]),
            ("foot", vec![]),
            ("wezterm", vec!["start", "--"]),
            ("x-terminal-emulator", vec!["-e"]),
            ("xterm", vec!["-e"]),
        ] {
            candidates.push((n.to_string(), a));
        }
        let terminal = candidates
            .into_iter()
            .find_map(|(n, a)| find_in_path(&n).map(|p| (p, a.into_iter().map(String::from).collect())));

        Self {
            freerdp,
            terminal,
            sshpass: bundled(&["sshpass"], "-V").or_else(|| find_in_path("sshpass")),
            vncviewer: bundled(&vnc_names, "-help").or_else(|| vnc_names.iter().find_map(|n| find_in_path(n))),
            opener: find_in_path("xdg-open"),
            xterm: bundled(&["xterm"], "-version").or_else(|| find_in_path("xterm")),
            home: dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }

    pub fn summary(&self) -> Vec<(&'static str, String)> {
        let label = |p: &str| if is_bundled(p) { format!("bundled with the AppImage ({p})") } else { p.to_string() };
        let show = |o: &Option<String>| o.as_deref().map(label).unwrap_or_else(|| "not found".into());
        vec![
            (
                "RDP (FreeRDP)",
                self.freerdp
                    .as_ref()
                    .map(|(p, v)| format!("{} – v{v}", label(p)))
                    .unwrap_or_else(|| "not found – package freerdp3-x11 or freerdp2-x11".into()),
            ),
            ("Terminal", self.terminal.as_ref().map(|(p, _)| p.clone()).unwrap_or_else(|| "not found".into())),
            ("sshpass (SSH password)", show(&self.sshpass)),
            ("VNC (TigerVNC)", self.vncviewer.as_deref().map(label).unwrap_or_else(|| "not found – package tigervnc-viewer".into())),
            ("Browser (xdg-open)", show(&self.opener)),
            ("xterm (SSH in tabs)", show(&self.xterm)),
        ]
    }
}

/// A fully planned process invocation.
#[derive(Debug, Default)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
    /// Environment variables for the client (FreeRDP 3: all arguments including the password,
    /// readable only by the user, unlike the command line).
    pub env: Vec<(String, String)>,
    /// Keep stdin open so that questions of the client (certificate checks) can be answered.
    pub interactive: bool,
    /// Files holding secrets: (path, content) – created with mode 0600 and deleted shortly after.
    pub secret_files: Vec<(PathBuf, Vec<u8>)>,
    pub working_dir: Option<String>,
    /// Command line for the log, without passwords
    pub display: String,
    pub warnings: Vec<String>,
    /// The client opens its own top-level window, which is moved into the tab (TigerVNC).
    pub adopt_window: bool,
}

fn secret_path(tag: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("rc-manager-{tag}-{}", uuid::Uuid::new_v4().simple()))
}

fn redact(args: &[String], secrets: &[&str]) -> String {
    args.iter()
        .map(|a| {
            let mut a = a.clone();
            for s in secrets.iter().filter(|s| !s.is_empty()) {
                a = a.replace(s, "••••");
            }
            if a.contains(' ') {
                format!("'{a}'")
            } else {
                a
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "1920x1080" → (1920, 1080); `None` for anything else, including zero sizes.
pub fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Windows keyboard layout ID ("00000407", "0x00000407"); `None` for empty, invalid or
/// "00000000", which the original uses for "no preselection".
pub fn keyboard_layout_id(s: &str) -> Option<u32> {
    let hex = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(hex, 16).ok().filter(|v| *v != 0)
}

fn split_extra(s: &str) -> Result<Vec<String>> {
    shell_words::split(s).map_err(|e| anyhow!("Invalid extra arguments: {e}"))
}

/// Host names end up as program arguments (and in `{T}` of external commands), so only
/// characters that can occur in DNS names and IPv4/IPv6 literals are allowed. A leading `-`
/// would be read as an option by ssh, telnet or vncviewer.
pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() {
        bail!("No target (host) given");
    }
    if host.starts_with('-') {
        bail!("Invalid host \"{host}\": must not start with \"-\"");
    }
    if let Some(c) = host.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']' | '%'))) {
        bail!("Invalid host \"{host}\": character {c:?} is not allowed");
    }
    Ok(())
}

/// Web connections only open http(s) URLs – no file://, smb:// or other scheme handlers.
pub fn validate_url(url: &str) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        bail!("Only http:// and https:// URLs can be opened (got \"{url}\")");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("The URL must not contain spaces or control characters");
    }
    Ok(())
}

const FREERDP_ARGS_ENV: &str = "RC_MANAGER_FREERDP_ARGS";

pub fn plan(conn: &Connection, cred: Option<&Credential>, tools: &Tools, embed: Option<&EmbedTarget>) -> Result<Plan> {
    match conn.protocol {
        Protocol::Web => {
            // Without a scheme the text is a host (optionally followed by a path) and gets https://.
            let host = conn.host.trim();
            if !host.contains("://") {
                validate_host(host.split('/').next().unwrap_or(""))?;
            }
            validate_url(&web_url(conn))?
        }
        Protocol::External if conn.host.trim().is_empty() => {}
        _ => validate_host(conn.host.trim())?,
    }
    match conn.protocol {
        Protocol::Rdp => plan_rdp(conn, cred, tools, embed),
        Protocol::Ssh | Protocol::Telnet => plan_terminal(conn, cred, tools, embed),
        Protocol::Vnc => plan_vnc(conn, cred, tools, embed),
        Protocol::Web => plan_web(conn, tools),
        Protocol::External => plan_external(conn, cred),
    }
}

fn plan_rdp(conn: &Connection, cred: Option<&Credential>, tools: &Tools, embed: Option<&EmbedTarget>) -> Result<Plan> {
    let (program, v) = tools.freerdp.clone().ok_or_else(|| anyhow!("FreeRDP is not installed (xfreerdp3/xfreerdp)"))?;
    let v3 = v >= 3;
    let o = &conn.options.rdp;
    let mut a: Vec<String> = Vec::new();
    a.push(format!("/v:{}", conn.host.trim()));
    a.push(format!("/port:{}", conn.effective_port()));
    a.push(if v3 { format!("/title:{}", conn.name) } else { format!("/t:{}", conn.name) });
    let mut secrets = Vec::new();
    if let Some(c) = cred {
        if !c.username.is_empty() {
            a.push(format!("/u:{}", c.username));
        }
        if !c.domain.is_empty() {
            a.push(format!("/d:{}", c.domain));
        }
        if !c.password.is_empty() {
            a.push(format!("/p:{}", c.password));
            secrets.push(c.password.as_str());
        }
    }
    if let Some(e) = embed {
        // Drawn into the tab. Either the remote resolution follows the tab size, or a fixed
        // resolution is scaled to it (smart sizing) – the two modes of the original.
        a.push(format!("/parent-window:{}", e.host));
        match parse_resolution(&o.resolution).filter(|_| !o.dynamic_resolution) {
            Some((w, h)) => {
                a.push(format!("/size:{w}x{h}"));
                a.push("/smart-sizing".into());
            }
            None => {
                a.push(format!("/size:{}x{}", e.width.max(64), e.height.max(64)));
                a.push("/dynamic-resolution".into());
            }
        }
    } else {
        match o.window_mode {
            WindowMode::Tab | WindowMode::Window => {
                if let Some((w, h)) = parse_resolution(&o.resolution) {
                    a.push(format!("/size:{w}x{h}"));
                }
            }
            WindowMode::Fullscreen => a.push("/f".into()),
            WindowMode::MultiMonitor => {
                a.push("/f".into());
                a.push("/multimon".into());
            }
        }
        if o.dynamic_resolution {
            a.push("/dynamic-resolution".into());
        }
    }
    if o.color_depth > 0 {
        a.push(format!("/bpp:{}", o.color_depth));
    }
    a.push(if o.clipboard { "+clipboard" } else { "-clipboard" }.into());
    if o.sound {
        a.push("/sound".into());
    }
    if o.microphone {
        a.push("/microphone".into());
    }
    if o.printers {
        a.push("/printer".into());
    }
    if o.smartcards {
        a.push("/smartcard".into());
    }
    if o.home_drive && !tools.home.is_empty() {
        a.push(format!("/drive:home,{}", tools.home));
    }
    for d in o.drives.iter().filter(|d| !d.trim().is_empty()) {
        let (name, path) = d.split_once('=').unwrap_or(("share", d.as_str()));
        a.push(format!("/drive:{},{}", name.trim(), path.trim()));
    }
    if o.console {
        a.push("/admin".into());
    }
    a.push(match (o.ignore_certificate, v3) {
        (true, true) => "/cert:ignore",
        (true, false) => "/cert-ignore",
        (false, true) => "/cert:tofu",
        (false, false) => "/cert-tofu",
    }
    .into());
    if !o.gateway.trim().is_empty() {
        // Gateway logon: its own credentials or those of the connection
        let (gu, gd, gp) = match cred {
            Some(c) if o.gateway_user.is_empty() => (c.username.clone(), c.domain.clone(), c.password.clone()),
            _ => (o.gateway_user.clone(), o.gateway_domain.clone(), String::new()),
        };
        if v3 {
            let mut g = format!("/gateway:g:{}", o.gateway.trim());
            if !gu.is_empty() {
                g += &format!(",u:{gu}");
            }
            if !gd.is_empty() {
                g += &format!(",d:{gd}");
            }
            if !gp.is_empty() {
                g += &format!(",p:{gp}");
            }
            a.push(g);
        } else {
            a.push(format!("/g:{}", o.gateway.trim()));
            if !gu.is_empty() {
                a.push(format!("/gu:{gu}"));
            }
            if !gd.is_empty() {
                a.push(format!("/gd:{gd}"));
            }
            if !gp.is_empty() {
                a.push(format!("/gp:{gp}"));
            }
        }
    }
    if let Some(id) = keyboard_layout_id(&o.keyboard_layout) {
        a.push(if v3 { format!("/kbd:layout:0x{id:08X}") } else { format!("/kbd:0x{id:08X}") });
    }
    if !o.remote_app.trim().is_empty() {
        a.push(if v3 { format!("/app:program:{}", o.remote_app.trim()) } else { format!("/app:{}", o.remote_app.trim()) });
    }
    a.extend(split_extra(&o.extra_args)?);

    let display = format!("{program} {}", redact(&a, &secrets));
    // stdin stays open: FreeRDP asks there whether to trust an unknown certificate.
    let mut plan = Plan { program, display, interactive: true, ..Default::default() };
    if v3 {
        // All arguments via an environment variable so the password does not show up in the
        // process list (/proc/<pid>/environ is readable only by the user).
        if a.iter().any(|x| x.contains('\n')) {
            bail!("Arguments must not contain line breaks");
        }
        plan.args = vec![format!("/args-from:env:{FREERDP_ARGS_ENV}")];
        plan.env.push((FREERDP_ARGS_ENV.into(), a.join("\n")));
    } else {
        if !secrets.is_empty() {
            plan.warnings.push("FreeRDP 2 can only take the password on the command line – other local users can see it in the process list. FreeRDP 3 is recommended.".into());
        }
        plan.args = a;
    }
    Ok(plan)
}

/// Runs a command in a terminal and keeps the window open on failure.
fn in_terminal(tools: &Tools, title: &str, cmd: Vec<String>) -> Result<(String, Vec<String>)> {
    let (term, prefix) = tools.terminal.clone().ok_or_else(|| anyhow!("No terminal emulator found (e.g. gnome-terminal, konsole, xterm)"))?;
    let mut args = Vec::new();
    let name = Path::new(&term).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.as_str() {
        "gnome-terminal" | "mate-terminal" | "xfce4-terminal" | "ptyxis" => args.extend(["--title".to_string(), title.to_string()]),
        "konsole" => args.extend(["-p".to_string(), format!("tabtitle={title}")]),
        "xterm" | "alacritty" => args.extend(["-T".to_string(), title.to_string()]),
        "kitty" => args.extend(["--title".to_string(), title.to_string()]),
        _ => {}
    }
    args.extend(prefix);
    args.extend(["sh".to_string(), "-c".to_string()]);
    args.push(r#""$@"; s=$?; if [ $s -ne 0 ]; then printf '\n[Exited with code %s – press Enter to close] ' $s; read _; fi"#.to_string());
    args.push("rc-manager".to_string());
    args.extend(cmd);
    Ok((term, args))
}

/// Runs a command in an xterm drawn into a tab, coloured like RC Manager's theme.
fn in_xterm(xterm: &str, e: &EmbedTarget, title: &str, cmd: Vec<String>) -> (String, Vec<String>) {
    let (bg, fg) = if e.dark { ("#101010", "#e1e1e1") } else { ("#ffffff", "#141414") };
    let mut args: Vec<String> = ["-into", &e.host.to_string(), "-T", title, "-fa", "DejaVu Sans Mono:size=11", "-bg", bg, "-fg", fg, "-cr", "#0066cc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(["+sb", "-bc", "-e", "sh", "-c"].map(String::from));
    args.push(r#""$@"; s=$?; if [ $s -ne 0 ]; then printf '\n[Exited with code %s – press Enter to close] ' $s; read _; fi"#.to_string());
    args.push("rc-manager".to_string());
    args.extend(cmd);
    (xterm.to_string(), args)
}

fn plan_terminal(conn: &Connection, cred: Option<&Credential>, tools: &Tools, embed: Option<&EmbedTarget>) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut cmd: Vec<String> = Vec::new();
    let port = conn.effective_port();
    if conn.protocol == Protocol::Telnet {
        cmd.extend(["telnet".into(), conn.host.trim().into(), port.to_string()]);
    } else {
        let o = &conn.options.ssh;
        let password = cred.map(|c| c.password.as_str()).unwrap_or("");
        if !password.is_empty() {
            match &tools.sshpass {
                Some(sp) => {
                    let file = secret_path("sshpass");
                    cmd.extend([sp.clone(), "-f".into(), file.to_string_lossy().into_owned()]);
                    plan.secret_files.push((file, password.as_bytes().to_vec()));
                }
                None => plan.warnings.push("sshpass is not installed – enter the password in the terminal.".into()),
            }
        }
        cmd.push("ssh".into());
        if plan.secret_files.iter().any(|(p, _)| p.to_string_lossy().contains("sshpass")) {
            // sshpass cannot answer "accept this host key?": store new keys (trust on first
            // use), but still refuse changed ones.
            cmd.extend(["-o".into(), "StrictHostKeyChecking=accept-new".into()]);
        }
        if port != 22 {
            cmd.extend(["-p".into(), port.to_string()]);
        }
        if let Some(c) = cred.filter(|c| !c.username.is_empty()) {
            cmd.extend(["-l".into(), c.username.clone()]);
        }
        if o.x11_forwarding {
            cmd.push("-X".into());
        }
        if o.compression {
            cmd.push("-C".into());
        }
        if !o.identity_file.trim().is_empty() {
            cmd.extend(["-i".into(), o.identity_file.trim().into()]);
        }
        cmd.extend(split_extra(&o.extra_args)?);
        // `--` ends option parsing, so the host can never be taken for an option.
        cmd.push("--".into());
        cmd.push(conn.host.trim().into());
        if !o.remote_command.trim().is_empty() {
            cmd.push(o.remote_command.trim().into());
        }
    }
    plan.display = redact(&cmd, &[]);
    let (program, args) = match (embed, &tools.xterm) {
        (Some(e), Some(xterm)) => in_xterm(xterm, e, &conn.name, cmd),
        _ => in_terminal(tools, &conn.name, cmd)?,
    };
    plan.program = program;
    plan.args = args;
    Ok(plan)
}

/// Password file in `vncpasswd` format (DES with a fixed key).
pub fn vnc_password_file(password: &str) -> Vec<u8> {
    use des::cipher::{BlockEncrypt, KeyInit};
    // Bit-reversed standard key {23,82,107,6,35,78,88,7}
    let key = [0xE8u8, 0x4A, 0xD6, 0x60, 0xC4, 0x72, 0x1A, 0xE0];
    let mut block = [0u8; 8];
    for (i, b) in password.bytes().take(8).enumerate() {
        block[i] = b;
    }
    let cipher = des::Des::new_from_slice(&key).expect("8-byte key");
    let mut ga = block.into();
    cipher.encrypt_block(&mut ga);
    ga.to_vec()
}

fn plan_vnc(conn: &Connection, cred: Option<&Credential>, tools: &Tools, embed: Option<&EmbedTarget>) -> Result<Plan> {
    let port = conn.effective_port();
    let Some(viewer) = tools.vncviewer.clone() else {
        let opener = tools.opener.clone().ok_or_else(|| anyhow!("Neither vncviewer nor xdg-open found"))?;
        let url = format!("vnc://{}:{port}", conn.host.trim());
        return Ok(Plan { program: opener.clone(), display: format!("{opener} {url}"), args: vec![url], ..Default::default() });
    };
    let o = &conn.options.vnc;
    let mut plan = Plan { program: viewer.clone(), adopt_window: embed.is_some(), ..Default::default() };
    let mut a = vec![format!("{}::{port}", conn.host.trim())];
    if let Some(c) = cred.filter(|c| !c.password.is_empty()) {
        let file = secret_path("vncpasswd");
        a.extend(["-passwd".into(), file.to_string_lossy().into_owned()]);
        plan.secret_files.push((file, vnc_password_file(&c.password)));
    }
    if o.view_only {
        a.push("-ViewOnly".into());
    }
    if o.fullscreen {
        a.push("-FullScreen".into());
    }
    a.push(format!("-Shared={}", o.shared as u8));
    a.extend(split_extra(&o.extra_args)?);
    plan.display = format!("{viewer} {}", redact(&a, &[]));
    plan.args = a;
    Ok(plan)
}

pub fn web_url(conn: &Connection) -> String {
    let host = conn.host.trim();
    if host.contains("://") {
        return host.to_string();
    }
    let scheme = if conn.port == 80 { "http" } else { "https" };
    match conn.port {
        0 | 80 | 443 => format!("{scheme}://{host}"),
        p => format!("{scheme}://{host}:{p}"),
    }
}

fn plan_web(conn: &Connection, tools: &Tools) -> Result<Plan> {
    let opener = tools.opener.clone().ok_or_else(|| anyhow!("xdg-open not found"))?;
    let url = web_url(conn);
    Ok(Plan { display: format!("{opener} {url}"), program: opener, args: vec![url], ..Default::default() })
}

/// Replaces the placeholders {T} {N} {U} {D} {P} {PORT} in every word of the template.
pub fn expand_template(template: &str, conn: &Connection, cred: Option<&Credential>) -> Result<Vec<String>> {
    let words = shell_words::split(template).map_err(|e| anyhow!("Invalid command line: {e}"))?;
    let (u, d, p) = cred.map(|c| (c.username.as_str(), c.domain.as_str(), c.password.as_str())).unwrap_or(("", "", ""));
    Ok(words
        .into_iter()
        .map(|w| {
            w.replace("{T}", conn.host.trim())
                .replace("{N}", &conn.name)
                .replace("{U}", u)
                .replace("{D}", d)
                .replace("{P}", p)
                .replace("{PORT}", &conn.effective_port().to_string())
        })
        .collect())
}

fn plan_external(conn: &Connection, cred: Option<&Credential>) -> Result<Plan> {
    let o = &conn.options.external;
    let words = expand_template(&o.command, conn, cred)?;
    let Some((program, args)) = words.split_first() else { bail!("No program given") };
    let pw = cred.map(|c| c.password.as_str()).unwrap_or("");
    let mut plan = Plan {
        program: program.clone(),
        args: args.to_vec(),
        display: redact(&words, &[pw]),
        working_dir: Some(o.working_dir.clone()).filter(|w| !w.trim().is_empty()),
        ..Default::default()
    };
    if o.command.contains("{P}") && !pw.is_empty() {
        plan.warnings.push("The password is passed as a command-line argument and is visible in the process list.".into());
    }
    Ok(plan)
}

/// Messages from running sessions to the UI.
pub enum Event {
    Output { session: u64, line: String },
    /// The client waits for an answer on stdin (e.g. FreeRDP's certificate question).
    Prompt { session: u64, text: String },
    Exited { session: u64, code: Option<i32> },
}

/// A started client.
pub struct Spawned {
    pub pid: u32,
    /// Open stdin for answering prompts (`Plan::interactive`).
    pub stdin: Option<ChildStdin>,
}

/// Questions that end without a line break and wait for input.
fn is_prompt(text: &str) -> bool {
    text.contains("(Y/T/N)") || text.contains("(Y/N)")
}

/// Splits a stream into lines and also reports an unfinished line that is a prompt.
fn forward_output(mut stream: impl Read, session: u64, tx: &Sender<Event>, wake: &dyn Fn()) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
            if tx.send(Event::Output { session, line }).is_err() {
                return;
            }
        }
        let rest = String::from_utf8_lossy(&buf).to_string();
        if is_prompt(&rest) {
            buf.clear();
            if tx.send(Event::Prompt { session, text: rest.trim().to_string() }).is_err() {
                return;
            }
        }
        wake();
    }
    if !buf.is_empty() {
        tx.send(Event::Output { session, line: String::from_utf8_lossy(&buf).trim_end().to_string() }).ok();
        wake();
    }
}

/// Secret files that still exist; removed after a delay or at the latest when the app exits.
static SECRET_FILES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

fn remove_secret_files(paths: &[PathBuf]) {
    let mut list = SECRET_FILES.lock().unwrap_or_else(|e| e.into_inner());
    for p in paths {
        std::fs::remove_file(p).ok();
        list.retain(|x| x != p);
    }
}

/// Deletes all secret files that have not been cleaned up yet (called on exit).
pub fn cleanup_secret_files() {
    let paths = SECRET_FILES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    remove_secret_files(&paths);
}

/// Starts the planned process, forwards its output and cleans up secret files.
/// Starts the planned process.
pub fn spawn(plan: &Plan, session: u64, events: Sender<Event>, wake: impl Fn() + Send + Clone + 'static) -> Result<Spawned> {
    let cleanup: Vec<PathBuf> = plan.secret_files.iter().map(|(p, _)| p.clone()).collect();
    SECRET_FILES.lock().unwrap_or_else(|e| e.into_inner()).extend(cleanup.iter().cloned());
    for (path, content) in &plan.secret_files {
        if let Err(e) = crate::vault::create_private(path, content) {
            remove_secret_files(&cleanup);
            return Err(e);
        }
    }
    if !cleanup.is_empty() {
        let later = cleanup.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            remove_secret_files(&later);
        });
    }

    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .envs(plan.env.iter().map(|(k, v)| (k, v)))
        .stdin(if plan.interactive { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &plan.working_dir {
        cmd.current_dir(dir);
    }
    let mut child: Child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            remove_secret_files(&cleanup);
            return Err(e).with_context(|| format!("{} could not be started", plan.program));
        }
    };
    let pid = child.id();
    let stdin = child.stdin.take();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as _)]
        .into_iter()
        .flatten()
    {
        let tx = events.clone();
        let wake = wake.clone();
        std::thread::spawn(move || forward_output(stream, session, &tx, &wake));
    }
    std::thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code());
        events.send(Event::Exited { session, code }).ok();
        wake();
    });
    Ok(Spawned { pid, stdin })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(v: u32) -> Tools {
        Tools {
            freerdp: Some(("/usr/bin/xfreerdp".into(), v)),
            terminal: Some(("/usr/bin/xterm".into(), vec!["-e".into()])),
            sshpass: Some("/usr/bin/sshpass".into()),
            vncviewer: Some("/usr/bin/vncviewer".into()),
            opener: Some("/usr/bin/xdg-open".into()),
            xterm: Some("/usr/bin/xterm".into()),
            home: "/home/test".into(),
        }
    }

    fn cred() -> Credential {
        Credential { username: "admin".into(), domain: "FIRMA".into(), password: "pa ss".into(), ..Credential::new("x") }
    }

    #[test]
    fn rdp_v3_passes_secrets_via_env() {
        let mut c = Connection::new("Srv", Protocol::Rdp, None);
        c.host = "10.0.0.5".into();
        c.port = 3390;
        c.options.rdp.window_mode = WindowMode::Window;
        c.options.rdp.resolution = "1280x720".into();
        c.options.rdp.dynamic_resolution = false;
        let p = plan(&c, Some(&cred()), &tools(3), None).unwrap();
        assert_eq!(p.args, vec!["/args-from:env:RC_MANAGER_FREERDP_ARGS"]);
        assert!(p.interactive, "stdin stays open for certificate questions");
        let (name, value) = &p.env[0];
        assert_eq!(name, "RC_MANAGER_FREERDP_ARGS");
        let lines: Vec<&str> = value.lines().collect();
        assert!(lines.contains(&"/v:10.0.0.5"));
        assert!(lines.contains(&"/port:3390"));
        assert!(lines.contains(&"/p:pa ss"));
        assert!(lines.contains(&"/size:1280x720"));
        assert!(lines.contains(&"/cert:tofu"));
        assert!(!p.display.contains("pa ss"));
    }

    #[test]
    fn prompts_are_detected() {
        let (tx, rx) = std::sync::mpsc::channel();
        let out = "Certificate details for 127.0.0.1:3389:\n\tCommon Name: srv\nDo you trust the above certificate? (Y/T/N) ";
        forward_output(out.as_bytes(), 7, &tx, &|| {});
        let events: Vec<Event> = rx.try_iter().collect();
        assert!(matches!(&events[0], Event::Output { line, .. } if line == "Certificate details for 127.0.0.1:3389:"));
        assert!(matches!(&events[2], Event::Prompt { session: 7, text } if text.ends_with("(Y/T/N)")));
    }

    #[test]
    fn rdp_v2_uses_legacy_syntax() {
        let mut c = Connection::new("Srv", Protocol::Rdp, None);
        c.host = "h".into();
        c.options.rdp.ignore_certificate = true;
        c.options.rdp.window_mode = WindowMode::MultiMonitor;
        c.options.rdp.gateway = "gw.example".into();
        let p = plan(&c, Some(&cred()), &tools(2), None).unwrap();
        assert!(p.env.is_empty());
        assert!(p.args.contains(&"/cert-ignore".to_string()));
        assert!(p.args.contains(&"/multimon".to_string()));
        assert!(p.args.contains(&"/g:gw.example".to_string()));
        assert!(p.args.contains(&"/gu:admin".to_string()));
        assert!(!p.warnings.is_empty());
    }

    #[test]
    fn ssh_in_terminal_with_sshpass_file() {
        let mut c = Connection::new("Linux", Protocol::Ssh, None);
        c.host = "box".into();
        c.port = 2222;
        let p = plan(&c, Some(&cred()), &tools(3), None).unwrap();
        assert_eq!(p.program, "/usr/bin/xterm");
        let pos = p.args.iter().position(|a| a == "/usr/bin/sshpass").unwrap();
        assert_eq!(p.args[pos + 1], "-f");
        assert_eq!(&p.args[pos + 3..], ["ssh", "-o", "StrictHostKeyChecking=accept-new", "-p", "2222", "-l", "admin", "--", "box"]);
        assert_eq!(p.secret_files.len(), 1);
        assert!(!p.args.iter().any(|a| a.contains("pa ss")));
    }

    #[test]
    fn vnc_password_file_format() {
        // Reference: `printf password | vncpasswd -f | xxd -p`
        let hex: String = vnc_password_file("password").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "dbd83cfd727a1458");
    }

    #[test]
    fn external_template() {
        let mut c = Connection::new("App", Protocol::External, None);
        c.host = "srv".into();
        c.options.external.command = "remmina -c 'rdp://{U}@{T}:{PORT}'".into();
        c.port = 3389;
        let p = plan(&c, Some(&cred()), &tools(3), None).unwrap();
        assert_eq!(p.program, "remmina");
        assert_eq!(p.args, vec!["-c", "rdp://admin@srv:3389"]);
    }

    #[test]
    fn rejects_option_injection_via_host() {
        for proto in [Protocol::Ssh, Protocol::Telnet, Protocol::Vnc, Protocol::Rdp, Protocol::External] {
            for host in ["-oProxyCommand=id", "host name", "a;id", "x
/p:secret", "$(id)"] {
                let mut c = Connection::new("x", proto, None);
                c.host = host.into();
                c.options.external.command = "sh -c 'ping {T}'".into();
                assert!(plan(&c, None, &tools(3), None).is_err(), "{proto:?} accepted host {host:?}");
            }
        }
    }

    #[test]
    fn web_only_opens_http_urls() {
        for url in ["file:///etc/passwd", "smb://evil/share", "javascript:alert(1)", "-x", "https://ok.example/ path"] {
            let mut c = Connection::new("W", Protocol::Web, None);
            c.host = url.into();
            assert!(plan(&c, None, &tools(3), None).is_err(), "accepted {url:?}");
        }
        let mut c = Connection::new("W", Protocol::Web, None);
        c.host = "http://intranet/app?x=1".into();
        assert!(plan(&c, None, &tools(3), None).is_ok());
    }

    #[test]
    fn no_keyboard_or_size_for_placeholders() {
        let mut c = Connection::new("Srv", Protocol::Rdp, None);
        c.host = "srv".into();
        c.options.rdp.dynamic_resolution = false;
        for (kbd, res) in [("00000000", "0x0"), ("", ""), ("zz", "800x"), ("0x0", "x600")] {
            c.options.rdp.keyboard_layout = kbd.into();
            c.options.rdp.resolution = res.into();
            let p = plan(&c, None, &tools(2), None).unwrap();
            assert!(!p.args.iter().any(|a| a.starts_with("/kbd") || a.starts_with("/size")), "{kbd:?} {res:?}: {:?}", p.args);
        }
        c.options.rdp.keyboard_layout = "00000407".into();
        c.options.rdp.resolution = "1280X720".into();
        let p = plan(&c, None, &tools(2), None).unwrap();
        assert!(p.args.contains(&"/kbd:0x00000407".to_string()));
        assert!(p.args.contains(&"/size:1280x720".to_string()));
        let p = plan(&c, None, &tools(3), None).unwrap();
        assert!(p.env[0].1.lines().any(|l| l == "/kbd:layout:0x00000407"));
    }

    #[test]
    fn embedded_sessions() {
        let e = EmbedTarget { host: 4242, width: 1000, height: 700, dark: true };
        let mut c = Connection::new("Srv", Protocol::Rdp, None);
        c.host = "srv".into();
        let p = plan(&c, None, &tools(2), Some(&e)).unwrap();
        assert!(p.args.contains(&"/parent-window:4242".to_string()));
        assert!(p.args.contains(&"/size:1000x700".to_string()));
        assert!(p.args.contains(&"/dynamic-resolution".to_string()));
        assert!(!p.args.iter().any(|a| a == "/f"));
        // Fixed resolution: scaled into the tab.
        c.options.rdp.dynamic_resolution = false;
        c.options.rdp.resolution = "1280x1024".into();
        let p = plan(&c, None, &tools(2), Some(&e)).unwrap();
        assert!(p.args.contains(&"/size:1280x1024".to_string()) && p.args.contains(&"/smart-sizing".to_string()));

        let mut c = Connection::new("Box", Protocol::Ssh, None);
        c.host = "box".into();
        let p = plan(&c, None, &tools(3), Some(&e)).unwrap();
        assert_eq!(p.program, "/usr/bin/xterm");
        assert_eq!(&p.args[..2], ["-into", "4242"]);
        assert!(p.args.ends_with(&["ssh".to_string(), "--".to_string(), "box".to_string()]));

        let mut c = Connection::new("V", Protocol::Vnc, None);
        c.host = "v".into();
        assert!(plan(&c, None, &tools(3), Some(&e)).unwrap().adopt_window);
        assert!(!plan(&c, None, &tools(3), None).unwrap().adopt_window);
    }

    #[test]
    fn loader_errors_are_detected() {
        assert!(loader_failed("xfreerdp: error while loading shared libraries: libwinpr3.so.3: cannot open shared object file"));
        assert!(loader_failed("/tmp/x/xfreerdp: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.34' not found (required by xfreerdp)"));
        assert!(!loader_failed("This is FreeRDP version 3.32.2 (n/a)"));
    }

    #[test]
    fn web_urls() {
        let mut c = Connection::new("W", Protocol::Web, None);
        c.host = "intranet".into();
        assert_eq!(web_url(&c), "https://intranet");
        c.port = 8080;
        assert_eq!(web_url(&c), "https://intranet:8080");
        c.host = "http://x/y".into();
        assert_eq!(web_url(&c), "http://x/y");
    }
}
