#!/usr/bin/env bash
# headless-screenshot.sh <binary> [args...]
#
# Runs <binary> under a HEADLESS `cage` compositor (wlroots headless backend),
# waits for it to render, captures a PNG via `grim`, then tears everything down.
# It NEVER touches the user's physical monitors or live Wayland session — the
# headless backend has no DRM/output path to real displays.
#
# This is the ONLY sanctioned way to run the tartarust UI during dev.
# Do NOT run `dx serve` / the UI binary bare — that inherits the real Wayland
# session and pops a window on the user's screen.
#
# Env knobs:
#   HEADLESS_OUTPUT   output PNG path        (default: ./screenshots/ui.png)
#   SETTLE_SECS       wait before screenshot (default: 3)
#   CAGE_TIMEOUT      hard kill after N secs (default: 30)
#   HEADLESS_WLR_RENDERER  compositor renderer (default: gles2)
#       gles2 | vulkan = GPU renderer; advertises linux-dmabuf, which the
#       GPUI/blade Vulkan client needs to present. Default since the GPUI
#       migration (docs/GPUI_MIGRATION.md).
#       pixman = pure software, shm clients only — kept for software-Vulkan
#       clients (lavapipe) and the retired webkit/Dioxus path.
#
# Requires: cage, grim. Sources: docs/research/04-dioxus-and-headless.md
set -euo pipefail

BINARY="${1:?Usage: $0 <binary> [args...]}"
shift
BINARY_ARGS=("$@")

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_PNG="${HEADLESS_OUTPUT:-$REPO_ROOT/screenshots/ui.png}"
SETTLE_SECS="${SETTLE_SECS:-3}"
CAGE_TIMEOUT="${CAGE_TIMEOUT:-30}"

mkdir -p "$(dirname "$OUTPUT_PNG")"

# --- wlroots: pure virtual, software-rendered (no GPU/DRM needed) ------------
export WLR_BACKENDS=headless
export WLR_HEADLESS_OUTPUTS=1
export WLR_RENDERER="${HEADLESS_WLR_RENDERER:-gles2}"
export WLR_RENDERER_ALLOW_SOFTWARE=1
export LIBSEAT_BACKEND="${LIBSEAT_BACKEND:-noop}"   # no real seat in headless

# --- Wayland session isolation ----------------------------------------------
# cage picks its own socket via wl_display_add_socket_auto() and overwrites
# WAYLAND_DISPLAY for its children. Unset ours so the child never briefly sees
# the live-session socket. Isolation is guaranteed by WLR_BACKENDS=headless.
unset WAYLAND_DISPLAY || true

# libwayland requires XDG_RUNTIME_DIR to be mode 0700; make a private one.
RUNTIME_DIR="${XDG_RUNTIME_DIR:-}"
CLEAN_RUNTIME=0
if [ -z "$RUNTIME_DIR" ] || [ "$RUNTIME_DIR" = "/tmp" ]; then
    RUNTIME_DIR="$(mktemp -d)"
    chmod 700 "$RUNTIME_DIR"
    CLEAN_RUNTIME=1
fi
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export XDG_SESSION_TYPE=wayland

# --- tartarust UI: keep the harness inert --------------------------------
# The harness runs the UI with NO daemon on purpose; without this gate the UI
# would autostart one, which EVIOCGRABs the real keypad while the user types.
# Desktop-entry/icon install from a throwaway screenshot run is also unwanted.
export TARTARUST_UI_NO_AUTOSTART=1
export TARTARUST_UI_NO_DESKTOP_INSTALL=1

# --- GTK / webkit2gtk: force Wayland + software compositing ------------------
export GDK_BACKEND=wayland
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_SANDBOX=1            # nested compositor has no DRM for sandbox
export LIBGL_ALWAYS_SOFTWARE=1

cleanup() {
    [ "$CLEAN_RUNTIME" = "1" ] && rm -rf "$RUNTIME_DIR" 2>/dev/null || true
}
trap cleanup EXIT

# Inner kiosk command cage runs; cage exits when it returns.
INNER_SCRIPT="$(cat <<'INNER'
set -eu
cleanup() { kill "$(jobs -p)" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

APP_BIN="$1"; shift
OUTPUT_FILE="$1"; shift
SETTLE="$1"; shift

"$APP_BIN" "$@" &
APP_PID=$!

sleep "$SETTLE"
# First-present race: a capture can land before the client's first frame and
# come out solid black. A solid-color 1280x720 PNG compresses to a few KB;
# a real UI frame is far larger — retry while the capture looks empty.
for attempt in 1 2 3 4 5; do
    grim "$OUTPUT_FILE"
    size=$(stat -c%s "$OUTPUT_FILE" 2>/dev/null || echo 0)
    if [ "$size" -ge 10000 ]; then
        break
    fi
    echo "[headless] capture $attempt looks empty (${size}B); retrying…"
    sleep 2
done
echo "[headless] screenshot saved: $OUTPUT_FILE"

kill "$APP_PID" 2>/dev/null || true
wait "$APP_PID" 2>/dev/null || true
INNER
)"

echo "[headless] launching '$BINARY' under headless cage (timeout ${CAGE_TIMEOUT}s)…"
timeout --signal=TERM "$CAGE_TIMEOUT" \
    cage -- bash -c "$INNER_SCRIPT" -- \
        "$BINARY" "$OUTPUT_PNG" "$SETTLE_SECS" "${BINARY_ARGS[@]+"${BINARY_ARGS[@]}"}"

if [ -s "$OUTPUT_PNG" ]; then
    echo "[headless] OK — $(file -b "$OUTPUT_PNG")"
else
    echo "[headless] FAILED — no screenshot produced" >&2
    exit 1
fi
