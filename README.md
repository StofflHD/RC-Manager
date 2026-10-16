# RC Manager

RC Manager (Remote Connection Manager) is a Rust re-implementation of the Windows program **RDP-Manager 4.9.9.3** (Cinspiration) for Linux:
a connection manager with a folder tree, stored credentials and quick connect. The sessions
themselves run in the established Linux clients (FreeRDP, OpenSSH, TigerVNC, your browser).

Look and feel (the "Rust" colour scheme with light and dark theme, icon style) match
[notepad-rs](https://github.com/StofflHD/notepad-rs) and the
[IP Calculator](https://github.com/StofflHD/IP-Calculator). Switch themes via *Options → Dark Theme*. The connection sidebar collapses to a narrow strip
(« button, *Options → Show Sidebar* or Ctrl+B); hovering the strip shows the sidebar as an
overlay that hides again when the mouse leaves, a click on the strip (or 📌) docks it again.
The log at the bottom can be hidden (▼ button, *Options → Show Log* or Ctrl+L); a status line
then keeps the active sessions and a counter of new warnings visible.

## Features

| Feature | Original (Windows) | Linux version |
|---|---|---|
| Folder tree, search, sort order | ✔ | ✔ |
| Sessions as tabs, scaled with the window | ✔ | ✔ (X11/XWayland) |
| RDP | embedded MSTSC ActiveX | FreeRDP 3 drawn into the tab; remote resolution follows the tab or the desktop is scaled |
| SSH / Telnet | embedded PuTTY | `ssh`/`telnet` in an xterm inside the tab, password via `sshpass` |
| VNC | UltraVNC/TightVNC | TigerVNC `vncviewer`, its window moved into the tab |
| Certificate check | warning dialog (default: none) | dialog with details; trusted fingerprints are remembered |
| Web | embedded Chromium (CEF) | default browser (`xdg-open`) |
| External programs with placeholders | ✔ | ✔ (`{T} {PORT} {N} {U} {D} {P}`) |
| Credentials, inheritance via folders | ✔ (per protocol) | ✔ (one set per folder) |
| Master / application password | ✔ | ✔ (Argon2id + ChaCha20-Poly1305) |
| Properties dialog in the context menu of an entry | ✔ | ✔ |
| Wake-on-LAN, reachability check | ✔ (`ping -t` window) | ✔ (dialog: `ping` and a TCP check of the port every 2 s) |
| Import of .rdp files | ✔ | ✔ |
| **Import of the Windows database** | – | ✔ including passwords and settings |
| Light / dark theme | – | ✔ |
| Backups (ZIP) of the Windows RDP-Manager | create/restore | import |
| Hyper-V dashboard, key sequences, session/task manager, plugins (AD import, SCCM, MSRA, port scanner) | ✔ | ✘ (Windows-specific or not ported) |

## Sessions in tabs

RDP, SSH/Telnet and VNC sessions open as tabs next to the connection properties and follow
the window size, like in the original:

* **RDP** – FreeRDP draws into the tab (`/parent-window`). With *Display → Adapt the remote
  resolution to the window size* (default) the remote desktop gets the size of the tab and
  follows every resize (needs Windows 8.1 / Server 2012 R2 or newer); without it, the desktop
  (fixed resolution or the tab size) is scaled into the tab.
* **SSH/Telnet** – an xterm inside the tab, coloured like the light or dark theme.
* **VNC** – TigerVNC's window is moved into the tab; the remote desktop is resized if the
  server supports it, otherwise shown centred (TigerVNC cannot scale).

Click into a session to type there; a click anywhere in RC Manager takes the keyboard back.
Menus, dialogs and the sidebar overlay are cut out of a session (X11 SHAPE), so it stays
visible around them; connected systems are marked in the accent colour in the tree. Close a session with
✖ or a middle click on its tab. *Options → Open Sessions in Tabs* switches back to separate
windows; the window mode of an RDP connection (*Tab*, *Separate window*, *Full screen*) applies
per connection. `rc-manager --connect <name>` opens connections at start.

Embedding uses X11: on Wayland desktops RC Manager runs via XWayland, which GNOME, KDE and
others provide (`RC_MANAGER_WAYLAND=1` forces native Wayland; sessions then open in separate
windows).

### Server certificates

If FreeRDP cannot verify a server certificate (self-signed, wrong name), RC Manager shows its
details – common name, validity, SHA-256 fingerprint – with *Trust once*, *Trust always* and
*Cancel connection*. *Trust always* stores the fingerprint for this host and port in RC
Manager's database; later connections accept exactly this certificate without asking. If the
server presents a different certificate later, the dialog appears again with a prominent
warning. FreeRDP runs with its own configuration directory below RC Manager's data, so its
certificate store is not used.

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
builds and checks the AppImage on every push; a tag `v*` publishes it as a release.

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

Imported are folders, connections (RDP, PuTTY → SSH/Telnet, VNC, web, external programs),
credentials including passwords, as well as port, resolution, colour depth, redirections and
gateway from the settings – also when they are inherited from folders. Deleted entries (recycle
bin) are skipped. From a backup the import dialog shows version, date and comment, and the
layout of the connection tree (auto-hide or docked) is taken over for the sidebar. Backups of
installations that used a database server instead of SQLite cannot be imported.

Passwords stored with "Windows user encryption" (DPAPI) cannot be decrypted on Linux. Switch
the original to AES before exporting.

## Storage and security

* Data: `~/.local/share/rc-manager/rc-manager.db3` (elsewhere: `--data-dir`); window settings
  such as the theme are kept by eframe in `~/.local/share/rc-manager/data/app.ron`.
  Data of earlier versions (named "RDP Manager", directory `rdp-manager`) is moved there on first start.
* Passwords are encrypted with ChaCha20-Poly1305. Without a master password the key lives in
  `vault.key` (mode 0600); with a master password it is derived with Argon2id and never stored.
* FreeRDP 3 gets passwords on stdin (`/args-from:stdin`), `sshpass` and `vncviewer` via
  short-lived files with mode 0600 in `$XDG_RUNTIME_DIR`. Only FreeRDP 2 and `{P}` in external
  commands put the password into the process list; the application warns about that.

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
  to ssh, telnet, vncviewer, FreeRDP or `{T}`; ssh additionally gets `--` before the host.
* Web connections only open `http://` and `https://` URLs.
* FreeRDP 3 gets its arguments via an environment variable (readable only by the user), so
  stdin stays free for certificate questions; with sshpass, ssh accepts new host keys on first
  use (`StrictHostKeyChecking=accept-new`) but still refuses changed ones.
* Passwords never appear on the command line with FreeRDP 3, sshpass and vncviewer; temporary
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
src/launch.rs          starting FreeRDP, ssh, vncviewer, the browser
src/embed.rs           X11 embedding of sessions into tabs
src/net.rs             Wake-on-LAN, port check
src/legacy/crypto.rs   .NET PasswordDeriveBytes + AES (passwords of the original)
src/legacy/nrbf.rs     reader for .NET BinaryFormatter blobs (settings of the original)
src/legacy/import.rs   import of the Windows database
src/legacy/rdpfile.rs  import of .rdp files
packaging/             icon (SVG/PNG) and .desktop file (rc-manager.*)
```

Tests: `cargo test` (reference values for the crypto and BinaryFormatter code were generated with .NET).
