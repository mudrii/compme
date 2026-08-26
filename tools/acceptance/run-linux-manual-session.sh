#!/usr/bin/env bash
# Interactive Linux manual-validation session (docs/MANUAL-VALIDATION-LINUX.md).
#
# Runs INSIDE the AT-SPI session harness:
#
#   tools/acceptance/run-linux-atspi-session.sh --run-in-session \
#     tools/acceptance/run-linux-manual-session.sh
#
# and brings up, on the harness's Xvfb display: x11vnc (localhost only, so a
# Wayland-native VNC viewer can show the sandbox without the desktop ever
# touching X11), gedit as the real target application, and the compme binary in
# deterministic stub-completion mode with an isolated config.
#
# The desktop this exists for forbids XWayland/xwayland-satellite by owner
# policy, so the X11 overlay and accept tap are exercised in this contained
# sandbox instead of the live session. Logs land in $COMPME_MANUAL_OUT
# (default /tmp/compme-manual); touch "$COMPME_MANUAL_OUT/stop" to end the
# session cleanly.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${COMPME_MANUAL_OUT:-/tmp/compme-manual}"
BIN="${COMPME_MANUAL_BIN:-$ROOT_DIR/target/debug/compme}"

unprovisioned() {
  echo "manual-session SKIP: $*" >&2
  echo "manual-session: on NixOS run inside:" >&2
  echo "  nix-shell -p gcc pkg-config gtk3 at-spi2-core glib xorg.xvfb dbus zenity dejavu_fonts x11vnc gedit" >&2
  exit 3
}

command -v x11vnc >/dev/null || unprovisioned "x11vnc not on PATH"
command -v gedit >/dev/null || unprovisioned "gedit not on PATH"
[ -x "$BIN" ] || unprovisioned "compme binary missing at $BIN (cargo build --locked -p app)"
[ -n "${DISPLAY:-}" ] || {
  echo "manual-session FAIL: no DISPLAY — run via run-linux-atspi-session.sh --run-in-session" >&2
  exit 1
}

mkdir -p "$OUT"
rm -f "$OUT/ready" "$OUT/stop"
echo "manual-session: DISPLAY=$DISPLAY out=$OUT" | tee "$OUT/session.info"

# The overlay scans directories, not fontconfig; hand it a concrete face.
FONT="${COMPME_FONT:-$(find /nix/store -maxdepth 6 -name DejaVuSans.ttf -path '*dejavu*' 2>/dev/null | head -1)}"
echo "manual-session: COMPME_FONT=$FONT" | tee -a "$OUT/session.info"

x11vnc -display "$DISPLAY" -localhost -nopw -forever -shared -quiet \
  -o "$OUT/x11vnc.log" -bg
echo "manual-session: x11vnc on localhost:5900 (view: wlvncc localhost 5900)" |
  tee -a "$OUT/session.info"

# --standalone: no single-instance handoff to a gedit outside the sandbox.
gedit --standalone >"$OUT/gedit.log" 2>&1 &
gedit_pid=$!

config_dir="$(mktemp -d "$OUT/config.XXXXXX")"
: >"$config_dir/config.env"

env \
  COMPME_DEBUG=1 \
  COMPME_STUB_COMPLETION="${COMPME_STUB_COMPLETION:- world}" \
  COMPME_CONFIG="$config_dir/config.env" \
  COMPME_FONT="$FONT" \
  "$BIN" >"$OUT/compme.log" 2>&1 &
compme_pid=$!

echo "manual-session: gedit pid $gedit_pid, compme pid $compme_pid" |
  tee -a "$OUT/session.info"
echo "manual-session: READY" | tee -a "$OUT/session.info"
touch "$OUT/ready"

cleanup() {
  kill "$compme_pid" "$gedit_pid" 2>/dev/null || true
}
trap cleanup EXIT

while kill -0 "$compme_pid" 2>/dev/null && [ ! -f "$OUT/stop" ]; do
  sleep 2
done
echo "manual-session: shutting down" | tee -a "$OUT/session.info"
