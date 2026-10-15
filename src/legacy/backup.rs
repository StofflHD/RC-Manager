//! Backups of the Windows RDP-Manager (`RDP_Manager_<date>.zip`).
//!
//! The original's BackupRestore tool zips the whole data folder and adds `BackupInfo.xml`:
//! `SqLiteDatabase.db3`, `fileConfig.xml` (database provider), `dockPanelStyle.xml`
//! (window layout) and, if present, `DisableShowPw`. Only these known root entries are read;
//! entry paths are never used to create files, and sizes are capped.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;

const MAX_DB: u64 = 1 << 30;
const MAX_SMALL: u64 = 1 << 20;

#[derive(Debug, Default, Clone)]
pub struct BackupInfo {
    /// RDP-Manager version that wrote the backup, e.g. "4.9.9.3".
    pub version: String,
    /// "YYYY-MM-DD HH:MM" (local time of the backup).
    pub date: String,
    pub comment: String,
    /// `DbProvider` from fileConfig.xml, e.g. "Cinspiration.RdpManager.Db.SqLite.dll".
    pub db_provider: String,
    /// The connection tree was set to auto-hide (or hidden) in dockPanelStyle.xml.
    pub sidebar_autohide: Option<bool>,
    pub has_database: bool,
}

/// Contents needed for the import, extracted into memory.
pub struct Extracted {
    pub info: BackupInfo,
    pub database: Vec<u8>,
    pub database_wal: Option<Vec<u8>>,
    pub disable_show_pw: Option<String>,
}

pub fn is_zip(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"PK\x03\x04"
}

/// Root-level entry name, case-insensitive; the original may write `\` as separator.
fn root_name(name: &str) -> Option<String> {
    let n = name.replace('\\', "/");
    (!n.contains('/')).then(|| n.to_ascii_lowercase())
}

fn read_entry(file: &mut zip::read::ZipFile<'_, std::fs::File>, limit: u64) -> Result<Vec<u8>> {
    if file.encrypted() {
        bail!("Password-protected backups are not supported");
    }
    let mut out = Vec::new();
    file.take(limit + 1).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        bail!("{} is too large", file.name());
    }
    Ok(out)
}

/// Decodes UTF-8 or UTF-16 (with BOM) XML text.
fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()),
        [0xFE, 0xFF, rest @ ..] => String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    Some(xml[start..end].trim().to_string())
}

fn attr(element: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = element.find(&key)? + key.len();
    let end = start + element[start..].find('"')?;
    Some(element[start..end].to_string())
}

/// `2026-04-22T14:48:20.5734857+02:00` → `2026-04-22 14:48`
fn short_date(iso: &str) -> String {
    match (iso.get(..10), iso.get(11..16)) {
        (Some(d), Some(t)) if iso.as_bytes().get(10) == Some(&b'T') => format!("{d} {t}"),
        _ => iso.to_string(),
    }
}

/// Finds the dock state of the connection tree (`frmTreeView`) in dockPanelStyle.xml.
fn tree_autohide(xml: &str) -> Option<bool> {
    // Elements are split at "<Content ", so put the separating space back for `attr`.
    let content = xml
        .split("<Content ")
        .skip(1)
        .map(|c| format!(" {}", &c[..c.find("/>").unwrap_or(c.len())]))
        .find(|c| c.contains("frmTreeView"))?;
    let content = content.as_str();
    if attr(content, "IsHidden").is_some_and(|h| h.eq_ignore_ascii_case("true")) {
        return Some(true);
    }
    let id = attr(content, "ID")?;
    let panes = &xml[xml.find("<Panes")?..xml.find("</Panes>")?];
    for pane in panes.split("<Pane ").skip(1) {
        let body = &pane[..pane.find("</Pane>").unwrap_or(pane.len())];
        if body.contains(&format!("RefID=\"{id}\"")) {
            return attr(&format!(" {body}"), "DockState").map(|s| s.ends_with("AutoHide"));
        }
    }
    None
}

fn parse(path: &Path, with_database: bool) -> Result<Extracted> {
    let file = std::fs::File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("Not a valid ZIP file")?;
    let mut ex = Extracted { info: BackupInfo::default(), database: Vec::new(), database_wal: None, disable_show_pw: None };
    ex.info.comment = String::from_utf8_lossy(archive.comment()).trim().to_string();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(name) = root_name(entry.name()) else { continue };
        match name.as_str() {
            "backupinfo.xml" => {
                let xml = decode(&read_entry(&mut entry, MAX_SMALL)?);
                ex.info.version = tag_text(&xml, "BackupVersion").unwrap_or_default();
                ex.info.date = tag_text(&xml, "BackupDate").map(|d| short_date(&d)).unwrap_or_default();
                if ex.info.comment.is_empty() {
                    ex.info.comment = tag_text(&xml, "Comment").unwrap_or_default();
                }
            }
            "fileconfig.xml" => ex.info.db_provider = tag_text(&decode(&read_entry(&mut entry, MAX_SMALL)?), "DbProvider").unwrap_or_default(),
            "dockpanelstyle.xml" => ex.info.sidebar_autohide = tree_autohide(&decode(&read_entry(&mut entry, MAX_SMALL)?)),
            "sqlitedatabase.db3" => {
                ex.info.has_database = true;
                if with_database {
                    ex.database = read_entry(&mut entry, MAX_DB)?;
                }
            }
            "sqlitedatabase.db3-wal" if with_database => ex.database_wal = Some(read_entry(&mut entry, MAX_DB)?),
            "disableshowpw" if with_database => ex.disable_show_pw = Some(decode(&read_entry(&mut entry, MAX_SMALL)?).trim().to_string()),
            _ => {}
        }
    }
    if ex.info.version.is_empty() && !ex.info.has_database {
        bail!("Not an RDP-Manager backup (neither BackupInfo.xml nor SqLiteDatabase.db3 found)");
    }
    if !ex.info.db_provider.is_empty() && !ex.info.db_provider.to_ascii_lowercase().contains("sqlite") {
        bail!("This backup uses the database provider \"{}\"; only SQLite databases can be imported", ex.info.db_provider);
    }
    if !ex.info.has_database {
        bail!("The backup contains no SqLiteDatabase.db3");
    }
    Ok(ex)
}

/// Reads the backup's metadata without loading the database (for the import dialog).
pub fn inspect(path: &Path) -> Result<BackupInfo> {
    parse(path, false).map(|e| e.info)
}

pub fn extract(path: &Path) -> Result<Extracted> {
    parse(path, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
    }

    fn make_zip(entries: &[(&str, Vec<u8>)], comment: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("rcm-backup-test-{}.zip", uuid::Uuid::new_v4()));
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        w.set_comment(comment).unwrap();
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
        path
    }

    fn original_backup() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("BackupInfo.xml", fixture("backup/BackupInfo.xml")),
            ("SqLiteDatabase.db3", fixture("legacy.db3")),
            ("fileConfig.xml", fixture("backup/fileConfig.xml")),
            ("dockPanelStyle.xml", fixture("backup/dockPanelStyle.xml")),
            ("DisableShowPw", b"".to_vec()),
            ("sub\\other.txt", b"ignored".to_vec()),
        ]
    }

    #[test]
    fn reads_backup_of_original() {
        let path = make_zip(&original_backup(), "Before migration");
        assert!(is_zip(&path));
        let info = inspect(&path).unwrap();
        assert_eq!(info.version, "4.9.9.3");
        assert_eq!(info.date, "2026-04-22 14:48");
        assert_eq!(info.comment, "Before migration");
        assert_eq!(info.db_provider, "Cinspiration.RdpManager.Db.SqLite.dll");
        assert_eq!(info.sidebar_autohide, Some(true));
        let ex = extract(&path).unwrap();
        assert_eq!(ex.database, fixture("legacy.db3"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_other_providers_and_foreign_zips() {
        let mut entries = original_backup();
        entries[2].1 = b"<FileConfig><DbProvider>Cinspiration.RdpManager.Db.MsSql.dll</DbProvider></FileConfig>".to_vec();
        let path = make_zip(&entries, "");
        assert!(inspect(&path).unwrap_err().to_string().contains("only SQLite"));
        std::fs::remove_file(path).ok();

        let path = make_zip(&[("readme.txt", b"hello".to_vec())], "");
        assert!(inspect(&path).is_err());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn docked_tree_is_not_autohide() {
        let xml = r#"<Contents Count="1"><Content ID="0" PersistString="Cinspiration.RdpManager.Forms.frmTreeView" IsHidden="False" /></Contents>
            <Panes Count="1"><Pane ID="0" DockState="DockLeft" ActiveContent="0"><Contents Count="1"><Content ID="0" RefID="0" /></Contents></Pane></Panes>"#;
        assert_eq!(tree_autohide(xml), Some(false));
    }
}
