#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

model="${COMPME_MODEL_GATE_PATH:-tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf}"
url="${COMPME_MODEL_GATE_URL:-https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf}"
expected="${COMPME_MODEL_GATE_SHA256:-ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484}"

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-model-gates.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  fake_bin="$tmp/bin"
  mkdir -p "$fake_bin" "$tmp/model-dir"
  cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s env=%s ctx=%s gpu=%s ctx_tokens=%s args=%s\n' "$PWD" "${COMPME_REQUIRE_MODEL_TESTS:-}" "${COMPME_REQUIRE_MODEL_CONTEXT:-}" "${COMPME_MODEL_GPU_LAYERS:-}" "${COMPME_MODEL_CONTEXT_TOKENS:-}" "$*" >>"$COMPME_MODEL_GATE_CARGO_LOG"
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
  grep -q 'env=1 ctx=1 gpu=0 ctx_tokens=256 args=test -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"
  grep -q 'tools/spike env=1 ctx= gpu= ctx_tokens= args=test --test model_integration -- --ignored --test-threads=1' "$tmp/cargo.log"

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
  grep -q 'env=1 ctx=1 gpu=0 ctx_tokens=256 args=test -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"
  grep -q 'tools/spike env=1 ctx= gpu= ctx_tokens= args=test --test model_integration -- --ignored --test-threads=1' "$tmp/cargo.log"

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
  grep -q 'env=1 ctx=1 gpu=0 ctx_tokens=256 args=test -p model_client --test latency -- --ignored --test-threads=1' "$tmp/cargo.log"

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

COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 cargo test -p model_client --test latency -- --ignored --test-threads=1
(
  cd "$repo_root/tools/spike"
  COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1
)
