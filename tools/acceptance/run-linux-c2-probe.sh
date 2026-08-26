#!/usr/bin/env bash
# C.2 product probe — real typing, deterministic stub completion, ghost shown.
#
# Runs the *product* binary against the live AT-SPI session's GTK fixture and
# asserts the run-loop's usage line reports shown>0. This is the repeatable
# form of the C.2 (`shown=0`) closure probe: focus the fixture entry AFTER the
# app has subscribed (the adapter's focus registry mints fields from focus
# events only, so a field focused before startup is invisible), type real X
# keys with xdotool, let the deterministic stub complete, and require the
# ghost to be shown.
#
# Normal invocation (from `run-linux-atspi-session.sh --c2-probe`, which
# provides the session and the running fixture):
#   COMPME_FONT=/path/to/Font.ttf tools/acceptance/run-linux-c2-probe.sh
#
# Env:
#   COMPME_BIN        product binary (default $ROOT_DIR/target/debug/compme)
#   COMPME_FONT       overlay font path — required; the font scan is env-bound
#   COMPME_STUB_COMPLETION  deterministic completion (default " world")
#   COMPME_C2_TYPED   text typed into the fixture (default " hello ")
#   COMPME_C2_RUN_MS  app lifetime in ms (default 14000)
#   COMPME_C2_LOG     log path override (self-test uses this)
#   COMPME_C2_NO_RUN  1 = skip app launch/typing; evaluate the log only
#
# Exit codes: 0 probe passed (shown>0) · 1 probe failed · 2 usage error.
# Deliberately omit `-e`: assertions accumulate through fail().
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPME_BIN:-$ROOT_DIR/target/debug/compme}"
FONT="${COMPME_FONT:-}"
STUB="${COMPME_STUB_COMPLETION:- world}"
TYPED="${COMPME_C2_TYPED:- hello }"
RUN_MS="${COMPME_C2_RUN_MS:-14000}"
LOG="${COMPME_C2_LOG:-$ROOT_DIR/tools/acceptance/logs/c2-probe-$(date +%Y%m%d-%H%M%S).log}"
NO_RUN="${COMPME_C2_NO_RUN:-0}"

fail() {
  echo "C2 PROBE FAIL: $*" >&2
  exit 1
}

# The last `shown=N` from the app's usage line (empty when absent).
parse_shown() {
  grep -oE 'shown=[0-9]+' "${1:--}" | tail -1 | cut -d= -f2
}

run_probe() {
  [ -x "$BIN" ] || fail "product binary not found at $BIN (build it first: cargo build -p app)"
  [ -n "$FONT" ] || fail "COMPME_FONT must name a usable .ttf/.otf — the overlay font scan is env-bound"
  command -v xdotool >/dev/null 2>&1 || fail "xdotool is required (type real X keys)"

  TMPD="$(mktemp -d "${TMPDIR:-/tmp}/compme-c2-probe.XXXXXX")"
  export TMPDIR="$TMPD"
  export COMPME_CONFIG="$TMPD/config.env"
  export COMPME_STUB_COMPLETION="$STUB"
  export COMPME_DEBUG=1
  export COMPME_RUN_MS="$RUN_MS"
  export COMPME_FONT="$FONT"

  mkdir -p "$(dirname "$LOG")"
  "$BIN" >"$LOG" 2>&1 &
  APP=$!

  # The fixture entry was focused before the app subscribed, so the focus
  # registry is empty and A7's filter drops every caret event. Move GTK focus
  # entry -> textview -> entry (clicks; GTK re-focuses on button press) so a
  # real state-changed:focused event fires for the entry, and WAIT for the
  # app's own focus log line — retrying the pair — so a slow subscription
  # cannot silently skip the event.
  wait_for_running "$LOG"
  sleep 1
  WIN="$(xdotool search --name 'compme AT-SPI fixture' | head -1)"
  [ -n "$WIN" ] || fail "fixture window not found"
  i=0
  while ! grep -q 'compme: focus ' "$LOG" 2>/dev/null; do
    xdotool mousemove --window "$WIN" 40 120 click 1 # textview
    sleep 0.3
    xdotool mousemove --window "$WIN" 40 20 click 1 # entry
    sleep 0.5
    i=$((i + 1))
    [ "$i" -le 12 ] || fail "no focus event delivered after 6 click pairs (log: $LOG)"
  done
  sleep 0.5
  xdotool type --delay 100 "$TYPED"
  # Bounded wait for the app's natural RUN_MS exit, so a wedged app cannot
  # hang the harness.
  while kill -0 "$APP" 2>/dev/null; do
    sleep 0.5
    i=$((i + 1))
    [ "$i" -le $((RUN_MS / 500 + 40)) ] || {
      kill "$APP" 2>/dev/null
      fail "app did not exit after its run window (log: $LOG)"
    }
  done
}

# Bounded wait for the app's `compme: running` line (subscriptions are armed
# after it), so a wedged startup fails with the log, not a hang.
wait_for_running() {
  local i=0
  until grep -q 'compme: running' "$1" 2>/dev/null; do
    sleep 0.5
    i=$((i + 1))
    [ "$i" -le 30 ] || fail "app did not reach 'compme: running' (log: $1)"
  done
}

case "${1:-}" in
  --self-test)
    status=0
    here="$(dirname "${BASH_SOURCE[0]}")"
    fixture_dir="$(mktemp -d "${TMPDIR:-/tmp}/compme-c2-probe-selftest.XXXXXX")"

    # 1. parse_shown decision table.
    zero_log="$fixture_dir/zero.log"
    ok_log="$fixture_dir/ok.log"
    empty_log="$fixture_dir/empty.log"
    printf 'compme: usage shown=0 accepted=0 dismissed=0 superseded=0 words=0\n' >"$zero_log"
    printf 'compme: usage shown=2 accepted=0 dismissed=0 superseded=1 words=0\n' >"$ok_log"
    printf 'compme: running (stub=true)\n' >"$empty_log"
    [ "$(parse_shown "$zero_log")" = 0 ] || { echo "FAIL self-test-c2-probe-parse-shown-zero" >&2; status=1; }
    [ "$(parse_shown "$ok_log")" = 2 ] || { echo "FAIL self-test-c2-probe-parse-shown-ok" >&2; status=1; }
    [ -z "$(parse_shown "$empty_log")" ] || { echo "FAIL self-test-c2-probe-parse-shown-empty" >&2; status=1; }

    # 2. Evaluation paths, hermetically: NO_RUN=1 evaluates the log only.
    set +e
    COMPME_C2_NO_RUN=1 COMPME_C2_LOG="$zero_log" "$0" >"$fixture_dir/zero.out" 2>&1
    zero_rc=$?
    COMPME_C2_NO_RUN=1 COMPME_C2_LOG="$ok_log" "$0" >"$fixture_dir/ok.out" 2>&1
    ok_rc=$?
    COMPME_C2_NO_RUN=1 COMPME_C2_LOG="$empty_log" "$0" >"$fixture_dir/empty.out" 2>&1
    empty_rc=$?
    set -e
    [ "$zero_rc" -eq 1 ] && grep -q 'shown=0' "$fixture_dir/zero.out" \
      || { echo "FAIL self-test-c2-probe-rejects-shown-zero (rc=$zero_rc)" >&2; status=1; }
    [ "$ok_rc" -eq 0 ] || { echo "FAIL self-test-c2-probe-accepts-shown-ok (rc=$ok_rc)" >&2; status=1; }
    [ "$empty_rc" -eq 1 ] && grep -q 'no usage line' "$fixture_dir/empty.out" \
      || { echo "FAIL self-test-c2-probe-rejects-missing-usage (rc=$empty_rc)" >&2; status=1; }

    rm -rf "$fixture_dir"
    [ "$status" -eq 0 ] || exit 1
    echo "c2-probe self-test PASS"
    exit 0
    ;;
  "" | *)
    [ "$#" -eq 0 ] || fail "unknown argument: $1 (use --self-test or no arguments)"
    ;;
esac

if [ "$NO_RUN" != 1 ]; then
  run_probe
fi

shown="$(parse_shown "$LOG")"
[ -n "$shown" ] || fail "no usage line in $LOG (did the app run?)"
[ "$shown" -ge 1 ] || {
  echo "C2 PROBE FAIL: shown=$shown (expected >= 1) — log tail:" >&2
  tail -30 "$LOG" >&2
  exit 1
}
echo "C2 PROBE PASS: shown=$shown (log: $LOG)"
