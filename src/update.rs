//! Updater (Help → Check for Updates): checks the GitHub releases of the project, downloads the
//! new package, verifies its SHA-256 checksum and installs it – the same updater as in
//! notepad-rs. Linux: the running AppImage file is replaced. Windows: a running program cannot
//! replace itself, so the installer (or, for the portable ZIP, the unpacked files) is applied by
//! a small PowerShell script once RC Manager has exited.
//!
//! HTTP goes through `curl` (or `wget`) so the binary needs no TLS stack of its own.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// `owner/repo` on GitHub (from `repository` in Cargo.toml).
pub fn repo() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/").trim_end_matches('/')
}

pub fn releases_page() -> String {
    format!("https://github.com/{}/releases", repo())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub notes: String,
    pub page: String,
    pub prerelease: bool,
    /// The package for this system – AppImage, Windows installer or portable ZIP: (name,
    /// download URL, size)
    pub package: Option<(String, String, u64)>,
    pub checksum_url: Option<String>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// Parses "v1.2.3" / "1.2" into comparable numbers (pre-release suffixes are ignored).
pub fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (mut a, mut b) = (parse_version(candidate), parse_version(current));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

/// Picks the newest release from the GitHub API answer (`/releases`). Pre-releases count as long
/// as the project has no final release (versions below 1.0) or when `include_pre` is set.
pub fn newest(json: &str, include_pre: bool) -> Result<Option<Release>, String> {
    let list: Vec<GhRelease> = serde_json::from_str(json).map_err(|e| format!("GitHub: {e}"))?;
    let suffix = package_suffix();
    let best = list
        .into_iter()
        .filter(|r| !r.draft && (include_pre || !r.prerelease || parse_version(&r.tag_name).first() == Some(&0)))
        .max_by(|a, b| parse_version(&a.tag_name).cmp(&parse_version(&b.tag_name)));
    Ok(best.map(|r| {
        let package = r.assets.iter().find(|a| a.name.ends_with(&suffix)).or_else(|| {
            // Older Linux releases: any AppImage.
            (!cfg!(windows)).then(|| r.assets.iter().find(|a| a.name.ends_with(".AppImage"))).flatten()
        });
        let checksum_url =
            package.and_then(|ai| r.assets.iter().find(|a| a.name == format!("{}.sha256", ai.name))).map(|a| a.browser_download_url.clone());
        Release {
            version: r.tag_name.trim_start_matches(['v', 'V']).to_string(),
            tag: r.tag_name.clone(),
            notes: r.body.unwrap_or_default(),
            page: r.html_url,
            prerelease: r.prerelease,
            package: package.map(|a| (a.name.clone(), a.browser_download_url.clone(), a.size)),
            checksum_url,
        }
    }))
}

fn which(cmd: &str) -> bool {
    // Windows 10/11 ship curl.exe.
    let names = [cmd.to_string(), format!("{cmd}.exe")];
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| names.iter().any(|n| d.join(n).is_file())))
}

/// Downloads `url` to `out` (or stdout when `None`) and returns stdout.
fn http(url: &str, proxy: &str, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let agent = format!("RCManager/{}", env!("CARGO_PKG_VERSION"));
    let mut cmd = if which("curl") {
        let mut c = Command::new("curl");
        c.args(["-fsSL", "--connect-timeout", "15", "-A", &agent, "-H", "Accept: application/vnd.github+json"]);
        if !proxy.is_empty() {
            c.args(["-x", proxy]);
        }
        if let Some(o) = out {
            c.arg("-o").arg(o);
        }
        c.arg(url);
        c
    } else if which("wget") {
        let mut c = Command::new("wget");
        c.args(["-q", "-T", "15", "-U", &agent]);
        if !proxy.is_empty() {
            c.env("https_proxy", proxy).env("http_proxy", proxy);
        }
        match out {
            Some(o) => c.arg("-O").arg(o),
            None => c.args(["-O", "-"]),
        };
        c.arg(url);
        c
    } else {
        return Err("The updater needs curl or wget.".into());
    };
    let o = cmd.output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("Download failed: {url}") } else { err });
    }
    Ok(o.stdout)
}

/// Looks for a newer release. `Ok(None)` = up to date.
pub fn check(proxy: &str) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=20", repo());
    let json = http(&url, proxy, None).map_err(|e| {
        if e.contains("404") {
            format!("{e}\nThe releases of github.com/{} cannot be read (the repository is not public).", repo())
        } else {
            e
        }
    })?;
    let rel = newest(&String::from_utf8_lossy(&json), false)?;
    Ok(rel.filter(|r| is_newer(&r.version, env!("CARGO_PKG_VERSION"))))
}

/// End of the release asset that fits this installation.
fn package_suffix() -> String {
    if cfg!(windows) {
        if crate::store::Store::portable_dir().is_some() {
            "-windows-x64-portable.zip".into()
        } else {
            "-windows-x64-setup.exe".into()
        }
    } else {
        format!("{}.AppImage", std::env::consts::ARCH)
    }
}

/// What an update replaces: the AppImage this process was started from (Linux) or the
/// running `rc-manager.exe` (Windows, installed or portable). `None`: no self-update.
pub fn update_target() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::current_exe().ok()
    } else {
        std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
    }
}

pub enum Msg {
    Checked(Result<Option<Release>, String>),
    Installed(Result<PathBuf, String>),
}

pub fn spawn_check(proxy: String, tx: Sender<Msg>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Checked(check(&proxy)));
        wake();
    });
}

/// Where the new AppImage goes: when the current file name contains the running version
/// (`RC-Manager-0.5.4-x86_64.AppImage`), the new one gets the new version in its name.
pub fn new_target(target: &Path, current: &str, new: &str) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !current.is_empty() && name.contains(current) {
        target.with_file_name(name.replacen(current, new, 1))
    } else {
        target.to_path_buf()
    }
}

/// Downloads the AppImage of `rel` next to `target`, verifies it and replaces `target` (renamed
/// to the new version when its name carries the old one). Returns the path of the new AppImage.
pub fn install(rel: &Release, target: &Path, proxy: &str) -> Result<PathBuf, String> {
    if cfg!(windows) {
        return install_windows(rel, target, proxy);
    }
    let (name, url, _) = rel.package.as_ref().ok_or_else(|| "The release has no AppImage for this system.".to_string())?;
    let dir = target.parent().ok_or("no parent directory")?;
    let tmp = dir.join(format!(".{name}.download"));
    let _ = std::fs::remove_file(&tmp);
    let result = (|| {
        http(url, proxy, Some(&tmp))?;
        let data = std::fs::read(&tmp).map_err(|e| e.to_string())?;
        if data.len() < 1024 * 1024 || &data[1..4] != b"ELF" {
            return Err("The downloaded file is not an AppImage.".to_string());
        }
        verify(rel, &data, proxy)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        // Rename is atomic; the running process keeps its (now unlinked) image mounted.
        let dest = new_target(target, env!("CARGO_PKG_VERSION"), &rel.version);
        std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        if dest != target {
            let _ = std::fs::remove_file(target);
        }
        Ok(dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Compares the SHA-256 of `data` with the release's `.sha256` file.
fn verify(rel: &Release, data: &[u8], proxy: &str) -> Result<(), String> {
    match &rel.checksum_url {
        Some(cu) => {
            let sum = String::from_utf8_lossy(&http(cu, proxy, None)?).split_whitespace().next().unwrap_or("").to_lowercase();
            let actual = format!("{:x}", Sha256::digest(data));
            if sum != actual {
                return Err(format!("Checksum mismatch (expected {sum}, got {actual})."));
            }
            Ok(())
        }
        None => Err("The release has no checksum file; the update was not installed.".into()),
    }
}

/// A path as a single-quoted PowerShell string.
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// Windows: downloads and verifies the installer or the portable ZIP and writes the script that
/// applies it after RC Manager has exited (see `finish_on_exit`). Returns that script.
fn install_windows(rel: &Release, exe: &Path, proxy: &str) -> Result<PathBuf, String> {
    let (name, url, _) = rel.package.as_ref().ok_or_else(|| "The release has no Windows package for this installation.".to_string())?;
    let dir = exe.parent().ok_or("no program directory")?;
    let work = std::env::temp_dir().join(format!("rc-manager-update-{}", rel.version));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let file = work.join(name);
    http(url, proxy, Some(&file))?;
    let data = std::fs::read(&file).map_err(|e| e.to_string())?;
    verify(rel, &data, proxy)?;
    let mut script = String::from("param([switch]$Restart)\n");
    script += &format!("Wait-Process -Id {} -ErrorAction SilentlyContinue\n", std::process::id());
    if name.ends_with(".zip") {
        if !data.starts_with(b"PK") {
            return Err("The downloaded file is not a ZIP archive.".into());
        }
        // Unpacked now (errors show in the dialog), copied over the program once it has exited.
        // The ZIP holds no `data` folder, so the user's data stays untouched.
        let staging = work.join("files");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&data)).map_err(|e| format!("ZIP: {e}"))?;
        zip.extract(&staging).map_err(|e| format!("ZIP: {e}"))?;
        if !staging.join("rc-manager.exe").is_file() {
            return Err("The ZIP archive contains no rc-manager.exe.".into());
        }
        script += &format!("Copy-Item -Path (Join-Path {} '*') -Destination {} -Recurse -Force\n", ps_quote(&staging), ps_quote(dir));
    } else {
        if !data.starts_with(b"MZ") {
            return Err("The downloaded file is not a Windows program.".into());
        }
        // Silent update of the existing installation (same AppId, same folder).
        script += &format!(
            "Start-Process -FilePath {} -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/SP-' -Wait\n",
            ps_quote(&file)
        );
    }
    script += &format!("if ($Restart) {{ Start-Process -FilePath {} }}\n", ps_quote(exe));
    script += &format!("Remove-Item -Path {} -Recurse -Force -ErrorAction SilentlyContinue\n", ps_quote(&work));
    let path = std::env::temp_dir().join(format!("rc-manager-update-{}.ps1", rel.version));
    std::fs::write(&path, script).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn spawn_install(rel: Release, target: PathBuf, proxy: String, tx: Sender<Msg>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Installed(install(&rel, &target, &proxy)));
        wake();
    });
}

/// Completes an installed update when RC Manager exits: Linux starts the new AppImage (only with
/// `restart`, the file is already in place); Windows runs the script from `install_windows`,
/// which applies the update and, with `restart`, starts RC Manager again.
pub fn finish_on_exit(installed: &Path, restart: bool) {
    if cfg!(windows) {
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"]).arg(installed);
        if restart {
            cmd.arg("-Restart");
        }
        let _ = crate::launch::hide_console(&mut cmd).spawn();
    } else if restart {
        restart_when_exited(installed);
    }
}

/// Starts `exe` once this process has exited (used for "restart after update").
fn restart_when_exited(exe: &Path) {
    let pid = std::process::id().to_string();
    let _ = Command::new("sh")
        .args(["-c", "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; exec \"$2\"", "sh", &pid])
        .arg(exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v0.6.0", "0.5.4"));
        assert!(is_newer("0.5.5", "0.5"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.5.4", "0.5.4"));
        assert!(!is_newer("0.5.4-beta", "0.5.4"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        assert_eq!(repo(), "StofflHD/RC-Manager");
    }

    #[test]
    fn picks_newest_release_with_assets() {
        let json = r#"[
          {"tag_name":"v0.6.0","body":"notes","html_url":"https://x/0.6","draft":false,"prerelease":true,
           "assets":[{"name":"RC-Manager-0.6.0-x86_64.AppImage","browser_download_url":"https://x/a","size":5},
                     {"name":"RC-Manager-0.6.0-x86_64.AppImage.sha256","browser_download_url":"https://x/s","size":1},
                     {"name":"RC-Manager-0.6.0-windows-x64-setup.exe","browser_download_url":"https://x/a","size":5},
                     {"name":"RC-Manager-0.6.0-windows-x64-setup.exe.sha256","browser_download_url":"https://x/s","size":1}]},
          {"tag_name":"v0.7.0","html_url":"https://x/0.7","draft":true,"prerelease":false,"assets":[]},
          {"tag_name":"v0.5.4","html_url":"https://x/0.5.4","draft":false,"prerelease":true,"assets":[]}
        ]"#;
        let r = newest(json, false).unwrap().unwrap();
        assert_eq!((r.version.as_str(), r.notes.as_str()), ("0.6.0", "notes"), "drafts are skipped");
        if std::env::consts::ARCH == "x86_64" {
            assert_eq!(r.package.as_ref().unwrap().1, "https://x/a");
            assert_eq!(r.checksum_url.as_deref(), Some("https://x/s"));
        }
        // From 1.0 on, pre-releases are only offered on request.
        let json = r#"[{"tag_name":"v1.1.0-rc1","html_url":"u","prerelease":true},{"tag_name":"v1.0.0","html_url":"u","prerelease":false}]"#;
        assert_eq!(newest(json, false).unwrap().unwrap().version, "1.0.0");
        assert_eq!(newest(json, true).unwrap().unwrap().version, "1.1.0-rc1");
    }

    #[test]
    fn versioned_file_names() {
        let p = Path::new("/apps/RC-Manager-0.5.4-x86_64.AppImage");
        assert_eq!(new_target(p, "0.5.4", "0.6.0"), Path::new("/apps/RC-Manager-0.6.0-x86_64.AppImage"));
        assert_eq!(new_target(Path::new("/apps/rc-manager.AppImage"), "0.5.4", "0.6.0"), Path::new("/apps/rc-manager.AppImage"));
    }

    #[cfg(unix)]
    #[test]
    fn install_verifies_checksum() {
        if !which("curl") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rcm-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut data = b"\x7fELF".to_vec();
        data.resize(2 * 1024 * 1024, 7);
        std::fs::write(dir.join("new.AppImage"), &data).unwrap();
        let sum = format!("{:x}  new.AppImage\n", Sha256::digest(&data));
        std::fs::write(dir.join("new.AppImage.sha256"), &sum).unwrap();
        std::fs::write(dir.join("bad.sha256"), "0000  x\n").unwrap();
        let target = dir.join("RC-Manager.AppImage");
        std::fs::write(&target, b"old").unwrap();
        let url = |f: &str| format!("file://{}", dir.join(f).display());
        let mut rel = Release { package: Some(("new.AppImage".into(), url("new.AppImage"), 0)), checksum_url: Some(url("bad.sha256")), ..Default::default() };
        assert!(install(&rel, &target, "").unwrap_err().contains("Checksum"));
        assert_eq!(std::fs::read(&target).unwrap(), b"old", "target untouched on error");
        rel.checksum_url = Some(url("new.AppImage.sha256"));
        install(&rel, &target, "").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), data);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o111, 0o111);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live {
    /// Queries the real GitHub releases (network): `cargo test live_github -- --ignored`
    #[test]
    #[ignore]
    fn live_github() {
        let json = super::http(&format!("https://api.github.com/repos/{}/releases?per_page=20", super::repo()), "", None).unwrap();
        let rel = super::newest(&String::from_utf8_lossy(&json), false).unwrap().expect("a release");
        println!("{rel:#?}");
        assert!(rel.package.is_some() && rel.checksum_url.is_some());
    }
}
