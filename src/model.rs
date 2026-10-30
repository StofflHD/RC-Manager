use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    Rdp,
    Ssh,
    Vnc,
    Web,
    External,
}

impl Protocol {
    pub const ALL: [Protocol; 5] = [Protocol::Rdp, Protocol::Ssh, Protocol::Vnc, Protocol::Web, Protocol::External];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Rdp => "RDP",
            Protocol::Ssh => "SSH",
            Protocol::Vnc => "VNC",
            Protocol::Web => "Web",
            Protocol::External => "External Program",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Protocol::Rdp => "rdp",
            Protocol::Ssh => "ssh",
            Protocol::Vnc => "vnc",
            Protocol::Web => "web",
            Protocol::External => "external",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Rdp => 3389,
            Protocol::Ssh => 22,
            Protocol::Vnc => 5900,
            Protocol::Web => 443,
            Protocol::External => 0,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Protocol::Rdp => "🖥",
            Protocol::Ssh => "⌨",
            Protocol::Vnc => "👁",
            Protocol::Web => "🌐",
            Protocol::External => "⚙",
        }
    }
}

/// Where a connection or folder takes its credentials from (like `CredentialSource` in the
/// original). A folder with `Inherit` takes them from its parent folder (recursively), which is
/// how nested folders share one set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub enum CredentialRef {
    #[default]
    None,
    /// Inherit from the parent folder (recursively).
    Inherit,
    Id(Uuid),
}

// Accept both the current form (serde enum: "None"/"Inherit"/{"Id":…}) and the legacy form a
// folder used before it became a `CredentialRef` (a bare UUID string, or null for "none"), so
// older database exports still import.
impl<'de> Deserialize<'de> for CredentialRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = CredentialRef;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a credential reference")
            }
            fn visit_unit<E>(self) -> Result<CredentialRef, E> {
                Ok(CredentialRef::None)
            }
            fn visit_none<E>(self) -> Result<CredentialRef, E> {
                Ok(CredentialRef::None)
            }
            fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<CredentialRef, D::Error> {
                d.deserialize_any(V)
            }
            fn visit_str<E>(self, s: &str) -> Result<CredentialRef, E> {
                Ok(match s {
                    "none" | "None" => CredentialRef::None,
                    "inherit" | "Inherit" => CredentialRef::Inherit,
                    _ => Uuid::parse_str(s).map(CredentialRef::Id).unwrap_or(CredentialRef::None),
                })
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<CredentialRef, A::Error> {
                let mut id = None;
                while let Some(k) = m.next_key::<String>()? {
                    if k == "Id" {
                        id = Some(m.next_value::<Uuid>()?);
                    } else {
                        let _: serde::de::IgnoredAny = m.next_value()?;
                    }
                }
                Ok(id.map(CredentialRef::Id).unwrap_or(CredentialRef::None))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WindowMode {
    /// Tab inside RC Manager's window, scaled with it (like the original's default).
    #[default]
    Tab,
    Window,
    Fullscreen,
    MultiMonitor,
}

impl WindowMode {
    pub fn label(self) -> &'static str {
        match self {
            WindowMode::Tab => "Tab in RC Manager",
            WindowMode::Window => "Separate window",
            WindowMode::Fullscreen => "Full screen",
            WindowMode::MultiMonitor => "Full screen (all monitors)",
        }
    }
}

/// How FreeRDP transfers the remote screen. Without the RDP8 graphics pipeline (`/gfx`)
/// FreeRDP falls back to the legacy bitmap-update path, which paints the desktop in tiles
/// and makes photos and video build up line by line. The pipeline (optionally with H.264)
/// streams whole frames and is far smoother; it needs a server that supports it (Windows 8 /
/// Server 2012 or newer) and falls back on its own on older servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RdpGraphics {
    /// Graphics pipeline on, codec negotiated with the server (`/gfx`).
    #[default]
    Auto,
    /// Graphics pipeline with H.264 (`/gfx:AVC444`) – smoothest for photos and video.
    H264,
    /// Classic RemoteFX codec (`/rfx`) – for servers without the graphics pipeline.
    RemoteFx,
    /// Legacy bitmap updates – most compatible, but the screen builds up in tiles.
    Bitmap,
}

impl RdpGraphics {
    pub const ALL: [RdpGraphics; 4] = [RdpGraphics::Auto, RdpGraphics::H264, RdpGraphics::RemoteFx, RdpGraphics::Bitmap];

    pub fn label(self) -> &'static str {
        match self {
            RdpGraphics::Auto => "Automatic (graphics pipeline)",
            RdpGraphics::H264 => "H.264 / AVC444 (video)",
            RdpGraphics::RemoteFx => "RemoteFX",
            RdpGraphics::Bitmap => "Bitmap (legacy, most compatible)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RdpOptions {
    pub window_mode: WindowMode,
    /// e.g. "1920x1080"; empty = automatic/dynamic
    pub resolution: String,
    pub dynamic_resolution: bool,
    /// How the remote screen is transferred (graphics pipeline / codec).
    pub graphics: RdpGraphics,
    /// 0 = default
    pub color_depth: u8,
    pub clipboard: bool,
    /// Play audio on this computer; otherwise on the remote computer (`sound_remote`) or not at all.
    pub sound: bool,
    pub sound_remote: bool,
    pub microphone: bool,
    pub printers: bool,
    pub smartcards: bool,
    pub home_drive: bool,
    /// Additional drives as "name=/path"
    pub drives: Vec<String>,
    /// Serial and parallel ports, Plug and Play devices (Windows RDP client only).
    pub redirect_ports: bool,
    pub redirect_pnp: bool,
    pub console: bool,
    /// Server authentication: connect without warning (`ignore_certificate`), refuse a
    /// certificate that cannot be verified (`refuse_unverified`), or warn (neither).
    pub ignore_certificate: bool,
    pub refuse_unverified: bool,
    /// Without Network Level Authentication (CredSSP).
    pub disable_nla: bool,
    /// Experience: what the remote desktop shows (performance flags).
    pub wallpaper: bool,
    pub full_window_drag: bool,
    pub menu_animations: bool,
    pub themes: bool,
    pub font_smoothing: bool,
    pub bitmap_caching: bool,
    /// Let client and server detect the connection quality.
    pub auto_detect: bool,
    pub disable_compression: bool,
    /// Where Windows key combinations such as Alt+Tab apply.
    pub shortcuts: RdpShortcuts,
    /// `None` (connections from before this option): use `gateway` when one is entered.
    pub gateway_mode: Option<GatewayMode>,
    pub gateway: String,
    pub gateway_bypass_local: bool,
    /// Own gateway credentials; otherwise `gateway_user`/`gateway_domain` or the connection's.
    pub gateway_credential: Option<Uuid>,
    pub gateway_user: String,
    pub gateway_domain: String,
    /// Filled from `gateway_credential` just before a connection starts; never stored.
    #[serde(skip)]
    pub gateway_password: String,
    pub keyboard_layout: String,
    pub remote_app: String,
    /// Program started instead of the desktop shell, and its working directory.
    pub start_program: String,
    pub start_dir: String,
    pub extra_args: String,
}

/// Where Windows key combinations apply (`KeyboardHookMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RdpShortcuts {
    Local,
    #[default]
    Remote,
    FullScreen,
}

impl RdpShortcuts {
    pub const ALL: [RdpShortcuts; 3] = [RdpShortcuts::Local, RdpShortcuts::Remote, RdpShortcuts::FullScreen];

    pub fn label(self) -> &'static str {
        match self {
            RdpShortcuts::Local => "On this computer",
            RdpShortcuts::Remote => "On the remote computer",
            RdpShortcuts::FullScreen => "Only in full screen",
        }
    }
}

/// RD Gateway use, as in the original: not at all, detected automatically, or this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayMode {
    Off,
    Auto,
    Manual,
}

impl RdpOptions {
    pub fn gateway_mode(&self) -> GatewayMode {
        self.gateway_mode.unwrap_or(if self.gateway.trim().is_empty() { GatewayMode::Off } else { GatewayMode::Manual })
    }

    /// The gateway server to use, if any.
    pub fn gateway_server(&self) -> Option<&str> {
        Some(self.gateway.trim()).filter(|g| self.gateway_mode() == GatewayMode::Manual && !g.is_empty())
    }
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            window_mode: WindowMode::Tab,
            resolution: String::new(),
            dynamic_resolution: true,
            graphics: RdpGraphics::Auto,
            color_depth: 0,
            clipboard: true,
            sound: true,
            sound_remote: false,
            microphone: false,
            printers: false,
            smartcards: false,
            home_drive: false,
            drives: Vec::new(),
            redirect_ports: false,
            redirect_pnp: false,
            console: false,
            ignore_certificate: false,
            refuse_unverified: false,
            disable_nla: false,
            wallpaper: true,
            full_window_drag: false,
            menu_animations: false,
            themes: true,
            font_smoothing: true,
            bitmap_caching: false,
            auto_detect: true,
            disable_compression: false,
            shortcuts: RdpShortcuts::Remote,
            gateway_mode: None,
            gateway: String::new(),
            gateway_bypass_local: false,
            gateway_credential: None,
            gateway_user: String::new(),
            gateway_domain: String::new(),
            gateway_password: String::new(),
            keyboard_layout: String::new(),
            remote_app: String::new(),
            start_program: String::new(),
            start_dir: String::new(),
            extra_args: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshOptions {
    pub x11_forwarding: bool,
    pub compression: bool,
    pub identity_file: String,
    pub remote_command: String,
    pub extra_args: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VncOptions {
    pub view_only: bool,
    pub fullscreen: bool,
    pub shared: bool,
    pub extra_args: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalOptions {
    /// Command line with placeholders {T} {N} {U} {D} {P} {PORT}
    pub command: String,
    pub working_dir: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolOptions {
    pub rdp: RdpOptions,
    pub ssh: SshOptions,
    pub vnc: VncOptions,
    pub external: ExternalOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub sorting: i64,
    pub expanded: bool,
    /// Credentials used by connections (and sub-folders) that inherit. `Inherit` takes them from
    /// the parent folder, so nested folders can share one set; `None` stops the inheritance here.
    pub credential: CredentialRef,
    /// Settings profiles (at most one per protocol) for connections in this folder and its
    /// sub-folders that inherit their settings.
    #[serde(default)]
    pub profiles: Vec<Uuid>,
}

impl Folder {
    pub fn new(name: &str, parent: Option<Uuid>) -> Self {
        let credential = if parent.is_some() { CredentialRef::Inherit } else { CredentialRef::None };
        Self { id: Uuid::new_v4(), parent, name: name.into(), description: String::new(), sorting: 0, expanded: true, credential, profiles: Vec::new() }
    }
}

/// Named settings for the connections of one protocol – the original's group settings. A
/// connection uses its own settings, a profile, or the profile its folder (or the nearest parent
/// folder with one for its protocol) has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsProfile {
    pub id: Uuid,
    pub name: String,
    pub protocol: Protocol,
    pub options: ProtocolOptions,
}

impl SettingsProfile {
    /// The protocols that have settings worth sharing.
    pub const PROTOCOLS: [Protocol; 3] = [Protocol::Rdp, Protocol::Ssh, Protocol::Vnc];

    pub fn new(name: &str, protocol: Protocol) -> Self {
        Self { id: Uuid::new_v4(), name: name.into(), protocol, options: ProtocolOptions::default() }
    }
}

/// Where a connection's protocol settings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SettingsRef {
    /// Its own `options`.
    #[default]
    Own,
    /// The profile of its folder or the nearest parent folder (its own options without one).
    Inherit,
    Profile(Uuid),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    /// 0 = default port of the protocol
    pub port: u16,
    pub description: String,
    pub credential: CredentialRef,
    /// Where the protocol settings come from: `options`, a profile, or the folders.
    #[serde(default)]
    pub settings: SettingsRef,
    pub options: ProtocolOptions,
    pub mac: String,
    pub sorting: i64,
    pub last_connected: Option<String>,
}

impl Connection {
    pub fn new(name: &str, protocol: Protocol, parent: Option<Uuid>) -> Self {
        Self {
            id: Uuid::new_v4(),
            parent,
            name: name.into(),
            protocol,
            host: String::new(),
            port: 0,
            description: String::new(),
            credential: if parent.is_some() { CredentialRef::Inherit } else { CredentialRef::None },
            settings: if parent.is_some() { SettingsRef::Inherit } else { SettingsRef::Own },
            options: ProtocolOptions::default(),
            mac: String::new(),
            sorting: 0,
            last_connected: None,
        }
    }

    pub fn effective_port(&self) -> u16 {
        if self.port == 0 {
            self.protocol.default_port()
        } else {
            self.port
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub username: String,
    pub domain: String,
    pub password: String,
}

impl Credential {
    pub fn new(name: &str) -> Self {
        Self { id: Uuid::new_v4(), name: name.into(), ..Default::default() }
    }

    pub fn display(&self) -> String {
        if self.domain.is_empty() {
            format!("{} ({})", self.name, self.username)
        } else {
            format!("{} ({}\\{})", self.name, self.domain, self.username)
        }
    }
}
