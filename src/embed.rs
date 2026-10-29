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

/// Result of typing text into a session (see `Embedder::type_text`), sent from its thread.
pub type Typed = std::sync::mpsc::Receiver<Result<(), String>>;

/// Pause after the key that wakes a locked screen, before the password is typed.
const WAKE_DELAY: std::time::Duration = std::time::Duration::from_millis(1500);
/// Pause between two typed characters.
const KEY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

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

        /// Whether RC Manager's window is the active one and an embedded client has the focus;
        /// on X11 the window manager restores the focus of embedded clients by itself.
        pub fn active(&self) -> bool {
            false
        }
        pub fn has_focus(&self, _win: Option<WinId>) -> bool {
            true
        }

        /// Types `text` and Enter into the client window `win` with XTEST, like a keyboard (the
        /// first key, Control, wakes a locked screen). Stops as soon as `win` loses the focus.
        pub fn type_text(&self, win: WinId, text: String) -> Result<super::Typed> {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(super::xtest_type(win, &text).map_err(|e| format!("{e:#}")));
            });
            Ok(rx)
        }

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

/// See `Embedder::type_text` (X11).
#[cfg(target_os = "linux")]
fn xtest_type(win: WinId, text: &str) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    use x11rb::connection::{Connection, RequestConnection};
    use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::{self, ConnectionExt as _};

    let (conn, screen) = x11rb::connect(None).context("Cannot connect to the X server")?;
    let root = conn.setup().roots[screen].root;
    if conn.extension_information(xtest::X11_EXTENSION_NAME)?.is_none() {
        bail!("the X server has no XTEST extension");
    }
    let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = (map.keysyms_per_keycode as usize).max(1);
    // Keycode and level of a keysym; levels 0/1 are plain/Shift, 4/5 AltGr/AltGr+Shift.
    let find = |sym: u32| {
        [0usize, 1, 4, 5].into_iter().find_map(|level| {
            map.keysyms.chunks(per).position(|ks| ks.get(level) == Some(&sym)).map(|i| (min + i as u8, level))
        })
    };
    let modifier = |sym: u32, name: &str| find(sym).map(|k| k.0).with_context(|| format!("no {name} key in the keyboard layout"));
    let (control, ret) = (modifier(0xffe3, "Control")?, modifier(0xff0d, "Return")?);
    let mut keys = Vec::new();
    for c in text.chars() {
        let cp = c as u32;
        let sym = if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) { cp } else { 0x0100_0000 | cp };
        // Never name the character: it is part of a password.
        let Some((code, level)) = find(sym) else { bail!("a character of the password is not on the keyboard layout") };
        keys.push((code, level % 2 == 1, level >= 4));
    }
    let shift = modifier(0xffe1, "Shift")?;
    let altgr = if keys.iter().any(|k| k.2) { Some(modifier(0xfe03, "AltGr")?) } else { None };
    let focused = || -> bool {
        let Ok(Ok(r)) = conn.get_input_focus().map(|c| c.reply()) else { return false };
        let mut w = r.focus;
        for _ in 0..64 {
            if w == win {
                return true;
            }
            if w == root || w == 0 {
                return false;
            }
            match conn.query_tree(w).map(|c| c.reply()) {
                Ok(Ok(t)) => w = t.parent,
                _ => return false,
            }
        }
        false
    };
    let key = |code: u8, down: bool| -> anyhow::Result<()> {
        let kind = if down { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT };
        conn.xtest_fake_input(kind, code, x11rb::CURRENT_TIME, root, 0, 0, 0)?;
        conn.flush()?;
        Ok(())
    };
    if !focused() {
        bail!("the session does not have the keyboard focus");
    }
    key(control, true)?;
    key(control, false)?;
    std::thread::sleep(WAKE_DELAY);
    for (code, with_shift, with_altgr) in keys.into_iter().chain(std::iter::once((ret, false, false))) {
        if !focused() {
            bail!("the session lost the keyboard focus, typing stopped");
        }
        if with_altgr {
            key(altgr.unwrap_or(control), true)?;
        }
        if with_shift {
            key(shift, true)?;
        }
        key(code, true)?;
        key(code, false)?;
        if with_shift {
            key(shift, false)?;
        }
        if with_altgr {
            key(altgr.unwrap_or(control), false)?;
        }
        std::thread::sleep(KEY_DELAY);
    }
    Ok(())
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
        pub fn active(&self) -> bool {
            false
        }
        pub fn has_focus(&self, _win: Option<WinId>) -> bool {
            true
        }
        pub fn type_text(&self, _win: WinId, _text: String) -> Result<super::Typed> {
            anyhow::bail!("typing into sessions is not supported on this system")
        }
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
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetKeyboardLayout, MapVirtualKeyExW, SendInput, SetFocus, VkKeyScanExW, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, VK_LBUTTON,
    };
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

    /// Whether `win` (or a window inside it) has the keyboard focus while RC Manager (`main`)
    /// is the foreground window.
    unsafe fn focus_in(main: HWND, win: HWND) -> bool {
        if GetForegroundWindow() != main {
            return false;
        }
        let thread = GetWindowThreadProcessId(main, std::ptr::null_mut());
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        GetGUIThreadInfo(thread, &mut info) != 0 && (info.hwndFocus == win || IsChild(win, info.hwndFocus) != 0)
    }

    /// One key press or release by scan code.
    unsafe fn send_key(scan: u16, extended: bool, up: bool) {
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
    }

    /// See `Embedder::type_text` (Windows).
    unsafe fn send_text(main: HWND, win: HWND, text: &str) -> Result<()> {
        const SHIFT: u16 = 0x2a;
        const CONTROL: u16 = 0x1d;
        const ALT: u16 = 0x38; // with the extended flag: AltGr
        const ENTER: u16 = 0x1c;
        // The keyboard layout of the client's thread, which wfreerdp sends the keys with.
        let layout = GetKeyboardLayout(GetWindowThreadProcessId(win, std::ptr::null_mut()));
        let mut keys = Vec::new();
        for c in text.encode_utf16() {
            let r = VkKeyScanExW(c, layout);
            let (vk, state) = ((r as u16 & 0xff) as u32, (r as u16 >> 8) & 0xff);
            let scan = if r == -1 { 0 } else { MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, layout) as u16 };
            // Never name the character: it is part of a password. Control or Alt alone would
            // be a shortcut, not a character.
            if scan == 0 || matches!(state & 6, 2 | 4) {
                bail!("a character of the password is not on the keyboard layout");
            }
            keys.push((scan, state & 1 != 0, state & 6 == 6));
        }
        if !focus_in(main, win) {
            bail!("the session does not have the keyboard focus");
        }
        send_key(CONTROL, false, false);
        send_key(CONTROL, false, true);
        std::thread::sleep(super::WAKE_DELAY);
        for (scan, shift, altgr) in keys.into_iter().chain(std::iter::once((ENTER, false, false))) {
            if !focus_in(main, win) {
                bail!("the session lost the keyboard focus, typing stopped");
            }
            if altgr {
                send_key(CONTROL, false, false);
                send_key(ALT, true, false);
            }
            if shift {
                send_key(SHIFT, false, false);
            }
            send_key(scan, false, false);
            send_key(scan, false, true);
            if shift {
                send_key(SHIFT, false, true);
            }
            if altgr {
                send_key(ALT, true, true);
                send_key(CONTROL, false, true);
            }
            std::thread::sleep(super::KEY_DELAY);
        }
        Ok(())
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

        /// Whether RC Manager's window is the foreground window (embedded clients are children
        /// of it, so this stays true while one of them has the focus).
        pub fn active(&self) -> bool {
            unsafe { GetForegroundWindow() == self.main }
        }

        /// Whether `win` (or a window inside it) has the keyboard focus of RC Manager's thread –
        /// with an embedded client that is the thread's focus, as their input is attached.
        pub fn has_focus(&self, win: Option<WinId>) -> bool {
            let Some(win) = win.map(hwnd) else { return true };
            unsafe {
                let thread = GetWindowThreadProcessId(self.main, std::ptr::null_mut());
                let mut info: GUITHREADINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
                if GetGUIThreadInfo(thread, &mut info) == 0 {
                    return true;
                }
                info.hwndFocus == win || IsChild(win, info.hwndFocus) != 0
            }
        }

        /// Types `text` and Enter into the client window `win` with `SendInput` (scan codes, as
        /// wfreerdp reads the keyboard with a hook; the first key, Control, wakes a locked
        /// screen). Stops as soon as `win` loses the focus or RC Manager the foreground.
        pub fn type_text(&self, win: WinId, text: String) -> Result<super::Typed> {
            let main = id(self.main);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(unsafe { send_text(hwnd(main), hwnd(win), &text) }.map_err(|e| format!("{e:#}")));
            });
            Ok(rx)
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
