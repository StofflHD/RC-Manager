//! Windows: RDP sessions with Microsoft's Remote Desktop ActiveX control (the one inside
//! mstsc.exe and the original RDP-Manager) instead of FreeRDP's deprecated Windows client.
//!
//! The control lives in RC Manager's process: `atl.dll` (part of Windows) hosts it in a child
//! window of the session tab. It is driven through `IDispatch` (all its interfaces are dual), so
//! no type library bindings are needed. The control draws with the GPU and decodes H.264 like
//! mstsc, keeps its input in this process (no input queue shared with another program) and shows
//! its own certificate and credential dialogs. The connection state is polled.

#[cfg(windows)]
pub use imp::RdpAx;

/// Text for the control's extended disconnect reason (`ExtendedDisconnectReason`).
#[cfg_attr(not(windows), allow(dead_code))]
fn extended_reason(code: i32) -> String {
    let text = match code {
        0 => "the connection was closed",
        1 => "the session was disconnected by an administrator or another program",
        2 => "the session was logged off by an administrator or another program",
        3 => "the session was idle for too long",
        4 => "the logon took too long",
        5 => "another user connected to the session",
        6 => "the server ran out of memory",
        7 => "the server denied the connection",
        8 => "the server denied the connection for security reasons",
        9 => "the user account has no permission to log on remotely",
        10 => "the server requires credentials to be entered again",
        11 => "the session was logged off from within it",
        12 => "the user logged off",
        256..=267 => "the server could not issue a Remote Desktop licence",
        _ => return format!("disconnected (reason {code})"),
    };
    text.to_string()
}

#[cfg(windows)]
mod imp {
    use crate::embed::WinId;
    use crate::model::{Connection, Credential, GatewayMode};
    use anyhow::{anyhow, bail, Context, Result};
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    use windows::core::{Interface, IUnknown, BSTR, GUID, HSTRING, PCWSTR, VARIANT};
    use windows::Win32::System::Com::{CLSIDFromProgID, StringFromGUID2, IDispatch, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS};
    use windows::Win32::System::Ole::{OleInitialize, DISPID_PROPERTYPUT};
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, GetClientRect, WS_CHILD, WS_CLIPCHILDREN, WS_VISIBLE};

    /// ProgIDs of the control, newest first ("not safe for scripting": the password may be set).
    const PROGIDS: [&str; 6] = ["MsTscAx.MsTscAx.13", "MsTscAx.MsTscAx.12", "MsTscAx.MsTscAx.11", "MsTscAx.MsTscAx.10", "MsTscAx.MsTscAx.9", "MsTscAx.MsTscAx.8"];

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Late-bound access to a dual COM interface.
    struct Disp(IDispatch);

    impl Disp {
        fn id(&self, name: &str) -> Result<i32> {
            let w = HSTRING::from(name);
            let p = PCWSTR(w.as_ptr());
            let mut id = 0;
            unsafe { self.0.GetIDsOfNames(&GUID::zeroed(), &p, 1, 0x0409, &mut id) }.with_context(|| format!("no property {name}"))?;
            Ok(id)
        }

        fn invoke(&self, name: &str, flags: DISPATCH_FLAGS, args: &mut [VARIANT], put: bool) -> Result<VARIANT> {
            let id = self.id(name)?;
            // Arguments are passed in reverse order.
            args.reverse();
            let mut named = [DISPID_PROPERTYPUT];
            let params = DISPPARAMS {
                rgvarg: if args.is_empty() { std::ptr::null_mut() } else { args.as_mut_ptr() },
                rgdispidNamedArgs: if put { named.as_mut_ptr() } else { std::ptr::null_mut() },
                cArgs: args.len() as u32,
                cNamedArgs: u32::from(put),
            };
            let mut result = VARIANT::default();
            unsafe { self.0.Invoke(id, &GUID::zeroed(), 0x0409, flags, &params, Some(&mut result), None, None) }.with_context(|| format!("{name} failed"))?;
            Ok(result)
        }

        fn put(&self, name: &str, value: impl Into<VARIANT>) -> Result<()> {
            self.invoke(name, DISPATCH_PROPERTYPUT, &mut [value.into()], true).map(|_| ())
        }

        fn get(&self, name: &str) -> Result<VARIANT> {
            self.invoke(name, DISPATCH_PROPERTYGET, &mut [], false)
        }

        fn call(&self, name: &str, mut args: Vec<VARIANT>) -> Result<VARIANT> {
            self.invoke(name, DISPATCH_METHOD, &mut args, false)
        }

        /// A property holding another interface (e.g. `AdvancedSettings9`).
        fn object(&self, name: &str) -> Result<Disp> {
            let v = self.get(name)?;
            // windows-core only converts VT_UNKNOWN; the control returns VT_DISPATCH. Both keep
            // the interface pointer in the same place.
            const VT_DISPATCH: u16 = 9;
            const VT_UNKNOWN: u16 = 13;
            let raw = v.as_raw();
            let (vt, ptr) = unsafe { (raw.Anonymous.Anonymous.vt, raw.Anonymous.Anonymous.Anonymous.punkVal) };
            if vt != VT_DISPATCH && vt != VT_UNKNOWN {
                bail!("{name}: not an object (type {vt})");
            }
            let unknown = unsafe { IUnknown::from_raw_borrowed(&ptr) }.with_context(|| format!("{name} is empty"))?.clone();
            Ok(Disp(unknown.cast()?))
        }

        /// The newest of several versioned properties the control has.
        fn newest(&self, names: &[&str]) -> Result<Disp> {
            names.iter().find_map(|n| self.object(n).ok()).with_context(|| format!("none of {names:?}"))
        }
    }

    type AtlAxWinInit = unsafe extern "system" fn() -> i32;
    type AtlAxGetControl = unsafe extern "system" fn(HWND, *mut *mut core::ffi::c_void) -> i32;

    pub struct RdpAx {
        window: HWND,
        client: Disp,
        /// The session should follow the tab size (`UpdateSessionDisplaySettings`).
        dynamic: Cell<bool>,
        /// Last connection state (0 = not connected, 1 = connected, 2 = connecting).
        state: Cell<i32>,
        /// The size the session has, the size the tab wants, and when it was last asked for.
        size: Cell<(u32, u32)>,
        want: Cell<(u32, u32)>,
        tried: Cell<Instant>,
        /// Since when the session refuses the tab size, whether the desktop is scaled meanwhile,
        /// and a message for RC Manager's log.
        refused: Cell<Option<Instant>>,
        scaled: Cell<bool>,
        note: Cell<Option<String>>,
    }

    impl RdpAx {
        /// Creates the control in `host` (a tab's window), sets up the connection and connects.
        pub fn connect(host: WinId, width: u32, height: u32, conn: &Connection, cred: Option<&Credential>) -> Result<Self> {
            unsafe {
                // eframe already initialises OLE on its thread (drag and drop); S_FALSE then.
                let _ = OleInitialize(None);
                let atl = LoadLibraryW(wide("atl.dll").as_ptr());
                if atl.is_null() {
                    bail!("atl.dll not found");
                }
                let init: AtlAxWinInit = std::mem::transmute(GetProcAddress(atl, c"AtlAxWinInit".as_ptr().cast()).context("AtlAxWinInit missing")?);
                let get: AtlAxGetControl =
                    std::mem::transmute(GetProcAddress(atl, c"AtlAxGetControl".as_ptr().cast()).context("AtlAxGetControl missing")?);
                if init() == 0 {
                    bail!("AtlAxWinInit failed");
                }
                let host = host as usize as HWND;
                let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(host, &mut r);
                let (w, h) = (r.right.max(width as i32), r.bottom.max(height as i32));
                let size = super::desktop_size(w as u32, h as u32);
                let mut last = anyhow!("the Remote Desktop ActiveX control is not installed");
                for progid in PROGIDS {
                    // ATL takes text that is no registered class for a web address and would
                    // quietly create a browser: only create classes that exist, by CLSID.
                    let Ok(clsid) = CLSIDFromProgID(&HSTRING::from(progid)) else { continue };
                    let mut text = [0u16; 40];
                    if StringFromGUID2(&clsid, &mut text) == 0 {
                        continue;
                    }
                    let window = CreateWindowExW(
                        0,
                        wide("AtlAxWin").as_ptr(),
                        text.as_ptr(),
                        WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
                        0,
                        0,
                        w,
                        h,
                        host,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null(),
                    );
                    if window.is_null() {
                        continue;
                    }
                    let mut raw = std::ptr::null_mut();
                    if get(window, &mut raw) < 0 || raw.is_null() {
                        DestroyWindow(window);
                        continue;
                    }
                    let unknown = IUnknown::from_raw(raw);
                    let ax = match unknown.cast::<IDispatch>().map_err(anyhow::Error::from).and_then(|d| Disp(d.clone()).id("Server").map(|_| d)) {
                        Ok(d) => RdpAx {
                            window,
                            client: Disp(d),
                            dynamic: Cell::new(false),
                            state: Cell::new(2),
                            size: Cell::new(size),
                            want: Cell::new(size),
                            tried: Cell::new(Instant::now()),
                            refused: Cell::new(None),
                            scaled: Cell::new(false),
                            note: Cell::new(None),
                        },
                        Err(e) => {
                            DestroyWindow(window);
                            last = anyhow!("{progid}: {e}");
                            continue;
                        }
                    };
                    // On an error, dropping `ax` closes the control's window again.
                    ax.dynamic.set(ax.setup(conn, cred, size.0, size.1)?);
                    ax.client.call("Connect", Vec::new())?;
                    return Ok(ax);
                }
                Err(last)
            }
        }

        /// Transfers the connection's settings; returns whether the session follows the tab size.
        fn setup(&self, conn: &Connection, cred: Option<&Credential>, width: u32, height: u32) -> Result<bool> {
            let o = &conn.options.rdp;
            let c = &self.client;
            c.put("Server", BSTR::from(conn.host.trim()))?;
            if let Some(k) = cred {
                if !k.username.is_empty() {
                    c.put("UserName", BSTR::from(k.username.as_str()))?;
                }
                if !k.domain.is_empty() {
                    c.put("Domain", BSTR::from(k.domain.as_str()))?;
                }
            }
            // A fixed resolution is scaled into the tab; otherwise the session gets the tab size.
            let fixed = o.resolution.split_once(['x', 'X']).and_then(|(w, h)| Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?)));
            let (w, h) = if o.dynamic_resolution { (width, height) } else { fixed.unwrap_or((width, height)) };
            let (w, h) = super::desktop_size(w, h);
            c.put("DesktopWidth", w as i32)?;
            c.put("DesktopHeight", h as i32)?;
            if o.color_depth > 0 {
                c.put("ColorDepth", o.color_depth as i32)?;
            }
            let a = c.newest(&["AdvancedSettings9", "AdvancedSettings8", "AdvancedSettings7", "AdvancedSettings6", "AdvancedSettings5", "AdvancedSettings2"])?;
            if let Some(k) = cred.filter(|k| !k.password.is_empty()) {
                a.put("ClearTextPassword", BSTR::from(k.password.as_str()))?;
            }
            a.put("RDPPort", conn.effective_port() as i32)?;
            a.put("EnableCredSspSupport", !o.disable_nla)?;
            // 0: connect without warning, 1: do not connect, 2: warn (the control's own dialog).
            a.put("AuthenticationLevel", if o.ignore_certificate { 0 } else if o.refuse_unverified { 1 } else { 2 })?;
            // Without dynamic resolution the desktop is scaled into the tab, as with FreeRDP.
            a.put("SmartSizing", !o.dynamic_resolution)?;
            a.put("EnableAutoReconnect", true)?;
            a.put("GrabFocusOnConnect", false).ok();
            a.put("RedirectPrinters", o.printers)?;
            a.put("RedirectSmartCards", o.smartcards)?;
            a.put("RedirectDrives", o.home_drive || !o.drives.is_empty())?;
            a.put("RedirectClipboard", o.clipboard).ok();
            a.put("ConnectToAdministerServer", o.console).ok();
            a.put("AudioCaptureRedirectionMode", o.microphone).ok();
            a.put("RedirectPorts", o.redirect_ports).ok();
            a.put("RedirectDevices", o.redirect_pnp).ok();
            // Experience: the flags switch features off (1 wallpaper, 2 full window drag,
            // 4 menu animations, 8 themes), 0x80 switches font smoothing on.
            let flags = [(!o.wallpaper, 0x1), (!o.full_window_drag, 0x2), (!o.menu_animations, 0x4), (!o.themes, 0x8), (o.font_smoothing, 0x80)]
                .iter()
                .filter(|(on, _)| *on)
                .fold(0, |f, (_, bit)| f | bit);
            a.put("PerformanceFlags", flags as i32).ok();
            a.put("BandwidthDetection", o.auto_detect).ok();
            a.put("BitmapPersistence", o.bitmap_caching as i32).ok();
            a.put("Compress", (!o.disable_compression) as i32).ok();
            if let Ok(s) = c.newest(&["SecuredSettings3", "SecuredSettings2"]) {
                // 0: play sounds on this computer, 1: on the remote computer, 2: do not play them.
                s.put("AudioRedirectionMode", if o.sound { 0 } else if o.sound_remote { 1 } else { 2 }).ok();
                s.put("KeyboardHookMode", o.shortcuts.hook_mode()).ok();
                if o.remote_app.trim().is_empty() && !o.start_program.trim().is_empty() {
                    s.put("StartProgram", BSTR::from(o.start_program.trim())).ok();
                    s.put("WorkDir", BSTR::from(o.start_dir.trim())).ok();
                }
            }
            let detect = o.gateway_mode() == GatewayMode::Auto;
            if detect || o.gateway_server().is_some() {
                // As in the original: 1 always, 2 except for local addresses, 3 detect.
                let t = c.newest(&["TransportSettings4", "TransportSettings3", "TransportSettings2", "TransportSettings"])?;
                if let Some(server) = o.gateway_server() {
                    t.put("GatewayHostname", BSTR::from(server))?;
                }
                t.put("GatewayUsageMethod", if o.gateway_bypass_local { 2 } else if detect { 3 } else { 1 })?;
                t.put("GatewayProfileUsageMethod", 1)?; // explicit settings
                if o.gateway_user.is_empty() {
                    t.put("GatewayCredSharing", 1).ok(); // the connection's credentials
                } else {
                    t.put("GatewayCredSharing", 0).ok();
                    t.put("GatewayUsername", BSTR::from(o.gateway_user.as_str())).ok();
                    if !o.gateway_domain.is_empty() {
                        t.put("GatewayDomain", BSTR::from(o.gateway_domain.as_str())).ok();
                    }
                    if !o.gateway_password.is_empty() {
                        t.put("GatewayPassword", BSTR::from(o.gateway_password.as_str())).ok();
                    }
                }
            }
            Ok(o.dynamic_resolution)
        }

        /// Checks the connection; `Some(reason)` once the session has ended.
        pub fn poll(&self) -> Option<String> {
            let state = self.client.get("Connected").ok().and_then(|v| i16::try_from(&v).ok()).map(i32::from).unwrap_or(0);
            let before = self.state.replace(state);
            if state == 1 {
                // After a (re)connection the session has the size it was opened with; a tab
                // size that could not be applied yet is retried until the session accepts it.
                if before != 1 {
                    self.size.set((0, 0));
                }
                self.apply();
            }
            if state != 0 || before == 0 {
                return None;
            }
            let code = self.client.get("ExtendedDisconnectReason").ok().and_then(|v| i32::try_from(&v).ok()).unwrap_or(0);
            Some(super::extended_reason(code))
        }

        /// The tab has a new size: the remote desktop follows it (dynamic resolution).
        pub fn resize(&self, width: u32, height: u32) {
            if !self.dynamic.get() || width < 200 || height < 200 {
                return;
            }
            self.want.set(super::desktop_size(width, height));
            self.tried.set(Instant::now() - Duration::from_secs(1));
            self.apply();
        }

        /// Asks the session for the wanted size. The control refuses it until the display
        /// channel is up (shortly after logon), so a refused size is retried every second.
        fn apply(&self) {
            let (w, h) = self.want.get();
            if !self.dynamic.get() || self.state.get() != 1 || self.size.get() == (w, h) || self.tried.get().elapsed() < Duration::from_secs(1) {
                return;
            }
            self.tried.set(Instant::now());
            // Physical size in millimetres, for 96 dpi.
            let (pw, ph) = (w * 254 / 960, h * 254 / 960);
            let args = vec![VARIANT::from(w), VARIANT::from(h), VARIANT::from(pw), VARIANT::from(ph), VARIANT::from(0u32), VARIANT::from(100u32), VARIANT::from(100u32)];
            match self.client.call("UpdateSessionDisplaySettings", args) {
                Ok(_) => {
                    self.size.set((w, h));
                    self.refused.set(None);
                    if self.scaled.replace(false) {
                        self.smart_sizing(false);
                    }
                }
                Err(e) => {
                    // Shortly after connecting this is normal. If the session keeps refusing,
                    // scale the desktop into the tab so nothing is cut off.
                    let since = *self.refused.get().get_or_insert_with(Instant::now);
                    self.refused.set(Some(since));
                    if since.elapsed() > Duration::from_secs(20) && !self.scaled.replace(true) {
                        self.smart_sizing(true);
                        self.note.set(Some(format!("the server does not take the tab size ({w}x{h}: {e:#}); the desktop is scaled instead")));
                    }
                }
            }
        }

        fn smart_sizing(&self, on: bool) {
            if let Ok(a) = self.client.newest(&["AdvancedSettings9", "AdvancedSettings8", "AdvancedSettings7", "AdvancedSettings6", "AdvancedSettings5", "AdvancedSettings2"]) {
                a.put("SmartSizing", on).ok();
            }
        }

        /// A message for RC Manager's log, once.
        pub fn note(&self) -> Option<String> {
            self.note.take()
        }

        /// Ends the session (RC Manager closes the tab or exits).
        pub fn disconnect(&self) {
            if self.state.get() != 0 {
                self.client.call("Disconnect", Vec::new()).ok();
            }
        }
    }

    impl Drop for RdpAx {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.window) };
        }
    }
}

/// Other systems have no ActiveX control; RDP always uses FreeRDP there.
#[cfg(not(windows))]
#[allow(dead_code)]
pub struct RdpAx;

#[cfg(not(windows))]
#[allow(dead_code)]
impl RdpAx {
    pub fn connect(
        _host: crate::embed::WinId,
        _width: u32,
        _height: u32,
        _conn: &crate::model::Connection,
        _cred: Option<&crate::model::Credential>,
    ) -> anyhow::Result<Self> {
        anyhow::bail!("the Remote Desktop ActiveX control exists only on Windows")
    }
    pub fn poll(&self) -> Option<String> {
        None
    }
    pub fn resize(&self, _width: u32, _height: u32) {}
    pub fn note(&self) -> Option<String> {
        None
    }
    pub fn disconnect(&self) {}
}

/// A desktop size RDP accepts: 200 to 8192 pixels, the width even (MS-RDPEDISP).
#[cfg_attr(not(windows), allow(dead_code))]
fn desktop_size(width: u32, height: u32) -> (u32, u32) {
    (width.clamp(200, 8192) & !1, height.clamp(200, 8192))
}

#[cfg(test)]
mod tests {
    #[test]
    fn disconnect_reasons() {
        assert_eq!(super::extended_reason(12), "the user logged off");
        assert_eq!(super::extended_reason(260), "the server could not issue a Remote Desktop licence");
        assert_eq!(super::extended_reason(9999), "disconnected (reason 9999)");
    }

    #[test]
    fn desktop_sizes() {
        assert_eq!(super::desktop_size(1281, 721), (1280, 721));
        assert_eq!(super::desktop_size(100, 9000), (200, 8192));
        assert_eq!(super::desktop_size(8193, 600), (8192, 600));
    }
}
