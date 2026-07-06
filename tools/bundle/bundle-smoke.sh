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
printf 'compme COMPME_RUN_MS=%s args=%s\n' "${COMPME_RUN_MS:-}" "$*" >>"$COMPME_BUNDLE_SMOKE_SELF_TEST_LOG"
exit "${COMPME_BUNDLE_SMOKE_APP_EXIT:-0}"
APP
chmod +x "$app/Contents/MacOS/compme"
SH
  chmod +x "$fake_make_app"

  COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$log" \
    "$0" "$tmp_dir/out" >"$tmp_dir/stdout"
  grep -Fq "make-app $tmp_dir/out" "$log"
  grep -Fq "compme COMPME_RUN_MS=1500 args=" "$log"

  custom_log="$tmp_dir/custom.log"
  COMPME_RUN_MS=77 \
    COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$custom_log" \
    "$0" "$tmp_dir/custom-out" >"$tmp_dir/stdout-custom"
  grep -Fq "compme COMPME_RUN_MS=77 args=" "$custom_log"

  fail_log="$tmp_dir/fail.log"
  if COMPME_BUNDLE_SMOKE_REPO_ROOT="$fake_repo" \
    COMPME_BUNDLE_SMOKE_MAKE_APP="$fake_make_app" \
    COMPME_BUNDLE_SMOKE_SELF_TEST_LOG="$fail_log" \
    COMPME_BUNDLE_SMOKE_APP_EXIT=42 \
    "$0" "$tmp_dir/fail-out" >"$tmp_dir/stdout-fail" 2>"$tmp_dir/stderr-fail"; then
    echo "bundle smoke self-test failed: app failure was accepted" >&2
    return 1
  fi
  grep -Fq "compme COMPME_RUN_MS=1500 args=" "$fail_log"

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

COMPME_RUN_MS="${COMPME_RUN_MS:-1500}" COMPME_STUB_COMPLETION="${COMPME_STUB_COMPLETION:- smoke}" "$app_bin"
