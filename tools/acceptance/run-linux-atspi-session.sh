#!/usr/bin/env bash
# Linux AT-SPI2 session harness (ROADMAP Phase 2.7).
#
# Brings up a throwaway accessibility session — Xvfb, a private D-Bus session
# bus, the AT-SPI bus launcher, and at-spi2-registryd — then builds and runs the
# GTK fixture (linux-atspi-fixture.c) and the AT-SPI client probe
# (linux-atspi-probe.c) against it. Pass means this host can exercise the
# capabilities the Linux adapter needs: field text read, caret offset,
# per-character screen extents, and an EditableText insert that reads back.
#
# With `--run-in-session CMD [ARG...]` the probe is skipped and CMD runs instead,
# inside the live session, with DISPLAY, DBUS_SESSION_BUS_ADDRESS,
# XDG_RUNTIME_DIR, COMPME_ATSPI_SESSION_DIR, and COMPME_ATSPI_FIXTURE_LOG
# exported. That is how the accept-key spike and the adapter's own tests run
# against a real accessibility stack without each re-implementing the bring-up.
#
# No desktop environment, display, or root is required, so it runs on a headless
# Linux box or a CI runner. It is a *harness*, not product code: it proves the
# session is usable before Phase 2.1-2.4 code exists to be tested in it, and
# afterwards it is where those tests run.
#
# Exit codes: 0 pass · 1 probe/session/payload failure · 3 host not provisioned
# (missing tool or dev package — printed with what to install).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURE_SRC="${COMPME_ATSPI_FIXTURE_SRC:-$ROOT_DIR/tools/acceptance/linux-atspi-fixture.c}"
PROBE_SRC="${COMPME_ATSPI_PROBE_SRC:-$ROOT_DIR/tools/acceptance/linux-atspi-probe.c}"
KEYTAP_SRC="${COMPME_ATSPI_KEYTAP_SRC:-$ROOT_DIR/tools/acceptance/linux-keytap-spike.c}"
# The accessible names the fixture publishes; overridable so a future test can
# target the text view instead of the entry.
APP_NAME="${COMPME_ATSPI_APP_NAME:-compme-fixture}"
FIELD_NAME="${COMPME_ATSPI_FIELD_NAME:-compme-fixture-entry}"
# Bounded waits: every readiness check polls instead of sleeping a guess, but
# each still needs a ceiling so a broken session fails instead of hanging CI.
WAIT_TRIES="${COMPME_ATSPI_WAIT_TRIES:-100}"
WAIT_SLEEP="${COMPME_ATSPI_WAIT_SLEEP:-0.1}"

fail() {
  echo "atspi-session FAIL: $*" >&2
  exit 1
}

unprovisioned() {
  echo "atspi-session SKIP: $*" >&2
  echo "atspi-session: on NixOS, run this script inside:" >&2
  echo "  nix-shell -p gcc pkg-config gtk3 at-spi2-core glib xvfb dbus --run tools/acceptance/run-linux-atspi-session.sh" >&2
  echo "atspi-session: on Debian/Ubuntu: apt-get install build-essential pkg-config libgtk-3-dev libatspi2.0-dev xvfb dbus-x11" >&2
  exit 3
}

# Inherited accessibility controls decide whether the ATK bridge loads at all.
# NO_AT_BRIDGE=1 is exported by default on some hosts (it is on the NixOS box
# this was developed against), and it makes every GTK app silently skip
# registration — the session comes up perfectly and the probe finds an empty
# desktop. Sanitize rather than trust the caller's environment.
sanitize_a11y_env() {
  unset NO_AT_BRIDGE GTK_MODULES GTK_A11Y AT_SPI_BUS GTK_PATH
}

# Read the display number Xvfb allocated for itself via -displayfd. Picking a
# free number in the shell first looks equivalent but races: the lock file only
# appears once the server starts, so two concurrent runs both "find" :99 free and
# the loser silently shares the winner's display. Letting the X server allocate
# is atomic; this only has to parse what it reports.
parse_display_number() {
  candidate="$(tr -d '[:space:]' <"$1")"
  case "$candidate" in
    "" | *[!0-9]*) return 1 ;;
    *) echo "$candidate" ;;
  esac
}

# Poll `command` until it succeeds. Returns 1 once the bounded attempts are
# exhausted, so callers report which stage never came up.
wait_until() {
  tries="$WAIT_TRIES"
  while [ "$tries" -gt 0 ]; do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$WAIT_SLEEP"
    tries=$((tries - 1))
  done
  return 1
}

require_tools() {
  for tool in "$@"; do
    command -v "$tool" >/dev/null 2>&1 || unprovisioned "missing tool: $tool"
  done
}

# Locate an at-spi2 helper binary. Distributions disagree about where these live
# and neither is on PATH: Nix puts them in `$prefix/libexec/`, Debian/Ubuntu in
# `$prefix/libexec/at-spi2-core/`, and some builds in `$prefix/lib/at-spi2-core/`.
# Deriving one path from the pkg-config prefix therefore works on exactly the
# distribution it was written on — which is how CI caught this. Search the known
# layouts and name every path tried when none matches.
FIND_HELPER_ERROR=""
find_atspi_helper() {
  helper_name="$1"
  prefix="${ATSPI_PREFIX:-/usr}"
  for candidate in \
    ${COMPME_ATSPI_LIBEXEC:+"$COMPME_ATSPI_LIBEXEC/$helper_name"} \
    "$prefix/libexec/$helper_name" \
    "$prefix/libexec/at-spi2-core/$helper_name" \
    "$prefix/lib/at-spi2-core/$helper_name"; do
    if [ -x "$candidate" ]; then
      echo "$candidate"
      return 0
    fi
  done
  # Last resort: some distributions do ship them on PATH.
  if command -v "$helper_name" >/dev/null 2>&1; then
    command -v "$helper_name"
    return 0
  fi
  FIND_HELPER_ERROR="missing at-spi2 helper $helper_name; looked in $prefix/libexec, $prefix/libexec/at-spi2-core, $prefix/lib/at-spi2-core, and PATH"
  return 1
}

require_pkgconfig() {
  for pkg in "$@"; do
    pkg-config --exists "$pkg" || unprovisioned "missing dev package: $pkg"
  done
}

run_self_test() {
  # Hermetic: an inherited COMPME_ATSPI_* control would silently retarget these
  # checks (at another fixture source, or different accessible names), so drop
  # them and re-derive the repo defaults the assertions are written against.
  # One `unset` per line: the release checker's hermetic-self-test contract reads
  # the names off lines beginning with `unset`, so a line continuation would hide
  # everything after the first line from it.
  unset COMPME_ATSPI_FIXTURE_SRC COMPME_ATSPI_PROBE_SRC COMPME_ATSPI_KEYTAP_SRC
  unset COMPME_ATSPI_APP_NAME COMPME_ATSPI_FIELD_NAME
  unset COMPME_ATSPI_WAIT_TRIES COMPME_ATSPI_WAIT_SLEEP COMPME_ATSPI_KEEP
  unset COMPME_ATSPI_LIBEXEC
  FIXTURE_SRC="$ROOT_DIR/tools/acceptance/linux-atspi-fixture.c"
  PROBE_SRC="$ROOT_DIR/tools/acceptance/linux-atspi-probe.c"
  KEYTAP_SRC="$ROOT_DIR/tools/acceptance/linux-keytap-spike.c"
  APP_NAME="compme-fixture"
  FIELD_NAME="compme-fixture-entry"

  tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t compme-atspi-self-test)"
  # shellcheck disable=SC2064 # expand tmp_dir now; it is gone by trap time otherwise
  trap "rm -rf '$tmp_dir'" EXIT
  status=0

  # 1. The env sanitizer must clear every inherited a11y control, because a
  # leftover NO_AT_BRIDGE turns a real failure into an empty-desktop mystery.
  export NO_AT_BRIDGE=1 GTK_MODULES=stale GTK_A11Y=none AT_SPI_BUS=stale GTK_PATH=/stale
  sanitize_a11y_env
  if [ -n "${NO_AT_BRIDGE:-}${GTK_MODULES:-}${GTK_A11Y:-}${AT_SPI_BUS:-}${GTK_PATH:-}" ]; then
    echo "FAIL self-test-atspi-session-env-sanitized" >&2
    status=1
  else
    echo "PASS self-test-atspi-session-env-sanitized"
  fi

  # 2. The display number comes from Xvfb's -displayfd, which writes a bare
  # number plus a newline. Accept that, and refuse anything that would turn into
  # a bogus DISPLAY (empty, partial write, or an error string) instead of
  # exporting ":" and failing later with a confusing X error.
  printf '107\n' >"$tmp_dir/display-ok"
  : >"$tmp_dir/display-empty"
  printf 'Fatal server error\n' >"$tmp_dir/display-garbage"
  picked="$(parse_display_number "$tmp_dir/display-ok")"
  if [ "$picked" = "107" ]; then
    echo "PASS self-test-atspi-session-parses-displayfd"
  else
    echo "FAIL self-test-atspi-session-parses-displayfd: got '$picked', want 107" >&2
    status=1
  fi
  if parse_display_number "$tmp_dir/display-empty" >/dev/null 2>&1 ||
    parse_display_number "$tmp_dir/display-garbage" >/dev/null 2>&1; then
    echo "FAIL self-test-atspi-session-rejects-bad-displayfd" >&2
    status=1
  else
    echo "PASS self-test-atspi-session-rejects-bad-displayfd"
  fi

  # 3. wait_until must give up rather than hang, and must succeed as soon as the
  # condition holds instead of always burning the full budget.
  if COMPME_ATSPI_WAIT_TRIES=3 WAIT_TRIES=3 WAIT_SLEEP=0 wait_until false; then
    echo "FAIL self-test-atspi-session-wait-gives-up" >&2
    status=1
  else
    echo "PASS self-test-atspi-session-wait-gives-up"
  fi
  if WAIT_TRIES=3 WAIT_SLEEP=0 wait_until true; then
    echo "PASS self-test-atspi-session-wait-succeeds"
  else
    echo "FAIL self-test-atspi-session-wait-succeeds" >&2
    status=1
  fi

  # 4. A host without the tools must report the provisioning skip (exit 3), not
  # a failure that reads like a product bug.
  probe_out="$tmp_dir/unprovisioned.out"
  set +e
  (require_tools compme-definitely-not-a-real-tool) >"$probe_out" 2>&1
  rc=$?
  set -e
  if [ "$rc" -eq 3 ] && grep -q 'atspi-session SKIP: missing tool' "$probe_out"; then
    echo "PASS self-test-atspi-session-unprovisioned-host-skips"
  else
    echo "FAIL self-test-atspi-session-unprovisioned-host-skips: rc=$rc" >&2
    cat "$probe_out" >&2
    status=1
  fi

  # 5. Helper lookup must handle every packaging layout, not just the one this
  # was developed on. CI found the Debian layout the hard way.
  for layout in libexec libexec/at-spi2-core lib/at-spi2-core; do
    helper_dir="$tmp_dir/prefix-$(echo "$layout" | tr / -)/$layout"
    mkdir -p "$helper_dir"
    printf '#!/bin/sh\n' >"$helper_dir/at-spi-bus-launcher"
    chmod +x "$helper_dir/at-spi-bus-launcher"
    found="$(ATSPI_PREFIX="$tmp_dir/prefix-$(echo "$layout" | tr / -)" find_atspi_helper at-spi-bus-launcher)"
    if [ "$found" = "$helper_dir/at-spi-bus-launcher" ]; then
      echo "PASS self-test-atspi-session-finds-helper-in-$layout"
    else
      echo "FAIL self-test-atspi-session-finds-helper-in-$layout: got '$found'" >&2
      status=1
    fi
  done
  if ATSPI_PREFIX="$tmp_dir/empty-prefix" find_atspi_helper compme-no-such-helper >/dev/null 2>&1; then
    echo "FAIL self-test-atspi-session-missing-helper-reports-paths" >&2
    status=1
  elif [ -n "$FIND_HELPER_ERROR" ] && case "$FIND_HELPER_ERROR" in
    *at-spi2-core*PATH*) true ;;
    *) false ;;
  esac then
    echo "PASS self-test-atspi-session-missing-helper-reports-paths"
  else
    echo "FAIL self-test-atspi-session-missing-helper-reports-paths: $FIND_HELPER_ERROR" >&2
    status=1
  fi

  # 6. Argument handling: --run-in-session must require a command (an empty
  # payload would otherwise silently fall through to the probe and "pass"), and
  # an unknown flag must be refused rather than ignored.
  set +e
  ("$0" --run-in-session) >"$tmp_dir/no-payload.out" 2>&1
  no_payload_rc=$?
  ("$0" --bogus-flag) >"$tmp_dir/bogus.out" 2>&1
  bogus_rc=$?
  set -e
  if [ "$no_payload_rc" -eq 1 ] && grep -q 'needs a command to run' "$tmp_dir/no-payload.out" &&
    [ "$bogus_rc" -eq 1 ] && grep -q 'unknown argument' "$tmp_dir/bogus.out"; then
    echo "PASS self-test-atspi-session-argument-handling"
  else
    echo "FAIL self-test-atspi-session-argument-handling: rc=$no_payload_rc/$bogus_rc" >&2
    cat "$tmp_dir/no-payload.out" "$tmp_dir/bogus.out" >&2
    status=1
  fi

  # 7. The fixture and probe sources must exist and agree with this script on the
  # accessible names — a rename on one side would otherwise fail only at runtime,
  # on a Linux host, long after the change.
  for src in "$FIXTURE_SRC" "$PROBE_SRC" "$KEYTAP_SRC"; do
    [ -f "$src" ] || { echo "FAIL self-test-atspi-session-sources-present: missing $src" >&2; status=1; }
  done
  if grep -q "\"$APP_NAME\"" "$FIXTURE_SRC" && grep -q "\"$FIELD_NAME\"" "$FIXTURE_SRC" &&
    grep -q "\"$APP_NAME\"" "$PROBE_SRC" && grep -q "\"$FIELD_NAME\"" "$PROBE_SRC"; then
    echo "PASS self-test-atspi-session-names-agree"
  else
    echo "FAIL self-test-atspi-session-names-agree: $APP_NAME/$FIELD_NAME not in both sources" >&2
    status=1
  fi

  [ "$status" -eq 0 ] || exit 1
  echo "atspi-session self-test PASS"
  exit 0
}

payload=()
build_keytap_spike=""
case "${1:-}" in
  --self-test)
    run_self_test
    ;;
  --run-in-session)
    shift
    [ "$#" -gt 0 ] || fail "--run-in-session needs a command to run"
    payload=("$@")
    ;;
  --keytap-spike)
    # Convenience wrapper over --run-in-session: the spike needs x11/xtst link
    # flags, and keeping them here rather than in a documented copy-paste command
    # is what makes the Phase 2.3 experiment reproducible.
    build_keytap_spike=1
    ;;
  "") ;;
  *)
    fail "unknown argument: $1 (use --self-test, --keytap-spike, --run-in-session CMD, or no arguments)"
    ;;
esac

[ "$(uname -s)" = Linux ] || unprovisioned "this harness targets Linux (host is $(uname -s))"
require_tools gcc pkg-config Xvfb dbus-daemon dbus-send
# x11 is needed by the fixture itself: with no window manager under Xvfb it has
# to call XSetInputFocus, or nothing in the accessibility tree is ever focused.
require_pkgconfig gtk+-3.0 atspi-2 x11
[ -z "$build_keytap_spike" ] || require_pkgconfig x11 xtst

ATSPI_PREFIX="$(pkg-config --variable=prefix atspi-2)"
BUS_LAUNCHER="$(find_atspi_helper at-spi-bus-launcher)" ||
  unprovisioned "$FIND_HELPER_ERROR"
REGISTRYD="$(find_atspi_helper at-spi2-registryd)" ||
  unprovisioned "$FIND_HELPER_ERROR"

sanitize_a11y_env
session_dir="$(mktemp -d 2>/dev/null || mktemp -d -t compme-atspi)"
xvfb_pid=""
launcher_pid=""
registryd_pid=""
fixture_pid=""
dbus_pid=""

cleanup() {
  for pid in "$fixture_pid" "$registryd_pid" "$launcher_pid" "$dbus_pid" "$xvfb_pid"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  # Reap so the harness does not exit while the X socket is still held.
  wait 2>/dev/null || true
  if [ -n "${COMPME_ATSPI_KEEP:-}" ]; then
    echo "atspi-session: keeping $session_dir" >&2
  else
    rm -rf "$session_dir"
  fi
}
trap cleanup EXIT

# Compiler flags must word-split, so read them into arrays rather than leaving an
# unquoted command substitution for shellcheck to (rightly) flag.
read -r -a gtk_flags <<<"$(pkg-config --cflags --libs gtk+-3.0 x11)"
# atspi-2 does not pull gobject/glib into its Libs, and this probe calls
# g_object_ref/g_free directly, so link them explicitly.
read -r -a atspi_flags <<<"$(pkg-config --cflags --libs atspi-2 gobject-2.0 glib-2.0)"
gcc -Wall -Wextra -o "$session_dir/fixture" "$FIXTURE_SRC" "${gtk_flags[@]}" ||
  fail "fixture build failed"
gcc -Wall -Wextra -o "$session_dir/probe" "$PROBE_SRC" "${atspi_flags[@]}" ||
  fail "probe build failed"

if [ -n "$build_keytap_spike" ]; then
  read -r -a x11_flags <<<"$(pkg-config --cflags --libs x11 xtst)"
  gcc -Wall -Wextra -o "$session_dir/keytap-spike" "$KEYTAP_SRC" "${x11_flags[@]}" ||
    fail "keytap spike build failed"
  payload=("$session_dir/keytap-spike")
fi

Xvfb -displayfd 3 -screen 0 1280x1024x24 -nolisten tcp \
  3>"$session_dir/display" >"$session_dir/xvfb.log" 2>&1 &
xvfb_pid=$!
wait_until test -s "$session_dir/display" ||
  fail "Xvfb never reported a display (see $session_dir/xvfb.log)"
display_num="$(parse_display_number "$session_dir/display")" ||
  fail "Xvfb reported an unusable display number: $(cat "$session_dir/display")"
export DISPLAY=":$display_num"
wait_until test -e "/tmp/.X11-unix/X$display_num" ||
  fail "Xvfb never created $DISPLAY (see $session_dir/xvfb.log)"

# A private runtime dir keeps the a11y bus socket inside the session directory,
# so concurrent runs and the host's real session never share one.
export XDG_RUNTIME_DIR="$session_dir/run"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export XDG_DATA_DIRS="$ATSPI_PREFIX/share${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"

DBUS_SESSION_BUS_ADDRESS="$(dbus-daemon --session --print-address --fork --print-pid=3 3>"$session_dir/dbus.pid")"
export DBUS_SESSION_BUS_ADDRESS
dbus_pid="$(cat "$session_dir/dbus.pid")"

"$BUS_LAUNCHER" --launch-immediately >"$session_dir/bus-launcher.log" 2>&1 &
launcher_pid=$!
a11y_address() {
  dbus-send --session --print-reply=literal --dest=org.a11y.Bus /org/a11y/bus org.a11y.Bus.GetAddress
}
wait_until a11y_address || fail "the a11y bus never answered (see $session_dir/bus-launcher.log)"

"$REGISTRYD" >"$session_dir/registryd.log" 2>&1 &
registryd_pid=$!
wait_until grep -q 'well-known name' "$session_dir/registryd.log" ||
  fail "at-spi2-registryd never claimed its name (see $session_dir/registryd.log)"

"$session_dir/fixture" >"$session_dir/fixture.log" 2>&1 &
fixture_pid=$!
wait_until grep -q FIXTURE_READY "$session_dir/fixture.log" ||
  fail "the GTK fixture never mapped (see $session_dir/fixture.log)"

export COMPME_ATSPI_SESSION_DIR="$session_dir"
export COMPME_ATSPI_FIXTURE_LOG="$session_dir/fixture.log"

if [ "${#payload[@]}" -gt 0 ]; then
  # The payload owns the verdict; the harness only guarantees the session. Its
  # output is not captured, so a test runner's progress stays live.
  payload_status=0
  "${payload[@]}" || payload_status=$?
  if [ "$payload_status" -ne 0 ]; then
    fail "in-session command exited $payload_status: ${payload[*]}"
  fi
  echo "atspi-session PASS: display=$DISPLAY ran ${payload[*]}"
  exit 0
fi

probe_status=0
"$session_dir/probe" "$APP_NAME" "$FIELD_NAME" >"$session_dir/probe.log" 2>"$session_dir/probe.err" ||
  probe_status=$?
cat "$session_dir/probe.log"
if [ "$probe_status" -ne 0 ]; then
  cat "$session_dir/probe.err" >&2
  fail "probe exited $probe_status"
fi
grep -q PROBE_OK "$session_dir/probe.log" || fail "probe did not report PROBE_OK"

echo "atspi-session PASS: display=$DISPLAY app=$APP_NAME field=$FIELD_NAME"
