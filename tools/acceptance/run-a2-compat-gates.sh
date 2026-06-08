#!/usr/bin/env bash
# A2 §16 compatibility + context live gates for the `complete-me` binary.
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
#   clipboard    works app + COMPLETE_ME_CLIPBOARD_CONTEXT=1; the copied text
#                shows up in the binary's `request`/prompt path (manual eyeball).
#
# This is the executable form of the §16 compatibility-matrix gate. It needs a
# console GUI session, Accessibility + Input Monitoring granted, the relevant
# apps installed/focused, and the target pid in COMPLETE_ME_ACCEPTANCE_PID.
# Per-app coverage is recorded in tools/acceptance/logs/ when run.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPLETE_ME_BIN:-$ROOT_DIR/target/debug/complete-me}"
PID="${COMPLETE_ME_ACCEPTANCE_PID:-}"
KIND="${1:-works}"            # works | unsupported | terminal-cmd | terminal-nlp | clipboard
RUN_MS="${COMPLETE_ME_RUN_MS:-3500}"
WARMUP_MS="${COMPLETE_ME_WARMUP_MS:-1200}"
PREFIX="${COMPLETE_ME_PREFIX:-Dear team, I wanted to }"
STUB="${COMPLETE_ME_STUB:- follow up about the }"
LOG_DIR="$ROOT_DIR/tools/acceptance/logs"
LOG="$LOG_DIR/a2-compat-${KIND}-$(date +%Y%m%d-%H%M%S).log"
mkdir -p "$LOG_DIR"

if [[ -z "$PID" ]]; then
  echo "FAIL: set COMPLETE_ME_ACCEPTANCE_PID to the target app's pid" >&2
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
if [[ "$KIND" == "clipboard" ]]; then
  /usr/bin/osascript -e 'set the clipboard to "CLIPBOARD-CONTEXT-MARKER"' >/dev/null 2>&1 || true
  clip_env=(COMPLETE_ME_CLIPBOARD_CONTEXT=1)
fi

# Seed the field, then run the binary against it with a deterministic stub.
/usr/bin/osascript >/dev/null 2>&1 <<OSA || true
tell application "System Events"
  set frontmost of (first process whose unix id is $PID) to true
end tell
delay 0.4
tell application "System Events" to keystroke "$PREFIX"
OSA

COMPLETE_ME_STUB_COMPLETION="$STUB" \
COMPLETE_ME_ACCEPTANCE_PID="$PID" \
COMPLETE_ME_RUN_MS="$RUN_MS" \
"${clip_env[@]}" \
"$BIN" >"$LOG" 2>&1 &
BIN_PID=$!
sleep "$(awk "BEGIN{print ($WARMUP_MS+$RUN_MS)/1000}")"
wait "$BIN_PID" 2>/dev/null || true

requested=0
grep -q "request gen=" "$LOG" && requested=1

pass() { echo "PASS: $KIND — $1 (log: $LOG)"; exit 0; }
fail() { echo "FAIL: $KIND — $1 (log: $LOG)"; exit 1; }

case "$KIND" in
  works|terminal-nlp|clipboard)
    [[ "$requested" == 1 ]] && pass "completion requested as expected" \
      || fail "expected a completion request, none logged" ;;
  unsupported|terminal-cmd)
    [[ "$requested" == 0 ]] && pass "completion correctly gated out" \
      || fail "expected NO completion request, but one was logged" ;;
  *)
    fail "unknown KIND '$KIND' (works|unsupported|terminal-cmd|terminal-nlp|clipboard)" ;;
esac
