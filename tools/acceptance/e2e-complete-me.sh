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
# A separate manual run with COMPME_E2E_REAL_MODEL=1 and a real model configured
# exercises the same path with LlamaModel; that asserts the document changed but
# not exact text, since real output is nondeterministic.
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
REAL_MODEL="${COMPME_E2E_REAL_MODEL:-0}"
STUB=""
PROMPT_MARKER="${COMPME_E2E_PROMPT_MARKER:-compme e2e marker $$}"
ACCEPT_MODE="${COMPME_E2E_ACCEPT:-full}"
LOG="${COMPME_E2E_LOG:-$ROOT_DIR/tools/acceptance/logs/e2e-compme-$(date +%Y%m%d-%H%M%S).log}"

fail() {
  echo "E2E FAIL: $*" >&2
  exit 1
}

configure_e2e_mode() {
  case "$ACCEPT_MODE" in
    full|word) ;;
    *) fail "COMPME_E2E_ACCEPT must be full or word" ;;
  esac

  case "$(printf '%s' "$REAL_MODEL" | tr '[:upper:]' '[:lower:]')" in
    1|true|yes|on) REAL_MODEL=1 ;;
    0|false|no|off|'') REAL_MODEL=0 ;;
    *) fail "COMPME_E2E_REAL_MODEL must be 0/1, true/false, yes/no, or on/off" ;;
  esac

  if [ "$REAL_MODEL" -eq 1 ]; then
    if [ "${COMPME_E2E_STUB+x}" = "x" ]; then
      fail "COMPME_E2E_REAL_MODEL=1 cannot be combined with COMPME_E2E_STUB"
    fi
    if [ "${COMPME_STUB_COMPLETION+x}" = "x" ]; then
      fail "COMPME_E2E_REAL_MODEL=1 cannot inherit COMPME_STUB_COMPLETION"
    fi
    STUB=""
    return 0
  fi

  if [ "${COMPME_E2E_STUB+x}" = "x" ]; then
    STUB="$COMPME_E2E_STUB"
  elif [ "$ACCEPT_MODE" = "word" ]; then
    STUB=" jumps over"
  else
    STUB=" jumps-$(date +%H%M%S)"
  fi
}

sleep_ms() {
  ms="$1"
  case "$ms" in
    ''|*[!0-9]*) return 0 ;;
  esac
  [ "$ms" -gt 0 ] || return 0
  sleep "$(awk "BEGIN { printf \"%.3f\", $ms / 1000 }")"
}

wait_for_app_status() {
  app_pid="$1"
  status=0
  wait "$app_pid" 2>/dev/null || status=$?
  WAIT_STATUS="$status"
}

record_app_status() {
  status="$1"
  if [ "$status" -eq 0 ]; then
    echo "E2E: compme exited successfully [PASS]"
    return 0
  fi
  echo "E2E: compme exited with status $status [FAIL]"
  return 1
}

print_evidence_summary() {
  log_file="$1"
  document_text="$2"
  log_lines=0
  log_bytes=0
  if [ -f "$log_file" ]; then
    log_lines="$(wc -l <"$log_file" | tr -d '[:space:]')"
    log_bytes="$(wc -c <"$log_file" | tr -d '[:space:]')"
  fi
  document_chars="$(printf '%s' "$document_text" | wc -m | tr -d '[:space:]')"
  echo "E2E evidence: log=$log_file log_lines=$log_lines log_bytes=$log_bytes document_chars=$document_chars"
}

assert_pipeline_evidence() {
  document_text="$1"
  expected_text="$2"
  log_file="$3"
  accept_mode="$4"
  app_status="$5"
  readback_mode="${6:-stub}"
  ok=1
  if ! record_app_status "$app_status"; then
    ok=0
  fi

  if [ "$readback_mode" = "real" ]; then
    document_chars="$(printf '%s' "$document_text" | wc -m | tr -d '[:space:]')"
    baseline_chars="$(printf '%s' "$expected_text" | wc -m | tr -d '[:space:]')"
    accepted_chars="$(sed -n 's/.*gen=[0-9][0-9]* candidate_lengths=\[\([0-9][0-9]*\).*/\1/p' "$log_file" | tail -n 1)"
    document_delta=$((document_chars - baseline_chars))
    if [ "$document_delta" -le 0 ]; then
      echo "E2E: document did not grow after real-model accept [FAIL]"
      ok=0
    elif [ -z "$accepted_chars" ]; then
      echo "E2E: missing accepted completion length evidence [FAIL]"
      ok=0
    elif [ "$document_delta" -eq "$accepted_chars" ]; then
      echo "E2E: document grew by accepted completion length [PASS]"
    else
      echo "E2E: document growth did not match accepted completion length [FAIL]"
      ok=0
    fi
  else
    if [ -n "$expected_text" ] && [[ "$document_text" == *"$expected_text"* ]]; then
      echo "E2E: stub text inserted into document [PASS]"
    else
      echo "E2E: stub text NOT found in document [FAIL]"
      ok=0
    fi
  fi

  if grep -Eq '^compme: focus( |$)' "$log_file"; then
    echo "E2E: stage present: 'focus' [PASS]"
  else
    echo "E2E: stage missing: 'focus' [FAIL]"
    ok=0
  fi

  request_pattern='^compme: request gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app=com\.apple\.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true$'
  request_line="$(grep -E "$request_pattern" "$log_file" | head -n 1 || true)"
  request_gen=""
  if [ -n "$request_line" ]; then
    request_gen="$(printf '%s\n' "$request_line" | sed -n 's/^compme: request gen=\([0-9][0-9]*\) .*/\1/p')"
    echo "E2E: stage present: 'request gen=' [PASS]"
  else
    echo "E2E: stage missing: 'request gen=' [FAIL]"
    ok=0
  fi

  if [ -n "$request_gen" ] && grep -Eq "^compme: completion gen=$request_gen candidate_count=[0-9][0-9]* candidate_lengths=\\[[0-9, ]*\\]$" "$log_file"; then
    echo "E2E: stage present: 'completion gen=$request_gen' [PASS]"
  else
    echo "E2E: stage missing: 'completion gen=${request_gen:-<request>}' [FAIL]"
    ok=0
  fi

  if [ "$accept_mode" = "word" ]; then
    if grep -Eq '^compme: accept Word$' "$log_file"; then
      echo "E2E: stage present: 'accept Word' [PASS]"
    else
      echo "E2E: stage missing: 'accept Word' [FAIL]"
      ok=0
    fi
  fi
  if grep -Eq '^compme: accept Full$' "$log_file"; then
    echo "E2E: stage present: 'accept Full' [PASS]"
  else
    echo "E2E: stage missing: 'accept Full' [FAIL]"
    ok=0
  fi

  [ "$ok" -eq 1 ]
}

run_self_tests() {
  failures=0
  ( exit 7 ) &
  fake_pid=$!
  wait_for_app_status "$fake_pid"
  fake_status="$WAIT_STATUS"
  if [ "$fake_status" -eq 7 ] && ! record_app_status "$fake_status" >/dev/null; then
    echo "PASS self-test-e2e-product-exit-status"
  else
    echo "FAIL self-test-e2e-product-exit-status: nonzero app exit was not observed as failure" >&2
    failures=$((failures + 1))
  fi
  if record_app_status 0 >/dev/null; then
    echo "PASS self-test-e2e-product-exit-status-success"
  else
    echo "FAIL self-test-e2e-product-exit-status-success: zero app exit was not observed as success" >&2
    failures=$((failures + 1))
  fi
  if grep -Eq '^[[:space:]]*cat "\$LOG"|^[[:space:]]*echo "\$RESULT"' "$0"; then
    echo "FAIL self-test-e2e-no-raw-output: live gate prints raw log or document output" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-no-raw-output"
  fi
  if grep -Eq '^[[:space:]]*echo .*prefix=.*\$PREFIX|^[[:space:]]*echo .*stub=.*\$STUB' "$0"; then
    echo "FAIL self-test-e2e-no-raw-banner: live gate prints raw prefix or stub output" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-no-raw-banner"
  fi
  if (
    unset COMPME_E2E_STUB COMPME_STUB_COMPLETION
    ACCEPT_MODE=word
    REAL_MODEL=0
    STUB=""
    configure_e2e_mode
    [ "$(printf '%s\n' "$STUB" | awk '{print NF}')" -ge 2 ]
  ); then
    echo "PASS self-test-e2e-word-default-has-remainder"
  else
    echo "FAIL self-test-e2e-word-default-has-remainder: word mode default stub must contain at least two words" >&2
    failures=$((failures + 1))
  fi
  if (
    COMPME_E2E_STUB=" custom completion"
    ACCEPT_MODE=word
    REAL_MODEL=0
    STUB=""
    configure_e2e_mode
    [ "$STUB" = " custom completion" ]
  ); then
    echo "PASS self-test-e2e-word-explicit-stub-preserved"
  else
    echo "FAIL self-test-e2e-word-explicit-stub-preserved: explicit word-mode stub was replaced" >&2
    failures=$((failures + 1))
  fi
  tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t compme-e2e-self-test)"
  hostile_log="$tmp_dir/hostile.log"
  printf '%s\n' \
    'compme: prompt_context=Some("Clipboard: RAW-CLIPBOARD-SENTINEL")' \
    'compme: request gen=7 prompt_chars=32' >"$hostile_log"
  hostile_doc='RAW-DOCUMENT-SENTINEL with private context'
  evidence="$(print_evidence_summary "$hostile_log" "$hostile_doc")"
  if [[ "$evidence" == *RAW-CLIPBOARD-SENTINEL* || "$evidence" == *RAW-DOCUMENT-SENTINEL* ]]; then
    echo "FAIL self-test-e2e-evidence-summary-redacts-hostile-content: raw sentinel leaked" >&2
    failures=$((failures + 1))
  elif [[ "$evidence" == *"log=$hostile_log"* && "$evidence" == *"log_lines=2"* && "$evidence" == *"document_chars=42"* ]]; then
    echo "PASS self-test-e2e-evidence-summary-redacts-hostile-content"
  else
    echo "FAIL self-test-e2e-evidence-summary-redacts-hostile-content: metadata missing from summary: $evidence" >&2
    failures=$((failures + 1))
  fi
  pipeline_log="$tmp_dir/pipeline.log"
  printf '%s\n' \
    'compme: focus TextEdit' \
    'compme: request gen=7 prompt_chars=32 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true' \
    'compme: completion gen=7 candidate_count=1 candidate_lengths=[8]' \
    'compme: accept Full' >"$pipeline_log"
  if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$pipeline_log" full 0 >/dev/null; then
    echo "PASS self-test-e2e-pipeline-evidence-full-success"
  else
    echo "FAIL self-test-e2e-pipeline-evidence-full-success" >&2
    failures=$((failures + 1))
  fi
  if assert_pipeline_evidence 'prefix accepted ok' 'prompt baseline' "$pipeline_log" full 0 real >/dev/null; then
    echo "FAIL self-test-e2e-pipeline-evidence-real-rejects-unrelated-growth" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-pipeline-evidence-real-rejects-unrelated-growth"
  fi
  if assert_pipeline_evidence 'prefix only' 'STUB-COMPLETE' "$pipeline_log" full 0 >/dev/null; then
    echo "FAIL self-test-e2e-pipeline-evidence-missing-readback: missing stub passed" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-pipeline-evidence-missing-readback"
  fi
  mismatch_log="$tmp_dir/mismatched-generation.log"
  printf '%s\n' \
    'compme: focus TextEdit' \
    'compme: request gen=7 prompt_chars=32 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true' \
    'compme: completion gen=8 candidate_count=1 candidate_lengths=[8]' \
    'compme: accept Full' >"$mismatch_log"
  if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$mismatch_log" full 0 >/dev/null; then
    echo "FAIL self-test-e2e-pipeline-evidence-mismatched-generation: mismatched request/completion passed" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-pipeline-evidence-mismatched-generation"
  fi
  hostile_request_failed=0
  for hostile_request in \
    'compme: request gen=7 prompt_chars=32 app=unknown app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true' \
    'compme: request gen=7 prompt_chars=32 app=com.apple.Terminal app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true' \
    'compme: request gen=7 prompt_chars=32 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=false'
  do
    hostile_request_log="$tmp_dir/hostile-request-$(printf '%s' "$hostile_request" | cksum | awk '{print $1}').log"
    printf '%s\n' \
      'compme: focus TextEdit' \
      "$hostile_request" \
      'compme: completion gen=7 candidate_count=1 candidate_lengths=[8]' \
      'compme: accept Full' >"$hostile_request_log"
    if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$hostile_request_log" full 0 >/dev/null; then
      echo "FAIL self-test-e2e-pipeline-evidence-hostile-request: malformed request passed: $hostile_request" >&2
      failures=$((failures + 1))
      hostile_request_failed=1
    fi
  done
  hostile_embedded_request_log="$tmp_dir/hostile-embedded-request.log"
  printf '%s\n' \
    'compme: focus TextEdit' \
    'compme: prompt_context=Some("compme: request gen=7 prompt_chars=32 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true")' \
    'compme: completion gen=7 candidate_count=1 candidate_lengths=[8]' \
    'compme: accept Full' >"$hostile_embedded_request_log"
  if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$hostile_embedded_request_log" full 0 >/dev/null; then
    echo "FAIL self-test-e2e-pipeline-evidence-embedded-request: embedded request text passed" >&2
    failures=$((failures + 1))
    hostile_request_failed=1
  fi
  if [ "$hostile_request_failed" -eq 0 ]; then
    echo "PASS self-test-e2e-pipeline-evidence-hostile-requests"
  fi
  hostile_stage_log="$tmp_dir/hostile-stage.log"
  printf '%s\n' \
    'compme: prompt_context=Some("focus request gen=7 completion gen=7 accept Full")' >"$hostile_stage_log"
  if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$hostile_stage_log" full 0 >/dev/null; then
    echo "FAIL self-test-e2e-pipeline-evidence-hostile-stage-text: raw context satisfied stage evidence" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-pipeline-evidence-hostile-stage-text"
  fi
  full_missing_failed=0
  for missing in 'focus' 'request gen=' 'completion gen=' 'accept Full'; do
    missing_log="$tmp_dir/full-missing-$(printf '%s' "$missing" | tr -c '[:alnum:]' '_').log"
    grep -v "$missing" "$pipeline_log" >"$missing_log"
    if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$missing_log" full 0 >/dev/null; then
      echo "FAIL self-test-e2e-pipeline-evidence-full-missing-stage: $missing passed" >&2
      failures=$((failures + 1))
      full_missing_failed=1
    fi
  done
  if [ "$full_missing_failed" -eq 0 ]; then
    echo "PASS self-test-e2e-pipeline-evidence-full-missing-stages"
  fi
  word_log="$tmp_dir/word.log"
  printf '%s\n' \
    'compme: focus TextEdit' \
    'compme: request gen=8 prompt_chars=32 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true' \
    'compme: completion gen=8 candidate_count=1 candidate_lengths=[8]' \
    'compme: accept Word' \
    'compme: accept Full' >"$word_log"
  word_missing_failed=0
  for missing in 'accept Word' 'accept Full'; do
    missing_log="$tmp_dir/word-missing-$(printf '%s' "$missing" | tr -c '[:alnum:]' '_').log"
    grep -v "$missing" "$word_log" >"$missing_log"
    if assert_pipeline_evidence 'prefix STUB-COMPLETE' 'STUB-COMPLETE' "$missing_log" word 0 >/dev/null; then
      echo "FAIL self-test-e2e-pipeline-evidence-word-missing-stage: $missing passed" >&2
      failures=$((failures + 1))
      word_missing_failed=1
    fi
  done
  if [ "$word_missing_failed" -eq 0 ]; then
    echo "PASS self-test-e2e-pipeline-evidence-word-missing-stages"
  fi
  if assert_pipeline_evidence 'prefix real output' 'prefix ' "$pipeline_log" full 0 real >/dev/null; then
    echo "PASS self-test-e2e-pipeline-evidence-real-model-readback-success"
  else
    echo "FAIL self-test-e2e-pipeline-evidence-real-model-readback-success" >&2
    failures=$((failures + 1))
  fi
  if assert_pipeline_evidence 'prefix ' 'prefix ' "$pipeline_log" full 0 real >/dev/null; then
    echo "FAIL self-test-e2e-pipeline-evidence-real-model-unchanged-readback: unchanged document passed" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-e2e-pipeline-evidence-real-model-unchanged-readback"
  fi
  if (
    ACCEPT_MODE=full
    REAL_MODEL=1
    STUB=""
    unset COMPME_E2E_STUB
    unset COMPME_STUB_COMPLETION
    configure_e2e_mode
    [ "$REAL_MODEL" -eq 1 ] && [ -z "$STUB" ]
  ); then
    echo "PASS self-test-e2e-real-model-configures-without-stub"
  else
    echo "FAIL self-test-e2e-real-model-configures-without-stub" >&2
    failures=$((failures + 1))
  fi
  real_mode_stub_log="$tmp_dir/real-mode-stub-completion.log"
  if (
    ACCEPT_MODE=full
    REAL_MODEL=1
    STUB=""
    unset COMPME_E2E_STUB
    COMPME_STUB_COMPLETION=" inherited-stub"
    export COMPME_STUB_COMPLETION
    configure_e2e_mode
  ) >"$real_mode_stub_log" 2>&1; then
    echo "FAIL self-test-e2e-real-model-rejects-stub-completion: inherited stub passed" >&2
    failures=$((failures + 1))
  elif grep -q 'COMPME_E2E_REAL_MODEL=1 cannot inherit COMPME_STUB_COMPLETION' "$real_mode_stub_log"; then
    echo "PASS self-test-e2e-real-model-rejects-stub-completion"
  else
    echo "FAIL self-test-e2e-real-model-rejects-stub-completion: expected error missing" >&2
    failures=$((failures + 1))
  fi
  rm -rf "$tmp_dir"
  [ "$failures" -eq 0 ] || return 1
  echo "E2E self-tests passed"
  return 0
}

if [ "${1:-}" = "--self-test" ]; then
  run_self_tests
  exit $?
fi

configure_e2e_mode

[ -x "$BIN" ] || fail "binary not built: $BIN (run: cargo build -p app)"
[ -n "$PID" ] || fail "set COMPME_ACCEPTANCE_PID to the TextEdit pid"
command -v osascript >/dev/null 2>&1 || fail "osascript unavailable (macOS only)"

mkdir -p "$(dirname "$LOG")"

PREFIX="${PREFIX}${PROMPT_MARKER} "
prefix_chars="$(printf '%s' "$PREFIX" | wc -m | tr -d '[:space:]')"
stub_chars="$(printf '%s' "$STUB" | wc -m | tr -d '[:space:]')"
echo "E2E compme: prefix_chars=$prefix_chars stub_chars=$stub_chars pid=$PID run_ms=$RUN_MS accept=$ACCEPT_MODE real_model=$REAL_MODEL"

# 1. Seed TextEdit with a known prefix and bring it to the front.
osascript - "$PREFIX" <<'OSA' || fail "could not seed TextEdit"
on run argv
  set prefixText to item 1 of argv
  tell application "TextEdit"
    activate
    if (count of documents) = 0 then make new document
    set text of front document to prefixText
  end tell
end run
OSA

sleep_ms 400

# 2. Launch the product binary against TextEdit. The default deterministic gate
#    uses the stub model; COMPME_E2E_REAL_MODEL=1 exercises LlamaModel instead.
if [ "$REAL_MODEL" -eq 1 ]; then
  env -u COMPME_STUB_COMPLETION \
    COMPME_ACCEPTANCE_PID="$PID" \
    COMPME_ACCEPTANCE_PROMPT_MARKER="$PROMPT_MARKER" \
    COMPME_RUN_MS="$RUN_MS" \
    "$BIN" >"$LOG" 2>&1 &
else
  COMPME_ACCEPTANCE_PID="$PID" \
    COMPME_ACCEPTANCE_PROMPT_MARKER="$PROMPT_MARKER" \
    COMPME_STUB_COMPLETION="$STUB" \
    COMPME_RUN_MS="$RUN_MS" \
    "$BIN" >"$LOG" 2>&1 &
fi
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
wait_for_app_status "$APP_PID"
app_status="$WAIT_STATUS"

# 6. Read the document back and assert.
RESULT="$(osascript -e 'tell application "TextEdit" to get text of front document' 2>/dev/null)"
print_evidence_summary "$LOG" "$RESULT"

ok=1
if [ "$REAL_MODEL" -eq 1 ]; then
  if ! assert_pipeline_evidence "$RESULT" "$PREFIX" "$LOG" "$ACCEPT_MODE" "$app_status" real; then
    ok=0
  fi
else
  if ! assert_pipeline_evidence "$RESULT" "$STUB" "$LOG" "$ACCEPT_MODE" "$app_status"; then
    ok=0
  fi
fi

[ "$ok" -eq 1 ] || fail "pipeline assertions failed (see log: $LOG)"
if [ "$REAL_MODEL" -eq 1 ]; then
  echo "E2E PASS: $ACCEPT_MODE focus->read->llama->ghost->accept->insert pipeline"
else
  echo "E2E PASS: $ACCEPT_MODE focus->read->infer->ghost->accept->insert pipeline"
fi
