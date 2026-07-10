#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
make_app="${COMPME_BUNDLE_SMOKE_MAKE_APP:-$repo_root/tools/bundle/make-app.sh}"

usage() {
  echo "usage: tools/bundle/bundle-smoke.sh [output-dir] | --self-test" >&2
}

run_self_test() {
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/compme-bundle-smoke.XXXXXX")"
  cleanup() {
    rm -rf "$tmp_dir"
  }
  trap cleanup EXIT

  fake_repo="$tmp_dir/repo"
  fake_make_app="$tmp_dir/make-app.sh"
  log="$tmp_dir/commands.log"
  mkdir -p "$fake_repo"

  cat >"$fake_make_app" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make-app %s\n' "$*" >>"$COMPME_BUNDLE_SMOKE_SELF_TEST_LOG"
out_dir="${1:-$COMPME_BUNDLE_SMOKE_REPO_ROOT/target/bundle}"
app="$out_dir/Compme.app"
mkdir -p "$app/Contents/MacOS"
cat >"$app/Contents/MacOS/compme" <<'APP'
#!/usr/bin/env bash
set -euo pipefail
printf 'compme COMPME_RUN_MS=%s COMPME_STUB_COMPLETION=%s COMPME_CONFIG=%s args=%s\n' "${COMPME_RUN_MS:-}" "${COMPME_STUB_COMPLETION:-}" "${COMPME_CONFIG:-}" "$*" >>"$COMPME_BUNDLE_SMOKE_SELF_TEST_LOG"
if [ "${COMPME_BUNDLE_SMOKE_APP_DUPLICATE:-0}" = 1 ]; then
  echo 'compme: another instance is already running — exiting' >&2
  exit 0
fi
acceptance_pid=None
if [ -n "${COMPME_ACCEPTANCE_PID:-}" ]; then
  acceptance_pid="Some(${COMPME_ACCEPTANCE_PID})"
fi
printf 'compme: running (acceptance_pid=%s stub=true run_ms=Some(%s))\n' "$acceptance_pid" "${COMPME_RUN_MS:-}" >&2
exit "${COMPME_BUNDLE_SMOKE_APP_EXIT:-0}"
APP
chmod +x "$app/Contents/MacOS/compme"
SH
  chmod +x "$fake_make_app"

  COMPME_RUN_MS= \
    COMPME_STUB_COMPLETION= \
    COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$log" \
    "$0" "$tmp_dir/out" >"$tmp_dir/stdout"
  grep -Fq "make-app $tmp_dir/out" "$log"
  grep -Eq "compme COMPME_RUN_MS=1500 COMPME_STUB_COMPLETION= smoke COMPME_CONFIG=.+ args=" "$log"

  hostile_log="$tmp_dir/hostile.log"
  if ! COMPME_ACCEPTANCE_PID=444 \
    COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$hostile_log" \
    "$0" "$tmp_dir/hostile-out" >"$tmp_dir/stdout-hostile" 2>"$tmp_dir/stderr-hostile"; then
    echo "bundle smoke self-test failed: hostile product environment leaked into app" >&2
    return 1
  fi

  custom_log="$tmp_dir/custom.log"
  COMPME_RUN_MS=77 \
    COMPME_STUB_COMPLETION= \
    COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$custom_log" \
    "$0" "$tmp_dir/custom-out" >"$tmp_dir/stdout-custom"
  grep -Eq "compme COMPME_RUN_MS=77 COMPME_STUB_COMPLETION= smoke COMPME_CONFIG=.+ args=" "$custom_log"

  custom_stub_log="$tmp_dir/custom-stub.log"
  COMPME_RUN_MS= \
    COMPME_STUB_COMPLETION=" custom" \
    COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$custom_stub_log" \
    "$0" "$tmp_dir/custom-stub-out" >"$tmp_dir/stdout-custom-stub"
  grep -Eq "compme COMPME_RUN_MS=1500 COMPME_STUB_COMPLETION= custom COMPME_CONFIG=.+ args=" "$custom_stub_log"

  fail_log="$tmp_dir/fail.log"
  if COMPME_RUN_MS= \
    COMPME_STUB_COMPLETION= \
    COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$fail_log" \
    COMPME_BUNDLE_SMOKE_APP_EXIT=42 \
    "$0" "$tmp_dir/fail-out" >"$tmp_dir/stdout-fail" 2>"$tmp_dir/stderr-fail"; then
    echo "bundle smoke self-test failed: app failure was accepted" >&2
    return 1
  fi
  grep -Eq "compme COMPME_RUN_MS=1500 COMPME_STUB_COMPLETION= smoke COMPME_CONFIG=.+ args=" "$fail_log"

  duplicate_log="$tmp_dir/duplicate.log"
  if COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$duplicate_log" \
    COMPME_BUNDLE_SMOKE_APP_DUPLICATE=1 \
    "$0" "$tmp_dir/duplicate-out" >"$tmp_dir/stdout-duplicate" 2>"$tmp_dir/stderr-duplicate"; then
    echo "bundle smoke self-test failed: duplicate-instance zero exit was accepted" >&2
    return 1
  fi
  grep -Fq "another instance is already running" "$tmp_dir/stdout-duplicate"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp_dir/self-test-argc.err"; then
    echo "bundle smoke self-test failed: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: tools/bundle/bundle-smoke.sh" "$tmp_dir/self-test-argc.err"

  if "$0" "$tmp_dir/out-extra" unexpected-extra >/dev/null 2>"$tmp_dir/normal-argc.err"; then
    echo "bundle smoke self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: tools/bundle/bundle-smoke.sh" "$tmp_dir/normal-argc.err"

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  if [[ "$#" -ne 1 ]]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi

if [[ "$#" -gt 1 ]]; then
  usage
  exit 2
fi

out_dir="${1:-"$repo_root/target/bundle"}"
"$make_app" "$out_dir"

app_bin="$out_dir/Compme.app/Contents/MacOS/compme"
if [[ ! -x "$app_bin" ]]; then
  echo "missing bundle executable: $app_bin" >&2
  exit 1
fi

runtime_dir="$(mktemp -d "${TMPDIR:-/tmp}/compme-bundle-smoke-run.XXXXXX")"
cleanup_runtime() {
  rm -rf "$runtime_dir"
}
trap cleanup_runtime EXIT

run_ms="${COMPME_RUN_MS:-1500}"
app_env=(
  env -i
  "PATH=${PATH:-/usr/bin:/bin}"
  "HOME=${HOME:-$runtime_dir}"
  "TMPDIR=${TMPDIR:-/tmp}"
  "COMPME_CONFIG=$runtime_dir/config.env"
  "COMPME_RUN_MS=$run_ms"
  "COMPME_STUB_COMPLETION=${COMPME_STUB_COMPLETION:- smoke}"
)
for self_test_var in \
  COMPME_BUNDLE_SMOKE_SELF_TEST_LOG \
  COMPME_BUNDLE_SMOKE_APP_EXIT \
  COMPME_BUNDLE_SMOKE_APP_DUPLICATE; do
  if [[ "${!self_test_var+x}" == x ]]; then
    app_env+=("$self_test_var=${!self_test_var}")
  fi
done
status=0
output="$(
  "${app_env[@]}" "$app_bin" 2>&1
)" || status=$?
printf '%s\n' "$output"
if [[ "$status" -ne 0 ]]; then
  echo "bundle smoke failed: app exited with status $status" >&2
  exit "$status"
fi
if grep -Fq 'compme: another instance is already running — exiting' <<<"$output"; then
  echo "bundle smoke failed: isolated app exited as a duplicate instance" >&2
  exit 1
fi
if ! grep -Fq "compme: running (acceptance_pid=None stub=true run_ms=Some($run_ms))" <<<"$output"; then
  echo "bundle smoke failed: app never reached the bounded stub runtime" >&2
  exit 1
fi
