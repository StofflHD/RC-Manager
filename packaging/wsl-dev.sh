#!/bin/bash
# Local development on Linux or WSL: build, unit tests, a test AppImage and the GUI test –
# in about a minute instead of a full CI run.
#
#   packaging/wsl-dev.sh test        cargo test
#   packaging/wsl-dev.sh build       quick build (profile "fast"), put into the AppDir (+ AppRun);
#                                    PROFILE=release for the full release optimisation
#   packaging/wsl-dev.sh gui [OUT]   GUI test on virtual X servers (Xvfb), screenshots to OUT
#   packaging/wsl-dev.sh run         start the built RC Manager on the desktop (WSLg)
#   packaging/wsl-dev.sh appimage    pack the AppDir into dist/RC-Manager-<version>-local-x86_64.AppImage
#   packaging/wsl-dev.sh all         test + build + gui
#
# The bundled clients (FreeRDP, TigerVNC, xterm, sshpass) come from the newest AppImage in
# dist/ (built by CI); only rc-manager itself is rebuilt here. A binary built on a new
# distribution needs its glibc: local AppImages are for testing, releases come from CI.
#
# Needs: build-essential pkg-config curl git squashfs-tools file, Rust (rustup), and for the
# GUI test xvfb imagemagick xdotool freerdp3-shadow-x11 x11-apps.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/rc-manager-dev"
# The target directory stays on the Linux file system: much faster than /mnt/… under WSL.
export CARGO_TARGET_DIR="$CACHE/target"
PROFILE="${PROFILE:-fast}"
APPDIR="$CACHE/AppDir"
PATH="$HOME/.cargo/bin:$PATH"
mkdir -p "$CACHE"

version() { sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1; }

appdir() {
    # Unpack the newest CI AppImage once (or again when a newer one appears in dist/).
    local img
    img="$(ls -t "$ROOT"/dist/RC-Manager-*-x86_64.AppImage 2>/dev/null | grep -v -- -local- | head -1 || true)"
    [ -n "$img" ] || { echo "No AppImage in dist/ – download one from the GitHub releases." >&2; exit 1; }
    if [ ! -d "$APPDIR" ] || [ "$img" -nt "$APPDIR/.from" ]; then
        echo "==> Unpacking $(basename "$img") (bundled clients)"
        rm -rf "$APPDIR" "$CACHE/squashfs-root"
        (cd "$CACHE" && "$img" --appimage-extract >/dev/null)
        mv "$CACHE/squashfs-root" "$APPDIR"
        touch "$APPDIR/.from"
    fi
}

cmd_test() {
    echo "==> cargo test"
    (cd "$ROOT" && cargo test --locked)
}

cmd_build() {
    echo "==> cargo build --profile $PROFILE"
    (cd "$ROOT" && cargo build --profile "$PROFILE" --locked)
    appdir
    install -m 755 "$CARGO_TARGET_DIR/$PROFILE/rc-manager" "$APPDIR/usr/bin/rc-manager"
    install -m 755 "$ROOT/packaging/AppRun" "$APPDIR/AppRun"
    echo "==> $APPDIR/AppRun (rc-manager $(version))"
}

cmd_run() {
    [ -x "$APPDIR/AppRun" ] || cmd_build
    exec "$APPDIR/AppRun" "$@"
}

cmd_appimage() {
    [ -x "$APPDIR/AppRun" ] || cmd_build
    local tool="$CACHE/appimagetool-x86_64.AppImage"
    if [ ! -x "$tool" ]; then
        curl -fsSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
        chmod +x "$tool"
    fi
    local out="$ROOT/dist/RC-Manager-$(version)-local-x86_64.AppImage"
    ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$APPDIR" "$out" >/dev/null
    echo "==> $out"
}

# Screenshots of virtual X servers only (:98 remote desktop, :99 RC Manager) – the real
# desktop is never touched.
cmd_gui() {
    local out="${1:-$ROOT/target/gui-local}"
    [ -x "$APPDIR/AppRun" ] || cmd_build
    for t in Xvfb xdotool import freerdp-shadow-cli3 xclock; do
        command -v "$t" >/dev/null || { echo "missing: $t (sudo apt install xvfb imagemagick xdotool freerdp3-shadow-x11 x11-apps)" >&2; exit 1; }
    done
    rm -rf "$out"; mkdir -p "$out"
    # Global, so that the EXIT trap still sees them after the function returned.
    WORK="$(mktemp -d)"
    PIDS=()
    cleanup() { kill "${PIDS[@]}" 2>/dev/null || true; rm -rf "$WORK"; }
    trap cleanup EXIT
    unset WAYLAND_DISPLAY
    Xvfb :98 -screen 0 1280x800x24 >/dev/null 2>&1 & PIDS+=($!)
    Xvfb :99 -screen 0 1920x1200x24 >/dev/null 2>&1 & PIDS+=($!)
    sleep 2
    DISPLAY=:98 xclock -geometry 300x300+60+60 -update 1 >/dev/null 2>&1 & PIDS+=($!)
    # The shadow server keeps its certificate in its own config directory.
    XDG_CONFIG_HOME="$WORK/shadow1" DISPLAY=:98 freerdp-shadow-cli3 /port:3390 -auth > "$out/shadow.log" 2>&1 &
    local shadow=$!; PIDS+=($shadow)
    sleep 3
    export DISPLAY=:99
    local app="$APPDIR/AppRun" data="$WORK/data"
    "$app" --data-dir "$data" --import "$ROOT/tests/fixtures/gui-test.db3" | tail -1

    echo "==> 0/1: certificate dialog, Trust always, session in the tab"
    "$app" --data-dir "$data" --connect DC01 > "$out/app-rdp.log" 2>&1 & local a=$!
    sleep 10
    import -window root "$out/0-certificate-dialog.png"
    xdotool mousemove 421 511 click 1      # "Trust always" (window 1150x780)
    sleep 8
    import -window root "$out/1-rdp-tab.png"
    echo "==> 2: window resized, the session follows"
    xdotool windowsize "$(xdotool search --name '^RC Manager$' | head -1)" 1700 1100
    sleep 5
    import -window root "$out/2-rdp-resized.png"
    kill $a; sleep 2

    echo "==> 3: reconnect without a question (trusted fingerprint)"
    "$app" --data-dir "$data" --connect DC01 > "$out/app-rdp2.log" 2>&1 & a=$!
    sleep 10
    import -window root "$out/3-reconnect.png"
    kill $a; sleep 2

    echo "==> 4: new server certificate – warning"
    kill $shadow; sleep 2
    XDG_CONFIG_HOME="$WORK/shadow2" DISPLAY=:98 freerdp-shadow-cli3 /port:3390 -auth > "$out/shadow2.log" 2>&1 &
    PIDS+=($!)
    sleep 3
    "$app" --data-dir "$data" --connect DC01 > "$out/app-rdp3.log" 2>&1 & a=$!
    sleep 10
    import -window root "$out/4-changed-certificate.png"
    kill $a; sleep 2

    echo "==> 5/6: context menu of VNC-Test – Properties, Check Reachability"
    "$app" --data-dir "$data" > "$out/app-menu.log" 2>&1 & a=$!
    sleep 6
    xdotool mousemove 55 232 click 3; sleep 1        # right-click "VNC-Test"
    xdotool mousemove 107 267 click 1; sleep 2       # "Properties …"
    import -window root "$out/5-properties.png"
    # Escape closes it (Xvfb has no window manager: give the window the keyboard focus first).
    local wid; wid="$(xdotool search --name '^RC Manager$' | head -1)"
    xdotool windowfocus "$wid"; xdotool key --window "$wid" Escape; sleep 1
    import -window root "$out/5b-properties-closed.png"
    xdotool mousemove 55 232 click 3; sleep 1
    xdotool mousemove 114 309 click 1; sleep 7       # "Check Reachability"
    import -window root "$out/6-reachability.png"
    kill $a; sleep 1
    echo "==> Screenshots: $out"
    ls "$out"
}

case "${1:-all}" in
    test) cmd_test ;;
    build) cmd_build ;;
    gui) shift; cmd_gui "$@" ;;
    run) shift; cmd_run "$@" ;;
    appimage) cmd_appimage ;;
    all) cmd_test; cmd_build; cmd_gui ;;
    *) sed -n '2,12p' "$0"; exit 1 ;;
esac
