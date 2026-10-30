//! Import from `SqLiteDatabase.db3` of the Windows RDP-Manager (database version 12).

use super::{backup, crypto, nrbf};
use crate::model::*;
use crate::store::Data;
use anyhow::{bail, Context, Result};
use rusqlite::types::ValueRef;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

const TYPE_RDP: &str = "604b598c-a359-4cd0-81fd-96298abb962d";
const TYPE_PUTTY: &str = "4cb65fa2-506e-4e3f-93e5-ab7a16f79397";
const TYPE_VNC: &str = "bec8fd2b-680f-446d-9cf9-ded005c41f8e";
const TYPE_WEB: &str = "8fc21de3-ea31-4125-97f7-e191ba32ee9b";
const TYPE_EXT: &str = "acc5553a-3334-40ff-ad95-1ae7d4f931a5";
const CRED_INHERIT: &str = "3338af54-19fb-4875-b752-8b6f035764d6";
const SETTING_INHERIT: &str = "e338af54-19fb-4875-b752-8b6f035764d6";

pub struct ImportResult {
    pub data: Data,
    pub warnings: Vec<String>,
    pub skipped_deleted: usize,
    /// Set when a backup ZIP was imported.
    pub backup: Option<backup::BackupInfo>,
}

type Row = HashMap<String, String>;

fn read_table(db: &rusqlite::Connection, table: &str) -> Result<Vec<Row>> {
    let mut stmt = match db.prepare(&format!("SELECT * FROM {table}")) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let mut m = Row::new();
        for (i, n) in names.iter().enumerate() {
            let v = match r.get_ref(i)? {
                ValueRef::Null => continue,
                ValueRef::Integer(i) => i.to_string(),
                ValueRef::Real(f) => f.to_string(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                ValueRef::Blob(b) => {
                    // BLOBs (BinVal) are read separately; GUIDs might be stored as binary.
                    if b.len() == 16 {
                        Uuid::from_bytes_le(b.try_into().unwrap()).to_string()
                    } else {
                        continue;
                    }
                }
            };
            m.insert(n.to_ascii_lowercase(), v);
        }
        out.push(m);
    }
    Ok(out)
}

fn read_blobs(db: &rusqlite::Connection, table: &str) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    if let Ok(mut stmt) = db.prepare(&format!("SELECT Id, BinVal FROM {table}")) {
        if let Ok(rows) = stmt.query_map([], |r| {
            let id = match r.get_ref(0)? {
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                _ => String::new(),
            };
            let blob = match r.get_ref(1)? {
                ValueRef::Blob(b) => b.to_vec(),
                _ => Vec::new(),
            };
            Ok((id, blob))
        }) {
            for (id, blob) in rows.flatten() {
                if let Some(g) = guid(&id) {
                    out.insert(g, blob);
                }
            }
        }
    }
    out
}

/// Normalises a GUID (any case, with or without braces) to lower case.
fn guid(s: &str) -> Option<String> {
    Uuid::parse_str(s.trim().trim_start_matches('{').trim_end_matches('}')).ok().map(|u| u.to_string())
}

fn truthy(s: Option<&String>) -> bool {
    matches!(s.map(|s| s.to_ascii_lowercase()).as_deref(), Some("1") | Some("true"))
}

fn get<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).map(String::as_str).unwrap_or("")
}

/// Reads the database. `app_password` is only needed if the original had an application password set.
/// Imports `SqLiteDatabase.db3` or a backup ZIP of the original (detected by content).
/// `app_password` is only needed if the original had an application password set.
pub fn import(path: &Path, app_password: Option<&str>) -> Result<ImportResult> {
    // Work on a private copy so the original file (WAL) is never modified.
    let tmp = std::env::temp_dir().join(format!("rcm-import-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    let result = (|| {
        // The copy holds passwords that are only protected by the original's built-in key.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
        }
        let copy = tmp.join("import.db3");
        if backup::is_zip(path) {
            let ex = backup::extract(path)?;
            crate::vault::create_private(&copy, &ex.database)?;
            if let Some(wal) = &ex.database_wal {
                crate::vault::create_private(&tmp.join("import.db3-wal"), wal)?;
            }
            let mut r = import_inner(&copy, ex.disable_show_pw.as_deref(), app_password)?;
            r.backup = Some(ex.info);
            Ok(r)
        } else {
            std::fs::copy(path, &copy).with_context(|| format!("Cannot read {}", path.display()))?;
            let wal = path.with_file_name(format!("{}-wal", path.file_name().unwrap_or_default().to_string_lossy()));
            if wal.exists() {
                std::fs::copy(&wal, tmp.join("import.db3-wal")).ok();
            }
            let disable_show_pw = std::fs::read_to_string(path.with_file_name("DisableShowPw")).ok();
            import_inner(&copy, disable_show_pw.as_deref(), app_password)
        }
    })();
    std::fs::remove_dir_all(&tmp).ok();
    result
}

fn import_inner(copy: &Path, disable_show_pw: Option<&str>, app_password: Option<&str>) -> Result<ImportResult> {
    let db = rusqlite::Connection::open(copy)?;
    let mut warnings = Vec::new();

    let app_settings: HashMap<String, String> =
        read_table(&db, "AppSettings")?.into_iter().map(|r| (get(&r, "key").to_string(), get(&r, "value").to_string())).collect();
    if app_settings.is_empty() && read_table(&db, "Connections")?.is_empty() {
        bail!("Not an RDP-Manager database (tables AppSettings/Connections missing or empty)");
    }

    let stored_hash = app_settings.get("AppPassword").cloned().unwrap_or_default();
    if !stored_hash.is_empty() {
        match app_password {
            Some(pw) if crypto::md5_upper_hex(pw).eq_ignore_ascii_case(&stored_hash) => {}
            Some(_) => bail!("The application password is wrong"),
            None => bail!("This database is protected by an application password – please enter it"),
        }
    }
    let key_password = crypto::effective_password(app_password.filter(|_| !stored_hash.is_empty()), disable_show_pw);
    let dpapi = app_settings.get("EncryptionType").map(|s| s.eq_ignore_ascii_case("WindowsUserEncryption")).unwrap_or(false);
    if dpapi {
        warnings.push("Passwords were stored with Windows user encryption (DPAPI) and cannot be decrypted on Linux. Switch the original to AES before exporting.".into());
    }

    // --- Folders ---
    let mut skipped_deleted = 0;
    let folder_rows = read_table(&db, "Folders")?;
    let mut folder_parent: HashMap<String, String> = HashMap::new();
    let mut folders = Vec::new();
    for r in &folder_rows {
        let Some(id) = guid(get(r, "id")) else { continue };
        if truthy(r.get("deleted")) {
            skipped_deleted += 1;
            continue;
        }
        folder_parent.insert(id.clone(), guid(get(r, "parent")).unwrap_or_default());
        folders.push((id, r));
    }
    let folder_ids: HashSet<String> = folder_parent.keys().cloned().collect();
    let parent_of = |p: &str| -> Option<Uuid> { guid(p).filter(|g| folder_ids.contains(g)).and_then(|g| Uuid::parse_str(&g).ok()) };

    // --- Credentials ---
    let mut credentials = Vec::new();
    let mut cred_type: HashMap<String, String> = HashMap::new();
    let mut decrypt_failed = 0;
    for r in read_table(&db, "Credentials")? {
        let Some(id) = guid(get(&r, "id")) else { continue };
        let enc = get(&r, "password");
        let password = if enc.is_empty() || dpapi {
            String::new()
        } else {
            crypto::decrypt_aes_string(enc, &key_password).unwrap_or_else(|_| {
                decrypt_failed += 1;
                String::new()
            })
        };
        cred_type.insert(id.clone(), guid(get(&r, "type")).unwrap_or_default());
        let name = get(&r, "name");
        credentials.push(Credential {
            id: Uuid::parse_str(&id)?,
            name: if name.is_empty() || guid(name).is_some() { get(&r, "username").to_string() } else { name.to_string() },
            username: get(&r, "username").to_string(),
            domain: get(&r, "domain").to_string(),
            password,
        });
    }
    if decrypt_failed > 0 {
        warnings.push(format!("{decrypt_failed} password(s) could not be decrypted (DisableShowPw file or application password missing?)."));
    }
    let cred_ids: HashSet<String> = cred_type.keys().cloned().collect();

    // Folder → credentials / settings per protocol type
    let mut folder_creds: HashMap<String, Vec<String>> = HashMap::new();
    for r in read_table(&db, "FolderCredential")? {
        if let (Some(f), Some(c)) = (guid(get(&r, "folderid")), guid(get(&r, "credentialid"))) {
            folder_creds.entry(f).or_default().push(c);
        }
    }
    let setting_rows = read_table(&db, "Settings")?;
    let setting_type: HashMap<String, String> =
        setting_rows.iter().filter_map(|r| Some((guid(get(r, "id"))?, guid(get(r, "type")).unwrap_or_default()))).collect();
    let setting_blobs = read_blobs(&db, "Settings");
    let mut folder_settings: HashMap<String, Vec<String>> = HashMap::new();
    for r in read_table(&db, "FolderSetting")? {
        if let (Some(f), Some(s)) = (guid(get(&r, "folderid")), guid(get(&r, "settingid"))) {
            folder_settings.entry(f).or_default().push(s);
        }
    }

    // Inheritance like GuiManager.GetConnectionCredentials/-Settings: up the folder chain,
    // the first entry with a matching protocol type wins.
    let inherited = |start: &str, links: &HashMap<String, Vec<String>>, types: &HashMap<String, String>, ty: &str| -> Option<String> {
        let mut cur = start.to_string();
        for _ in 0..256 {
            if let Some(found) = links.get(&cur).and_then(|ids| ids.iter().find(|id| types.get(*id).map(String::as_str) == Some(ty))) {
                return Some(found.clone());
            }
            let parent = folder_parent.get(&cur)?;
            if parent == &cur {
                return None;
            }
            cur = parent.clone();
        }
        None
    };

    let mut bad_folders = 0;
    let out_folders: Vec<Folder> = folders
        .iter()
        .filter_map(|(id, r)| {
            // A corrupt database could hold a folder id that is not a GUID: skip it instead of
            // aborting the whole import.
            let Ok(uuid) = Uuid::parse_str(id) else {
                bad_folders += 1;
                return None;
            };
            // For the simplified inheritance the folder preferably gets the RDP credentials.
            let creds = folder_creds.get(id).cloned().unwrap_or_default();
            let pick = creds
                .iter()
                .find(|c| cred_type.get(*c).map(String::as_str) == Some(TYPE_RDP))
                .or_else(|| creds.first())
                .filter(|c| cred_ids.contains(*c));
            Some(Folder {
                id: uuid,
                parent: parent_of(get(r, "parent")),
                name: get(r, "name").to_string(),
                description: get(r, "description").to_string(),
                sorting: get(r, "sorting").parse().unwrap_or(0),
                expanded: truthy(r.get("expandet")),
                credential: pick.and_then(|c| Uuid::parse_str(c).ok()).map(CredentialRef::Id).unwrap_or(CredentialRef::None),
            })
        })
        .collect();
    if bad_folders > 0 {
        warnings.push(format!("{bad_folders} folder(s) with an unreadable id were skipped."));
    }
    let folder_cred_simple: HashMap<String, Option<Uuid>> = out_folders
        .iter()
        .map(|f| (f.id.to_string(), if let CredentialRef::Id(id) = f.credential { Some(id) } else { None }))
        .collect();
    let simple_inherited = |start: &str| -> Option<Uuid> {
        let mut cur = start.to_string();
        for _ in 0..256 {
            if let Some(Some(c)) = folder_cred_simple.get(&cur) {
                return Some(*c);
            }
            cur = folder_parent.get(&cur)?.clone();
        }
        None
    };

    // --- Connections ---
    let mut connections = Vec::new();
    let mut unknown_types = 0;
    for r in read_table(&db, "Connections")? {
        let Some(id) = guid(get(&r, "id")) else { continue };
        if truthy(r.get("deleted")) {
            skipped_deleted += 1;
            continue;
        }
        let ty = guid(get(&r, "type")).unwrap_or_default();
        let (protocol, mut port) = match ty.as_str() {
            TYPE_RDP => (Protocol::Rdp, 0u16),
            TYPE_PUTTY => (Protocol::Ssh, 0),
            TYPE_VNC => (Protocol::Vnc, 0),
            TYPE_WEB => (Protocol::Web, 0),
            TYPE_EXT => (Protocol::External, 0),
            _ => {
                unknown_types += 1;
                continue;
            }
        };
        let parent_raw = guid(get(&r, "parent")).unwrap_or_default();
        let mut conn = Connection::new(get(&r, "name"), protocol, parent_of(&parent_raw));
        conn.id = Uuid::parse_str(&id)?;
        conn.sorting = get(&r, "sorting").parse().unwrap_or(0);
        conn.mac = get(&r, "mac").to_string();
        conn.last_connected = r.get("lastconnection").cloned();
        // "DescriptionEx" is RTF in the original; turn it (and a plain description) into text.
        conn.description = [rtf_to_text(get(&r, "description")), rtf_to_text(get(&r, "descriptionex"))]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        let target = get(&r, "target").trim().to_string();
        if protocol == Protocol::Web {
            conn.host = target;
        } else if let Some((h, p)) = target.rsplit_once(':').filter(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok()) {
            conn.host = h.to_string();
            port = p.parse().unwrap();
        } else {
            conn.host = target;
        }

        // Credentials
        let cred = guid(get(&r, "credential")).unwrap_or_default();
        let effective = if cred.is_empty() || cred == CRED_INHERIT || cred == Uuid::nil().to_string() {
            inherited(&parent_raw, &folder_creds, &cred_type, &ty)
        } else if cred_ids.contains(&cred) {
            Some(cred)
        } else {
            None
        };
        conn.credential = match effective.and_then(|c| Uuid::parse_str(&c).ok()) {
            None => CredentialRef::None,
            Some(c) if conn.parent.is_some() && simple_inherited(&parent_raw) == Some(c) => CredentialRef::Inherit,
            Some(c) => CredentialRef::Id(c),
        };

        // Settings
        let setting = guid(get(&r, "setting")).unwrap_or_default();
        let setting = if setting.is_empty() || setting == SETTING_INHERIT || setting == Uuid::nil().to_string() {
            inherited(&parent_raw, &folder_settings, &setting_type, &ty)
        } else {
            Some(setting)
        };
        if let Some(blob) = setting.and_then(|s| setting_blobs.get(&s)).filter(|b| !b.is_empty()) {
            match nrbf::parse(blob) {
                Ok(g) => {
                    if let Some(f) = g.root_fields() {
                        apply_settings(&mut conn, &f, &mut port, &mut warnings);
                        gateway_credentials(&mut conn, &mut credentials);
                    }
                }
                Err(e) => warnings.push(format!("Settings of \"{}\" are unreadable: {e}", conn.name)),
            }
        }
        conn.port = if port == conn.protocol.default_port() { 0 } else { port };
        connections.push(conn);
    }
    // Command lines come from the imported file and are executed on connect: point them out.
    let custom: Vec<&str> = connections
        .iter()
        .filter(|c| !c.options.ssh.extra_args.trim().is_empty() || !c.options.external.command.trim().is_empty())
        .map(|c| c.name.as_str())
        .collect();
    if !custom.is_empty() {
        warnings.push(format!(
            "{} imported connection(s) run custom command lines – review them before connecting: {}",
            custom.len(),
            custom.join(", ")
        ));
    }
    if unknown_types > 0 {
        warnings.push(format!("{unknown_types} connection(s) of unknown type (plugins) skipped."));
    }

    Ok(ImportResult { data: Data { folders: out_folders, connections, credentials }, warnings, skipped_deleted, backup: None })
}

/// Splits a Windows command line: whitespace separates, double quotes group, backslashes
/// are ordinary characters (paths like `C:\keys\id.ppk`).
fn split_windows_cmdline(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in cmd.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(cur);
    }
    words
}

/// OpenSSH equivalent of a PuTTY command line (the original's "own command line").
#[derive(Debug, Default, PartialEq)]
struct PuttyCmd {
    args: Vec<String>,
    port: Option<u16>,
    x11: bool,
    compression: bool,
    /// Options without an OpenSSH equivalent.
    dropped: Vec<String>,
}

fn translate_putty(cmd: &str) -> PuttyCmd {
    let words = split_windows_cmdline(cmd);
    let mut out = PuttyCmd::default();
    let mut it = words.into_iter();
    while let Some(w) = it.next() {
        match w.as_str() {
            "-P" => out.port = it.next().and_then(|p| p.parse().ok()).filter(|p| *p > 0),
            // Credentials come from the connection's credentials, never from the command line.
            "-l" | "-pw" | "-pwfile" => {
                it.next();
            }
            // Protocol switches of the original: every connection opens as SSH.
            "-ssh" | "-telnet" | "-2" | "-batch" => {}
            "-X" => out.x11 = true,
            "-C" => out.compression = true,
            "-A" | "-a" | "-4" | "-6" | "-v" | "-N" | "-T" | "-t" | "-x" | "-Y" => out.args.push(w),
            "-L" | "-R" | "-D" => {
                if let Some(v) = it.next() {
                    out.args.push(w);
                    out.args.push(v);
                }
            }
            "-i" => match it.next() {
                Some(k) if !k.to_ascii_lowercase().ends_with(".ppk") && !k.contains('\\') => {
                    out.args.push(w);
                    out.args.push(k);
                }
                Some(k) => out.dropped.push(format!("-i {k}")),
                None => {}
            },
            "-load" | "-m" | "-sercfg" | "-hostkey" | "-proxycmd" => out.dropped.push(format!("{w} {}", it.next().unwrap_or_default())),
            other if other.starts_with('-') => out.dropped.push(w),
            // A host or user@host: the connection's host is used.
            _ => {}
        }
    }
    out
}

/// The original's external-program placeholders (`ExtAppView.ParseArgs`) in RC Manager's syntax.
fn translate_ext_placeholders(args: &str) -> String {
    args.replace("%target%", "{T}").replace("%hostname%", "{T}").replace("%name%", "{N}").replace("%username%", "{U}").replace("%password%", "{P}")
}

/// Converts RTF (how the Windows RDP-Manager stores a rich "DescriptionEx") to plain text.
/// Font/colour tables and `\*` destinations are dropped, `\par`/`\line` become line breaks,
/// `\tab` a tab, `\'xx` the byte. A string that is not RTF is returned unchanged.
pub fn rtf_to_text(s: &str) -> String {
    if !s.trim_start().starts_with("{\\rtf") {
        return s.to_string();
    }
    let mut out = String::new();
    let mut chars = s.trim_start().chars().peekable();
    let mut depth = 0i32;
    // >0 while inside an ignored destination group (font/colour table, `\*` …); its start depth.
    let mut skip_at = 0i32;
    while let Some(c) = chars.next() {
        match c {
            '{' => depth += 1,
            '}' => {
                if skip_at != 0 && depth <= skip_at {
                    skip_at = 0;
                }
                depth -= 1;
            }
            '\\' => match chars.next() {
                None => break,
                Some(e @ ('\\' | '{' | '}')) => {
                    if skip_at == 0 {
                        out.push(e);
                    }
                }
                Some('*') => {
                    if skip_at == 0 {
                        skip_at = depth;
                    }
                }
                Some('\'') => {
                    let hex: String = (0..2).filter_map(|_| chars.next()).collect();
                    if let (0, Ok(b)) = (skip_at, u8::from_str_radix(&hex, 16)) {
                        out.push(b as char);
                    }
                }
                Some(a) if a.is_ascii_alphabetic() => {
                    let mut word = String::from(a);
                    while chars.peek().is_some_and(|n| n.is_ascii_alphabetic()) {
                        word.push(chars.next().unwrap());
                    }
                    if chars.peek() == Some(&'-') {
                        chars.next();
                    }
                    while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                        chars.next();
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                    if skip_at == 0 {
                        match word.as_str() {
                            "par" | "line" | "sect" => out.push('\n'),
                            "tab" => out.push('\t'),
                            "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "generator" => skip_at = depth,
                            _ => {}
                        }
                    }
                }
                Some(_) => {}
            },
            '\r' | '\n' => {}
            c => {
                if skip_at == 0 {
                    out.push(c);
                }
            }
        }
    }
    out.trim().to_string()
}

/// Key of the original's `RdpSettings.GwPassword` (`SecurityCrypt.EncryptAesString`).
const GATEWAY_KEY: &str = "jiosd23489034j(";

/// Moves own gateway credentials of an imported connection into a credential set (an
/// existing one with the same values, or a new one), so the password is stored encrypted.
fn gateway_credentials(conn: &mut Connection, credentials: &mut Vec<Credential>) {
    let o = &mut conn.options.rdp;
    if o.gateway_user.is_empty() && o.gateway_password.is_empty() {
        return;
    }
    let (user, domain, password) = (std::mem::take(&mut o.gateway_user), std::mem::take(&mut o.gateway_domain), std::mem::take(&mut o.gateway_password));
    let id = match credentials.iter().find(|c| c.username == user && c.domain == domain && c.password == password) {
        Some(c) => c.id,
        None => {
            let mut c = Credential::new(&format!("Gateway {}", if o.gateway.trim().is_empty() { user.as_str() } else { o.gateway.trim() }));
            c.username = user;
            c.domain = domain;
            c.password = password;
            let id = c.id;
            credentials.push(c);
            id
        }
    };
    o.gateway_credential = Some(id);
}

fn apply_settings(conn: &mut Connection, f: &nrbf::Fields, port: &mut u16, warnings: &mut Vec<String>) {
    match conn.protocol {
        Protocol::Rdp => {
            if let Some(p) = f.int(&["RdpPort", "_rdpPort"]).filter(|p| (1..=65535).contains(p)) {
                *port = p as u16;
            }
            let o = &mut conn.options.rdp;
            // RdpWindowStartType: 0 tabbed (fits the tab), 1 window and 5 tabbed with a fixed
            // resolution, 2/4 full screen, 3 all monitors. The original only uses
            // WindowResolution for 1 and 5; its setter forces 800x600 for every connection.
            let window_type = f.int(&["WindowType"]).unwrap_or(0);
            o.window_mode = match window_type {
                1 => WindowMode::Window,
                2 | 4 => WindowMode::Fullscreen,
                3 => WindowMode::MultiMonitor,
                _ => WindowMode::Tab,
            };
            if matches!(window_type, 1 | 5) {
                if let Some(res) = f.string(&["WindowResolution"]).and_then(|r| crate::launch::parse_resolution(&r)) {
                    o.resolution = format!("{}x{}", res.0, res.1);
                    o.dynamic_resolution = false;
                }
            }
            if let Some(cd) = f.int(&["ColorDepth"]).filter(|c| [8, 15, 16, 24, 32].contains(c)) {
                o.color_depth = cd as u8;
            }
            o.clipboard = f.bool(&["Clipboard"]).unwrap_or(true);
            o.printers = f.bool(&["Printer"]).unwrap_or(false);
            o.smartcards = f.bool(&["Smartcards"]).unwrap_or(false);
            o.console = f.bool(&["Console"]).unwrap_or(false);
            o.microphone = f.bool(&["RemoteAudioCapture"]).unwrap_or(false);
            // RdpSoundMode: 0 = play locally, 1 = remote, 2 = not at all
            let sound = f.int(&["Sound"]).unwrap_or(0);
            o.sound = sound == 0;
            o.sound_remote = sound == 1;
            // Experience and redirection, passed to the control as in the original.
            for (name, value) in [
                ("BackgroundPicture", &mut o.wallpaper),
                ("ShowWindowContentsWhileDragging", &mut o.full_window_drag),
                ("MenuAndWindowAnimation", &mut o.menu_animations),
                ("VisualStyles", &mut o.themes),
                ("FontSmoothing", &mut o.font_smoothing),
                ("BitmapCaching", &mut o.bitmap_caching),
                ("AutoQuality", &mut o.auto_detect),
                ("DisableCompression", &mut o.disable_compression),
                ("DisableSsp", &mut o.disable_nla),
                ("RedirectPorts", &mut o.redirect_ports),
                ("RedirectPnP", &mut o.redirect_pnp),
            ] {
                if let Some(v) = f.bool(&[name]) {
                    *value = v;
                }
            }
            // RdpShortcutMode: 0 = this computer, 1 = remote computer, 2 = only in full screen
            match f.int(&["Shortcuts"]) {
                Some(0) => o.shortcuts = RdpShortcuts::Local,
                Some(1) => o.shortcuts = RdpShortcuts::Remote,
                Some(2) => o.shortcuts = RdpShortcuts::FullScreen,
                _ => {}
            }
            // RdpServerAuth: 0 = connect without warning (the original's default), 1 = do not
            // connect, 2 = warn. An import never switches certificate checks off.
            o.refuse_unverified = f.int(&["ServerAuthentication"]) == Some(1);
            // RdpDriveList: a list of drive letters plus an "all drives" flag. Windows drive
            // letters have no meaning here, so any shared drive becomes the home folder.
            let all_drives = f.nested(&["LocalDrives"]).and_then(|d| d.bool(&["AllDrives"])).unwrap_or(false);
            o.home_drive = all_drives || !f.strings(&["LocalDrives"]).is_empty();
            // RdpGatewayType: 0 and 4 = no gateway, 1 = this server, 2 and 3 = detect
            // automatically (2 with local bypass). The server field may be filled anyway.
            o.gateway_mode = Some(match f.int(&["GwType"]) {
                Some(1) => GatewayMode::Manual,
                Some(2 | 3) => GatewayMode::Auto,
                _ => GatewayMode::Off,
            });
            o.gateway = f.string(&["GwServer"]).unwrap_or_default();
            o.gateway_bypass_local = f.bool(&["GwBypass"]).unwrap_or(false);
            // Own gateway credentials become a credential set (see `gateway_credentials`).
            if o.gateway_mode != Some(GatewayMode::Off) && f.bool(&["UseOwnGwCreds"]).unwrap_or(false) {
                o.gateway_user = f.string(&["GwUser"]).unwrap_or_default();
                o.gateway_domain = f.string(&["GwDomain"]).unwrap_or_default();
                let enc = f.string(&["_gwPassword", "GwPassword"]).unwrap_or_default();
                if !enc.is_empty() {
                    match crypto::decrypt_aes_string(&enc, GATEWAY_KEY) {
                        Ok(p) => o.gateway_password = p,
                        Err(_) => warnings.push(format!("{}: the gateway password could not be decrypted.", conn.name)),
                    }
                }
            }
            // "00000000" means "no preselection" in the original (its default).
            o.keyboard_layout = f.string(&["KeyboardLayout"]).filter(|k| crate::launch::keyboard_layout_id(k).is_some()).unwrap_or_default();
            // A program started in the session instead of the desktop (not a RemoteApp).
            if f.bool(&["RunAppOnConnect"]).unwrap_or(false) {
                o.start_program = f.string(&["RunAppOnConnectPath"]).unwrap_or_default();
                o.start_dir = f.string(&["RunAppOnConnectWorkingDir"]).unwrap_or_default();
            }
        }
        Protocol::Ssh => {
            if let Some(p) = f.int(&["Port"]).filter(|p| (1..=65535).contains(p)) {
                *port = p as u16;
            }
            // PuttyProtocols of the original: every value opens as SSH.
            conn.options.ssh.compression = f.bool(&["SshCompression"]).unwrap_or(false);
            if f.bool(&["UseOwnCmd"]).unwrap_or(false) {
                // The own command line replaces all other options in the original (PuttyView).
                let cmd = translate_putty(&f.string(&["CommandLine"]).unwrap_or_default());
                *port = cmd.port.unwrap_or(0);
                conn.options.ssh.x11_forwarding = cmd.x11;
                conn.options.ssh.compression = cmd.compression;
                conn.options.ssh.extra_args = shell_words::join(&cmd.args);
                if !cmd.dropped.is_empty() {
                    warnings.push(format!("PuTTY options of \"{}\" without OpenSSH equivalent were left out: {}", conn.name, cmd.dropped.join(", ")));
                }
            } else if let Some(profile) = f.string(&["ProfileName"]).filter(|p| !matches!(p.as_str(), "Default Settings" | "Nicht verwenden" | "Do not use")) {
                warnings.push(format!("\"{}\" used the PuTTY session \"{profile}\"; its settings are not available on Linux", conn.name));
            }
        }
        Protocol::Vnc => {
            if let Some(p) = f.int(&["Port"]).filter(|p| (1..=65535).contains(p)) {
                *port = p as u16;
            }
            conn.options.vnc.view_only = f.bool(&["ViewOnly"]).unwrap_or(false);
            conn.options.vnc.shared = f.bool(&["Shared"]).unwrap_or(false);
        }
        Protocol::External => {
            let file = f.string(&["ProcessFile"]).unwrap_or_default();
            let args = translate_ext_placeholders(&f.string(&["Args"]).unwrap_or_default());
            conn.options.external.command = format!("{file} {args}").trim().to_string();
            conn.options.external.working_dir = f.string(&["WorkingDir"]).unwrap_or_default();
        }
        Protocol::Web => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    // Fixture with the original schema (DB version 12); passwords and settings blobs
    // were generated with .NET (see crypto.rs / nrbf.rs).
    #[test]
    fn imports_original_database() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy.db3");
        let r = import(&path, None).unwrap();
        let d = &r.data;
        assert_eq!(r.skipped_deleted, 2);
        assert!(r.warnings.iter().any(|w| w.contains("unknown type")), "{:?}", r.warnings);
        // Linux-Box's own PuTTY command line "-X" becomes X11 forwarding, nothing custom remains.
        assert!(!r.warnings.iter().any(|w| w.contains("custom command lines")), "{:?}", r.warnings);

        assert_eq!(d.folders.len(), 2);
        let fa = d.folders.iter().find(|f| f.name == "Kunde A").unwrap();
        let fb = d.folders.iter().find(|f| f.name == "Server").unwrap();
        assert_eq!(fa.parent, None);
        assert!(fa.expanded);
        assert_eq!(fa.credential, CredentialRef::Id(id("cccccccc-0000-0000-0000-000000000001")));
        assert_eq!(fb.parent, Some(fa.id));

        let admin = d.credentials.iter().find(|c| c.username == "administrator").unwrap();
        assert_eq!(admin.name, "Domänen-Admin");
        assert_eq!(admin.domain, "FIRMA");
        assert_eq!(admin.password, "Geheim123!");
        assert_eq!(d.credentials.iter().find(|c| c.username == "root").unwrap().password, "äöü ß €");

        assert_eq!(d.connections.len(), 3);
        let dc = d.connections.iter().find(|c| c.name == "DC01").unwrap();
        assert_eq!(dc.protocol, Protocol::Rdp);
        assert_eq!(dc.parent, Some(fb.id));
        assert_eq!(dc.host, "dc01.firma.local");
        assert_eq!(dc.port, 3390, "port from settings inherited from the folder");
        assert_eq!(dc.options.rdp.resolution, "1920x1080");
        assert_eq!(dc.options.rdp.color_depth, 24);
        assert!(dc.options.rdp.clipboard);
        assert_eq!(dc.credential, CredentialRef::Inherit);
        assert_eq!(dc.mac, "00:11:22:33:44:55");

        let ssh = d.connections.iter().find(|c| c.name == "Linux-Box").unwrap();
        assert_eq!(ssh.protocol, Protocol::Ssh);
        // With an own command line the original ignores the port setting; PuTTY then uses 22.
        assert_eq!(ssh.port, 0);
        assert!(ssh.options.ssh.x11_forwarding);
        assert_eq!(ssh.options.ssh.extra_args, "");
        assert_eq!(ssh.credential, CredentialRef::Id(id("cccccccc-0000-0000-0000-000000000002")));

        let web = d.connections.iter().find(|c| c.name == "Intranet").unwrap();
        assert_eq!(web.protocol, Protocol::Web);
        assert_eq!(web.host, "https://intranet.firma.local");
        assert_eq!(web.parent, None);
        assert_eq!(web.credential, CredentialRef::None);
    }

    #[test]
    fn imports_backup_zip() {
        use std::io::Write;
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let zip_path = std::env::temp_dir().join(format!("rcm-import-zip-{}.zip", Uuid::new_v4()));
        let mut w = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        for (name, file) in [("BackupInfo.xml", "backup/BackupInfo.xml"), ("SqLiteDatabase.db3", "legacy.db3"), ("fileConfig.xml", "backup/fileConfig.xml")] {
            w.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(&std::fs::read(fx.join(file)).unwrap()).unwrap();
        }
        w.finish().unwrap();
        let r = import(&zip_path, None).unwrap();
        std::fs::remove_file(&zip_path).ok();
        assert_eq!(r.data.connections.len(), 3);
        assert_eq!(r.data.credentials.iter().find(|c| c.username == "administrator").unwrap().password, "Geheim123!");
        assert_eq!(r.backup.unwrap().version, "4.9.9.3");
    }

    #[test]
    fn own_gateway_credentials_become_a_credential_set() {
        let mut creds = vec![Credential { id: Uuid::new_v4(), name: "gw".into(), username: "gwu".into(), domain: "D".into(), password: "pw".into() }];
        let mut a = Connection::new("a", Protocol::Rdp, None);
        a.options.rdp.gateway = "gw.local".into();
        (a.options.rdp.gateway_user, a.options.rdp.gateway_domain, a.options.rdp.gateway_password) = ("gwu".into(), "D".into(), "pw".into());
        let mut b = a.clone();
        b.options.rdp.gateway_password = "other".into();
        gateway_credentials(&mut a, &mut creds);
        gateway_credentials(&mut b, &mut creds);
        assert_eq!(a.options.rdp.gateway_credential, Some(creds[0].id), "an equal set is reused");
        assert_eq!(creds.len(), 2);
        assert_eq!(b.options.rdp.gateway_credential, Some(creds[1].id));
        assert_eq!(creds[1].name, "Gateway gw.local");
        assert!(b.options.rdp.gateway_user.is_empty() && b.options.rdp.gateway_password.is_empty());
    }

    // Settings blob of a connection with the original's default values (generated with .NET).
    #[test]
    fn original_defaults_are_not_passed_on() {
        let g = nrbf::parse(include_bytes!("../../tests/fixtures/rdpsettings_defaults.bin")).unwrap();
        let f = g.root_fields().unwrap();
        let mut c = Connection::new("x", Protocol::Rdp, None);
        let mut port = 0;
        apply_settings(&mut c, &f, &mut port, &mut Vec::new());
        let o = &c.options.rdp;
        assert_eq!(o.keyboard_layout, "", "00000000 = no preselection");
        assert_eq!(o.window_mode, WindowMode::Tab);
        assert_eq!(o.resolution, "", "tabbed connections fit the window");
        assert!(o.dynamic_resolution);
        assert_eq!(o.gateway_server(), None, "gateway type 0 = no gateway");
        assert_eq!(o.gateway_user, "");
        assert_eq!(o.gateway_mode, Some(GatewayMode::Off));
        assert!(!o.ignore_certificate && !o.refuse_unverified, "server authentication 0 is not taken over");
        assert_eq!(o.start_program, "");
        assert_eq!(o.remote_app, "");
        assert!(o.home_drive, "AllDrives");
        assert_eq!(o.color_depth, 32);
        assert_eq!(port, 0, "port 0 keeps the default");
    }

    #[test]
    fn putty_command_lines_become_openssh_options() {
        let c = translate_putty(r#"-ssh -P 2222 -X -C -l root -pw geheim -L 8080:localhost:80 -load "Mein Profil" -i C:\keys\id.ppk user@host"#);
        assert_eq!(c.port, Some(2222));
        assert!(c.x11 && c.compression);
        assert_eq!(c.args, ["-L", "8080:localhost:80"]);
        assert_eq!(c.dropped, ["-load Mein Profil", r"-i C:\keys\id.ppk"]);
        assert!(!format!("{c:?}").contains("geheim"), "password must not survive");
        // The original's protocol switch is accepted but ignored (opens as SSH), not reported as dropped.
        assert!(translate_putty("-telnet").dropped.is_empty());
        assert_eq!(translate_ext_placeholders("mstsc /v:%target% /u:%username%"), "mstsc /v:{T} /u:{U}");
    }

    #[test]
    fn rejects_non_database() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rdpsettings.bin");
        assert!(import(&path, None).is_err());
    }

    #[test]
    fn rtf_becomes_plain_text() {
        // An empty rich description (as the original writes it) becomes empty.
        let empty = r"{\rtf1\ansi\ansicpg1252\deff0\deflang1031{\fonttbl{\f0\fnil\fcharset0 Microsoft Sans Serif;}}\viewkind4\uc1\pard\f0\fs17\par}";
        assert_eq!(rtf_to_text(empty), "");
        // Actual text with a line break and an accented character (\'e4 = ä).
        let rich = r"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\pard Line one\par Zweite Zeile mit \'e4\par}";
        assert_eq!(rtf_to_text(rich), "Line one\nZweite Zeile mit ä");
        // A plain-text description is left untouched.
        assert_eq!(rtf_to_text("just a note"), "just a note");
    }
}
