//! Embedding of client windows (FreeRDP, xterm, TigerVNC) into RC Manager's window.
//!
//! The Windows original shows sessions as tabs via the RDP ActiveX control. On Linux the
//! clients are separate programs, so each session tab gets an X11 child window ("host")
//! inside RC Manager's window: FreeRDP draws into it (`/parent-window`), xterm creates its
//! window inside it (`-into`), and TigerVNC's window is reparented into it. The host is moved
//! and resized with the tab; the client window inside is resized to fill it, which makes
//! FreeRDP adapt the remote resolution (`/dynamic-resolution`) and xterm its terminal size.
//!
//! This needs X11; on Wayland desktops RC Manager runs via XWayland (see `main.rs`).

#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{Context, Result};
    use x11rb::connection::{Connection, RequestConnection};
    use x11rb::protocol::res::{self, ClientIdMask, ClientIdSpec, ConnectionExt as _};
    use x11rb::protocol::xproto::{
        ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, KeyButMask, MapState, WindowClass,
    };
    use x11rb::rust_connection::RustConnection;

    pub struct Embedder {
        conn: RustConnection,
        main: u32,
        root: u32,
        black: u32,
    }

    /// Pointer position relative to RC Manager's window and whether the left button is down.
    pub struct Pointer {
        pub x: i32,
        pub y: i32,
        pub pressed: bool,
    }

    impl Embedder {
        pub fn connect(main: u32) -> Result<Self> {
            let (conn, screen) = x11rb::connect(None).context("Cannot connect to the X server")?;
            let s = &conn.setup().roots[screen];
            let (root, black) = (s.root, s.black_pixel);
            Ok(Self { conn, main, root, black })
        }

        /// Creates an (unmapped) host window for one session.
        pub fn create_host(&self) -> Result<u32> {
            let id = self.conn.generate_id()?;
            let aux = CreateWindowAux::new().background_pixel(self.black).event_mask(EventMask::SUBSTRUCTURE_NOTIFY);
            self.conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                id,
                self.main,
                0,
                0,
                64,
                64,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &aux,
            )?;
            self.conn.flush()?;
            Ok(id)
        }

        /// Moves/resizes the host (physical pixels, relative to RC Manager's window) and
        /// makes every client window inside fill it.
        pub fn place(&self, host: u32, x: i32, y: i32, w: u32, h: u32, visible: bool) -> Result<()> {
            let (w, h) = (w.max(1), h.max(1));
            self.conn.configure_window(host, &ConfigureWindowAux::new().x(x).y(y).width(w).height(h))?;
            for child in self.conn.query_tree(host)?.reply()?.children {
                self.conn.configure_window(child, &ConfigureWindowAux::new().x(0).y(0).width(w).height(h))?;
            }
            if visible {
                self.conn.map_window(host)?;
            } else {
                self.conn.unmap_window(host)?;
            }
            self.conn.flush()?;
            Ok(())
        }

        /// The client window inside a host, once the client has created it.
        pub fn client_of(&self, host: u32) -> Option<u32> {
            self.conn.query_tree(host).ok()?.reply().ok()?.children.last().copied()
        }

        /// Finds the largest mapped top-level window created by `pid` (looking one level into
        /// window manager frames) using the X-Resource extension.
        pub fn window_of_pid(&self, pid: u32) -> Option<u32> {
            let mut candidates = Vec::new();
            for top in self.conn.query_tree(self.root).ok()?.reply().ok()?.children {
                candidates.push(top);
                if let Ok(Ok(tree)) = self.conn.query_tree(top).map(|c| c.reply()) {
                    candidates.extend(tree.children);
                }
            }
            let mut best: Option<(u32, u32)> = None;
            for win in candidates {
                let Ok(Ok(attr)) = self.conn.get_window_attributes(win).map(|c| c.reply()) else { continue };
                if attr.map_state != MapState::VIEWABLE || attr.class != WindowClass::INPUT_OUTPUT {
                    continue;
                }
                let spec = ClientIdSpec { client: win, mask: ClientIdMask::LOCAL_CLIENT_PID };
                let Ok(Ok(ids)) = self.conn.res_query_client_ids(&[spec]).map(|c| c.reply()) else { continue };
                if !ids.ids.iter().any(|id| id.value.first() == Some(&pid)) {
                    continue;
                }
                let Ok(Ok(geo)) = self.conn.get_geometry(win).map(|c| c.reply()) else { continue };
                let area = geo.width as u32 * geo.height as u32;
                if area > best.map(|b| b.1).unwrap_or(0) {
                    best = Some((win, area));
                }
            }
            best.map(|b| b.0)
        }

        /// Moves a top-level client window into a host.
        pub fn adopt(&self, win: u32, host: u32) -> Result<()> {
            self.conn.unmap_window(win)?;
            self.conn.reparent_window(win, host, 0, 0)?;
            self.conn.map_window(win)?;
            self.conn.flush()?;
            Ok(())
        }

        pub fn pointer(&self) -> Option<Pointer> {
            let r = self.conn.query_pointer(self.main).ok()?.reply().ok()?;
            Some(Pointer { x: r.win_x as i32, y: r.win_y as i32, pressed: r.mask.contains(KeyButMask::BUTTON1) })
        }

        /// Gives the keyboard to a client window (or back to RC Manager with `None`).
        pub fn focus(&self, win: Option<u32>) {
            let target = win.unwrap_or(self.main);
            let _ = self.conn.set_input_focus(InputFocus::PARENT, target, x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
        }

        pub fn destroy(&self, host: u32) {
            let _ = self.conn.destroy_window(host);
            let _ = self.conn.flush();
        }

        /// The X-Resource extension is needed to find the windows of TigerVNC.
        pub fn can_adopt(&self) -> bool {
            self.conn.extension_information(res::X11_EXTENSION_NAME).ok().flatten().is_some()
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use anyhow::{bail, Result};

    pub struct Embedder;

    pub struct Pointer {
        pub x: i32,
        pub y: i32,
        pub pressed: bool,
    }

    impl Embedder {
        pub fn connect(_main: u32) -> Result<Self> {
            bail!("Embedding sessions needs X11")
        }
        pub fn create_host(&self) -> Result<u32> {
            unreachable!()
        }
        pub fn place(&self, _host: u32, _x: i32, _y: i32, _w: u32, _h: u32, _visible: bool) -> Result<()> {
            Ok(())
        }
        pub fn client_of(&self, _host: u32) -> Option<u32> {
            None
        }
        pub fn window_of_pid(&self, _pid: u32) -> Option<u32> {
            None
        }
        pub fn adopt(&self, _win: u32, _host: u32) -> Result<()> {
            Ok(())
        }
        pub fn pointer(&self) -> Option<Pointer> {
            None
        }
        pub fn focus(&self, _win: Option<u32>) {}
        pub fn destroy(&self, _host: u32) {}
        pub fn can_adopt(&self) -> bool {
            false
        }
    }
}

pub use imp::Embedder;

/// X11 window id of RC Manager's own window, if it runs on X11 (or XWayland).
pub fn main_window(frame: &eframe::Frame) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok(),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}
