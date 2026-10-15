//! Import of Microsoft .rdp files (like the plugin `RdpManager.Plugin.RdpImport`).

use crate::model::*;
use anyhow::{bail, Result};
use std::path::Path;
use uuid::Uuid;

pub struct RdpFile {
    pub connection: Connection,
    pub username: String,
    pub domain: String,
    pub warnings: Vec<String>,
}

fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) || (bytes.len() > 1 && bytes[1] == 0 && bytes[0] != 0) {
        let start = if bytes.starts_with(&[0xFF, 0xFE]) { 2 } else { 0 };
        let units: Vec<u16> = bytes[start..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)).into_owned()
    }
}

pub fn parse(name: &str, content: &[u8], parent: Option<Uuid>) -> Result<RdpFile> {
    let mut c = Connection::new(name, Protocol::Rdp, parent);
    c.options.rdp.dynamic_resolution = false;
    let (mut user, mut domain) = (String::new(), String::new());
    let mut warnings = Vec::new();
    let (mut width, mut height) = (0u32, 0u32);
    let mut port = 0u16;
    for line in decode(content).lines() {
        let mut parts = line.trim().splitn(3, ':');
        let (Some(key), Some(_ty), Some(val)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim();
        let flag = val == "1";
        let o = &mut c.options.rdp;
        match key.as_str() {
            "full address" => {
                if let Some((h, p)) = val.rsplit_once(':').filter(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok()) {
                    c.host = h.to_string();
                    port = p.parse().unwrap();
                } else {
                    c.host = val.trim_matches(['[', ']']).to_string();
                }
            }
            "server port" => port = val.parse().unwrap_or(port),
            "username" => {
                if let Some((d, u)) = val.split_once('\\') {
                    domain = d.to_string();
                    user = u.to_string();
                } else {
                    user = val.to_string();
                }
            }
            "domain" if !val.is_empty() => domain = val.to_string(),
            "screen mode id" => {
                if val == "2" && o.window_mode == WindowMode::Window {
                    o.window_mode = WindowMode::Fullscreen;
                }
            }
            "use multimon" if flag => o.window_mode = WindowMode::MultiMonitor,
            "desktopwidth" => width = val.parse().unwrap_or(0),
            "desktopheight" => height = val.parse().unwrap_or(0),
            "dynamic resolution" => o.dynamic_resolution = flag,
            "session bpp" => o.color_depth = val.parse().unwrap_or(0),
            "redirectclipboard" => o.clipboard = flag,
            "redirectprinters" => o.printers = flag,
            "redirectsmartcards" => o.smartcards = flag,
            "audiomode" => o.sound = val == "0",
            "audiocapturemode" => o.microphone = flag,
            "drivestoredirect" => o.home_drive = !val.is_empty(),
            "administrative session" | "connect to console" => o.console = flag,
            "gatewayhostname" => o.gateway = val.to_string(),
            "remoteapplicationprogram" => o.remote_app = val.to_string(),
            "alternate shell" if o.remote_app.is_empty() => o.remote_app = val.to_string(),
            // A file from elsewhere must not silently switch off server authentication.
            "authentication level" if val == "0" => {
                warnings.push(format!("{name}: the file disables certificate checks; not applied (enable it in the connection if really needed)"))
            }
            _ => {}
        }
    }
    if c.host.is_empty() {
        bail!("No target address (\"full address\") found");
    }
    if width > 0 && height > 0 && c.options.rdp.window_mode == WindowMode::Window {
        c.options.rdp.resolution = format!("{width}x{height}");
    }
    c.port = if port == 3389 { 0 } else { port };
    Ok(RdpFile { connection: c, username: user, domain, warnings })
}

pub fn parse_file(path: &Path, parent: Option<Uuid>) -> Result<RdpFile> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Import".into());
    parse(&name, &std::fs::read(path)?, parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_rdp_file() {
        let text = "screen mode id:i:1\r\nuse multimon:i:0\r\ndesktopwidth:i:1600\r\ndesktopheight:i:900\r\nsession bpp:i:32\r\n\
                    full address:s:ts01.firma.local:3390\r\nusername:s:FIRMA\\jdoe\r\nredirectclipboard:i:1\r\nredirectprinters:i:0\r\naudiomode:i:2\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let r = parse("ts01", &bytes, None).unwrap();
        assert_eq!(r.connection.host, "ts01.firma.local");
        assert_eq!(r.connection.port, 3390);
        assert_eq!(r.username, "jdoe");
        assert_eq!(r.domain, "FIRMA");
        let o = &r.connection.options.rdp;
        assert_eq!(o.resolution, "1600x900");
        assert_eq!(o.color_depth, 32);
        assert!(o.clipboard && !o.printers && !o.sound);
    }

    #[test]
    fn certificate_checks_stay_on() {
        let r = parse("x", b"full address:s:h
authentication level:i:0
", None).unwrap();
        assert!(!r.connection.options.rdp.ignore_certificate);
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn missing_address() {
        assert!(parse("x", b"username:s:foo\n", None).is_err());
    }
}
