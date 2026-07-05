#!/usr/bin/env bash
# Product-boundary smoke for first-run/stale-model recovery.
#
# Runs the compiled binary with a missing COMPME_MODEL_PATH and a bounded
# lifetime. Pass means startup remains nonfatal, setup recovery guidance is
# logged, and no completion request is submitted while the model is unavailable.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${COMPME_BIN:-$ROOT_DIR/target/debug/compme}"
RUN_MS="${COMPME_MISSING_MODEL_RUN_MS:-500}"
LOG="${COMPME_MISSING_MODEL_LOG:-$ROOT_DIR/tools/acceptance/logs/missing-model-startup-$(date +%Y%m%d-%H%M%S).log}"

fail() {
  echo "missing-model startup FAIL: $*" >&2
  exit 1
}

validate_log() {
  status="$1"
  log_file="$2"
  [ "$status" -eq 0 ] || fail "compme exited with status $status (log: $log_file)"
  grep -q '^compme: model unavailable at startup:' "$log_file" \
    || fail "missing startup-unavailable log (log: $log_file)"
  grep -q '^compme: setup remains available; download or select a model, then relaunch$' "$log_file" \
    || fail "missing setup recovery log (log: $log_file)"
  grep -q '^compme: setup: Model file not ready$' "$log_file" \
    || fail "missing setup model-not-ready log (log: $log_file)"
  # A missing model must leave the app Blocked (no suggestions). On a live,
  # untrusted run the higher-ranked Blocked(Permission) wins over
  # Blocked(ModelUnavailable) (derive_status ordering), so accept either —
  # both prove the app is blocked. The self-test fake emits ModelUnavailable.
  grep -Eq '^compme: status=Blocked\((ModelUnavailable|Permission)\)' "$log_file" \
    || fail "missing blocked status log (log: $log_file)"
  if grep -Eq '^compme: request gen=' "$log_file"; then
    fail "completion request was submitted without a model (log: $log_file)"
  fi
}

run_self_test() {
  tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t compme-missing-model-self-test)"
  trap 'rm -rf "$tmp_dir"' EXIT
  fake_bin="$tmp_dir/fake-compme"
  cat >"$fake_bin" <<'SH'
#!/usr/bin/env bash
mode="${COMPME_FAKE_MODE:-ok}"
[ "$mode" = omit-startup ] || printf '%s\n' 'compme: model unavailable at startup: model file not found: /tmp/missing.gguf'
[ "$mode" = omit-recovery ] || printf '%s\n' 'compme: setup remains available; download or select a model, then relaunch'
[ "$mode" = omit-setup ] || printf '%s\n' 'compme: setup: Model file not ready'
[ "$mode" = omit-status ] || printf '%s\n' 'compme: status=Blocked(ModelUnavailable) enabled=false snoozed=false'
case "$mode" in
  request) printf '%s\n' 'compme: request gen=1 prompt_chars=10' ;;
  bad-exit) exit 7 ;;
esac
SH
  chmod +x "$fake_bin"

  if COMPME_BIN="$fake_bin" COMPME_MISSING_MODEL_LOG="$tmp_dir/ok.log" "$0" >/dev/null; then
    echo "PASS self-test-missing-model-startup-success"
  else
    echo "FAIL self-test-missing-model-startup-success" >&2
    exit 1
  fi

  if COMPME_FAKE_MODE=request COMPME_BIN="$fake_bin" COMPME_MISSING_MODEL_LOG="$tmp_dir/request.log" "$0" >"$tmp_dir/request.out" 2>&1; then
    echo "FAIL self-test-missing-model-startup-request-rejected: request passed" >&2
    exit 1
  elif grep -q 'completion request was submitted without a model' "$tmp_dir/request.out"; then
    echo "PASS self-test-missing-model-startup-request-rejected"
  else
    echo "FAIL self-test-missing-model-startup-request-rejected: expected error missing" >&2
    cat "$tmp_dir/request.out" >&2
    exit 1
  fi

  if COMPME_FAKE_MODE=bad-exit COMPME_BIN="$fake_bin" COMPME_MISSING_MODEL_LOG="$tmp_dir/bad-exit.log" "$0" >"$tmp_dir/bad-exit.out" 2>&1; then
    echo "FAIL self-test-missing-model-startup-exit-status: bad exit passed" >&2
    exit 1
  elif grep -q 'compme exited with status 7' "$tmp_dir/bad-exit.out"; then
    echo "PASS self-test-missing-model-startup-exit-status"
  else
    echo "FAIL self-test-missing-model-startup-exit-status: expected error missing" >&2
    cat "$tmp_dir/bad-exit.out" >&2
    exit 1
  fi

  assert_missing_line_rejected() {
    mode="$1"
    expected="$2"
    label="$3"
    if COMPME_FAKE_MODE="$mode" COMPME_BIN="$fake_bin" COMPME_MISSING_MODEL_LOG="$tmp_dir/$mode.log" "$0" >"$tmp_dir/$mode.out" 2>&1; then
      echo "FAIL self-test-missing-model-startup-$label: missing line passed" >&2
      exit 1
    elif grep -q "$expected" "$tmp_dir/$mode.out"; then
      echo "PASS self-test-missing-model-startup-$label"
    else
      echo "FAIL self-test-missing-model-startup-$label: expected error missing" >&2
      cat "$tmp_dir/$mode.out" >&2
      exit 1
    fi
  }

  assert_missing_line_rejected omit-startup 'missing startup-unavailable log' startup-log-required
  assert_missing_line_rejected omit-recovery 'missing setup recovery log' recovery-log-required
  assert_missing_line_rejected omit-setup 'missing setup model-not-ready log' setup-log-required
  assert_missing_line_rejected omit-status 'missing blocked status log' status-log-required

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp_dir/self-test-argc.err"; then
    echo "FAIL self-test-missing-model-startup-argc: extra self-test argument passed" >&2
    exit 1
  fi
  grep -Fq "usage: missing-model-startup.sh" "$tmp_dir/self-test-argc.err"
  echo "PASS self-test-missing-model-startup-argc"
  if "$0" unexpected-extra >/dev/null 2>"$tmp_dir/normal-argc.err"; then
    echo "FAIL self-test-missing-model-startup-normal-argc: extra normal argument passed" >&2
    exit 1
  fi
  grep -Fq "usage: missing-model-startup.sh" "$tmp_dir/normal-argc.err"
  echo "PASS self-test-missing-model-startup-normal-argc"

  echo "Missing-model startup self-tests passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    echo "usage: missing-model-startup.sh [--self-test]" >&2
    exit 2
  fi
  run_self_test
  exit 0
fi

if [ "$#" -ne 0 ]; then
  echo "usage: missing-model-startup.sh [--self-test]" >&2
  exit 2
fi

[ -x "$BIN" ] || fail "binary not built: $BIN (run: cargo build -p app)"

tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t compme-missing-model)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

mkdir -p "$(dirname "$LOG")"
missing_model="$tmp_dir/missing.gguf"

status=0
env -u COMPME_STUB_COMPLETION \
  COMPME_MODEL_PATH="$missing_model" \
  COMPME_RUN_MS="$RUN_MS" \
  COMPME_ENABLED=false \
  "$BIN" >"$LOG" 2>&1 || status=$?

validate_log "$status" "$LOG"

log_lines="$(wc -l <"$LOG" | tr -d '[:space:]')"
log_bytes="$(wc -c <"$LOG" | tr -d '[:space:]')"
echo "missing-model startup PASS: log=$LOG log_lines=$log_lines log_bytes=$log_bytes"
