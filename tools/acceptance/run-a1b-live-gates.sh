#!/usr/bin/env bash
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DRY_RUN=0
FORCE=0
SKIP_BUILD=0
SKIP_TEXTEDIT=0
SKIP_E2E=0
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
  --skip-e2e             Do not run the end-to-end complete-me pipeline gate.
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

Production accept keys use transient Carbon hotkeys. macOS synthetic key posts
(`System Events` / `CGEventPost`) do not exercise `RegisterEventHotKey` the way
physical keypresses do, so Carbon accept/consume gates are recorded as manual
physical-key gates rather than automated pass/fail checks.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --force) FORCE=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --skip-textedit) SKIP_TEXTEDIT=1 ;;
    --skip-e2e) SKIP_E2E=1 ;;
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
COMPLETE_ME_BIN="$ROOT_DIR/target/debug/complete-me"
E2E_SCRIPT="$ROOT_DIR/tools/acceptance/e2e-complete-me.sh"

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
  echo
  echo "== $name =="
  echo "SKIP $name: $reason"
  skips=$((skips + 1))
}

manual_gate() {
  name="$1"
  reason="$2"
  echo
  echo "== $name =="
  echo "MANUAL $name: $reason"
  manuals=$((manuals + 1))
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
  for bin in "$TEXTEDIT_BIN" "$ACCEPT_BIN" "$ACCEPT_INSERT_BIN" "$MARKER_BIN" "$OVERLAY_BIN" "$POPUP_FIXTURE_BIN"; do
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
  run_gate "build-complete-me" cargo build -p app
fi

if [ "$DRY_RUN" -eq 0 ] && ! require_bins; then
  echo "Build the examples first or rerun without --skip-build." >&2
  exit 2
fi

if [ "$SKIP_TEXTEDIT" -eq 1 ]; then
  skip_gate "textedit-read" "--skip-textedit"
  skip_gate "textedit-insert-axset" "--skip-textedit"
  skip_gate "textedit-insert-synthetic" "--skip-textedit"
  skip_gate "textedit-insert-clipboard" "--skip-textedit"
  skip_gate "caret-marker-textedit-any" "--skip-textedit"
  skip_gate "accept-insert-full" "--skip-textedit"
  skip_gate "accept-insert-word" "--skip-textedit"
  skip_gate "e2e-complete-me-pipeline" "--skip-textedit"
  skip_gate "e2e-complete-me-word-remainder" "--skip-textedit"
else
  resolve_textedit_pid
  if [ -z "$TEXTEDIT_PID" ]; then
    skip_gate "textedit-read" "TextEdit is not running; open TextEdit and focus an editable document"
    skip_gate "textedit-insert-axset" "TextEdit is not running"
    skip_gate "textedit-insert-synthetic" "TextEdit is not running"
    skip_gate "textedit-insert-clipboard" "TextEdit is not running"
    skip_gate "caret-marker-textedit-any" "TextEdit is not running"
    skip_gate "accept-insert-full" "TextEdit is not running"
    skip_gate "accept-insert-word" "TextEdit is not running"
    skip_gate "e2e-complete-me-pipeline" "TextEdit is not running"
    skip_gate "e2e-complete-me-word-remainder" "TextEdit is not running"
  else
    run_retryable_gate "textedit-read" env COMPLETE_ME_ACCEPTANCE_PID="$TEXTEDIT_PID" "$TEXTEDIT_BIN" "$TIMEOUT_MS" read
    run_retryable_gate "textedit-insert-synthetic" env COMPLETE_ME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPLETE_ME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT-synthetic" "$TEXTEDIT_BIN" "$TIMEOUT_MS" synthetic
    run_retryable_gate "textedit-insert-clipboard" env COMPLETE_ME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPLETE_ME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT-clipboard" "$TEXTEDIT_BIN" "$TIMEOUT_MS" clipboard
    run_retryable_gate "textedit-insert-axset" env COMPLETE_ME_ACCEPTANCE_PID="$TEXTEDIT_PID" COMPLETE_ME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT" "$TEXTEDIT_BIN" "$TIMEOUT_MS" insert
    run_retryable_gate "caret-marker-textedit-any" env COMPLETE_ME_ACCEPTANCE_PID="$TEXTEDIT_PID" "$MARKER_BIN" "$SHORT_TIMEOUT_MS" any
    manual_gate "accept-insert-full" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
    manual_gate "accept-insert-word" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
    manual_gate "e2e-complete-me-pipeline" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
    manual_gate "e2e-complete-me-word-remainder" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
  fi
fi

if [ -n "$BROWSER_PID" ]; then
  run_retryable_gate "caret-marker-browser-marker" env COMPLETE_ME_ACCEPTANCE_PID="$BROWSER_PID" "$MARKER_BIN" "$SHORT_TIMEOUT_MS" marker
else
  skip_gate "caret-marker-browser-marker" "pass --browser-pid after focusing a Chrome/Safari text field"
fi

if [ -n "$POPUP_PID" ]; then
  run_retryable_gate "popup-fallback" env COMPLETE_ME_ACCEPTANCE_PID="$POPUP_PID" COMPLETE_ME_ACCEPTANCE_INSERT_TEXT="$INSERT_TEXT-popup" "$TEXTEDIT_BIN" "$SHORT_TIMEOUT_MS" popup
else
  run_gate "popup-fallback-fixture" "$POPUP_FIXTURE_BIN" "$SHORT_TIMEOUT_MS"
fi

manual_gate "accept-tap-inactive" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
manual_gate "accept-tap-full" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
manual_gate "accept-tap-word" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
manual_gate "accept-tap-escape" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
manual_gate "accept-tap-option-tab" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
manual_gate "accept-tap-cycle" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"
manual_gate "accept-tap-delayed-hide" "physical Carbon hotkey gate; synthetic key posts do not fire RegisterEventHotKey"

run_gate "overlay-presenter" "$OVERLAY_BIN" "$TIMEOUT_MS"

echo
echo "Summary: pass=$passes fail=$failures skip=$skips manual=$manuals logs=$LOG_DIR"
if [ "$DRY_RUN" -eq 1 ]; then
  exit 0
fi
if [ "$failures" -gt 0 ]; then
  exit 1
fi
exit 0
