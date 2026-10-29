//! RC Manager for Linux and Windows – a re-implementation of the Windows program "RDP-Manager"
//! (Cinspiration).

// Windows: a GUI program without a console window (command-line options attach to the console
// they were started from, see `attach_console`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod embed;
mod launch;
mod legacy;
mod model;
mod net;
mod rdpax;
mod store;
mod theme;
mod update;
mod vault;

use std::path::PathBuf;

fn usage() {
    println!(
        "RC Manager {}\n\n\
         Usage: rc-manager [OPTIONS]\n\n\
         Options:\n  \
         --data-dir <PATH>          Data directory (default: {})\n  \
         --import <backup.zip|SqLiteDatabase.db3> [--app-password <PW>]\n                             Import a Windows RDP-Manager backup or database and exit\n  \
         --list                     List all connections\n  \
         --tools                    Show the clients found (FreeRDP, terminal, VNC, …)\n  \
         -h, --help                Show this help\n\n\
         With a master password set, --import/--list read it from RC_MANAGER_MASTER_PASSWORD.",
        env!("CARGO_PKG_VERSION"),
        store::Store::default_dir().display()
    );
}

/// Windows: output of command-line options (--help, --list, --tools …) goes to the console
/// RC Manager was started from.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    if std::env::args().len() > 1 {
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_console();
    // AppRun preloads the system's libxkbcommon for RC Manager only; launched clients use
    // their own libraries.
    if std::env::var_os("RC_MANAGER_UNSET_PRELOAD").is_some() {
        std::env::remove_var("LD_PRELOAD");
        std::env::remove_var("RC_MANAGER_UNSET_PRELOAD");
    }
    let mut args = std::env::args().skip(1);
    let mut data_dir = store::Store::default_dir();
    let mut custom_dir = false;
    let mut import: Option<PathBuf> = None;
    let mut app_password: Option<String> = None;
    let mut list = false;
    let mut connect = Vec::new();
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
            "--connect" => connect.push(args.next().ok_or_else(|| anyhow::anyhow!("--connect expects a connection name"))?),
            "--tools" => {
                for (name, value) in launch::Tools::detect().summary() {
                    println!("{name:<24} {value}");
                }
                return Ok(());
            }
            other => anyhow::bail!("Unknown option {other} (see --help)"),
        }
    }

    if !custom_dir && store::Store::portable_dir().is_none() {
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
    // OpenGL first (with the wgpu feature on Windows eframe would otherwise start with wgpu).
    let mut options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, ..Default::default() };
    // Portable: the window settings stay with the data next to the program, too.
    if store::Store::portable_dir().is_some() {
        options.persistence_path = Some(data_dir.join("app.ron"));
    }
    // Sessions are embedded as X11 child windows; on Wayland desktops RC Manager therefore
    // runs via XWayland (set RC_MANAGER_WAYLAND=1 for native Wayland, sessions then open in
    // separate windows).
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() && std::env::var_os("RC_MANAGER_WAYLAND").is_none() {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }));
    }
    // What App::new takes, kept so that a second renderer can still create the app.
    type Start = std::rc::Rc<std::cell::RefCell<Option<(store::Store, launch::Tools, Vec<String>)>>>;
    let start: Start = std::rc::Rc::new(std::cell::RefCell::new(Some((store, tools, connect))));
    let creator = |start: Start| -> eframe::AppCreator<'static> {
        Box::new(move |cc| {
            let (store, tools, connect) = start.borrow_mut().take().expect("the app is created once");
            Ok(Box::new(app::App::new(cc, store, tools, connect)))
        })
    };
    // Windows without OpenGL 2 (virtual machines, servers, some remote sessions): Direct3D via
    // wgpu instead – eframe keeps its event loop, so the second attempt can open a window again.
    #[cfg(windows)]
    let fallback = options.clone();
    let result = eframe::run_native("RC Manager", options, creator(start.clone()));
    #[cfg(windows)]
    if result.is_err() && start.borrow().is_some() {
        eprintln!("OpenGL failed ({}), trying Direct3D", result.as_ref().err().map(|e| e.to_string()).unwrap_or_default());
        let options = eframe::NativeOptions { renderer: eframe::Renderer::Wgpu, ..fallback };
        return eframe::run_native("RC Manager", options, creator(start)).map_err(|e| anyhow::anyhow!("{e}"));
    }
    result.map_err(|e| anyhow::anyhow!("{e}"))
}
