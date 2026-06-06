#!/usr/bin/env bash
# End-to-end live gate for the `complete-me` integration binary.
#
# Drives the *product* binary through the whole pipeline against a real TextEdit
# document with a deterministic stub completion, so the gate is reproducible:
#   focus -> AX read -> infer (stub) -> show ghost -> Tab accept -> insert.
#
# Pass = the stub text ends up in the document AND the binary logged each stage.
# A separate manual run (omit COMPLETE_ME_E2E_STUB / set a real model) exercises
# the same path with the real LlamaModel; that asserts an insert occurred but not
# exact text, since real output is nondeterministic.
#
# Requires: macOS, osascript, Accessibility + Input Monitoring granted to the
# terminal, an unlocked session, and the TextEdit pid in COMPLETE_ME_ACCEPTANCE_PID.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPLETE_ME_BIN:-$ROOT_DIR/target/debug/complete-me}"
PID="${COMPLETE_ME_ACCEPTANCE_PID:-}"
RUN_MS="${COMPLETE_ME_E2E_RUN_MS:-5000}"
WARMUP_MS="${COMPLETE_ME_E2E_WARMUP_MS:-1200}"
TAB_AFTER_MS="${COMPLETE_ME_E2E_TAB_AFTER_MS:-1800}"
PREFIX="${COMPLETE_ME_E2E_PREFIX:-The quick brown fox }"
STUB="${COMPLETE_ME_E2E_STUB:- jumps-$(date +%H%M%S)}"
LOG="${COMPLETE_ME_E2E_LOG:-$ROOT_DIR/tools/acceptance/logs/e2e-complete-me-$(date +%Y%m%d-%H%M%S).log}"

fail() {
  echo "E2E FAIL: $*" >&2
  exit 1
}

[ -x "$BIN" ] || fail "binary not built: $BIN (run: cargo build -p app)"
[ -n "$PID" ] || fail "set COMPLETE_ME_ACCEPTANCE_PID to the TextEdit pid"
command -v osascript >/dev/null 2>&1 || fail "osascript unavailable (macOS only)"

mkdir -p "$(dirname "$LOG")"

sleep_ms() {
  ms="$1"
  case "$ms" in
    ''|*[!0-9]*) return 0 ;;
  esac
  [ "$ms" -gt 0 ] || return 0
  sleep "$(awk "BEGIN { printf \"%.3f\", $ms / 1000 }")"
}

echo "E2E complete-me: prefix=\"$PREFIX\" stub=\"$STUB\" pid=$PID run_ms=$RUN_MS"

# 1. Seed TextEdit with a known prefix and bring it to the front.
osascript <<OSA || fail "could not seed TextEdit"
tell application "TextEdit"
  activate
  if (count of documents) = 0 then make new document
  set text of front document to "$PREFIX"
end tell
OSA

sleep_ms 400

# 2. Launch the product binary against TextEdit with the deterministic stub.
COMPLETE_ME_ACCEPTANCE_PID="$PID" \
  COMPLETE_ME_STUB_COMPLETION="$STUB" \
  COMPLETE_ME_RUN_MS="$RUN_MS" \
  "$BIN" >"$LOG" 2>&1 &
APP_PID=$!

# 3. After warm-up, move the caret to end-of-line so a selection-changed
#    notification fires and the binary reads context + requests a completion.
sleep_ms "$WARMUP_MS"
osascript -e 'tell application "System Events" to key code 119' >/dev/null 2>&1 # End

# 4. Give the ghost time to appear, then press Tab to accept it.
sleep_ms "$TAB_AFTER_MS"
osascript -e 'tell application "System Events" to key code 48' >/dev/null 2>&1 # Tab

# 5. Wait for the bounded run to finish on its own (COMPLETE_ME_RUN_MS).
wait "$APP_PID" 2>/dev/null

# 6. Read the document back and assert.
RESULT="$(osascript -e 'tell application "TextEdit" to get text of front document' 2>/dev/null)"
echo "---- binary log ----"
cat "$LOG"
echo "---- document ----"
echo "$RESULT"
echo "--------------------"

ok=1
case "$RESULT" in
  *"$STUB"*) echo "E2E: stub text inserted into document [PASS]" ;;
  *) echo "E2E: stub text NOT found in document [FAIL]"; ok=0 ;;
esac

for stage in "focus " "request gen=" "completion gen=" "accept Full"; do
  if grep -q "$stage" "$LOG"; then
    echo "E2E: stage present: '$stage' [PASS]"
  else
    echo "E2E: stage missing: '$stage' [FAIL]"
    ok=0
  fi
done

[ "$ok" -eq 1 ] || fail "pipeline assertions failed (see log above)"
echo "E2E PASS: full focus->read->infer->ghost->accept->insert pipeline"
