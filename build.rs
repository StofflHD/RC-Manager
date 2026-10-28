//! Windows: embeds the program icon and version information into rc-manager.exe.

fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/rc-manager.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/windows/rc-manager.ico")
        .set("ProductName", "RC Manager")
        .set("FileDescription", "RC Manager – Remote Connection Manager")
        .set("LegalCopyright", "© StofflHD and Claude – GPL-3.0-or-later");
    // Without a resource compiler (e.g. a cross build) the program just has no icon.
    if let Err(e) = res.compile() {
        println!("cargo:warning=No Windows icon embedded: {e}");
    }
}
