use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    Rdp,
    Ssh,
    Telnet,
    Vnc,
    Web,
    External,
}

impl Protocol {
    pub const ALL: [Protocol; 6] = [Protocol::Rdp, Protocol::Ssh, Protocol::Telnet, Protocol::Vnc, Protocol::Web, Protocol::External];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Rdp => "RDP",
            Protocol::Ssh => "SSH",
            Protocol::Telnet => "Telnet",
            Protocol::Vnc => "VNC",
            Protocol::Web => "Web",
            Protocol::External => "External Program",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Protocol::Rdp => "rdp",
            Protocol::Ssh => "ssh",
            Protocol::Telnet => "telnet",
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
            Protocol::Telnet => 23,
            Protocol::Vnc => 5900,
            Protocol::Web => 443,
            Protocol::External => 0,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Protocol::Rdp => "🖥",
            Protocol::Ssh | Protocol::Telnet => "⌨",
            Protocol::Vnc => "👁",
            Protocol::Web => "🌐",
            Protocol::External => "⚙",
        }
    }
}

/// Where a connection takes its credentials from (like `CredentialSource` in the original).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CredentialRef {
    #[default]
    None,
    /// Inherit from the parent folder (recursively).
    Inherit,
    Id(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WindowMode {
    #[default]
    Window,
    Fullscreen,
    MultiMonitor,
}

impl WindowMode {
    pub fn label(self) -> &'static str {
        match self {
            WindowMode::Window => "Window",
            WindowMode::Fullscreen => "Full screen",
            WindowMode::MultiMonitor => "Full screen (all monitors)",
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
    /// 0 = default
    pub color_depth: u8,
    pub clipboard: bool,
    pub sound: bool,
    pub microphone: bool,
    pub printers: bool,
    pub smartcards: bool,
    pub home_drive: bool,
    /// Additional drives as "name=/path"
    pub drives: Vec<String>,
    pub console: bool,
    pub ignore_certificate: bool,
    pub gateway: String,
    pub gateway_user: String,
    pub gateway_domain: String,
    pub keyboard_layout: String,
    pub remote_app: String,
    pub extra_args: String,
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            window_mode: WindowMode::Window,
            resolution: String::new(),
            dynamic_resolution: true,
            color_depth: 0,
            clipboard: true,
            sound: true,
            microphone: false,
            printers: false,
            smartcards: false,
            home_drive: false,
            drives: Vec::new(),
            console: false,
            ignore_certificate: false,
            gateway: String::new(),
            gateway_user: String::new(),
            gateway_domain: String::new(),
            keyboard_layout: String::new(),
            remote_app: String::new(),
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

#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub sorting: i64,
    pub expanded: bool,
    /// Credentials inherited by connections with `CredentialRef::Inherit`.
    pub credential: Option<Uuid>,
}

impl Folder {
    pub fn new(name: &str, parent: Option<Uuid>) -> Self {
        Self { id: Uuid::new_v4(), parent, name: name.into(), description: String::new(), sorting: 0, expanded: true, credential: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq, Default)]
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
