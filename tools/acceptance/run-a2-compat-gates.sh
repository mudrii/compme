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
#   clipboard    works app + COMPME_CLIPBOARD_CONTEXT=1; diagnostic context
#                metadata proves clipboard context reached submit.
#   screen       works app + COMPME_SCREEN_CONTEXT=1; Screen Recording must
#                be granted and OCR must return context before submit.
#   browser-domain-allow
#                focused Safari/Chrome/Brave URL resolves to host-only domain
#                metadata and still submits.
#   browser-domain-exclude
#                same browser-domain proof, but COMPME_EXCLUDED_DOMAINS blocks.
#   matrix       table-driven runner over COMPME_A2_MATRIX_TARGETS row_id=pid
#                entries; writes a per-row ledger under COMPME_A2_LOG_DIR.
#
# This is the executable form of the §16 compatibility-matrix gate. It needs a
# console GUI session, Accessibility granted, the relevant apps installed/focused,
# and the target pid in COMPME_ACCEPTANCE_PID. The `screen` gate also needs
# Screen Recording permission.
# Default logs go under tools/acceptance/logs/. For release evidence, set
# COMPME_A2_LOG_DIR=tools/acceptance/evidence/a2/<run>, then commit the TSV and
# every row log referenced by the ledger.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPME_BIN:-$ROOT_DIR/target/debug/compme}"
PID="${COMPME_ACCEPTANCE_PID:-}"
KIND="${1:-works}"            # works | unsupported | terminal-cmd | terminal-nlp | clipboard | screen | browser-domain-allow | browser-domain-exclude | matrix
RUN_MS="${COMPME_RUN_MS:-3500}"
WARMUP_MS="${COMPME_WARMUP_MS:-1200}"
PREFIX="${COMPME_PREFIX:-Dear team, I wanted to }"
STUB="${COMPME_STUB:- follow up about the }"
PROMPT_MARKER="${COMPME_PROMPT_MARKER:-compme a2 marker ${KIND} $$}"
LOG_DIR="${COMPME_A2_LOG_DIR:-$ROOT_DIR/tools/acceptance/logs}"
LOG="${COMPME_A2_LOG:-$LOG_DIR/a2-compat-${KIND}-$(date +%Y%m%d-%H%M%S).log}"
mkdir -p "$LOG_DIR"

REQUEST_LINE_PREFIX='^compme: request gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app='
REQUEST_LINE_SUFFIX=' app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true$'
WORKS_APP_PATTERN='(com\.apple\.Safari|com\.google\.Chrome|com\.apple\.mail|com\.microsoft\.Word|com\.apple\.TextEdit|com\.apple\.Notes|notion\.id|md\.obsidian|com\.apple\.MobileSMS)'
TERMINAL_APP_PATTERN='(com\.apple\.Terminal|com\.googlecode\.iterm2)'
BROWSER_APP_PATTERN='(com\.apple\.Safari|com\.google\.Chrome|com\.brave\.Browser)'

A2_MATRIX_ROWS=(
  "textedit|works|com.apple.TextEdit|request"
  "notes|works|com.apple.Notes|request"
  "mail|works|com.apple.mail|request"
  "word|works|com.microsoft.Word|request"
  "safari|browser-domain-allow|com.apple.Safari|domain-request"
  "chrome|browser-domain-allow|com.google.Chrome|domain-request"
  "brave|browser-domain-allow|com.brave.Browser|domain-request"
  "browser-exclude|browser-domain-exclude|browser-domain|blocked-prefs"
  "terminal-cmd|terminal-cmd|terminal|blocked-terminal"
  "terminal-nlp|terminal-nlp|terminal|request"
  "unsupported|unsupported|unsupported-app|blocked-app"
  "clipboard|clipboard|works-app|context-request"
  "screen|screen|works-app|context-request"
)

terminal_cmd_prefix() {
  printf 'git status # %s ' "$PROMPT_MARKER"
}

has_request_for_app_pattern() {
  grep -Eq "${REQUEST_LINE_PREFIX}$2${REQUEST_LINE_SUFFIX}" "$1"
}

has_unknown_request_app() {
  grep -Eq "${REQUEST_LINE_PREFIX}unknown " "$1"
}

has_request() {
  has_request_for_app_pattern "$1" '[^[:space:]]+' \
    && ! has_unknown_request_app "$1"
}

has_works_request() {
  has_request_for_app_pattern "$1" "$WORKS_APP_PATTERN"
}

has_terminal_nlp_request() {
  has_request_for_app_pattern "$1" "$TERMINAL_APP_PATTERN"
}

has_browser_domain_host() {
  grep -Eq "^compme: domain=[[:alnum:].-]+ \(${BROWSER_APP_PATTERN}\)$" "$1" \
    && ! grep -Eq '^compme: domain=(https?://|[^ ]*[/?:#])' "$1"
}

has_browser_domain_allow_evidence() {
  has_browser_domain_host "$1" \
    && has_request_for_app_pattern "$1" "$BROWSER_APP_PATTERN"
}

has_browser_domain_exclude_evidence() {
  has_browser_domain_host "$1" \
    && grep -Eq "^compme: request blocked gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app=${BROWSER_APP_PATTERN} app_allows=true terminal_ok=true domain_ready=true prefs_ok=false prompt_marker=true$" "$1" \
    && ! has_request "$1"
}

has_clipboard_prompt_context() {
  grep -Eq 'prompt_context=Some\("sources=[^"]*clipboard[^"]*clipboard_chars=[1-9][0-9]*([^0-9]|")' "$1" \
    && grep -Eq 'clipboard_context=Some\(chars=[1-9][0-9]* marker=true\)' "$1"
}

has_screen_prompt_context() {
  grep -Eq 'prompt_context=Some\("sources=[^"]*screen[^"]*screen_chars=[1-9][0-9]*([^0-9]|")' "$1"
}

has_raw_prompt_context_payload() {
  grep -Eq 'prompt_context=Some\("[^"]*(Clipboard:|On screen:|Recent:)' "$1"
}

has_no_raw_prompt_context_payload() {
  ! has_raw_prompt_context_payload "$1"
}

has_unsupported_block_evidence() {
  grep -Eq 'compme: request blocked .*prompt_chars=[1-9][0-9]* .*app_allows=false .*prompt_marker=true$' "$1"
}

has_terminal_cmd_block_evidence() {
  grep -Eq 'compme: request blocked .*prompt_chars=[1-9][0-9]* .*terminal_ok=false .*prompt_marker=true$' "$1"
}

wait_for_product_status() {
  product_pid="$1"
  status=0
  wait "$product_pid" 2>/dev/null || status=$?
  WAIT_STATUS="$status"
}

product_status_ok() {
  [[ "$1" -eq 0 ]]
}

matrix_target_pid() {
  row_id="$1"
  printf '%s\n' "${COMPME_A2_MATRIX_TARGETS:-}" \
    | tr ', ' '\n' \
    | awk -F= -v id="$row_id" '$1 == id { print $2; exit }'
}

matrix_rows_are_unique() {
  printf '%s\n' "${A2_MATRIX_ROWS[@]}" \
    | awk -F'|' 'seen[$1]++ { duplicate = 1 } END { exit duplicate }'
}

self_test_assert() {
  name="$1"
  expected="$2"
  shift 2
  if "$@"; then
    actual=1
  else
    actual=0
  fi
  if [[ "$actual" == "$expected" ]]; then
    echo "PASS self-test-$name"
  else
    echo "FAIL self-test-$name: expected $expected got $actual" >&2
    return 1
  fi
}

run_self_tests() {
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/a2-compat-tests.XXXXXX")"
  failures=0
  good="$tmp_dir/good.log"
  raw_prompt_context="$tmp_dir/raw-prompt-context.log"
  clipboard_source_wrong_length="$tmp_dir/clipboard-source-wrong-length.log"
  clipboard_marker_false="$tmp_dir/clipboard-marker-false.log"
  screen_source_missing_length="$tmp_dir/screen-source-missing-length.log"
  producer_only="$tmp_dir/producer-only.log"
  unsupported_block="$tmp_dir/unsupported-block.log"
  terminal_block="$tmp_dir/terminal-block.log"
  unsupported_block_marker_false="$tmp_dir/unsupported-block-marker-false.log"
  terminal_block_marker_missing="$tmp_dir/terminal-block-marker-missing.log"
  bare_request="$tmp_dir/bare-request.log"
  custom_app_request="$tmp_dir/custom-app-request.log"
  mixed_unknown_request="$tmp_dir/mixed-unknown-request.log"
  mixed_malformed_unknown_request="$tmp_dir/mixed-malformed-unknown-request.log"
  unresolved_request="$tmp_dir/unresolved-request.log"
  marker_missing_request="$tmp_dir/marker-missing-request.log"
  embedded_request="$tmp_dir/embedded-request.log"
  terminal_request="$tmp_dir/terminal-request.log"
  browser_domain_allow="$tmp_dir/browser-domain-allow.log"
  browser_domain_raw_url="$tmp_dir/browser-domain-raw-url.log"
  browser_domain_exclude="$tmp_dir/browser-domain-exclude.log"
  focus_only="$tmp_dir/focus-only.log"
  empty="$tmp_dir/empty.log"

  cat >"$good" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: clipboard_context=Some(chars=24 marker=true)
compme: screen_context=Some(12)
compme: prompt_context=Some("sources=clipboard,screen chars=36 clipboard_chars=24 screen_chars=12")
LOG
  varied_clipboard="$tmp_dir/varied-clipboard.log"
  cat >"$varied_clipboard" <<'LOG'
compme: focus ax:1
compme: request gen=8 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: clipboard_context=Some(chars=17 marker=true)
compme: prompt_context=Some("sources=clipboard chars=17 clipboard_chars=17")
LOG
  cat >"$raw_prompt_context" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: screen_context=Some(12)
compme: prompt_context=Some("Clipboard: ada@example.com | On screen: sk-live-secret | Recent: private draft")
LOG
  cat >"$clipboard_source_wrong_length" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: clipboard_context=Some(chars=24 marker=false)
compme: prompt_context=Some("sources=clipboard chars=12 clipboard_chars=12")
LOG
  cat >"$clipboard_marker_false" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: clipboard_context=Some(chars=24 marker=false)
compme: prompt_context=Some("sources=clipboard chars=24 clipboard_chars=24")
LOG
  cat >"$screen_source_missing_length" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: screen_context=Some(12)
compme: prompt_context=Some("sources=screen chars=12")
LOG
  cat >"$producer_only" <<'LOG'
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: screen_context=Some(12)
LOG
  cat >"$unsupported_block" <<'LOG'
compme: focus ax:1
compme: request blocked gen=7 prompt_chars=28 app=com.mitchellh.ghostty app_allows=false terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$terminal_block" <<'LOG'
compme: focus ax:1
compme: request blocked gen=8 prompt_chars=20 app=com.apple.Terminal app_allows=true terminal_ok=false domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$unsupported_block_marker_false" <<'LOG'
compme: focus ax:1
compme: request blocked gen=7 prompt_chars=28 app=com.mitchellh.ghostty app_allows=false terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=false
LOG
  cat >"$terminal_block_marker_missing" <<'LOG'
compme: focus ax:1
compme: request blocked gen=8 prompt_chars=20 app=com.apple.Terminal app_allows=true terminal_ok=false domain_ready=true prefs_ok=true
LOG
  cat >"$bare_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5
LOG
  cat >"$custom_app_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.example.CustomEditor app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$mixed_unknown_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: request gen=8 prompt_chars=5 app=unknown app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$mixed_malformed_unknown_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
compme: request gen=8 prompt_chars=5 app=unknown unresolved metadata
LOG
  cat >"$unresolved_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=unknown app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$marker_missing_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=false
LOG
  cat >"$embedded_request" <<'LOG'
compme: prompt_context=Some("compme: request gen=7 prompt_chars=5 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true")
LOG
  cat >"$terminal_request" <<'LOG'
compme: focus ax:1
compme: request gen=7 prompt_chars=44 app=com.apple.Terminal app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$browser_domain_allow" <<'LOG'
compme: domain=docs.google.com (com.apple.Safari)
compme: request gen=7 prompt_chars=44 app=com.apple.Safari app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$browser_domain_raw_url" <<'LOG'
compme: domain=https://docs.google.com/document/d/secret?token=abc (com.apple.Safari)
compme: request gen=7 prompt_chars=44 app=com.apple.Safari app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true
LOG
  cat >"$browser_domain_exclude" <<'LOG'
compme: domain=docs.google.com (com.google.Chrome)
compme: request blocked gen=8 prompt_chars=44 app=com.google.Chrome app_allows=true terminal_ok=true domain_ready=true prefs_ok=false prompt_marker=true
LOG
  cat >"$focus_only" <<'LOG'
compme: focus ax:1
LOG
  : >"$empty"

  self_test_assert "request-present" 1 has_request "$good" || failures=$((failures + 1))
  self_test_assert "request-absent" 0 has_request "$empty" || failures=$((failures + 1))
  self_test_assert "request-without-app-metadata-is-not-submit-proof" 0 has_request "$bare_request" || failures=$((failures + 1))
  self_test_assert "request-allows-resolved-custom-app" 1 has_request "$custom_app_request" || failures=$((failures + 1))
  self_test_assert "mixed-unknown-request-is-not-submit-proof" 0 has_request "$mixed_unknown_request" || failures=$((failures + 1))
  self_test_assert "mixed-malformed-unknown-request-is-not-submit-proof" 0 has_request "$mixed_malformed_unknown_request" || failures=$((failures + 1))
  self_test_assert "request-without-resolved-app-is-not-submit-proof" 0 has_request "$unresolved_request" || failures=$((failures + 1))
  self_test_assert "request-without-prompt-marker-is-not-submit-proof" 0 has_request "$marker_missing_request" || failures=$((failures + 1))
  self_test_assert "embedded-request-text-is-not-submit-proof" 0 has_request "$embedded_request" || failures=$((failures + 1))
  self_test_assert "works-request-present" 1 has_works_request "$good" || failures=$((failures + 1))
  self_test_assert "works-request-requires-non-terminal-app" 0 has_works_request "$terminal_request" || failures=$((failures + 1))
  self_test_assert "terminal-nlp-request-requires-terminal-app" 0 has_terminal_nlp_request "$good" || failures=$((failures + 1))
  self_test_assert "terminal-nlp-request-present" 1 has_terminal_nlp_request "$terminal_request" || failures=$((failures + 1))
  self_test_assert "browser-domain-allow-evidence" 1 has_browser_domain_allow_evidence "$browser_domain_allow" || failures=$((failures + 1))
  self_test_assert "browser-domain-rejects-raw-url" 0 has_browser_domain_allow_evidence "$browser_domain_raw_url" || failures=$((failures + 1))
  self_test_assert "browser-domain-exclude-evidence" 1 has_browser_domain_exclude_evidence "$browser_domain_exclude" || failures=$((failures + 1))
  self_test_assert "browser-domain-exclude-requires-block" 0 has_browser_domain_exclude_evidence "$browser_domain_allow" || failures=$((failures + 1))
  self_test_assert "clipboard-prompt-context" 1 has_clipboard_prompt_context "$good" || failures=$((failures + 1))
  self_test_assert "clipboard-prompt-context-varied-length" 1 has_clipboard_prompt_context "$varied_clipboard" || failures=$((failures + 1))
  self_test_assert "screen-prompt-context" 1 has_screen_prompt_context "$good" || failures=$((failures + 1))
  self_test_assert "metadata-prompt-context-is-not-raw" 1 has_no_raw_prompt_context_payload "$good" || failures=$((failures + 1))
  self_test_assert "raw-prompt-context-detected" 1 has_raw_prompt_context_payload "$raw_prompt_context" || failures=$((failures + 1))
  self_test_assert "clipboard-source-without-marker-length-is-not-submit-proof" 0 has_clipboard_prompt_context "$clipboard_source_wrong_length" || failures=$((failures + 1))
  self_test_assert "clipboard-source-without-marker-match-is-not-submit-proof" 0 has_clipboard_prompt_context "$clipboard_marker_false" || failures=$((failures + 1))
  self_test_assert "screen-source-without-length-is-not-submit-proof" 0 has_screen_prompt_context "$screen_source_missing_length" || failures=$((failures + 1))
  self_test_assert "raw-prompt-context-is-not-submit-proof" 0 has_screen_prompt_context "$raw_prompt_context" || failures=$((failures + 1))
  self_test_assert "screen-producer-alone-is-not-submit-context" 0 has_screen_prompt_context "$producer_only" || failures=$((failures + 1))
  self_test_assert "unsupported-block-evidence" 1 has_unsupported_block_evidence "$unsupported_block" || failures=$((failures + 1))
  self_test_assert "terminal-block-evidence" 1 has_terminal_cmd_block_evidence "$terminal_block" || failures=$((failures + 1))
  self_test_assert "unsupported-block-without-prompt-marker-is-not-evidence" 0 has_unsupported_block_evidence "$unsupported_block_marker_false" || failures=$((failures + 1))
  self_test_assert "terminal-block-without-prompt-marker-is-not-evidence" 0 has_terminal_cmd_block_evidence "$terminal_block_marker_missing" || failures=$((failures + 1))
  self_test_assert "focus-only-is-not-baseline" 0 has_unsupported_block_evidence "$focus_only" || failures=$((failures + 1))
  self_test_assert "baseline-missing" 0 has_terminal_cmd_block_evidence "$empty" || failures=$((failures + 1))
  hostile_prefix=$'quote " backslash \\ dollar $PREFIX\nline two'
  if round_tripped_prefix=$(/usr/bin/osascript - "$hostile_prefix" <<'OSA'
on run argv
  return item 1 of argv
end run
OSA
  ); then
    if [[ "$round_tripped_prefix" == "$hostile_prefix" ]]; then
      echo "PASS self-test-applescript-prefix-argv-roundtrip"
    else
      echo "FAIL self-test-applescript-prefix-argv-roundtrip: argv text changed" >&2
      failures=$((failures + 1))
    fi
  else
    echo "FAIL self-test-applescript-prefix-argv-roundtrip: osascript argv probe failed" >&2
    failures=$((failures + 1))
  fi
  if grep -Eq 'keystroke "\$PREFIX"|set text of front document to "\$PREFIX"' \
    "$ROOT_DIR/tools/acceptance/run-a2-compat-gates.sh" \
    "$ROOT_DIR/tools/acceptance/e2e-complete-me.sh"; then
    echo "FAIL self-test-applescript-prefix-argv: PREFIX is embedded in AppleScript source" >&2
    failures=$((failures + 1))
  else
    echo "PASS self-test-applescript-prefix-argv"
  fi
  terminal_cmd_prefix_value="$(terminal_cmd_prefix)"
  if [[ "$terminal_cmd_prefix_value" == *"$PROMPT_MARKER"* ]] \
    && [[ "$terminal_cmd_prefix_value" == git\ status\ \#* ]]; then
    echo "PASS self-test-terminal-cmd-prefix-carries-marker"
  else
    echo "FAIL self-test-terminal-cmd-prefix-carries-marker: $terminal_cmd_prefix_value" >&2
    failures=$((failures + 1))
  fi
  if grep -Eq '^[[:space:]]*env -i[[:space:]]*\\' "$ROOT_DIR/tools/acceptance/run-a2-compat-gates.sh"; then
    echo "PASS self-test-a2-product-env-isolated"
  else
    echo "FAIL self-test-a2-product-env-isolated: product launch must use env -i" >&2
    failures=$((failures + 1))
  fi
  ( exit 7 ) &
  fake_pid=$!
  wait_for_product_status "$fake_pid"
  fake_status="$WAIT_STATUS"
  if [[ "$fake_status" -eq 7 ]] && ! product_status_ok "$fake_status"; then
    echo "PASS self-test-product-exit-status-a2"
  else
    echo "FAIL self-test-product-exit-status-a2: nonzero compme exit was not observed as failure" >&2
    failures=$((failures + 1))
  fi
  if matrix_rows_are_unique; then
    echo "PASS self-test-a2-matrix-row-ids-unique"
  else
    echo "FAIL self-test-a2-matrix-row-ids-unique" >&2
    failures=$((failures + 1))
  fi
  if [[ "${#A2_MATRIX_ROWS[@]}" -ge 12 ]]; then
    echo "PASS self-test-a2-matrix-row-count"
  else
    echo "FAIL self-test-a2-matrix-row-count: ${#A2_MATRIX_ROWS[@]}" >&2
    failures=$((failures + 1))
  fi
  matrix_fail_dir="$tmp_dir/matrix-fail"
  mkdir -p "$matrix_fail_dir"
  if COMPME_A2_LOG_DIR="$matrix_fail_dir" COMPME_A2_MATRIX_TARGETS="" "$0" matrix >/dev/null 2>"$matrix_fail_dir/matrix.err"; then
    echo "FAIL self-test-a2-matrix-missing-targets-fail: missing target rows passed without COMPME_A2_MATRIX_ALLOW_SKIP=1" >&2
    failures=$((failures + 1))
  elif grep -Eq 'failures=[1-9][0-9]* skipped=[1-9][0-9]*' "$matrix_fail_dir/matrix.err"; then
    echo "PASS self-test-a2-matrix-missing-targets-fail"
  else
    echo "FAIL self-test-a2-matrix-missing-targets-fail: failure output did not report skipped rows" >&2
    failures=$((failures + 1))
  fi
  matrix_skip_dir="$tmp_dir/matrix-skip"
  mkdir -p "$matrix_skip_dir"
  if COMPME_A2_LOG_DIR="$matrix_skip_dir" COMPME_A2_MATRIX_TARGETS="" COMPME_A2_MATRIX_ALLOW_SKIP=1 "$0" matrix >/dev/null 2>"$matrix_skip_dir/matrix.err"; then
    ledger="$(ls "$matrix_skip_dir"/a2-compat-matrix-*.tsv 2>/dev/null | tail -n 1)"
    if [[ -n "$ledger" ]] && awk -F '\t' -v expected="${#A2_MATRIX_ROWS[@]}" 'NR == 1 && $8 == "log_path" { header = 1 } NR > 1 && $6 == "SKIP" && $8 == "" { skip++ } END { exit header && skip == expected ? 0 : 1 }' "$ledger"; then
      echo "PASS self-test-a2-matrix-allow-skip-ledger"
    else
      echo "FAIL self-test-a2-matrix-allow-skip-ledger: SKIP ledger did not cover every row" >&2
      failures=$((failures + 1))
    fi
  else
    echo "FAIL self-test-a2-matrix-allow-skip-ledger: COMPME_A2_MATRIX_ALLOW_SKIP=1 still failed" >&2
    failures=$((failures + 1))
  fi

  rm -rf "$tmp_dir"
  if [[ "$failures" -gt 0 ]]; then
    echo "Self-test failures: $failures" >&2
    return 1
  fi
  echo "Self-tests passed"
  return 0
}

run_matrix() {
  if [[ "$#" -ne 1 ]]; then
    echo "usage: run-a2-compat-gates.sh matrix" >&2
    exit 2
  fi
  matrix_stamp="$(date +%Y%m%d-%H%M%S)"
  matrix_started_at="$(date +%s)"
  ledger="$LOG_DIR/a2-compat-matrix-$matrix_stamp.tsv"
  printf 'generated_at_epoch\trow_id\tkind\tapp\tpid\tstatus\texpect\tlog_path\n' >"$ledger"
  failures=0
  skipped=0
  for row in "${A2_MATRIX_ROWS[@]}"; do
    IFS='|' read -r row_id row_kind row_app row_expect <<<"$row"
    row_pid="$(matrix_target_pid "$row_id")"
    row_log="$LOG_DIR/a2-compat-matrix-$matrix_stamp-$row_id.log"
    if [[ -z "$row_pid" ]]; then
      printf '%s\t%s\t%s\t%s\t\tSKIP\t%s\t\n' "$matrix_started_at" "$row_id" "$row_kind" "$row_app" "$row_expect" >>"$ledger"
      skipped=$((skipped + 1))
      if [[ "${COMPME_A2_MATRIX_ALLOW_SKIP:-0}" != "1" ]]; then
        failures=$((failures + 1))
      fi
      continue
    fi
    if COMPME_A2_LOG="$row_log" COMPME_ACCEPTANCE_PID="$row_pid" "$0" "$row_kind"; then
      printf '%s\t%s\t%s\t%s\t%s\tPASS\t%s\t%s\n' "$matrix_started_at" "$row_id" "$row_kind" "$row_app" "$row_pid" "$row_expect" "$row_log" >>"$ledger"
    else
      printf '%s\t%s\t%s\t%s\t%s\tFAIL\t%s\t%s\n' "$matrix_started_at" "$row_id" "$row_kind" "$row_app" "$row_pid" "$row_expect" "$row_log" >>"$ledger"
      failures=$((failures + 1))
    fi
  done
  if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: matrix — failures=$failures skipped=$skipped (ledger: $ledger)" >&2
    exit 1
  fi
  echo "PASS: matrix — ${#A2_MATRIX_ROWS[@]} rows (ledger: $ledger)"
}

if [[ "$KIND" == "--self-test" ]]; then
  if [[ "$#" -ne 1 ]]; then
    echo "usage: run-a2-compat-gates.sh [works|unsupported|terminal-cmd|terminal-nlp|clipboard|screen|browser-domain-allow|browser-domain-exclude|matrix|--self-test]" >&2
    exit 2
  fi
  run_self_tests
  status=$?
  tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t compme-a2-clipboard-self-test)"
  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp_dir/self-test-argc.err"; then
    echo "FAIL self-test-a2-rejects-extra-self-test-arg: extra argument passed" >&2
    status=1
  elif grep -q 'usage: run-a2-compat-gates.sh' "$tmp_dir/self-test-argc.err"; then
    echo "PASS self-test-a2-rejects-extra-self-test-arg"
  else
    echo "FAIL self-test-a2-rejects-extra-self-test-arg: expected usage missing" >&2
    status=1
  fi
  if "$0" works unexpected-extra >/dev/null 2>"$tmp_dir/normal-argc.err"; then
    echo "FAIL self-test-a2-rejects-extra-normal-arg: extra argument passed" >&2
    status=1
  elif grep -q 'usage: run-a2-compat-gates.sh' "$tmp_dir/normal-argc.err"; then
    echo "PASS self-test-a2-rejects-extra-normal-arg"
  else
    echo "FAIL self-test-a2-rejects-extra-normal-arg: expected usage missing" >&2
    status=1
  fi
  rm -rf "$tmp_dir"
  exit "$status"
fi

if [[ "$KIND" == "matrix" ]]; then
  run_matrix "$@"
  exit 0
fi

if [[ "$#" -gt 1 ]]; then
  echo "usage: run-a2-compat-gates.sh [works|unsupported|terminal-cmd|terminal-nlp|clipboard|screen|browser-domain-allow|browser-domain-exclude|matrix|--self-test]" >&2
  exit 2
fi

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
  terminal-cmd) PREFIX="$(terminal_cmd_prefix)" ;;
  terminal-nlp) PREFIX="please summarize the recent changes in " ;;
esac

if [[ "$KIND" != "terminal-cmd" ]]; then
  PREFIX="${PREFIX}${PROMPT_MARKER} "
fi

clip_env=()
screen_env=()
prefs_env=()

if [[ "$KIND" == "clipboard" ]]; then
  /usr/bin/osascript -e 'set the clipboard to "CLIPBOARD-CONTEXT-MARKER"' >/dev/null 2>&1 \
    || { echo "FAIL: could not seed clipboard context marker" >&2; exit 2; }
  clip_env=(COMPME_CLIPBOARD_CONTEXT=1 COMPME_DIAG_CONTEXT=1 COMPME_DIAG_CLIPBOARD_MARKER=CLIPBOARD-CONTEXT-MARKER)
fi

if [[ "$KIND" == "screen" ]]; then
  screen_env=(COMPME_SCREEN_CONTEXT=1 COMPME_DIAG_CONTEXT=1)
fi

if [[ "$KIND" == "browser-domain-exclude" ]]; then
  if [[ -z "${COMPME_A2_BROWSER_EXCLUDED_DOMAIN:-}" ]]; then
    echo "FAIL: set COMPME_A2_BROWSER_EXCLUDED_DOMAIN to the focused browser host" >&2
    exit 2
  fi
  prefs_env=(COMPME_EXCLUDED_DOMAINS="$COMPME_A2_BROWSER_EXCLUDED_DOMAIN")
fi

# Seed the field, then run the binary against it with a deterministic stub.
/usr/bin/osascript - "$PID" "$PREFIX" >/dev/null 2>&1 <<'OSA' || true
on run argv
  set targetPid to (item 1 of argv) as integer
  set prefixText to item 2 of argv
  tell application "System Events"
    set frontmost of (first process whose unix id is targetPid) to true
  end tell
  delay 0.4
  tell application "System Events" to keystroke prefixText
end run
OSA

env -i \
  PATH="${PATH:-/usr/bin:/bin:/usr/sbin:/sbin}" \
  HOME="${HOME:-}" \
  TMPDIR="${TMPDIR:-/tmp}" \
  RUST_BACKTRACE="${RUST_BACKTRACE:-}" \
  COMPME_STUB_COMPLETION="$STUB" \
  COMPME_ACCEPTANCE_PID="$PID" \
  COMPME_ACCEPTANCE_PROMPT_MARKER="$PROMPT_MARKER" \
  COMPME_RUN_MS="$RUN_MS" \
  ${clip_env[@]+"${clip_env[@]}"} \
  ${screen_env[@]+"${screen_env[@]}"} \
  ${prefs_env[@]+"${prefs_env[@]}"} \
  "$BIN" >"$LOG" 2>&1 &
BIN_PID=$!
sleep "$(awk "BEGIN{print ($WARMUP_MS+$RUN_MS)/1000}")"
wait_for_product_status "$BIN_PID"
app_status="$WAIT_STATUS"

requested=0
has_request "$LOG" && requested=1
works_requested=0
has_works_request "$LOG" && works_requested=1
terminal_nlp_requested=0
has_terminal_nlp_request "$LOG" && terminal_nlp_requested=1

pass() { echo "PASS: $KIND — $1 (log: $LOG)"; exit 0; }
fail() { echo "FAIL: $KIND — $1 (log: $LOG)"; exit 1; }

if ! product_status_ok "$app_status"; then
  fail "compme exited with status $app_status"
fi

case "$KIND" in
  works)
    [[ "$works_requested" == 1 ]] && pass "completion requested as expected" \
      || fail "expected a completion request with non-terminal target identity and prompt marker, none logged" ;;
  terminal-nlp)
    [[ "$terminal_nlp_requested" == 1 ]] && pass "completion requested as expected" \
      || fail "expected a completion request, none logged" ;;
  clipboard)
    [[ "$requested" == 1 ]] || fail "expected a completion request, none logged"
    has_no_raw_prompt_context_payload "$LOG" \
      || fail "diagnostic prompt_context leaked raw context payload"
    has_clipboard_prompt_context "$LOG" \
      && pass "clipboard context metadata reached the submit path" \
      || fail "expected clipboard source metadata in diagnostic prompt_context" ;;
  screen)
    [[ "$requested" == 1 ]] || fail "expected a completion request, none logged"
    has_no_raw_prompt_context_payload "$LOG" \
      || fail "diagnostic prompt_context leaked raw context payload"
    has_screen_prompt_context "$LOG" \
      && pass "screen OCR metadata was included in a submitted prompt" \
      || { grep -Eq 'screen_context=Some\([1-9][0-9]*\)' "$LOG" \
        && fail "OCR ran but no submitted prompt included it (timing) — retry with steadier typing" \
        || fail "expected non-empty screen context; check Screen Recording grant and visible text"; } ;;
  browser-domain-allow)
    has_browser_domain_allow_evidence "$LOG" \
      && pass "browser URL resolved to host-only domain metadata and completion submitted" \
      || fail "expected host-only browser domain metadata and a submitted request" ;;
  browser-domain-exclude)
    has_browser_domain_exclude_evidence "$LOG" \
      && pass "browser domain exclusion blocked the request" \
      || fail "expected host-only browser domain metadata and prefs_ok=false blocked-request evidence" ;;
  unsupported)
    has_unsupported_block_evidence "$LOG" || fail "no unsupported-app blocked-request evidence; cannot prove a gated-out request"
    [[ "$requested" == 0 ]] && pass "completion correctly gated out" \
      || fail "expected NO completion request, but one was logged" ;;
  terminal-cmd)
    has_terminal_cmd_block_evidence "$LOG" || fail "no terminal-command blocked-request evidence; cannot prove a gated-out request"
    [[ "$requested" == 0 ]] && pass "completion correctly gated out" \
      || fail "expected NO completion request, but one was logged" ;;
  *)
    fail "unknown KIND '$KIND' (works|unsupported|terminal-cmd|terminal-nlp|clipboard|screen|browser-domain-allow|browser-domain-exclude|matrix)" ;;
esac
