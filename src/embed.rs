//! Embedding of client windows (FreeRDP, xterm, TigerVNC) into RC Manager's window.
//!
//! The Windows original shows sessions as tabs via the RDP ActiveX control. On Linux the
//! clients are separate programs, so each session tab gets an X11 child window ("host")
//! inside RC Manager's window: FreeRDP draws into it (`/parent-window`), xterm creates its
//! window inside it (`-into`), and TigerVNC's window is reparented into it. The host is moved
//! and resized with the tab; the client window inside is resized to fill it, which makes
//! FreeRDP adapt the remote resolution (`/dynamic-resolution`) and xterm its terminal size.
//!
//! X11 child windows are drawn above everything RC Manager paints. Where a menu, dialog or
//! the sidebar overlay covers a session, that area is cut out of the host window (SHAPE
//! extension), so the session stays visible around it.
//!
//! This needs X11; on Wayland desktops RC Manager runs via XWayland (see `main.rs`). On
//! Windows the same is done with Win32 child windows (see the `windows` implementation below).

/// Window id: an X11 window (32 bits) or a Win32 `HWND`.
#[cfg(not(windows))]
pub type WinId = u32;
#[cfg(windows)]
pub type WinId = u64;

#[cfg(target_os = "linux")]
mod imp {
    use super::WinId;
    use anyhow::{Context, Result};
    use x11rb::connection::{Connection, RequestConnection};
    use x11rb::protocol::res::{self, ClientIdMask, ClientIdSpec, ConnectionExt as _};
    use x11rb::protocol::shape::{self, ConnectionExt as _, SK, SO};
    use x11rb::protocol::xproto::{
        ClipOrdering, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, KeyButMask, MapState, Rectangle,
        WindowClass,
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
        pub fn connect(main: WinId) -> Result<Self> {
            let (conn, screen) = x11rb::connect(None).context("Cannot connect to the X server")?;
            let s = &conn.setup().roots[screen];
            let (root, black) = (s.root, s.black_pixel);
            Ok(Self { conn, main, root, black })
        }

        /// Creates an (unmapped) host window for one session.
        pub fn create_host(&self) -> Result<WinId> {
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
        /// makes every client window inside fill it. `holes` (relative to the host) are cut
        /// out of it, so RC Manager's own content shows (and gets the clicks) there.
        // Position, size, visibility and holes are the natural parameters of an X11 placement.
        #[allow(clippy::too_many_arguments)]
        pub fn place(&self, host: WinId, x: i32, y: i32, w: u32, h: u32, visible: bool, holes: &[[i32; 4]]) -> Result<()> {
            let (w, h) = (w.max(1), h.max(1));
            self.conn.configure_window(host, &ConfigureWindowAux::new().x(x).y(y).width(w).height(h))?;
            for child in self.conn.query_tree(host)?.reply()?.children {
                self.conn.configure_window(child, &ConfigureWindowAux::new().x(0).y(0).width(w).height(h))?;
            }
            if self.can_shape() {
                if holes.is_empty() {
                    // Back to the plain rectangle.
                    self.conn.shape_mask(SO::SET, SK::BOUNDING, host, 0, 0, x11rb::NONE)?;
                } else {
                    let full = Rectangle { x: 0, y: 0, width: w as u16, height: h as u16 };
                    self.conn.shape_rectangles(SO::SET, SK::BOUNDING, ClipOrdering::UNSORTED, host, 0, 0, &[full])?;
                    let cut: Vec<Rectangle> = holes
                        .iter()
                        .map(|&[x, y, w, h]| Rectangle { x: x as i16, y: y as i16, width: w.max(0) as u16, height: h.max(0) as u16 })
                        .collect();
                    self.conn.shape_rectangles(SO::SUBTRACT, SK::BOUNDING, ClipOrdering::UNSORTED, host, 0, 0, &cut)?;
                }
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
        pub fn client_of(&self, host: WinId) -> Option<WinId> {
            self.conn.query_tree(host).ok()?.reply().ok()?.children.last().copied()
        }

        /// Finds the largest mapped top-level window created by `pid` (looking one level into
        /// window manager frames) using the X-Resource extension.
        pub fn window_of_pid(&self, pid: u32) -> Option<WinId> {
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
        pub fn adopt(&self, win: WinId, host: WinId) -> Result<()> {
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
        pub fn focus(&self, win: Option<WinId>) {
            let target = win.unwrap_or(self.main);
            let _ = self.conn.set_input_focus(InputFocus::PARENT, target, x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
        }

        /// The host's size has settled (X11 clients follow their window size by themselves).
        pub fn resized(&self, _host: WinId) {}

        pub fn destroy(&self, host: WinId) {
            let _ = self.conn.destroy_window(host);
            let _ = self.conn.flush();
        }

        /// The X-Resource extension is needed to find the windows of TigerVNC.
        pub fn can_adopt(&self) -> bool {
            self.conn.extension_information(res::X11_EXTENSION_NAME).ok().flatten().is_some()
        }

        /// Without the SHAPE extension a covered session has to be hidden.
        pub fn can_shape(&self) -> bool {
            self.conn.extension_information(shape::X11_EXTENSION_NAME).ok().flatten().is_some()
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::WinId;
    use anyhow::{bail, Result};

    pub struct Embedder;

    pub struct Pointer {
        pub x: i32,
        pub y: i32,
        pub pressed: bool,
    }

    impl Embedder {
        pub fn connect(_main: WinId) -> Result<Self> {
            bail!("Embedding sessions needs X11 or Windows")
        }
        pub fn create_host(&self) -> Result<WinId> {
            unreachable!()
        }
        pub fn place(&self, _host: WinId, _x: i32, _y: i32, _w: WinId, _h: WinId, _visible: bool, _holes: &[[i32; 4]]) -> Result<()> {
            Ok(())
        }
        pub fn client_of(&self, _host: WinId) -> Option<WinId> {
            None
        }
        pub fn window_of_pid(&self, _pid: u32) -> Option<WinId> {
            None
        }
        pub fn adopt(&self, _win: WinId, _host: WinId) -> Result<()> {
            Ok(())
        }
        pub fn pointer(&self) -> Option<Pointer> {
            None
        }
        pub fn focus(&self, _win: Option<WinId>) {}
        pub fn resized(&self, _host: WinId) {}
        pub fn destroy(&self, _host: WinId) {}
        pub fn can_adopt(&self) -> bool {
            false
        }
        pub fn can_shape(&self) -> bool {
            false
        }
    }
}

#[cfg(windows)]
mod imp {
    //! Win32: every tab gets a child window of RC Manager's window. FreeRDP draws into it
    //! (`/parent-window`), PuTTY and TigerVNC are moved into it with `SetParent` and keep their
    //! frame, which lies outside the host and is cut off (as in the original RDP-Manager).
    //! Covered areas are cut out with a window region (the counterpart of the X11 SHAPE extension).
    use super::WinId;
    use anyhow::{bail, Result};
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, CombineRgn, CreateRectRgn, DeleteObject, GetStockObject, ScreenToClient, SetWindowRgn, BLACK_BRUSH, RGN_DIFF};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, SetFocus, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub struct Embedder {
        main: HWND,
    }

    /// Pointer position relative to RC Manager's window and whether the left button is down.
    pub struct Pointer {
        pub x: i32,
        pub y: i32,
        pub pressed: bool,
    }

    fn hwnd(id: WinId) -> HWND {
        id as usize as HWND
    }

    fn id(h: HWND) -> WinId {
        h as usize as WinId
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const HOST_CLASS: &str = "RcManagerSessionHost";

    /// Direct child windows of `parent` (the client windows inside a host).
    fn children(parent: HWND) -> Vec<HWND> {
        let mut out = Vec::new();
        unsafe {
            let mut c = GetWindow(parent, GW_CHILD);
            while !c.is_null() {
                out.push(c);
                c = GetWindow(c, GW_HWNDNEXT);
            }
        }
        out
    }

    fn class_name(w: HWND) -> String {
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(w, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// Turns an adopted top-level window into a child window. Its frame stays: clients such as
    /// PuTTY restore a removed one, so `fit` moves it out of sight instead.
    unsafe fn make_child(win: HWND) {
        let style = GetWindowLongPtrW(win, GWL_STYLE) as u32;
        SetWindowLongPtrW(win, GWL_STYLE, ((style & !WS_POPUP) | WS_CHILD) as isize);
        let ex = GetWindowLongPtrW(win, GWL_EXSTYLE) as u32;
        SetWindowLongPtrW(win, GWL_EXSTYLE, (ex & !WS_EX_APPWINDOW) as isize);
    }

    /// Places a client window so that its client area (and scroll bar) fills a `w`×`h` host:
    /// title bar and frame lie outside the host, which clips them away.
    unsafe fn fit(win: HWND, w: i32, h: i32) {
        let mut wr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let mut cr = wr;
        let mut origin = POINT { x: 0, y: 0 };
        let (mut left, mut top, mut right, mut bottom) = (0, 0, 0, 0);
        if GetWindowRect(win, &mut wr) != 0 && GetClientRect(win, &mut cr) != 0 && ClientToScreen(win, &mut origin) != 0 {
            left = origin.x - wr.left;
            top = origin.y - wr.top;
            right = wr.right - origin.x - cr.right;
            bottom = wr.bottom - origin.y - cr.bottom;
            // Scroll bars belong to the non-client area, but stay visible.
            let style = GetWindowLongPtrW(win, GWL_STYLE) as u32;
            if style & WS_VSCROLL != 0 {
                right -= GetSystemMetrics(SM_CXVSCROLL);
            }
            if style & WS_HSCROLL != 0 {
                bottom -= GetSystemMetrics(SM_CYHSCROLL);
            }
        }
        let (left, top, right, bottom) = (left.max(0), top.max(0), right.max(0), bottom.max(0));
        SetWindowPos(win, std::ptr::null_mut(), -left, -top, w + left + right, h + top + bottom, SWP_NOACTIVATE | SWP_NOZORDER);
    }

    impl Embedder {
        pub fn connect(main: WinId) -> Result<Self> {
            let main = hwnd(main);
            unsafe {
                if IsWindow(main) == 0 {
                    bail!("RC Manager's window handle is not valid");
                }
                // RC Manager's own drawing must not paint over the session windows.
                let style = GetWindowLongPtrW(main, GWL_STYLE);
                SetWindowLongPtrW(main, GWL_STYLE, style | (WS_CLIPCHILDREN as isize));
                let class = wide(HOST_CLASS);
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: 0,
                    lpfnWndProc: Some(DefWindowProcW),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: GetModuleHandleW(std::ptr::null()),
                    hIcon: std::ptr::null_mut(),
                    hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                    hbrBackground: GetStockObject(BLACK_BRUSH) as _,
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class.as_ptr(),
                    hIconSm: std::ptr::null_mut(),
                };
                // A second registration (e.g. after a re-detection) fails harmlessly.
                RegisterClassExW(&wc);
            }
            Ok(Self { main })
        }

        /// Creates an (invisible) host window for one session.
        pub fn create_host(&self) -> Result<WinId> {
            let class = wide(HOST_CLASS);
            let h = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    0,
                    0,
                    64,
                    64,
                    self.main,
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                )
            };
            if h.is_null() {
                bail!("Cannot create a window for the session (error {})", std::io::Error::last_os_error());
            }
            Ok(id(h))
        }

        /// Moves/resizes the host (physical pixels, relative to RC Manager's window) and makes
        /// every client window inside fill it. `holes` (relative to the host) are cut out of it,
        /// so RC Manager's own content shows (and gets the clicks) there.
        #[allow(clippy::too_many_arguments)]
        pub fn place(&self, host: WinId, x: i32, y: i32, w: u32, h: u32, visible: bool, holes: &[[i32; 4]]) -> Result<()> {
            let host = hwnd(host);
            let (w, h) = (w.max(1) as i32, h.max(1) as i32);
            unsafe {
                SetWindowPos(host, HWND_TOP, x, y, w, h, SWP_NOACTIVATE);
                for c in children(host) {
                    fit(c, w, h);
                }
                if holes.is_empty() {
                    SetWindowRgn(host, std::ptr::null_mut(), 1);
                } else {
                    // The system owns the region after SetWindowRgn; the holes are freed here.
                    let rgn = CreateRectRgn(0, 0, w, h);
                    for &[hx, hy, hw, hh] in holes {
                        let hole = CreateRectRgn(hx, hy, hx + hw.max(0), hy + hh.max(0));
                        CombineRgn(rgn, rgn, hole, RGN_DIFF);
                        DeleteObject(hole);
                    }
                    SetWindowRgn(host, rgn, 1);
                }
                ShowWindow(host, if visible { SW_SHOWNA } else { SW_HIDE });
            }
            Ok(())
        }

        /// The client window inside a host, once the client has created it.
        pub fn client_of(&self, host: WinId) -> Option<WinId> {
            children(hwnd(host)).first().map(|&c| id(c))
        }

        /// Finds the largest visible top-level window of `pid` – not a dialog (PuTTY's host key
        /// question or TigerVNC's password prompt stay separate windows until answered).
        pub fn window_of_pid(&self, pid: u32) -> Option<WinId> {
            struct Search {
                pid: u32,
                best: Option<(HWND, i64)>,
            }
            unsafe extern "system" fn each(w: HWND, data: LPARAM) -> BOOL {
                let s = &mut *(data as *mut Search);
                let mut owner = 0u32;
                GetWindowThreadProcessId(w, &mut owner);
                if owner == s.pid && IsWindowVisible(w) != 0 && class_name(w) != "#32770" {
                    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    GetWindowRect(w, &mut r);
                    let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                    if area > s.best.map(|b| b.1).unwrap_or(0) {
                        s.best = Some((w, area));
                    }
                }
                1
            }
            let mut s = Search { pid, best: None };
            unsafe { EnumWindows(Some(each), &mut s as *mut Search as LPARAM) };
            s.best.map(|b| id(b.0))
        }

        /// Moves a top-level client window into a host: it becomes a child window, its frame clipped away.
        pub fn adopt(&self, win: WinId, host: WinId) -> Result<()> {
            let (win, host) = (hwnd(win), hwnd(host));
            unsafe {
                make_child(win);
                if SetParent(win, host).is_null() {
                    bail!("SetParent failed ({})", std::io::Error::last_os_error());
                }
                let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(host, &mut r);
                SetWindowPos(win, std::ptr::null_mut(), 0, 0, r.right, r.bottom, SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW);
                fit(win, r.right, r.bottom);
            }
            Ok(())
        }

        pub fn pointer(&self) -> Option<Pointer> {
            unsafe {
                let mut p = POINT { x: 0, y: 0 };
                if GetCursorPos(&mut p) == 0 || ScreenToClient(self.main, &mut p) == 0 {
                    return None;
                }
                let pressed = (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0;
                Some(Pointer { x: p.x, y: p.y, pressed })
            }
        }

        /// Gives the keyboard to a client window (or back to RC Manager with `None`). Child
        /// windows of other processes share RC Manager's input queue, so SetFocus works.
        pub fn focus(&self, win: Option<WinId>) {
            let target = win.map(hwnd).unwrap_or(self.main);
            unsafe { SetFocus(target) };
        }

        /// The host's size has settled: wfreerdp sends a new resolution (/dynamic-resolution)
        /// only when a user ends dragging its window, which this message tells it.
        pub fn resized(&self, host: WinId) {
            for c in children(hwnd(host)) {
                unsafe { PostMessageW(c, WM_EXITSIZEMOVE, 0, 0) };
            }
        }

        pub fn destroy(&self, host: WinId) {
            unsafe { DestroyWindow(hwnd(host)) };
        }

        pub fn can_adopt(&self) -> bool {
            true
        }

        pub fn can_shape(&self) -> bool {
            true
        }
    }
}

pub use imp::Embedder;

/// Window id of RC Manager's own window: X11 (or XWayland) or Win32.
pub fn main_window(frame: &eframe::Frame) -> Option<WinId> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        #[cfg(not(windows))]
        RawWindowHandle::Xlib(h) => WinId::try_from(h.window).ok(),
        #[cfg(not(windows))]
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        #[cfg(windows)]
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as usize as WinId),
        _ => None,
    }
}
