//! "Add to Application Menu": moves the running AppImage to `~/Applications` and creates a
//! desktop entry + icon, so the program appears in the start menu. No root rights are needed, and
//! the updater can still replace the file. Removing deletes entry, icon and the AppImage in
//! `~/Applications`; settings stay.

use std::path::{Path, PathBuf};

const DESKTOP_ID: &str = "rc-manager.desktop";
const ICON_FILE: &str = "rc-manager.png";
/// The desktop entry shipped in the AppImage (Exec and Icon are replaced).
const TEMPLATE: &str = include_str!("../packaging/rc-manager.desktop");
const ICON_PNG: &[u8] = include_bytes!("../packaging/rc-manager.png");

pub struct Dirs {
    /// `$XDG_DATA_HOME` (`~/.local/share`).
    pub data: PathBuf,
}

impl Dirs {
    pub fn user() -> Dirs {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let data = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or(home.join(".local/share"));
        Dirs { data }
    }

    pub fn desktop(&self) -> PathBuf {
        self.data.join("applications").join(DESKTOP_ID)
    }

    fn icon(&self) -> PathBuf {
        self.data.join("icons/hicolor/256x256/apps").join(ICON_FILE)
    }
}

/// `~/Applications`, the usual place for AppImages (also used by AppImageLauncher / Gear Lever).
pub fn apps_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Applications")
}

/// A path for display, with the home directory shortened to `~`.
pub fn tilde(p: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

/// The program has a desktop entry in the application menu.
pub fn is_installed(dirs: &Dirs) -> bool {
    dirs.desktop().is_file()
}

fn io_err(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

fn exec_line(exe: &Path) -> String {
    // Keep the arguments of the shipped entry (e.g. `%f`).
    let args = TEMPLATE.lines().find_map(|l| l.strip_prefix("Exec=")).and_then(|e| e.split_once(' ').map(|(_, a)| a.to_string()));
    match args {
        Some(a) => format!("Exec=\"{}\" {a}", exe.display()),
        None => format!("Exec=\"{}\"", exe.display()),
    }
}

fn entry(exe: &Path, icon: &Path) -> String {
    let mut out = String::new();
    for l in TEMPLATE.lines() {
        if l.starts_with("Exec=") {
            out.push_str(&exec_line(exe));
        } else if l.starts_with("Icon=") {
            out.push_str(&format!("Icon={}", icon.display()));
        } else if l.starts_with("TryExec=") {
            continue;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    out
}

/// Refreshes the desktop database / KDE service cache when the tools exist (not required).
fn refresh_caches(dirs: &Dirs) {
    use std::process::{Command, Stdio};
    let _ = Command::new("update-desktop-database").arg(dirs.data.join("applications")).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = Command::new("kbuildsycoca6").stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Writes the desktop entry and icon for `exe`.
pub fn write_entry(dirs: &Dirs, exe: &Path) -> Result<(), String> {
    let icon = dirs.icon();
    std::fs::create_dir_all(icon.parent().unwrap()).map_err(|e| io_err(&icon, e))?;
    std::fs::write(&icon, ICON_PNG).map_err(|e| io_err(&icon, e))?;
    let desktop = dirs.desktop();
    std::fs::create_dir_all(desktop.parent().unwrap()).map_err(|e| io_err(&desktop, e))?;
    std::fs::write(&desktop, entry(exe, &icon)).map_err(|e| io_err(&desktop, e))?;
    refresh_caches(dirs);
    Ok(())
}

/// Points an installed desktop entry at `exe` (the AppImage may have been moved, renamed or
/// replaced by the updater). Returns true when the entry was changed.
pub fn refresh_exec(dirs: &Dirs, exe: &Path) -> bool {
    let desktop = dirs.desktop();
    let Ok(text) = std::fs::read_to_string(&desktop) else { return false };
    let want = exec_line(exe);
    if text.lines().any(|l| l == want) {
        return false;
    }
    let new: String = text.lines().map(|l| if l.starts_with("Exec=") { want.as_str() } else { l }).map(|l| format!("{l}\n")).collect();
    if std::fs::write(&desktop, new).is_err() {
        return false;
    }
    refresh_caches(dirs);
    true
}

/// Moves (or, across file systems, copies) `appimage` into `dest_dir`. Returns the new path.
fn place(appimage: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dest_dir).map_err(|e| io_err(dest_dir, e))?;
    let name = appimage.file_name().ok_or("no file name")?;
    let dest = dest_dir.join(name);
    if dest == appimage {
        return Ok(dest);
    }
    if std::fs::rename(appimage, &dest).is_err() {
        // Different file system (or no write access to the source folder): copy.
        let tmp = dest_dir.join(format!(".{}.part", name.to_string_lossy()));
        std::fs::copy(appimage, &tmp).map_err(|e| io_err(&tmp, e))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| io_err(&tmp, e))?;
        std::fs::rename(&tmp, &dest).map_err(|e| io_err(&dest, e))?;
        let _ = std::fs::remove_file(appimage);
    }
    Ok(dest)
}

/// Installs: AppImage into `dest_dir`, desktop entry and icon. Returns the installed AppImage.
pub fn install(dirs: &Dirs, appimage: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let dest = place(appimage, dest_dir)?;
    write_entry(dirs, &dest)?;
    Ok(dest)
}

/// Removes desktop entry and icon; deletes the AppImage when it lives in `dest_dir` (the running
/// program keeps working until it exits).
pub fn uninstall(dirs: &Dirs, appimage: Option<&Path>, dest_dir: &Path) -> Result<(), String> {
    for p in [dirs.desktop(), dirs.icon()] {
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io_err(&p, e)),
            _ => {}
        }
    }
    refresh_caches(dirs);
    if let Some(a) = appimage.filter(|a| a.parent() == Some(dest_dir)) {
        let _ = std::fs::remove_file(a);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_refresh_uninstall() {
        let dir = std::env::temp_dir().join(format!("{}-install-{}", "rc-manager", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dirs = Dirs { data: dir.join("data") };
        let downloads = dir.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let src = downloads.join("App-1.2.3-x86_64.AppImage");
        std::fs::write(&src, b"appimage").unwrap();
        let apps = dir.join("Applications");
        assert!(!is_installed(&dirs));

        let dest = install(&dirs, &src, &apps).unwrap();
        assert_eq!(dest, apps.join("App-1.2.3-x86_64.AppImage"));
        assert!(dest.is_file() && !src.exists(), "moved, not copied");
        assert!(is_installed(&dirs));
        let e = std::fs::read_to_string(dirs.desktop()).unwrap();
        assert!(e.starts_with("[Desktop Entry]") && e.contains(&format!("Exec=\"{}\"", dest.display())), "{e}");
        assert!(e.contains(&format!("Icon={}", dirs.icon().display())) && dirs.icon().is_file(), "{e}");
        assert_eq!(e.lines().filter(|l| l.starts_with("Name=")).count(), TEMPLATE.lines().filter(|l| l.starts_with("Name=")).count());
        assert_eq!(install(&dirs, &dest, &apps).unwrap(), dest, "installing again changes nothing");

        // Renamed/moved AppImage: only the Exec line follows.
        let moved = apps.join("App-1.2.4-x86_64.AppImage");
        assert!(refresh_exec(&dirs, &moved));
        let e2 = std::fs::read_to_string(dirs.desktop()).unwrap();
        assert!(e2.contains(&exec_line(&moved)) && e2.lines().count() == e.lines().count(), "{e2}");
        assert!(!refresh_exec(&dirs, &moved), "unchanged the second time");

        uninstall(&dirs, Some(&dest), &apps).unwrap();
        assert!(!is_installed(&dirs) && !dest.exists() && !dirs.icon().exists());
        assert!(!refresh_exec(&dirs, &moved), "no entry installed");
        // An AppImage outside ~/Applications is never deleted.
        std::fs::write(&src, b"appimage").unwrap();
        install(&dirs, &src, &downloads).unwrap();
        uninstall(&dirs, Some(&src), &apps).unwrap();
        assert!(src.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
