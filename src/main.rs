//! RC Manager for Linux – a re-implementation of the Windows program "RDP-Manager" (Cinspiration).

mod app;
mod launch;
mod legacy;
mod model;
mod net;
mod store;
mod theme;
mod vault;

use std::path::PathBuf;

fn usage() {
    println!(
        "RC Manager {}\n\n\
         Usage: rc-manager [OPTIONS]\n\n\
         Options:\n  \
         --data-dir <PATH>          Data directory (default: {})\n  \
         --import <SqLiteDatabase.db3> [--app-password <PW>]\n                             Import a Windows RDP-Manager database and exit\n  \
         --list                     List all connections\n  \
         -h, --help                 Show this help\n\n\
         With a master password set, --import/--list read it from RC_MANAGER_MASTER_PASSWORD.",
        env!("CARGO_PKG_VERSION"),
        store::Store::default_dir().display()
    );
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut data_dir = store::Store::default_dir();
    let mut custom_dir = false;
    let mut import: Option<PathBuf> = None;
    let mut app_password: Option<String> = None;
    let mut list = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            "--data-dir" => {
                data_dir = PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!("--data-dir expects a path"))?);
                custom_dir = true;
            }
            "--import" => import = Some(PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!("--import expects a path"))?)),
            "--app-password" => app_password = args.next(),
            "--list" => list = true,
            other => anyhow::bail!("Unknown option {other} (see --help)"),
        }
    }

    if !custom_dir {
        if let Err(e) = store::Store::migrate_legacy_dir() {
            eprintln!("Warning: could not move the old data directory: {e}");
        }
    }
    let mut store = store::Store::open(&data_dir)?;

    if import.is_some() || list {
        let master = std::env::var("RC_MANAGER_MASTER_PASSWORD").ok();
        store.unlock(master.as_deref())?;
        if let Some(path) = import {
            let r = legacy::import::import(&path, app_password.as_deref())?;
            store.save_all(&r.data)?;
            println!(
                "Imported: {} folders, {} connections, {} credentials ({} deleted entries skipped)",
                r.data.folders.len(),
                r.data.connections.len(),
                r.data.credentials.len(),
                r.skipped_deleted
            );
            for w in r.warnings {
                eprintln!("Warning: {w}");
            }
        }
        if list {
            let data = store.load()?;
            for c in &data.connections {
                println!("{:<8} {:<40} {}:{}", c.protocol.label(), c.name, c.host, c.effective_port());
            }
        }
        return Ok(());
    }

    let tools = launch::Tools::detect();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RC Manager")
        .with_app_id("rc-manager")
        .with_inner_size([1150.0, 780.0])
        .with_min_inner_size([760.0, 480.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../packaging/rc-manager.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("RC Manager", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, store, tools)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}
