#!/usr/bin/env bash
# End-to-end live gate for the `compme` integration binary.
#
# Drives the *product* binary through the whole pipeline against a real TextEdit
# document with a deterministic stub completion, so the gate is reproducible:
#   focus -> AX read -> infer (stub) -> show ghost -> accept -> insert.
#   Accept binding (Cotypist-parity): grave/backtick (key code 50) = full accept,
#   Tab (key code 48) = next-word accept.
#
# Pass = the stub text ends up in the document AND the binary logged each stage.
# A separate manual run (omit COMPME_E2E_STUB / set a real model) exercises
# the same path with the real LlamaModel; that asserts an insert occurred but not
# exact text, since real output is nondeterministic.
#
# Requires: macOS, osascript, Accessibility granted to the terminal, an unlocked
# session, and the TextEdit pid in COMPME_ACCEPTANCE_PID. Production accept
# keys use Carbon hotkeys and do not require Input Monitoring.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPME_BIN:-$ROOT_DIR/target/debug/compme}"
PID="${COMPME_ACCEPTANCE_PID:-}"
RUN_MS="${COMPME_E2E_RUN_MS:-5000}"
WARMUP_MS="${COMPME_E2E_WARMUP_MS:-1200}"
TAB_AFTER_MS="${COMPME_E2E_TAB_AFTER_MS:-1800}"
SECOND_TAB_AFTER_MS="${COMPME_E2E_SECOND_TAB_AFTER_MS:-700}"
PREFIX="${COMPME_E2E_PREFIX:-The quick brown fox }"
STUB="${COMPME_E2E_STUB:- jumps-$(date +%H%M%S)}"
ACCEPT_MODE="${COMPME_E2E_ACCEPT:-full}"
LOG="${COMPME_E2E_LOG:-$ROOT_DIR/tools/acceptance/logs/e2e-compme-$(date +%Y%m%d-%H%M%S).log}"

fail() {
  echo "E2E FAIL: $*" >&2
  exit 1
}

[ -x "$BIN" ] || fail "binary not built: $BIN (run: cargo build -p app)"
[ -n "$PID" ] || fail "set COMPME_ACCEPTANCE_PID to the TextEdit pid"
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

case "$ACCEPT_MODE" in
  full|word) ;;
  *) fail "COMPME_E2E_ACCEPT must be full or word" ;;
esac

if [ "$ACCEPT_MODE" = "word" ] && [ "${COMPME_E2E_STUB+x}" != "x" ]; then
  STUB=" jumps over"
fi

echo "E2E compme: prefix=\"$PREFIX\" stub=\"$STUB\" pid=$PID run_ms=$RUN_MS accept=$ACCEPT_MODE"

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
COMPME_ACCEPTANCE_PID="$PID" \
  COMPME_STUB_COMPLETION="$STUB" \
  COMPME_RUN_MS="$RUN_MS" \
  "$BIN" >"$LOG" 2>&1 &
APP_PID=$!

# 3. After warm-up, move the caret to end-of-line so a selection-changed
#    notification fires and the binary reads context + requests a completion.
sleep_ms "$WARMUP_MS"
osascript -e 'tell application "System Events" to key code 119' >/dev/null 2>&1 # End

# 4. Give the ghost time to appear, then accept it. Cotypist-parity binding:
#    Tab (key code 48) = accept next word, grave/backtick (key code 50) = accept
#    full. Word mode accepts the first word with Tab, then the remainder with grave.
sleep_ms "$TAB_AFTER_MS"
if [ "$ACCEPT_MODE" = "word" ]; then
  osascript -e 'tell application "System Events" to key code 48' >/dev/null 2>&1 # Tab = next word
  sleep_ms "$SECOND_TAB_AFTER_MS"
  osascript -e 'tell application "System Events" to key code 50' >/dev/null 2>&1 # grave = full (remaining)
else
  osascript -e 'tell application "System Events" to key code 50' >/dev/null 2>&1 # grave = full
fi

# 5. Wait for the bounded run to finish on its own (COMPME_RUN_MS).
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

stages="focus
request gen=
completion gen="
if [ "$ACCEPT_MODE" = "word" ]; then
  stages="${stages}
accept Word
accept Full"
else
  stages="${stages}
accept Full"
fi

while IFS= read -r stage; do
  [ -n "$stage" ] || continue
  if grep -q "$stage" "$LOG"; then
    echo "E2E: stage present: '$stage' [PASS]"
  else
    echo "E2E: stage missing: '$stage' [FAIL]"
    ok=0
  fi
done <<EOF
$stages
EOF

[ "$ok" -eq 1 ] || fail "pipeline assertions failed (see log above)"
echo "E2E PASS: $ACCEPT_MODE focus->read->infer->ghost->accept->insert pipeline"
