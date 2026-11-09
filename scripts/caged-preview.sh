#!/usr/bin/env bash
# Caged Wayland preview for Bamdad.
#
# Layout: cage (headless wlroots) -> nested niri -> Bamdad.
# Nothing is rendered into, or controlled in, the caller's live session.
#
#   ./scripts/caged-preview.sh start
#   BAMDAD_PREVIEW_PAGE=1 BAMDAD_PREVIEW_PING=1 ./scripts/caged-preview.sh start
#   ./scripts/caged-preview.sh shot /tmp/bamdad.png 380
#   ./scripts/caged-preview.sh stop
#
# By default the app uses an isolated data dir under /tmp so real profiles are
# never touched. Set BAMDAD_CAGE_DATA to reuse a data dir (the app expects
# <data>/bamdad).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE="${BAMDAD_CAGE_DIR:-/tmp/bamdad-cage}"
RUNTIME="$BASE/runtime"
STATE="$BASE/state"
DATA="${BAMDAD_CAGE_DATA:-$BASE/data}"
BIN="${BAMDAD_CAGE_BIN:-$ROOT/target/debug/bamdad}"
NIRI_CONFIG="$BASE/niri.kdl"
SOCKET_FILE="$STATE/niri_socket"
DISPLAY_FILE="$STATE/display"
APP_PID_FILE="$STATE/app_pid"
CAGE_PID_FILE="$STATE/cage_pid"

mkdir -p "$BASE" "$STATE"

# Prefer cores bundled next to the executable (the release layout). Otherwise
# fall back to the managed cores, so isolated data dirs still find them.
sidecar_dir="$(dirname "$BIN")/bin"
if [[ -z "${BAMDAD_XRAY_BIN:-}" && ! -x "$sidecar_dir/xray" && ! -x "$sidecar_dir/xray.exe" ]]; then
    if [[ -x "$HOME/.local/share/bamdad/bin/xray" ]]; then
        export BAMDAD_XRAY_BIN="$HOME/.local/share/bamdad/bin/xray"
    fi
fi
if [[ -z "${BAMDAD_SINGBOX_BIN:-}" && ! -x "$sidecar_dir/sing-box" && ! -x "$sidecar_dir/sing-box.exe" ]]; then
    if [[ -x "$HOME/.local/share/bamdad/bin/sing-box" ]]; then
        export BAMDAD_SINGBOX_BIN="$HOME/.local/share/bamdad/bin/sing-box"
    fi
fi

stop_session() {
    if [[ -f "$APP_PID_FILE" ]]; then
        local app
        app="$(cat "$APP_PID_FILE")"
        # Stop core children first; a hard-killed app cannot clean them up.
        while read -r child; do
            [[ -n "$child" ]] && kill "$child" 2>/dev/null || true
        done < <(pgrep -P "$app" 2>/dev/null || true)
        kill "$app" 2>/dev/null || true
    fi
    if [[ -f "$CAGE_PID_FILE" ]]; then
        local cage
        cage="$(cat "$CAGE_PID_FILE")"
        kill -- -"$cage" 2>/dev/null || true
    fi
    rm -f "$APP_PID_FILE" "$CAGE_PID_FILE" "$SOCKET_FILE" "$DISPLAY_FILE"
}

niri_cmd() {
    local socket
    socket="$(cat "$SOCKET_FILE")"
    XDG_RUNTIME_DIR="$RUNTIME" NIRI_SOCKET="$socket" niri "$@"
}

start_session() {
    command -v cage >/dev/null || { echo "cage is required" >&2; exit 1; }
    command -v niri >/dev/null || { echo "niri is required" >&2; exit 1; }
    command -v grim >/dev/null || { echo "grim is required" >&2; exit 1; }
    [[ -x "$BIN" ]] || {
        echo "app binary not found: $BIN (run cargo build -p bamdad-app)" >&2
        exit 1
    }

    stop_session
    mkdir -p "$DATA" "$STATE"
    rm -rf "$RUNTIME"
    mkdir -p "$RUNTIME"
    chmod 700 "$RUNTIME"

    cat > "$NIRI_CONFIG" <<'EOF'
hotkey-overlay {
    skip-at-startup
}
EOF

    local env_args=(
        "XDG_RUNTIME_DIR=$RUNTIME"
        "XDG_DATA_HOME=$DATA"
        "WLR_BACKENDS=headless"
        "WLR_HEADLESS_OUTPUTS=1"
        "WLR_RENDERER=pixman"
    )
    for key in BAMDAD_XRAY_BIN BAMDAD_SINGBOX_BIN BAMDAD_PREVIEW_PAGE \
        BAMDAD_PREVIEW_PING BAMDAD_PREVIEW_CONNECT; do
        if [[ -n "${!key:-}" ]]; then
            env_args+=("$key=${!key}")
        fi
    done

    setsid env "${env_args[@]}" cage -- niri --config "$NIRI_CONFIG" -- "$BIN" \
        >"$BASE/session.log" 2>&1 &
    local cage_pid=$!
    echo "$cage_pid" > "$CAGE_PID_FILE"

    local socket=""
    for _ in $(seq 1 100); do
        socket="$(ls "$RUNTIME"/niri.*.sock 2>/dev/null | head -1 || true)"
        [[ -n "$socket" ]] && break
        sleep 0.05
    done
    if [[ -z "$socket" ]]; then
        echo "niri did not start; see $BASE/session.log" >&2
        stop_session
        exit 1
    fi
    echo "$socket" > "$SOCKET_FILE"

    # cage gets wayland-0; nested niri is the other socket.
    local display=""
    for _ in $(seq 1 100); do
        display="$(find "$RUNTIME" -maxdepth 1 -type s -name 'wayland-*' ! -name 'wayland-0' -printf '%f\n' 2>/dev/null | head -1 || true)"
        [[ -n "$display" ]] && break
        sleep 0.05
    done
    echo "${display:-wayland-1}" > "$DISPLAY_FILE"

    local app_pid=""
    for _ in $(seq 1 100); do
        app_pid="$(niri_cmd msg -j windows 2>/dev/null | python3 -c '
import json, sys
try:
    windows = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for window in windows:
    if window.get("title") == "Bamdad" and window.get("pid"):
        print(window["pid"])
        break
' || true)"
        [[ -n "$app_pid" ]] && break
        sleep 0.05
    done
    if [[ -n "$app_pid" ]]; then
        echo "$app_pid" > "$APP_PID_FILE"
    fi
    echo "caged session ready (cage pid $cage_pid${app_pid:+, app pid $app_pid})"
}

window_geometry() {
    # Prints "width height" of the Bamdad tile.
    niri_cmd msg -j windows | python3 -c '
import json, sys
windows = json.load(sys.stdin)
for window in windows:
    if window.get("title") != "Bamdad":
        continue
    layout = window.get("layout") or {}
    size = layout.get("tile_size") or layout.get("window_size")
    if size:
        print(int(size[0]), int(size[1]))
        break
'
}

case "${1:-start}" in
    start)
        start_session
        ;;
    stop)
        stop_session
        echo "caged session stopped"
        ;;
    windows)
        niri_cmd msg windows
        ;;
    resize)
        [[ -n "${2:-}" ]] || { echo "usage: $0 resize WIDTH" >&2; exit 1; }
        niri_cmd msg action set-window-width "$2"
        ;;
    shot)
        out="${2:-/tmp/bamdad-cage.png}"
        width="${3:-}"
        [[ -f "$SOCKET_FILE" ]] || { echo "start the caged session first" >&2; exit 1; }
        if [[ -n "$width" ]]; then
            niri_cmd msg action set-window-width "$width" >/dev/null
            sleep 0.7
        fi
        display="$(cat "$DISPLAY_FILE" 2>/dev/null || echo wayland-1)"
        XDG_RUNTIME_DIR="$RUNTIME" WAYLAND_DISPLAY="$display" grim "$out"
        geometry="$(window_geometry || true)"
        if [[ -n "$geometry" ]] && command -v magick >/dev/null 2>&1; then
            read -r w h <<< "$geometry"
            magick "$out" -crop "${w}x${h}+0+0" +repage "$out"
        fi
        echo "$out"
        ;;
    *)
        echo "usage: $0 {start|stop|windows|resize WIDTH|shot OUT [WIDTH]}" >&2
        exit 1
        ;;
esac
