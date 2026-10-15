#!/usr/bin/env bash
# Builds dist/RC-Manager-<version>-x86_64.AppImage.
#
# RC Manager itself links only against glibc; OpenGL, X11/Wayland and xkbcommon are
# loaded at runtime from the host system, as with any desktop application.
#
# With BUNDLE_TOOLS=1 the clients are shipped inside the AppImage as well:
#   FreeRDP (xfreerdp, from FREERDP_PREFIX, default /opt/freerdp), TigerVNC (vncviewer)
#   and sshpass, each with the libraries they need (via linuxdeploy). The AppImage then
#   requires the glibc of the build machine. RC Manager prefers these bundled clients and
#   falls back to installed ones if a bundled client cannot run.
#
# Requirements: Rust toolchain (cargo), curl (only if appimagetool/linuxdeploy are not cached).
# Optional: cargo-zigbuild + zig to build RC Manager for an older glibc (ZIGBUILD=1, GLIBC=2.31).
# Usage:  packaging/build-appimage.sh      (APPIMAGETOOL=/path, LINUXDEPLOY=/path to skip downloads)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
WORK="$ROOT/target/appimage"
APPDIR="$WORK/AppDir"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
export APPIMAGE_EXTRACT_AND_RUN=1

echo "==> RC Manager $VERSION"
if [ "${ZIGBUILD:-0}" = "1" ]; then
    GLIBC="${GLIBC:-2.31}"
    "$CARGO" zigbuild --release --target "x86_64-unknown-linux-gnu.$GLIBC" --manifest-path "$ROOT/Cargo.toml"
    BIN="$ROOT/target/x86_64-unknown-linux-gnu/release/rc-manager"
else
    "$CARGO" build --release --manifest-path "$ROOT/Cargo.toml"
    BIN="$ROOT/target/release/rc-manager"
fi

fetch() { # fetch <file> <url>
    if [ ! -x "$1" ]; then
        echo "==> Downloading $(basename "$1")"
        curl -fsSL -o "$1" "$2"
        chmod +x "$1"
    fi
}

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" "$APPDIR/usr/share/icons/hicolor/scalable/apps" "$DIST" "$WORK"
install -m 755 "$BIN" "$APPDIR/usr/bin/"
install -m 755 "$ROOT/packaging/AppRun" "$APPDIR/AppRun"
cp "$ROOT/packaging/rc-manager.desktop" "$APPDIR/"
cp "$ROOT/packaging/rc-manager.desktop" "$APPDIR/usr/share/applications/"
cp "$ROOT/packaging/rc-manager.png" "$APPDIR/"
cp "$ROOT/packaging/rc-manager.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/"
cp "$ROOT/packaging/rc-manager.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
ln -sf rc-manager.png "$APPDIR/.DirIcon"

if [ "${BUNDLE_TOOLS:-0}" = "1" ]; then
    FREERDP_PREFIX="${FREERDP_PREFIX:-/opt/freerdp}"
    XFREERDP=""
    for name in xfreerdp3 xfreerdp; do
        if [ -x "$FREERDP_PREFIX/bin/$name" ]; then XFREERDP="$FREERDP_PREFIX/bin/$name"; break; fi
    done
    VNCVIEWER="$(readlink -f "$(command -v vncviewer)")"
    SSHPASS="$(command -v sshpass)"
    for b in "$XFREERDP" "$VNCVIEWER" "$SSHPASS"; do
        [ -x "$b" ] || { echo "missing client: '$b'" >&2; exit 1; }
    done
    echo "==> Bundling $XFREERDP, $VNCVIEWER, $SSHPASS"
    LINUXDEPLOY="${LINUXDEPLOY:-$WORK/linuxdeploy-x86_64.AppImage}"
    fetch "$LINUXDEPLOY" https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
    # linuxdeploy copies each binary to usr/bin, its libraries to usr/lib (except the
    # base system libraries on the AppImage exclude list) and points RPATH at usr/lib.
    LD_LIBRARY_PATH="$FREERDP_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
        "$LINUXDEPLOY" --appdir "$APPDIR" \
        --executable "$XFREERDP" --executable "$VNCVIEWER" --executable "$SSHPASS"
    # linuxdeploy may replace AppRun; ours exports the bundle directory.
    install -m 755 "$ROOT/packaging/AppRun" "$APPDIR/AppRun"
    # Licences of the bundled clients.
    mkdir -p "$APPDIR/usr/share/doc/rc-manager"
    cp "$FREERDP_PREFIX/share/doc/freerdp-LICENSE" "$APPDIR/usr/share/doc/rc-manager/FreeRDP-LICENSE" 2>/dev/null || true
    for pkg in tigervnc-viewer sshpass; do
        [ -f "/usr/share/doc/$pkg/copyright" ] && cp "/usr/share/doc/$pkg/copyright" "$APPDIR/usr/share/doc/rc-manager/$pkg-copyright"
    done
fi

TOOL="${APPIMAGETOOL:-$WORK/appimagetool-x86_64.AppImage}"
fetch "$TOOL" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage

OUT="$DIST/RC-Manager-$VERSION-x86_64.AppImage"
# Build next to the target and rename, so a running copy does not block the build.
ARCH=x86_64 "$TOOL" --no-appstream "$APPDIR" "$OUT.new"
chmod +x "$OUT.new"
mv -f "$OUT.new" "$OUT"
echo "==> Done: $OUT"
