#!/usr/bin/env bash
# A2 §16 compatibility + context live gates for the `compme` binary.
#
# Verifies the per-app behaviour the deterministic `compat` policies imply, by
# driving the *product* binary against real apps and asserting whether a
# completion request was submitted (the run loop logs `request gen=` only AFTER
# the compat/terminal/prefs gate passes — so its presence/absence is the gate
# signal). Each scenario is `(pid, kind, expect)`:
#
#   works        TextEdit/Notes/Mail ... → expect a `request gen=` line.
#   unsupported  Ghostty/Pages/Warp ...  → expect NO `request gen=` line.
#   terminal-cmd Terminal/iTerm, type a shell command  → NO request.
#   terminal-nlp Terminal/iTerm, type a natural-language prompt → request.
#   clipboard    works app + COMPME_CLIPBOARD_CONTEXT=1; the copied text
#                is logged by the diagnostic context path before submit.
#   screen       works app + COMPME_SCREEN_CONTEXT=1; Screen Recording must
#                be granted and OCR must return context before submit.
#
# This is the executable form of the §16 compatibility-matrix gate. It needs a
# console GUI session, Accessibility granted, the relevant apps installed/focused,
# and the target pid in COMPME_ACCEPTANCE_PID. The `screen` gate also needs
# Screen Recording permission.
# Per-app coverage is recorded in tools/acceptance/logs/ when run.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPME_BIN:-$ROOT_DIR/target/debug/compme}"
PID="${COMPME_ACCEPTANCE_PID:-}"
KIND="${1:-works}"            # works | unsupported | terminal-cmd | terminal-nlp | clipboard | screen
RUN_MS="${COMPME_RUN_MS:-3500}"
WARMUP_MS="${COMPME_WARMUP_MS:-1200}"
PREFIX="${COMPME_PREFIX:-Dear team, I wanted to }"
STUB="${COMPME_STUB:- follow up about the }"
LOG_DIR="$ROOT_DIR/tools/acceptance/logs"
LOG="$LOG_DIR/a2-compat-${KIND}-$(date +%Y%m%d-%H%M%S).log"
mkdir -p "$LOG_DIR"

if [[ -z "$PID" ]]; then
  echo "FAIL: set COMPME_ACCEPTANCE_PID to the target app's pid" >&2
  exit 2
fi
if [[ ! -x "$BIN" ]]; then
  echo "FAIL: build first: cargo build -p app  (missing $BIN)" >&2
  exit 2
fi

# Terminal NL-prompt vs shell-command prefixes drive the terminal heuristic.
case "$KIND" in
  terminal-cmd) PREFIX="git status && ls -la " ;;
  terminal-nlp) PREFIX="please summarize the recent changes in " ;;
esac

clip_env=()
screen_env=()

if [[ "$KIND" == "clipboard" ]]; then
  /usr/bin/osascript -e 'set the clipboard to "CLIPBOARD-CONTEXT-MARKER"' >/dev/null 2>&1 || true
  clip_env=(COMPME_CLIPBOARD_CONTEXT=1 COMPME_DIAG_CONTEXT=1)
fi

if [[ "$KIND" == "screen" ]]; then
  screen_env=(COMPME_SCREEN_CONTEXT=1 COMPME_DIAG_CONTEXT=1)
fi

# Seed the field, then run the binary against it with a deterministic stub.
/usr/bin/osascript >/dev/null 2>&1 <<OSA || true
tell application "System Events"
  set frontmost of (first process whose unix id is $PID) to true
end tell
delay 0.4
tell application "System Events" to keystroke "$PREFIX"
OSA

env \
  COMPME_STUB_COMPLETION="$STUB" \
  COMPME_ACCEPTANCE_PID="$PID" \
  COMPME_RUN_MS="$RUN_MS" \
  ${clip_env[@]+"${clip_env[@]}"} \
  ${screen_env[@]+"${screen_env[@]}"} \
  "$BIN" >"$LOG" 2>&1 &
BIN_PID=$!
sleep "$(awk "BEGIN{print ($WARMUP_MS+$RUN_MS)/1000}")"
wait "$BIN_PID" 2>/dev/null || true

requested=0
grep -q "request gen=" "$LOG" && requested=1

pass() { echo "PASS: $KIND — $1 (log: $LOG)"; exit 0; }
fail() { echo "FAIL: $KIND — $1 (log: $LOG)"; exit 1; }

case "$KIND" in
  works|terminal-nlp)
    [[ "$requested" == 1 ]] && pass "completion requested as expected" \
      || fail "expected a completion request, none logged" ;;
  clipboard)
    [[ "$requested" == 1 ]] || fail "expected a completion request, none logged"
    grep -q 'clipboard_context=Some("CLIPBOARD-CONTEXT-MARKER")' "$LOG" \
      && pass "clipboard context marker reached the submit path" \
      || fail "expected CLIPBOARD-CONTEXT-MARKER in diagnostic clipboard context" ;;
  screen)
    [[ "$requested" == 1 ]] || fail "expected a completion request, none logged"
    grep -Eq 'screen_context=Some\([1-9][0-9]*\)' "$LOG" \
      && pass "screen OCR context reached the submit path" \
      || fail "expected non-empty screen_context diagnostic; check Screen Recording grant and visible text" ;;
  unsupported|terminal-cmd)
    [[ "$requested" == 0 ]] && pass "completion correctly gated out" \
      || fail "expected NO completion request, but one was logged" ;;
  *)
    fail "unknown KIND '$KIND' (works|unsupported|terminal-cmd|terminal-nlp|clipboard|screen)" ;;
esac
