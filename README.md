# RC Manager

RC Manager (Remote Connection Manager) is a Rust re-implementation of the Windows program **RDP-Manager** (Cinspiration) for Linux
and Windows: a connection manager with a folder tree, stored credentials and quick connect. The
sessions themselves run in established clients – FreeRDP, OpenSSH/xterm or PuTTY, TigerVNC and
your browser – embedded as tabs in RC Manager's window.

Look and feel (the "Rust" colour scheme with light and dark theme, icon style) match
[notepad-rs](https://github.com/StofflHD/notepad-rs) and the
[IP Calculator](https://github.com/StofflHD/IP-Calculator). Switch themes via *Options → Dark Theme*. The connection sidebar collapses to a narrow strip
(« button, *Options → Show Sidebar* or Ctrl+B); hovering the strip shows the sidebar as an
overlay that hides again when the mouse leaves, a click on the strip (or 📌) docks it again.
The log at the bottom can be hidden (▼ button, *Options → Show Log* or Ctrl+L); a status line
then keeps the active sessions and a counter of new warnings visible.

## Features

| Feature | Original (Windows) | RC Manager (Linux and Windows) |
|---|---|---|
| Folder tree, search, sort order | ✔ | ✔ |
| Sessions as tabs, scaled with the window | ✔ | ✔ (Linux: X11/XWayland) |
| RDP | embedded MSTSC ActiveX | FreeRDP 3 drawn into the tab; remote resolution follows the tab or the desktop is scaled |
| Unlock a locked RDP session | ✔ (send password) | ✔ (*Unlock* types the stored password, no reconnect) |
| SSH | embedded PuTTY | Linux: `ssh` in an xterm inside the tab, password via `sshpass`; Windows: PuTTY inside the tab |
| VNC | UltraVNC/TightVNC | TigerVNC `vncviewer`, its window moved into the tab |
| Certificate check | warning dialog (default: none) | dialog with details; trusted fingerprints are remembered |
| Web | embedded Chromium (CEF) | default browser |
| External programs with placeholders | ✔ | ✔ (`{T} {PORT} {N} {U} {D} {P}`) |
| Credentials, inheritance via folders | ✔ (per protocol) | ✔ (one set per folder) |
| Credential manager | list, edit form, delete with confirmation | table with search, explicit save/revert, copy user/password, list of the connections and folders using a set; new credentials can also be created in a connection's properties (*+ New …*) |
| Master / application password | ✔ | ✔ (Argon2id + ChaCha20-Poly1305) |
| Properties dialog in the context menu of an entry | ✔ | ✔ |
| Wake-on-LAN, reachability check | ✔ (`ping -t` window) | ✔ (dialog: `ping` and a TCP check of the port every 2 s) |
| Import of .rdp files | ✔ | ✔ |
| **Import of the Windows database** | – | ✔ including passwords and settings |
| Light / dark theme | – | ✔ |
| Backups (ZIP) of the Windows RDP-Manager | create/restore | import |
| Hyper-V dashboard, key sequences, session/task manager, plugins (AD import, SCCM, MSRA, port scanner) | ✔ | ✘ (Windows-specific or not ported) |

## Sessions in tabs

RDP, SSH and VNC sessions open as tabs next to the connection properties and follow
the window size, like in the original:

* **RDP** – FreeRDP draws into the tab (`/parent-window`). With *Display → Adapt the remote
  resolution to the window size* (default) the remote desktop gets the size of the tab and
  follows every resize (needs Windows 8.1 / Server 2012 R2 or newer); without it, the desktop
  (fixed resolution or the tab size) is scaled into the tab.
* **SSH** – Linux: an xterm inside the tab, coloured like the light or dark theme. Windows:
  PuTTY's window is moved into the tab (Consolas 12, green on black).
* **VNC** – TigerVNC's window is moved into the tab; the remote desktop is resized if the
  server supports it, otherwise shown centred (TigerVNC cannot scale).

Click into a session to type there; a click anywhere in RC Manager takes the keyboard back, and
the session gets it again when RC Manager comes back to the foreground.
Menus, dialogs and the sidebar overlay are cut out of a session (X11 SHAPE, on Windows a window
region), so it stays visible around them; connected systems are marked in the accent colour in
the tree. Close a session with ✖ or a middle click on its tab. *Options → Open Sessions in Tabs*
switches back to separate windows; the window mode of an RDP connection (*Tab*, *Separate
window*, *Full screen*) applies per connection. `rc-manager --connect <name>` opens connections
at start.

**Unlock** (context menu of a connected RDP entry or of its tab) types the stored password and
Enter into the open session – e.g. on its lock screen – without reconnecting. It works like a
keyboard (Windows: `SendInput` with scan codes, Linux: XTEST): a Control key press first wakes
the lock screen, and typing stops as soon as the session loses the keyboard focus, so the
password never ends up in another window. Special characters are typed with the local keyboard
layout.

Embedding uses X11: on Wayland desktops RC Manager runs via XWayland, which GNOME, KDE and
others provide (`RC_MANAGER_WAYLAND=1` forces native Wayland; sessions then open in separate
windows).

### Server certificates

If FreeRDP cannot verify a server certificate (self-signed, wrong name), RC Manager shows its
details – common name, validity, SHA-256 fingerprint – with *Trust once*, *Trust always* and
*Cancel connection*. *Trust always* stores the fingerprint for this host and port in RC
Manager's database; later connections accept exactly this certificate without asking. If the
server presents a different certificate later, the dialog appears again with a prominent
warning. On Linux FreeRDP runs with its own configuration directory below RC Manager's data, so
its certificate store is not used; on Windows the patched `wfreerdp` asks RC Manager instead of
showing its own dialog (see *Installation (Windows)*).

## Installation (AppImage)

Download `RC-Manager-<version>-x86_64.AppImage` from the releases, then:

```bash
chmod +x RC-Manager-*-x86_64.AppImage
./RC-Manager-*-x86_64.AppImage
```

The AppImage contains **FreeRDP 3**, the **TigerVNC** viewer, **xterm** and **sshpass**, so
nothing else needs to be installed – except `ssh` (and a terminal emulator if sessions are opened
in separate windows), which every desktop has. It needs glibc ≥ 2.35 (Ubuntu 22.04, Debian 12, Fedora 36 or newer) and X11 or Wayland.
If a bundled client cannot run on a system, RC Manager uses the installed one instead;
`rc-manager --tools` (or *Options → External Programs*) shows which clients are used.

### Updates

RC Manager checks the GitHub releases at start (at most once a day; *Help → Check for Updates
at Start* turns this off) and on *Help → Check for Updates …*. An update downloads the new
AppImage next to the running one, installs it only if its SHA-256 matches the `.sha256` file of
the release, replaces the old file (a version number in the file name is updated) and offers a
restart. On Windows the installer or portable ZIP is downloaded and verified the same way and
applied when RC Manager exits (see below). Downloads use `curl` or `wget`; *Help → Updater Proxy …* sets a proxy.
`RC_MANAGER_NO_UPDATE=1` disables the automatic check. Without an AppImage (e.g. a self-built
binary) the dialog links to the release page.

## Installation (Windows)

Two packages for Windows 10/11 (x64), both with **FreeRDP 3** (`wfreerdp`), **PuTTY** and the
**TigerVNC** viewer in the folder `clients` next to `rc-manager.exe`:

* `RC-Manager-<version>-windows-x64-setup.exe` – installer with a Start menu entry. It installs
  for the current user without administrator rights (an installation for all users can be
  chosen in the setup).
* `RC-Manager-<version>-windows-x64-portable.zip` – unpack anywhere (e.g. a USB stick) and start
  `rc-manager.exe`. The file `rc-manager.portable` keeps all data and settings in the folder
  `data` next to it.

Sessions open as tabs like on Linux: FreeRDP draws into the tab, PuTTY's and TigerVNC's windows
are moved into it (their title bar and frame lie outside the tab, as in the original). The
installed version stores its data in `%APPDATA%\rc-manager`. FreeRDP's Windows client is built
from source with a small patch (`packaging/windows/patch-freerdp.py`): when RC Manager starts
it, it asks certificate and logon questions on its console instead of in its own message boxes,
so RC Manager's certificate dialog and trusted-fingerprint store apply as on Linux, and its
keyboard hook also works while it is embedded. PuTTY takes its font and colours from its own
saved session `RC-Manager` (in the registry, rewritten at every start); PuTTY's default settings
and your own sessions stay untouched. Without OpenGL 2 (virtual machines, servers, some remote
sessions) RC Manager draws with Direct3D. The updater replaces the installation (silent setup)
or the files of the portable folder once RC Manager has exited; the `data` folder stays
untouched.

## Running without the AppImage

Install the clients you need yourself:

```bash
# Debian/Ubuntu
sudo apt install freerdp3-x11 tigervnc-viewer sshpass
# Fedora
sudo dnf install freerdp tigervnc sshpass
# Arch
sudo pacman -S freerdp tigervnc sshpass
```

File dialogs use the XDG desktop portal (present on GNOME, KDE and most desktops); without it,
`zenity` is used. The plain binary needs glibc ≥ 2.31 and X11 or Wayland.

## Building

```bash
cargo build --release
./target/release/rc-manager
```

AppImage (`dist/RC-Manager-<version>-x86_64.AppImage`):

```bash
packaging/build-appimage.sh                    # RC Manager only
BUNDLE_TOOLS=1 packaging/build-appimage.sh     # with FreeRDP, TigerVNC and sshpass
```

`BUNDLE_TOOLS=1` takes FreeRDP from `FREERDP_PREFIX` (default `/opt/freerdp`) and TigerVNC and
sshpass from the system, and bundles them with their libraries via linuxdeploy. With `ZIGBUILD=1`
RC Manager itself is built via `cargo zigbuild` for glibc 2.31. GitHub Actions
(`.github/workflows/appimage.yml`) builds FreeRDP from source on Ubuntu 22.04, runs the tests,
builds and checks the AppImage on every push; the same workflow builds the Windows packages on
Windows Server 2022 (FreeRDP from source with the patch, PuTTY and TigerVNC downloaded and
checked against SHA-256, `packaging/windows/package.sh` with Inno Setup) and runs a GUI test
there. A tag `v*` publishes everything as a release.

### Local development (Linux or WSL)

`packaging/wsl-dev.sh` builds and tests in about a minute instead of a full CI run. It reuses the
bundled clients of the newest AppImage in `dist/` and only rebuilds RC Manager (profile `fast`:
no LTO, incremental):

```bash
packaging/wsl-dev.sh test       # unit tests
packaging/wsl-dev.sh build      # build into ~/.cache/rc-manager-dev/AppDir
packaging/wsl-dev.sh gui        # GUI test on Xvfb with a local RDP shadow server, screenshots in target/gui-local
packaging/wsl-dev.sh run        # start it (under WSL on the Windows desktop via WSLg)
packaging/wsl-dev.sh appimage   # pack dist/RC-Manager-<version>-local-x86_64.AppImage
```

Local builds need the glibc of the build system; published AppImages come from CI.

Desktop integration without AppImage: copy `packaging/rc-manager.desktop` to
`~/.local/share/applications/` and `packaging/rc-manager.png` to
`~/.local/share/icons/hicolor/256x256/apps/`.

## Migrating from the Windows RDP-Manager

1. In the Windows RDP-Manager create a backup (a ZIP file such as `RDP_Manager_2026.04.22.zip`),
   or copy `SqLiteDatabase.db3` from its data folder (if "do not show passwords" was enabled,
   also copy the `DisableShowPw` file next to it – a backup contains it automatically).
2. In RC Manager use *File → Import Windows RDP-Manager Backup or Database …* and pick the file
   in the file dialog – or on the command line:

```bash
rc-manager --import ~/RDP_Manager_2026.04.22.zip [--app-password SECRET]
```

Imported are folders, connections (RDP, PuTTY → SSH, VNC, web, external programs),
credentials including passwords, as well as port, resolution, colour depth, redirections and
gateway from the settings – also when they are inherited from folders. Deleted entries (recycle
bin) are skipped. From a backup the import dialog shows version, date and comment, and the
layout of the connection tree (auto-hide or docked) is taken over for the sidebar. Backups of
installations that used a database server instead of SQLite cannot be imported.

Passwords stored with "Windows user encryption" (DPAPI) cannot be decrypted on Linux. Switch
the original to AES before exporting.

## Storage and security

* Data: `~/.local/share/rc-manager/rc-manager.db3` on Linux, `%APPDATA%\rc-manager` on Windows,
  the folder `data` next to the program in the portable version (elsewhere: `--data-dir`);
  window settings such as the theme are kept by eframe in `app.ron` there.
  Data of earlier versions (named "RDP Manager", directory `rdp-manager`) is moved there on first start.
* Passwords are encrypted with ChaCha20-Poly1305. Without a master password the key lives in
  `vault.key` (mode 0600); with a master password it is derived with Argon2id and never stored.
* FreeRDP 3 gets passwords via an environment variable (`/args-from:env:`), `sshpass`,
  `vncviewer` and PuTTY (`-pwfile`) via short-lived files with mode 0600 in `$XDG_RUNTIME_DIR`
  (Windows: the user's temp folder). Only FreeRDP 2 and `{P}` in external commands put the
  password into the process list; the application warns about that.

## Security notes and differences to the original

The decompiled original has several weaknesses; this re-implementation handles them as follows:

| Original (Windows) | This version |
|---|---|
| Passwords encrypted with a key built into the program (AES via `PasswordDeriveBytes`, fixed salt, SHA-1); anyone with the database can decrypt them unless an app password is set | Random per-installation key (file with mode 0600) or Argon2id-derived key from the master password; ChaCha20-Poly1305 with a random nonce per value |
| App password stored as unsalted MD5 hash | Only an encrypted verifier is stored; Argon2id makes guessing expensive |
| Settings stored with .NET `BinaryFormatter` – deserialising a manipulated database can execute code | JSON settings; the importer reads BinaryFormatter data with its own parser that never instantiates types and limits depth and size |
| Plugins loaded from the program folder (`Assembly.LoadFile`) | No plugin loading |
| Embedded Chromium 89 (2021) for web connections | Opens the system browser (http/https only) |

Hardening in this version:

* Host names are validated (no leading `-`, no spaces or shell characters) before they are passed
  to ssh, vncviewer, FreeRDP or `{T}`; ssh additionally gets `--` before the host.
* Web connections only open `http://` and `https://` URLs.
* FreeRDP 3 gets its arguments via an environment variable (readable only by the user), so
  stdin stays free for certificate questions; with sshpass, ssh accepts new host keys on first
  use (`StrictHostKeyChecking=accept-new`) but still refuses changed ones.
* Passwords never appear on the command line with FreeRDP 3, sshpass, vncviewer and PuTTY; temporary
  password files are created exclusively (`O_EXCL`, mode 0600) and removed after 20 s or on exit.
* The data directory is mode 0700, the database 0600 (host and user names are stored in plain text).
* Changing the master password is a single transaction; the old local key file is deleted.
* With a master password the app locks itself after 15 minutes of inactivity (configurable in
  *Options → Master Password*) or via *Options → Lock Now* (Ctrl+Shift+L).
* Imports: `.rdp` files cannot switch off certificate checks; imported connections with custom
  command lines are listed so they can be reviewed before use.

Remaining by design: external programs and "extra arguments" run whatever is configured there
(review imported ones); FreeRDP 2 and `{P}` put the password into the process list (the app warns);
decrypted passwords stay in memory while the app is unlocked.

## Layout

```
src/main.rs            start-up, command line
src/app.rs             user interface (egui)
src/theme.rs           colours (light/dark theme), header, logo
src/model.rs           folders, connections, credentials
src/store.rs           SQLite storage
src/vault.rs           password encryption
src/launch.rs          starting FreeRDP, ssh/PuTTY, vncviewer, the browser
src/embed.rs           embedding of sessions into tabs (X11 and Win32), typing for Unlock
src/net.rs             Wake-on-LAN, port check
src/update.rs          updater (GitHub releases, SHA-256 check, replaces the AppImage or the
                       Windows installation)
src/legacy/crypto.rs   .NET PasswordDeriveBytes + AES (passwords of the original)
src/legacy/nrbf.rs     reader for .NET BinaryFormatter blobs (settings of the original)
src/legacy/import.rs   import of the Windows database
src/legacy/rdpfile.rs  import of .rdp files
packaging/             icon (SVG/PNG), .desktop file (rc-manager.*), AppImage and WSL scripts
packaging/windows/     Windows package (Inno Setup, portable ZIP), FreeRDP patch, CI GUI test
build.rs               program icon and version information of rc-manager.exe
```

Tests: `cargo test` (reference values for the crypto and BinaryFormatter code were generated with .NET).

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). The licence is also shown under *Help → About*.

---

© StofflHD and Claude
