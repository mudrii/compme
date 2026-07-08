#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

default_model="tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"
default_url="https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf"
default_expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"

release_context() {
  [ "${GITHUB_ACTIONS:-}" = "true" ] && [ "${GITHUB_REF_TYPE:-}" = "tag" ]
}

reject_release_overrides() {
  if release_context && [ "${COMPME_ALLOW_MODEL_GATE_OVERRIDE:-}" != "1" ]; then
    for name in COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256; do
      if [ -n "${!name:-}" ]; then
        echo "refusing $name override in GitHub release context; set COMPME_ALLOW_MODEL_GATE_OVERRIDE=1 for an intentional recovery run" >&2
        return 1
      fi
    done
  fi
}

model="${COMPME_MODEL_GATE_PATH:-$default_model}"
url="${COMPME_MODEL_GATE_URL:-$default_url}"
expected="${COMPME_MODEL_GATE_SHA256:-$default_expected}"

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-model-gates.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  unset GITHUB_ACTIONS GITHUB_REF_TYPE COMPME_ALLOW_MODEL_GATE_OVERRIDE
  unset COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256
  unset COMPME_MODEL_GATE_CARGO_FAIL COMPME_MODEL_GATE_CURL_FAIL COMPME_MODEL_GATE_CURL_BODY

  fake_bin="$tmp/bin"
  mkdir -p "$fake_bin" "$tmp/model-dir"
  cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s env=%s ctx=%s latency=%s gpu=%s ctx_tokens=%s spike_model=%s args=%s\n' "$PWD" "${COMPME_REQUIRE_MODEL_TESTS:-}" "${COMPME_REQUIRE_MODEL_CONTEXT:-}" "${COMPME_REQUIRE_LATENCY_BUDGET:-}" "${COMPME_MODEL_GPU_LAYERS:-}" "${COMPME_MODEL_CONTEXT_TOKENS:-}" "${COMPME_SPIKE_MODEL_PATH:-}" "$*" >>"$COMPME_MODEL_GATE_CARGO_LOG"
if [ -n "${COMPME_MODEL_GATE_CARGO_FAIL:-}" ]; then
  exit 43
fi
SH
  cat >"$fake_bin/curl" <<'SH'
#!/usr/bin/env bash
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--output" ]; then
    out="$arg"
  fi
  prev="$arg"
done
printf 'curl %s\n' "$*" >>"$COMPME_MODEL_GATE_CURL_LOG"
if [ -n "${COMPME_MODEL_GATE_CURL_FAIL:-}" ]; then
  exit 42
fi
printf '%s' "${COMPME_MODEL_GATE_CURL_BODY:-downloaded-model}" >"$out"
SH
  chmod +x "$fake_bin/cargo" "$fake_bin/curl"

  model_path="$tmp/model-dir/model.gguf"
  printf 'cached-model' >"$model_path"
  cached_sha="$(shasum -a 256 "$model_path" | awk '{print $1}')"
  if PATH="$fake_bin:$PATH" \
    GITHUB_ACTIONS=true \
    GITHUB_REF_TYPE=tag \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null 2>"$tmp/release-override.err"; then
    echo "run-model-gates self-test failed: release override was accepted" >&2
    return 1
  fi
  grep -q 'refusing COMPME_MODEL_GATE_PATH override in GitHub release context' "$tmp/release-override.err"

  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  PATH="$fake_bin:$PATH" \
    GITHUB_ACTIONS=true \
    GITHUB_REF_TYPE=tag \
    COMPME_ALLOW_MODEL_GATE_OVERRIDE=1 \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null
  grep -q 'env=1 ctx=1 latency=1 gpu=0 ctx_tokens=256 spike_model= args=test --locked -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"

  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null
  test ! -s "$tmp/curl.log"
  grep -q 'env=1 ctx=1 latency=1 gpu=0 ctx_tokens=256 spike_model= args=test --locked -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"
  grep -q "tools/spike env=1 ctx= latency=1 gpu= ctx_tokens= spike_model=$model_path args=test --locked --test model_integration -- --ignored --test-threads=1" "$tmp/cargo.log"

  rm -f "$model_path"
  downloaded_sha="$(printf 'downloaded-model' | shasum -a 256 | awk '{print $1}')"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$downloaded_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
  "$0" >/dev/null
  grep -q 'curl -L --fail --retry 3 --retry-delay 5 --output' "$tmp/curl.log"
  test "$(cat "$model_path")" = "downloaded-model"
  grep -q 'env=1 ctx=1 latency=1 gpu=0 ctx_tokens=256 spike_model= args=test --locked -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"
  grep -q "tools/spike env=1 ctx= latency=1 gpu= ctx_tokens= spike_model=$model_path args=test --locked --test model_integration -- --ignored --test-threads=1" "$tmp/cargo.log"

  rm -f "$model_path"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
    COMPME_MODEL_GATE_CURL_BODY="wrong-model" \
    "$0" >/dev/null 2>"$tmp/fail.err"; then
    echo "run-model-gates self-test failed: checksum failure was accepted" >&2
    return 1
  fi
  test ! -e "$model_path"

  rm -f "$model_path"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$downloaded_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
    COMPME_MODEL_GATE_CURL_FAIL=1 \
    "$0" >/dev/null 2>"$tmp/curl-fail.err"; then
    echo "run-model-gates self-test failed: curl failure was accepted" >&2
    return 1
  fi
  grep -q 'curl -L --fail --retry 3 --retry-delay 5 --output' "$tmp/curl.log"
  test ! -e "$model_path"
  test ! -s "$tmp/cargo.log"

  printf 'cached-model' >"$model_path"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_MODEL_GATE_CARGO_LOG="$tmp/cargo.log" \
    COMPME_MODEL_GATE_CURL_LOG="$tmp/curl.log" \
    COMPME_MODEL_GATE_CARGO_FAIL=1 \
    "$0" >/dev/null 2>"$tmp/cargo-fail.err"; then
    echo "run-model-gates self-test failed: cargo failure was accepted" >&2
    return 1
  fi
  test ! -s "$tmp/curl.log"
  grep -q 'env=1 ctx=1 latency=1 gpu=0 ctx_tokens=256 spike_model= args=test --locked -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "run-model-gates self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: .*run-model-gates\.sh \[--self-test\]$' "$tmp/self-test-argc.err"
  if "$0" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "run-model-gates self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: .*run-model-gates\.sh \[--self-test\]$' "$tmp/normal-argc.err"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    echo "usage: $0 [--self-test]" >&2
    exit 2
  fi
  run_self_test
  exit 0
fi
if [ "$#" -ne 0 ]; then
  echo "usage: $0 [--self-test]" >&2
  exit 2
fi

reject_release_overrides

cd "$repo_root"

mkdir -p "$(dirname "$model")"

if [ -s "$model" ] && printf '%s  %s\n' "$expected" "$model" | shasum -a 256 -c - >/dev/null 2>&1; then
  echo "model-backed test GGUF already verified: $model"
else
  tmp="$(mktemp "${model}.XXXXXX")"
  cleanup() {
    rm -f "$tmp"
  }
  trap cleanup EXIT
  curl -L --fail --retry 3 --retry-delay 5 --output "$tmp" "$url"
  printf '%s  %s\n' "$expected" "$tmp" | shasum -a 256 -c -
  mv "$tmp" "$model"
  trap - EXIT
fi

printf '%s  %s\n' "$expected" "$model" | shasum -a 256 -c -

case "$model" in
  /*) spike_model="$model" ;;
  *) spike_model="$repo_root/$model" ;;
esac

# The latency budget is enforced by default (the pre-tag run on a real,
# Metal-capable Mac — RELEASING step 3). Hosted CI runners are virtualized
# without usable Metal and can never meet it, so the release workflow runs
# with COMPME_REQUIRE_LATENCY_BUDGET=0: correctness still asserted, timing
# evidence comes from the mandatory local run.
require_latency_budget="${COMPME_REQUIRE_LATENCY_BUDGET:-1}"
COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET="$require_latency_budget" cargo test --locked -p model_client --test latency -- --ignored --test-threads=1
(
  cd "$repo_root/tools/spike"
  COMPME_SPIKE_MODEL_PATH="$spike_model" COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_LATENCY_BUDGET="$require_latency_budget" cargo test --locked --test model_integration -- --ignored --test-threads=1
)
