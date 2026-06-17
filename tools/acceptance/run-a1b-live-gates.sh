#!/usr/bin/env bash
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DRY_RUN=0
FORCE=0
SKIP_BUILD=0
SKIP_TEXTEDIT=0
SKIP_E2E=0
ALLOW_INCOMPLETE=0
SELF_TEST=0
TIMEOUT_MS=3000
SHORT_TIMEOUT_MS=1500
RETRIES="${A1B_RETRIES:-3}"
GATE_PAUSE_MS="${A1B_GATE_PAUSE_MS:-1000}"
TEXTEDIT_PID="${A1B_TEXTEDIT_PID:-}"
POPUP_PID="${A1B_POPUP_PID:-}"
BROWSER_PID="${A1B_BROWSER_PID:-}"
LOG_DIR="${A1B_LOG_DIR:-"$ROOT_DIR/tools/acceptance/logs/a1b-live-$(date +%Y%m%d-%H%M%S)"}"
INSERT_TEXT="${A1B_INSERT_TEXT:-" a1b-live-$(date +%H%M%S)"}"
POST_TAB_AFTER_MS="${A1B_POST_TAB_AFTER_MS:-300}"
HIDE_AFTER_MS="${A1B_HIDE_AFTER_MS:-100}"

passes=0
failures=0
skips=0
incomplete_skips=0
manuals=0

usage() {
  cat <<'USAGE'
Run A1b macOS live acceptance gates.

Usage:
  tools/acceptance/run-a1b-live-gates.sh [options]

Options:
  --dry-run              Print commands without executing them.
  --force                Run gates even when preflight sees locked screen or Secure Input.
  --skip-build           Do not run cargo build -p platform_macos --examples first.
  --skip-textedit        Do not run TextEdit gates. Useful when a browser or
                         popup target must stay focused.
  --skip-e2e             Do not run the end-to-end compme pipeline gate. This is
                         incomplete unless paired with --allow-incomplete.
  --allow-incomplete     Allow mandatory live gates to be skipped and still exit
                         zero. Without this, missing TextEdit coverage fails.
  --self-test            Run runner classification self-tests and exit.
  --textedit-pid PID     Use this TextEdit pid instead of pgrep -x TextEdit.
  --popup-pid PID        Also run popup fallback gate against a focused writable no-rect target.
  --browser-pid PID      Also run the browser marker gate requiring marker geometry.
  --timeout-ms MS        Duration for TextEdit and overlay gates. Default: 3000.
  --short-timeout-ms MS  Duration for accept/marker gates. Default: 1500.
  --retries N            Retry retry-safe AX observer gates. Default: 3.
  --gate-pause-ms MS     Pause before live gate attempts to avoid AX churn. Default: 1000.
  --log-dir DIR          Directory for per-gate logs.
  -h, --help             Show this help.

Before running, unlock the macOS session and focus an editable TextEdit document.
TextEdit must be frontmost for SyntheticKeys and Clipboard insertion gates.
Popup fallback uses the repo-local AppKit fixture by default. Pass --popup-pid
only when you intentionally want to validate a separate focused writable target
with no usable caret geometry.
For browser marker validation, focus a Chrome/Safari text field and pass --browser-pid.

Production accept keys use transient Carbon hotkeys. [CORR 2026-06-11] The
accept-tap, accept-insert, and e2e gates are SCRIPTED again: the example
harnesses now pump NSApp events each poll, so synthetic key posts fire
RegisterEventHotKey (see docs/ACCEPTANCE.md [CORR 2026-06-10]; validated
live 2026-06-11 — synthetic Tab/grave/Esc/Down all fired through the
rebuilt harness). The hotkeys are system-wide: keep hands off the keyboard
during the run — ambient Tab/grave/Esc/Down presses contaminate the
exact-match control checks (mismatched runs retry up to --retries).

The runner also prints MANUAL gates for live UI/permission checks that cannot be
driven safely from a shell script. Treat those lines as the repeatable checklist
to execute and record after the deterministic gates pass. The Input Monitoring
revoked spot-check is conditionally scripted only when read-only preflight shows
the current process is already revoked; this runner never requests or changes
Input Monitoring.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --force) FORCE=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --skip-textedit) SKIP_TEXTEDIT=1 ;;
    --skip-e2e) SKIP_E2E=1 ;;
    --allow-incomplete) ALLOW_INCOMPLETE=1 ;;
    --self-test) SELF_TEST=1 ;;
    --textedit-pid)
      [ "$#" -ge 2 ] || { echo "--textedit-pid requires a pid" >&2; exit 2; }
      TEXTEDIT_PID="$2"
      shift 2
      continue
      ;;
    --popup-pid)
      [ "$#" -ge 2 ] || { echo "--popup-pid requires a pid" >&2; exit 2; }
      POPUP_PID="$2"
      shift 2
      continue
      ;;
    --browser-pid)
      [ "$#" -ge 2 ] || { echo "--browser-pid requires a pid" >&2; exit 2; }
      BROWSER_PID="$2"
      shift 2
      continue
      ;;
    --timeout-ms)
      [ "$#" -ge 2 ] || { echo "--timeout-ms requires a value" >&2; exit 2; }
      TIMEOUT_MS="$2"
      shift 2
      continue
      ;;
    --short-timeout-ms)
      [ "$#" -ge 2 ] || { echo "--short-timeout-ms requires a value" >&2; exit 2; }
      SHORT_TIMEOUT_MS="$2"
      shift 2
      continue
      ;;
    --retries)
      [ "$#" -ge 2 ] || { echo "--retries requires a value" >&2; exit 2; }
      RETRIES="$2"
      shift 2
      continue
      ;;
    --gate-pause-ms)
      [ "$#" -ge 2 ] || { echo "--gate-pause-ms requires a value" >&2; exit 2; }
      GATE_PAUSE_MS="$2"
      shift 2
      continue
      ;;
    --log-dir)
      [ "$#" -ge 2 ] || { echo "--log-dir requires a directory" >&2; exit 2; }
      LOG_DIR="$2"
      shift 2
      continue
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

cd "$ROOT_DIR" || exit 2
mkdir -p "$LOG_DIR"

TEXTEDIT_BIN="$ROOT_DIR/target/debug/examples/textedit_observer_acceptance"
ACCEPT_BIN="$ROOT_DIR/target/debug/examples/accept_tap_acceptance"
ACCEPT_INSERT_BIN="$ROOT_DIR/target/debug/examples/accept_insert_acceptance"
MARKER_BIN="$ROOT_DIR/target/debug/examples/caret_marker_acceptance"
OVERLAY_BIN="$ROOT_DIR/target/debug/examples/overlay_presenter_acceptance"
POPUP_FIXTURE_BIN="$ROOT_DIR/target/debug/examples/popup_fallback_acceptance"
INPUT_MONITORING_BIN="$ROOT_DIR/target/debug/examples/input_monitoring_preflight_acceptance"
COMPME_BIN="$ROOT_DIR/target/debug/compme"
E2E_SCRIPT="$ROOT_DIR/tools/acceptance/e2e-complete-me.sh" # historical harness kept under its original name

print_cmd() {
  printf '  '
  printf '%q ' "$@"
  printf '\n'
}

sleep_ms() {
  ms="$1"
  case "$ms" in
    ''|*[!0-9]*) return 0 ;;
  esac
  [ "$ms" -gt 0 ] || return 0
  sleep "$(awk "BEGIN { printf \"%.3f\", $ms / 1000 }")"
}

classify_blocker() {
  log_file="$1"
  if grep -Eq 'CGS?SessionScreenIsLocked.*Yes' "$log_file"; then
    echo "locked-screen: CGSSessionScreenIsLocked=Yes"
  elif grep -Eq 'SecureInputEnabled|SecureInput \{ state: SecureInputEnabled \}|kCGSSessionSecureInputPID' "$log_file"; then
    echo "secure-input: global Secure Input is enabled"
  elif grep -Eq 'PermissionMissing|AXIsProcessTrusted|not trusted|failed to subscribe accept' "$log_file"; then
    echo "permission: check Accessibility grant"
  elif grep -Eq 'AX text value unavailable|AX selected text range unavailable|DIAG_ERROR no field observed' "$log_file"; then
    echo "focus-target: focus an editable text field, not the app shell"
  elif grep -q 'front_app=Some("pid:' "$log_file" && pgrep -x loginwindow >/dev/null 2>&1; then
    login_pid="$(pgrep -x loginwindow | head -n 1)"
    if [ -n "$login_pid" ] && grep -q "front_app=Some(\"pid:$login_pid\")" "$log_file"; then
      echo "locked-session: front app is loginwindow pid $login_pid"
    else
      echo "unknown: see log"
    fi
  else
    echo "unknown: see log"
  fi
}

run_gate() {
  name="$1"
  shift
  log_file="$LOG_DIR/$name.log"

  echo
  echo "== $name =="
  print_cmd "$@"

  if [ "$DRY_RUN" -eq 1 ]; then
    return 0
  fi

  sleep_ms "$GATE_PAUSE_MS"
  "$@" >"$log_file" 2>&1
  status=$?
  cat "$log_file"

  if [ "$status" -eq 0 ]; then
    echo "PASS $name"
    passes=$((passes + 1))
  else
    echo "FAIL $name ($status): $(classify_blocker "$log_file")"
    failures=$((failures + 1))
  fi
}

run_retryable_gate() {
  name="$1"
  shift
  attempts="$RETRIES"
  case "$attempts" in
    ''|*[!0-9]*) attempts=1 ;;
  esac
  [ "$attempts" -ge 1 ] || attempts=1

  echo
  echo "== $name =="
  print_cmd "$@"

  if [ "$DRY_RUN" -eq 1 ]; then
    return 0
  fi

  attempt=1
  while [ "$attempt" -le "$attempts" ]; do
    sleep_ms "$GATE_PAUSE_MS"
    if [ "$attempts" -gt 1 ]; then
      log_file="$LOG_DIR/$name.attempt-$attempt.log"
      echo "-- attempt $attempt/$attempts --"
    else
      log_file="$LOG_DIR/$name.log"
    fi

    "$@" >"$log_file" 2>&1
    status=$?
    cat "$log_file"

    if [ "$status" -eq 0 ]; then
      echo "PASS $name"
      passes=$((passes + 1))
      return 0
    fi

    if [ "$attempt" -lt "$attempts" ] \
      && grep -Eq 'AX cannot complete request|failed to subscribe focus|failed to subscribe caret' "$log_file" \
      && ! grep -Eq '^(INSERT|POST_INSERT_READ)' "$log_file"; then
      echo "RETRY $name ($status): transient AX observer setup failure"
      attempt=$((attempt + 1))
      continue
    fi

    echo "FAIL $name ($status): $(classify_blocker "$log_file")"
    failures=$((failures + 1))
    return "$status"
  done
}

skip_gate() {
  name="$1"
  reason="$2"
  required="${3:-optional}"
  echo
  echo "== $name =="
  echo "SKIP $name: $reason"
  skips=$((skips + 1))
  if [ "$required" = "mandatory" ]; then
    incomplete_skips=$((incomplete_skips + 1))
  fi
}

skip_e2e_gate() {
  skip_gate "$1" "$2" mandatory
}

manual_gate() {
  name="$1"
  reason="$2"
  echo
  echo "== $name =="
  echo "MANUAL $name: $reason"
  manuals=$((manuals + 1))
}

run_accept_tap_gate() {
  name="$1"
  shift
  attempts="$RETRIES"
  case "$attempts" in
    ''|*[!0-9]*) attempts=1 ;;
  esac
  [ "$attempts" -ge 1 ] || attempts=1

  echo
  echo "== $name =="
  print_cmd "$@"

  if [ "$DRY_RUN" -eq 1 ]; then
    return 0
  fi

  attempt=1
  while [ "$attempt" -le "$attempts" ]; do
    sleep_ms "$GATE_PAUSE_MS"
    if [ "$attempts" -gt 1 ]; then
      log_file="$LOG_DIR/$name.attempt-$attempt.log"
      echo "-- attempt $attempt/$attempts --"
    else
      log_file="$LOG_DIR/$name.log"
    fi

    "$@" >"$log_file" 2>&1
    status=$?
    cat "$log_file"

    if [ "$status" -eq 0 ]; then
      echo "PASS $name"
      passes=$((passes + 1))
      return 0
    fi

    # The accept gates assert an EXACT control set, and the Carbon hotkeys
    # are system-wide — any Tab/grave/Esc/Down pressed on the machine during
    # the gate window lands in the captured controls. A run that completed
    # (SUMMARY printed) but mismatched is retried; a genuine wrong-control
    # bug fails every attempt the same way.
    if [ "$attempt" -lt "$attempts" ] && grep -q '^SUMMARY controls=' "$log_file"; then
      echo "RETRY $name ($status): control set mismatched — ambient key presses contaminate the system-wide hotkeys; keep hands off the keyboard"
      attempt=$((attempt + 1))
      continue
    fi

    echo "FAIL $name ($status): $(classify_blocker "$log_file")"
    failures=$((failures + 1))
    return "$status"
  done
}

run_input_monitoring_revoked_carbon_gate() {
  name="input-monitoring-revoked-carbon-accept"

  echo
  echo "== $name =="
  print_cmd "$INPUT_MONITORING_BIN" revoked
  print_cmd env COMPME_ACCEPTANCE_REQUIRE_INPUT_MONITORING_REVOKED=1 COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" full
  print_cmd env COMPME_ACCEPTANCE_REQUIRE_INPUT_MONITORING_REVOKED=1 COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" word

  if [ "$DRY_RUN" -eq 1 ]; then
    echo "MANUAL $name: if read-only preflight reports Input Monitoring is granted, revoke it and rerun to confirm transient Carbon accept still works"
    manuals=$((manuals + 1))
    return 0
  fi

  sleep_ms "$GATE_PAUSE_MS"
  preflight_log="$LOG_DIR/$name.preflight.log"
  "$INPUT_MONITORING_BIN" revoked >"$preflight_log" 2>&1
  preflight_status=$?
  cat "$preflight_log"
  if [ "$preflight_status" -ne 0 ]; then
    echo "MANUAL $name: current process is not in the revoked Input Monitoring state; revoke it manually and rerun to automate this spot-check"
    manuals=$((manuals + 1))
    return 0
  fi

  for mode in full word; do
    attempts="$RETRIES"
    case "$attempts" in
      ''|*[!0-9]*) attempts=1 ;;
    esac
    [ "$attempts" -ge 1 ] || attempts=1

    attempt=1
    while [ "$attempt" -le "$attempts" ]; do
      sleep_ms "$GATE_PAUSE_MS"
      if [ "$attempts" -gt 1 ]; then
        log_file="$LOG_DIR/$name-$mode.attempt-$attempt.log"
        echo "-- $mode attempt $attempt/$attempts --"
      else
        log_file="$LOG_DIR/$name-$mode.log"
      fi

      env COMPME_ACCEPTANCE_REQUIRE_INPUT_MONITORING_REVOKED=1 COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" "$mode" >"$log_file" 2>&1
      status=$?
      cat "$log_file"

      if [ "$status" -eq 0 ]; then
        break
      fi

      if [ "$attempt" -lt "$attempts" ] && grep -q '^SUMMARY controls=' "$log_file"; then
        echo "RETRY $name-$mode ($status): control set mismatched — ambient key presses contaminate the system-wide hotkeys; keep hands off the keyboard"
        attempt=$((attempt + 1))
        continue
      fi

      echo "FAIL $name ($status): $(classify_blocker "$log_file")"
      failures=$((failures + 1))
      return "$status"
    done
  done

  echo "PASS $name"
  passes=$((passes + 1))
}

final_status() {
  if [ "$failures" -gt 0 ]; then
    return 1
  fi
  if [ "$incomplete_skips" -gt 0 ] && [ "$ALLOW_INCOMPLETE" -eq 0 ]; then
    return 1
  fi
  return 0
}

check_preflight() {
  preflight_log="$LOG_DIR/preflight.log"
  : >"$preflight_log"

  echo "Logs: $LOG_DIR"
  if ! command -v ioreg >/dev/null 2>&1; then
    echo "BLOCKER preflight: ioreg is unavailable; this runner is macOS-specific" | tee -a "$preflight_log"
    return 2
  fi

  ioreg -l -w 0 | grep -E 'IOConsoleUsers|CGS?SessionScreenIsLocked|kCGSSessionSecureInputPID|CGS?SessionScreenLockedTime' >"$preflight_log" || true
  cat "$preflight_log"

  locked=0
  secure=0
  grep -Eq 'CGS?SessionScreenIsLocked.*Yes' "$preflight_log" && locked=1
  secure_pid="$(sed -n 's/.*"kCGSSessionSecureInputPID"=\([0-9][0-9]*\).*/\1/p' "$preflight_log" | head -n 1)"
  [ -n "$secure_pid" ] && secure=1

  if [ "$locked" -eq 1 ]; then
    echo "BLOCKER preflight: screen is locked. Unlock the macOS session before rerunning."
  fi
  if [ "$secure" -eq 1 ]; then
    secure_owner="$(ps -p "$secure_pid" -o comm= 2>/dev/null | sed 's/^ *//')"
    echo "BLOCKER preflight: global Secure Input owner pid=$secure_pid ${secure_owner:-unknown}."
  fi

  if { [ "$locked" -eq 1 ] || [ "$secure" -eq 1 ]; } && [ "$FORCE" -eq 0 ]; then
    echo "Use --force only when you intentionally want raw blocked harness logs."
    return 2
  fi
  return 0
}

assert_classifies() {
  name="$1"
  body="$2"
  expected="$3"
  log_file="$self_test_dir/$name.log"
  printf '%s\n' "$body" >"$log_file"
  actual="$(classify_blocker "$log_file")"
  if [ "$actual" = "$expected" ]; then
    echo "PASS classify-$name"
    return 0
  fi
  echo "FAIL classify-$name: expected '$expected' got '$actual'" >&2
  return 1
}

assert_log_contains() {
  name="$1"
  file="$2"
  pattern="$3"
  if grep -Eq "$pattern" "$file"; then
    echo "PASS $name"
    return 0
  fi
  echo "FAIL $name: missing pattern $pattern" >&2
  return 1
}

run_self_tests() {
  self_test_dir="$(mktemp -d "${TMPDIR:-/tmp}/a1b-runner-tests.XXXXXX")"
  self_failures=0

  assert_classifies "locked-screen" 'CGSSessionScreenIsLocked = Yes' \
    "locked-screen: CGSSessionScreenIsLocked=Yes" || self_failures=$((self_failures + 1))
  assert_classifies "secure-input" 'kCGSSessionSecureInputPID=1234' \
    "secure-input: global Secure Input is enabled" || self_failures=$((self_failures + 1))
  assert_classifies "permission" 'PermissionMissing("Accessibility")' \
    "permission: check Accessibility grant" || self_failures=$((self_failures + 1))
  assert_classifies "focus-target" 'DIAG_ERROR no field observed' \
    "focus-target: focus an editable text field, not the app shell" || self_failures=$((self_failures + 1))
  assert_classifies "unknown" 'some unrelated output' \
    "unknown: see log" || self_failures=$((self_failures + 1))

  failures=0
  incomplete_skips=1
  ALLOW_INCOMPLETE=0
  if final_status; then
    echo "FAIL final-status-mandatory-skip" >&2
    self_failures=$((self_failures + 1))
  else
    echo "PASS final-status-mandatory-skip"
  fi
  ALLOW_INCOMPLETE=1
  if final_status; then
    echo "PASS final-status-allow-incomplete"
  else
    echo "FAIL final-status-allow-incomplete" >&2
    self_failures=$((self_failures + 1))
  fi

  missing_bin_log="$self_test_dir/require-bins-missing-input-monitoring.log"
  if (
    TEXTEDIT_BIN=/bin/sh
    ACCEPT_BIN=/bin/sh
    ACCEPT_INSERT_BIN=/bin/sh
    MARKER_BIN=/bin/sh
    OVERLAY_BIN=/bin/sh
    POPUP_FIXTURE_BIN=/bin/sh
    INPUT_MONITORING_BIN="$self_test_dir/missing-input-monitoring-helper"
    require_bins
  ) >"$missing_bin_log" 2>&1; then
    echo "FAIL require-bins-input-monitoring-missing" >&2
    self_failures=$((self_failures + 1))
  else
    assert_log_contains "require-bins-input-monitoring-missing" "$missing_bin_log" \
      'Missing example binary: .*/missing-input-monitoring-helper$' \
      || self_failures=$((self_failures + 1))
  fi

  fake_input_monitoring="$self_test_dir/fake-input-monitoring"
  fake_accept="$self_test_dir/fake-accept"
  fake_accept_log="$self_test_dir/fake-accept.invocations"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'echo "INPUT_MONITORING granted=false"' \
    'exit 0' >"$fake_input_monitoring"
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s\n" "$*" >>"$FAKE_ACCEPT_LOG"' \
    'echo "SUMMARY controls=expected"' \
    'exit 0' >"$fake_accept"
  chmod +x "$fake_input_monitoring" "$fake_accept"
  revoked_branch_log="$self_test_dir/input-monitoring-revoked-branch.log"
  if (
    DRY_RUN=0
    LOG_DIR="$self_test_dir/input-monitoring-revoked-branch-logs"
    mkdir -p "$LOG_DIR"
    INPUT_MONITORING_BIN="$fake_input_monitoring"
    ACCEPT_BIN="$fake_accept"
    FAKE_ACCEPT_LOG="$fake_accept_log"
    export FAKE_ACCEPT_LOG
    SHORT_TIMEOUT_MS=321
    POST_TAB_AFTER_MS=123
    GATE_PAUSE_MS=0
    RETRIES=1
    passes=0
    failures=0
    manuals=0
    run_input_monitoring_revoked_carbon_gate
    [ "$passes" -eq 1 ] && [ "$failures" -eq 0 ] && [ "$manuals" -eq 0 ]
  ) >"$revoked_branch_log" 2>&1; then
    assert_log_contains "input-monitoring-revoked-branch-full" "$fake_accept_log" '^321 full$' \
      || self_failures=$((self_failures + 1))
    assert_log_contains "input-monitoring-revoked-branch-word" "$fake_accept_log" '^321 word$' \
      || self_failures=$((self_failures + 1))
  else
    echo "FAIL input-monitoring-revoked-branch" >&2
    self_failures=$((self_failures + 1))
  fi

  failures=0
  skips=0
  incomplete_skips=0
  ALLOW_INCOMPLETE=0
  skip_e2e_gate "e2e-compme-pipeline" "self-test e2e skip" >/dev/null
  if final_status; then
    echo "FAIL final-status-e2e-skip-mandatory" >&2
    self_failures=$((self_failures + 1))
  else
    echo "PASS final-status-e2e-skip-mandatory"
  fi

  dry_run_log="$self_test_dir/default-dry-run.log"
  A1B_LOG_DIR="$self_test_dir/default-dry-run-logs" "$0" --dry-run >"$dry_run_log" 2>&1
  dry_run_status=$?
  if [ "$dry_run_status" -eq 0 ]; then
    echo "PASS default-dry-run-exits-zero"
  else
    echo "FAIL default-dry-run-exits-zero: $dry_run_status" >&2
    self_failures=$((self_failures + 1))
  fi
  for gate in \
    build-platform-macos-examples \
    build-compme \
    textedit-read \
    textedit-insert-synthetic \
    textedit-insert-clipboard \
    textedit-insert-axset \
    caret-marker-textedit-any \
    accept-insert-full \
    accept-insert-word \
    accept-insert-option-tab \
    e2e-compme-pipeline \
    e2e-compme-word-remainder \
    caret-marker-browser-marker \
    popup-fallback-fixture \
    accept-tap-inactive \
    accept-tap-full \
    accept-tap-word \
    accept-tap-escape \
    accept-tap-option-tab \
    accept-tap-cycle \
    accept-tap-delayed-hide \
    overlay-presenter; do
    assert_log_contains "default-dry-run-gate-$gate" "$dry_run_log" "^== $gate ==$" \
      || self_failures=$((self_failures + 1))
  done
  for gate in \
    encrypted-memory-all-monitored-live \
    input-monitoring-revoked-carbon-accept; do
    assert_log_contains "default-dry-run-manual-$gate" "$dry_run_log" "^MANUAL $gate:" \
      || self_failures=$((self_failures + 1))
  done
  assert_log_contains "default-dry-run-manual-all-monitored-residuals" "$dry_run_log" \
    '^MANUAL encrypted-memory-all-monitored-live: .*secure-input.*snoozed.*volatile-pid' \
    || self_failures=$((self_failures + 1))
  assert_log_contains "default-dry-run-optional-browser-skip" "$dry_run_log" \
    '^SKIP caret-marker-browser-marker: pass --browser-pid after focusing a Chrome/Safari text field$' \
    || self_failures=$((self_failures + 1))
  assert_log_contains "default-dry-run-input-monitoring-revoked-full-harness" "$dry_run_log" \
    'COMPME_ACCEPTANCE_REQUIRE_INPUT_MONITORING_REVOKED=1 .*accept_tap_acceptance .* full' \
    || self_failures=$((self_failures + 1))
  assert_log_contains "default-dry-run-input-monitoring-revoked-word-harness" "$dry_run_log" \
    'COMPME_ACCEPTANCE_REQUIRE_INPUT_MONITORING_REVOKED=1 .*accept_tap_acceptance .* word' \
    || self_failures=$((self_failures + 1))

  skip_textedit_log="$self_test_dir/skip-textedit-dry-run.log"
  A1B_LOG_DIR="$self_test_dir/skip-textedit-dry-run-logs" "$0" --dry-run --skip-textedit >"$skip_textedit_log" 2>&1
  skip_textedit_status=$?
  if [ "$skip_textedit_status" -eq 0 ]; then
    echo "PASS skip-textedit-dry-run-exits-zero"
  else
    echo "FAIL skip-textedit-dry-run-exits-zero: $skip_textedit_status" >&2
    self_failures=$((self_failures + 1))
  fi
  assert_log_contains "skip-textedit-option-tab-is-mandatory" "$skip_textedit_log" \
    '^SKIP accept-insert-option-tab: --skip-textedit$' \
    || self_failures=$((self_failures + 1))
  assert_log_contains "skip-textedit-counts-option-tab-incomplete" "$skip_textedit_log" \
    '^Summary: pass=0 fail=0 skip=11 incomplete=10 manual=2 logs=' \
    || self_failures=$((self_failures + 1))

  rm -rf "$self_test_dir"
  if [ "$self_failures" -gt 0 ]; then
    echo "Self-test failures: $self_failures" >&2
    return 1
  fi
  echo "Self-tests passed"
  return 0
}

resolve_textedit_pid() {
  if [ -n "$TEXTEDIT_PID" ]; then
    return 0
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    TEXTEDIT_PID="TEXTEDIT_PID"
    return 0
  fi
  TEXTEDIT_PID="$(pgrep -x TextEdit | head -n 1 || true)"
}

require_bins() {
  missing=0
  for bin in "$TEXTEDIT_BIN" "$ACCEPT_BIN" "$ACCEPT_INSERT_BIN" "$MARKER_BIN" "$OVERLAY_BIN" "$POPUP_FIXTURE_BIN" "$INPUT_MONITORING_BIN"; do
    if [ ! -x "$bin" ]; then
      echo "Missing example binary: $bin" >&2
      missing=1
    fi
  done
  return "$missing"
}

echo "A1b live acceptance runner"
echo "Root: $ROOT_DIR"

if [ "$SELF_TEST" -eq 1 ]; then
  run_self_tests
  exit $?
fi

if [ "$DRY_RUN" -eq 0 ]; then
  check_preflight
  preflight_status=$?
  if [ "$preflight_status" -ne 0 ]; then
    exit "$preflight_status"
  fi
else
  echo "DRY RUN: commands will not be executed."
fi

if [ "$SKIP_BUILD" -eq 0 ]; then
  run_gate "build-platform-macos-examples" cargo build -p platform_macos --examples
  run_gate "build-compme" cargo build -p app
fi

if [ "$DRY_RUN" -eq 0 ] && ! require_bins; then
  echo "Build the examples first or rerun without --skip-build." >&2
  exit 2
fi

if [ "$SKIP_TEXTEDIT" -eq 1 ]; then
  skip_gate "textedit-read" "--skip-textedit" mandatory
  skip_gate "textedit-insert-axset" "--skip-textedit" mandatory
  skip_gate "textedit-insert-synthetic" "--skip-textedit" mandatory
  skip_gate "textedit-insert-clipboard" "--skip-textedit" mandatory
  skip_gate "caret-marker-textedit-any" "--skip-textedit" mandatory
  skip_gate "accept-insert-full" "--skip-textedit" mandatory
  skip_gate "accept-insert-word" "--skip-textedit" mandatory
  skip_gate "accept-insert-option-tab" "--skip-textedit" mandatory
  skip_gate "e2e-compme-pipeline" "--skip-textedit" mandatory
  skip_gate "e2e-compme-word-remainder" "--skip-textedit" mandatory
else
  resolve_textedit_pid
  if [ -z "$TEXTEDIT_PID" ]; then
    skip_gate "textedit-read" "TextEdit is not running; open TextEdit and focus an editable document" mandatory
    skip_gate "textedit-insert-axset" "TextEdit is not running" mandatory
    skip_gate "textedit-insert-synthetic" "TextEdit is not running" mandatory
    skip_gate "textedit-insert-clipboard" "TextEdit is not running" mandatory
    skip_gate "caret-marker-textedit-any" "TextEdit is not running" mandatory
    skip_gate "accept-insert-full" "TextEdit is not running" mandatory
    skip_gate "accept-insert-word" "TextEdit is not running" mandatory
    skip_gate "accept-insert-option-tab" "TextEdit is not running" mandatory
    skip_gate "e2e-compme-pipeline" "TextEdit is not running" mandatory
    skip_gate "e2e-compme-word-remainder" "TextEdit is not running" mandatory
  else
    run_retryable_gate "textedit-read" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" "$TEXTEDIT_BIN" "$TIMEOUT_MS" read
    run_retryable_gate "textedit-insert-synthetic" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT-synthetic" "$TEXTEDIT_BIN" "$TIMEOUT_MS" synthetic
    run_retryable_gate "textedit-insert-clipboard" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT-clipboard" "$TEXTEDIT_BIN" "$TIMEOUT_MS" clipboard
    run_retryable_gate "textedit-insert-axset" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT" "$TEXTEDIT_BIN" "$TIMEOUT_MS" insert
    run_retryable_gate "caret-marker-textedit-any" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" "$MARKER_BIN" "$SHORT_TIMEOUT_MS" any
    run_retryable_gate "accept-insert-full" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_INSERT_BIN" "$SHORT_TIMEOUT_MS" full
    run_retryable_gate "accept-insert-word" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_INSERT_BIN" "$SHORT_TIMEOUT_MS" word
    run_retryable_gate "accept-insert-option-tab" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_INSERT_BIN" "$SHORT_TIMEOUT_MS" option-tab
    if [ "$SKIP_E2E" -eq 1 ]; then
      skip_e2e_gate "e2e-compme-pipeline" "--skip-e2e"
      skip_e2e_gate "e2e-compme-word-remainder" "--skip-e2e"
    elif [ ! -x "$COMPME_BIN" ]; then
      skip_e2e_gate "e2e-compme-pipeline" "compme binary not built (run: cargo build -p app)"
      skip_e2e_gate "e2e-compme-word-remainder" "compme binary not built (run: cargo build -p app)"
    else
      run_gate "e2e-compme-pipeline" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_E2E_ACCEPT=full "$E2E_SCRIPT"
      run_gate "e2e-compme-word-remainder" env COMPME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPME_E2E_ACCEPT=word "$E2E_SCRIPT"
    fi
  fi
fi

if [ -n "$BROWSER_PID" ]; then
  run_retryable_gate "caret-marker-browser-marker" env COMPME_ACCEPTANCE_PID="$BROWSER_PID" "$MARKER_BIN" "$SHORT_TIMEOUT_MS" marker
else
  skip_gate "caret-marker-browser-marker" "pass --browser-pid after focusing a Chrome/Safari text field"
fi

if [ -n "$POPUP_PID" ]; then
  run_retryable_gate "popup-fallback" env COMPME_ACCEPTANCE_PID="$POPUP_PID" COMPME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT-popup" "$TEXTEDIT_BIN" "$SHORT_TIMEOUT_MS" popup
else
  run_gate "popup-fallback-fixture" "$POPUP_FIXTURE_BIN" "$SHORT_TIMEOUT_MS"
fi

# Scripted Carbon accept gates [2026-06-11]: the harness pumps NSApp events so
# synthetic posts dispatch to the installed hotkey handler. inactive/option-tab/
# delayed-hide post an unconsumed key that lands in the frontmost app — keep an
# editable scratch target (TextEdit) frontmost so the stray Tab is harmless.
run_accept_tap_gate "accept-tap-inactive" env COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" inactive
run_accept_tap_gate "accept-tap-full" env COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" full
run_accept_tap_gate "accept-tap-word" env COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" word
run_accept_tap_gate "accept-tap-escape" env COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" escape
run_accept_tap_gate "accept-tap-option-tab" env COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" option-tab
run_accept_tap_gate "accept-tap-cycle" env COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" cycle
run_accept_tap_gate "accept-tap-delayed-hide" env COMPME_ACCEPTANCE_HIDE_AFTER_MS="$HIDE_AFTER_MS" COMPME_ACCEPTANCE_POST_TAB_AFTER_MS="$POST_TAB_AFTER_MS" "$ACCEPT_BIN" "$SHORT_TIMEOUT_MS" delayed-hide

run_gate "overlay-presenter" "$OVERLAY_BIN" "$TIMEOUT_MS"

manual_gate "encrypted-memory-all-monitored-live" "remaining residual after 2026-06-17 TextEdit and Chrome product-loop proofs: confirm secure-input, snoozed policy transition, and volatile-pid cases add no rows"
run_input_monitoring_revoked_carbon_gate

echo
echo "Summary: pass=$passes fail=$failures skip=$skips incomplete=$incomplete_skips manual=$manuals logs=$LOG_DIR"
if [ "$DRY_RUN" -eq 1 ]; then
  exit 0
fi
if ! final_status; then
  if [ "$failures" -eq 0 ] && [ "$incomplete_skips" -gt 0 ]; then
    echo "FAIL incomplete: mandatory gates skipped; rerun with TextEdit ready or pass --allow-incomplete intentionally" >&2
  fi
  exit 1
fi
exit 0
