#!/usr/bin/env bash
# Builds dist/RC-Manager-<version>-x86_64.AppImage.
#
# The binary links only against glibc; OpenGL, X11/Wayland and xkbcommon are
# loaded at runtime from the host system, as with any desktop application.
#
# Requirements: Rust toolchain (cargo), curl (only if appimagetool is not cached).
# Optional: cargo-zigbuild + zig to target an older glibc (ZIGBUILD=1, default glibc 2.31),
# so the AppImage also runs on older distributions than the build machine.
# Usage:  packaging/build-appimage.sh      (APPIMAGETOOL=/path/to/tool to skip the download)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
WORK="$ROOT/target/appimage"
APPDIR="$WORK/AppDir"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"

echo "==> RC Manager $VERSION"
if [ "${ZIGBUILD:-0}" = "1" ]; then
    GLIBC="${GLIBC:-2.31}"
    "$CARGO" zigbuild --release --target "x86_64-unknown-linux-gnu.$GLIBC" --manifest-path "$ROOT/Cargo.toml"
    BIN="$ROOT/target/x86_64-unknown-linux-gnu/release/rc-manager"
else
    "$CARGO" build --release --manifest-path "$ROOT/Cargo.toml"
    BIN="$ROOT/target/release/rc-manager"
fi

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

TOOL="${APPIMAGETOOL:-$WORK/appimagetool-x86_64.AppImage}"
if [ ! -x "$TOOL" ]; then
    echo "==> Downloading appimagetool"
    curl -fsSL -o "$TOOL" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$TOOL"
fi

OUT="$DIST/RC-Manager-$VERSION-x86_64.AppImage"
# Build next to the target and rename, so a running copy does not block the build.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT.new"
chmod +x "$OUT.new"
mv -f "$OUT.new" "$OUT"
echo "==> Done: $OUT"
