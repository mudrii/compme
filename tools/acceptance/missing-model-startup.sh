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

[ "$status" -eq 0 ] || fail "compme exited with status $status (log: $LOG)"
grep -q '^compme: model unavailable at startup:' "$LOG" \
  || fail "missing startup-unavailable log (log: $LOG)"
grep -q '^compme: setup remains available; download or select a model, then relaunch$' "$LOG" \
  || fail "missing setup recovery log (log: $LOG)"
grep -q '^compme: setup: Model file not ready$' "$LOG" \
  || fail "missing setup model-not-ready log (log: $LOG)"
if grep -Eq '^compme: request gen=' "$LOG"; then
  fail "completion request was submitted without a model (log: $LOG)"
fi

log_lines="$(wc -l <"$LOG" | tr -d '[:space:]')"
log_bytes="$(wc -c <"$LOG" | tr -d '[:space:]')"
echo "missing-model startup PASS: log=$LOG log_lines=$log_lines log_bytes=$log_bytes"
