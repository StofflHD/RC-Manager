#!/bin/bash
# Packs RC Manager for Windows (run by CI in Git Bash on a Windows runner, after
# `cargo build --release`): a folder with rc-manager.exe and the clients, the portable ZIP and
# the installer (Inno Setup).
#
#   CLIENTS=<folder with wfreerdp.exe putty.exe vncviewer.exe and their licences>
#   packaging/windows/package.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
CLIENTS="${CLIENTS:?CLIENTS must name the folder with the bundled clients}"
NAME="RC-Manager-$VERSION-windows-x64"
OUT="$ROOT/dist/$NAME"

rm -rf "$OUT" "$ROOT/dist/$NAME-portable.zip" "$ROOT/dist/$NAME-setup.exe"
mkdir -p "$OUT/clients" "$OUT/licenses"
cp "$ROOT/target/release/rc-manager.exe" "$OUT/"
for exe in wfreerdp.exe putty.exe vncviewer.exe; do
    cp "$CLIENTS/$exe" "$OUT/clients/"
done
cp "$ROOT/LICENSE" "$OUT/LICENSE.txt"
cp "$CLIENTS"/licenses/* "$OUT/licenses/"
cat > "$OUT/README.txt" <<EOF
RC Manager $VERSION for Windows (x64)
https://github.com/StofflHD/RC-Manager

rc-manager.exe     the program
clients\\           bundled clients: FreeRDP (wfreerdp.exe), PuTTY (putty.exe), TigerVNC viewer
                   (vncviewer.exe) - used for RDP, SSH and VNC sessions in tabs
licenses\\          licences of the bundled clients (FreeRDP: Apache-2.0, OpenH264 in
                   wfreerdp.exe: BSD-2-Clause, PuTTY: MIT, TigerVNC: GPL-2.0,
                   source: https://github.com/TigerVNC/tigervnc)
LICENSE.txt        RC Manager: GNU General Public License v3.0 or later

Portable ZIP: the file rc-manager.portable keeps all data in the "data" folder next to
rc-manager.exe. Without it RC Manager stores its data in %APPDATA%\\rc-manager.
EOF

# Portable ZIP: files at the top level (the updater expects rc-manager.exe there).
STAGE="$ROOT/dist/portable-stage"
rm -rf "$STAGE"
cp -r "$OUT" "$STAGE"
: > "$STAGE/rc-manager.portable"
powershell.exe -NoProfile -Command "Compress-Archive -Path '$(cygpath -w "$STAGE")\\*' -DestinationPath '$(cygpath -w "$ROOT/dist/$NAME-portable.zip")' -Force"
rm -rf "$STAGE"

# Installer
ISCC="${ISCC:-/c/Program Files (x86)/Inno Setup 6/ISCC.exe}"
"$ISCC" //Q "//DAppVersion=$VERSION" "//DSrcDir=$(cygpath -w "$OUT")" "$(cygpath -w "$ROOT/packaging/windows/rc-manager.iss")"

ls -la "$ROOT/dist"
