#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

default_model="tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"
default_url="https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf"
default_expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"
default_corpus="tools/release/quality-corpus.jsonl"

# Measured baseline with the pinned GGUF (2026-07-20, macOS, CPU-forced greedy
# decoding): 20/21 cases pass (95%) — comfortably above the harness's 80%
# threshold (17 of 21 must pass). The one failing case (typo-occured) is the
# documented honest miss, absorbed by the threshold by design.

release_context() {
  [ "${GITHUB_ACTIONS:-}" = "true" ] && [ "${GITHUB_REF_TYPE:-}" = "tag" ]
}

reject_release_overrides() {
  if release_context && [ "${COMPME_ALLOW_MODEL_GATE_OVERRIDE:-}" != "1" ]; then
    for name in COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256 COMPME_QUALITY_CORPUS; do
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
corpus="${COMPME_QUALITY_CORPUS:-$default_corpus}"

run_self_test() {
  for name in \
    GITHUB_ACTIONS GITHUB_REF_TYPE COMPME_ALLOW_MODEL_GATE_OVERRIDE \
    COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256 \
    COMPME_REQUIRE_MODEL_TESTS COMPME_REQUIRE_MODEL_CONTEXT COMPME_QUALITY_CORPUS; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "check-quality self-test failed: inherited $name" >&2
      return 1
    fi
  done
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-check-quality.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  unset COMPME_ALLOW_MODEL_GATE_OVERRIDE
  unset COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256
  unset COMPME_REQUIRE_MODEL_TESTS COMPME_REQUIRE_MODEL_CONTEXT COMPME_QUALITY_CORPUS
  unset COMPME_QUALITY_CARGO_FAIL COMPME_QUALITY_CURL_FAIL COMPME_QUALITY_CURL_BODY

  # The inherited-env guard above must reject user-preset COMPME_* vars: the
  # entrypoint scrubs only the CI-provided GITHUB_* vars, so a preset
  # COMPME_* var reaches the guard and fails the self-test loudly.
  if COMPME_QUALITY_CORPUS="$tmp/poisoned.jsonl" "$0" --self-test >/dev/null 2>"$tmp/poison-corpus.err"; then
    echo "check-quality self-test failed: inherited COMPME_QUALITY_CORPUS was accepted" >&2
    return 1
  fi
  grep -q 'check-quality self-test failed: inherited COMPME_QUALITY_CORPUS' "$tmp/poison-corpus.err"
  if COMPME_MODEL_GATE_PATH="$tmp/poisoned.gguf" "$0" --self-test >/dev/null 2>"$tmp/poison-model.err"; then
    echo "check-quality self-test failed: inherited COMPME_MODEL_GATE_PATH was accepted" >&2
    return 1
  fi
  grep -q 'check-quality self-test failed: inherited COMPME_MODEL_GATE_PATH' "$tmp/poison-model.err"

  fake_bin="$tmp/bin"
  mkdir -p "$fake_bin" "$tmp/model-dir"
  cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
printf 'cwd=%s gpu=%s ctx_tokens=%s require_tests=%s require_ctx=%s gate_path=%s corpus=%s args=%s\n' "$PWD" "${COMPME_MODEL_GPU_LAYERS:-}" "${COMPME_MODEL_CONTEXT_TOKENS:-}" "${COMPME_REQUIRE_MODEL_TESTS:-}" "${COMPME_REQUIRE_MODEL_CONTEXT:-}" "${COMPME_MODEL_GATE_PATH:-}" "${COMPME_QUALITY_CORPUS:-}" "$*" >>"$COMPME_QUALITY_CARGO_LOG"
# A malformed corpus (a non-empty line not starting with '{') fails the way
# the real harness fails: non-zero exit.
while IFS= read -r line; do
  case "$line" in
    "" | "{"*) ;;
    *) exit 45 ;;
  esac
done <"${COMPME_QUALITY_CORPUS:-/dev/null}"
if [ -n "${COMPME_QUALITY_CARGO_FAIL:-}" ]; then
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
printf 'curl %s\n' "$*" >>"$COMPME_QUALITY_CURL_LOG"
if [ -n "${COMPME_QUALITY_CURL_FAIL:-}" ]; then
  exit 42
fi
printf '%s' "${COMPME_QUALITY_CURL_BODY:-downloaded-model}" >"$out"
SH
  chmod +x "$fake_bin/cargo" "$fake_bin/curl"

  corpus_ok="$tmp/corpus-ok.jsonl"
  cat >"$corpus_ok" <<'JSONL'
{"id": "fake-one", "path": "completion", "left": "Hello", "expect": {"type": "contains", "value": "world"}}
{"id": "fake-two", "path": "grammar", "left": "I wrote", "word": "teh", "expect": {"type": "single_word_vetted", "value": "the"}}
JSONL
  corpus_bad="$tmp/corpus-bad.jsonl"
  printf '%s\n' '{"id": "ok", "path": "completion", "left": "Hello", "expect": {"type": "contains", "value": "world"}}' 'not json at all' >"$corpus_bad"

  model_path="$tmp/model-dir/model.gguf"
  printf 'cached-model' >"$model_path"
  cached_sha="$(shasum -a 256 "$model_path" | awk '{print $1}')"
  if PATH="$fake_bin:$PATH" \
    GITHUB_ACTIONS=true \
    GITHUB_REF_TYPE=tag \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null 2>"$tmp/release-override.err"; then
    echo "check-quality self-test failed: release override was accepted" >&2
    return 1
  fi
  grep -q 'refusing COMPME_MODEL_GATE_PATH override in GitHub release context' "$tmp/release-override.err"

  # The corpus override fully determines what the gate asserts, so it is
  # refused in release context like the model-gate overrides.
  if PATH="$fake_bin:$PATH" \
    GITHUB_ACTIONS=true \
    GITHUB_REF_TYPE=tag \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null 2>"$tmp/release-corpus-override.err"; then
    echo "check-quality self-test failed: release corpus override was accepted" >&2
    return 1
  fi
  grep -q 'refusing COMPME_QUALITY_CORPUS override in GitHub release context' "$tmp/release-corpus-override.err"

  # Cached model (sha256 match) skips the download; the quality test runs with
  # the pinned env contract (threshold-pass stand-in: fake cargo exits 0).
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null
  test ! -s "$tmp/curl.log"
  grep -q "gpu=0 ctx_tokens=256 require_tests=1 require_ctx=1 gate_path=$model_path corpus=$corpus_ok args=test --locked -p model_client --test quality -- --ignored --test-threads=1" "$tmp/cargo.log"

  # Missing model downloads, verifies, then runs the test.
  rm -f "$model_path"
  downloaded_sha="$(printf 'downloaded-model' | shasum -a 256 | awk '{print $1}')"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$downloaded_sha" \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null
  grep -q 'curl -L --fail --retry 3 --retry-delay 5 --output' "$tmp/curl.log"
  test "$(cat "$model_path")" = "downloaded-model"
  grep -q "args=test --locked -p model_client --test quality -- --ignored --test-threads=1" "$tmp/cargo.log"

  # Threshold failure (fake cargo exits non-zero) propagates.
  printf 'cached-model' >"$model_path"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    COMPME_QUALITY_CARGO_FAIL=1 \
    "$0" >/dev/null 2>"$tmp/cargo-fail.err"; then
    echo "check-quality self-test failed: cargo failure was accepted" >&2
    return 1
  fi
  test ! -s "$tmp/curl.log"
  grep -q "args=test --locked -p model_client --test quality -- --ignored --test-threads=1" "$tmp/cargo.log"

  # A malformed corpus line fails the run (the fake harness rejects it).
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_QUALITY_CORPUS="$corpus_bad" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    "$0" >/dev/null 2>"$tmp/malformed.err"; then
    echo "check-quality self-test failed: malformed corpus was accepted" >&2
    return 1
  fi
  grep -q "corpus=$corpus_bad" "$tmp/cargo.log"

  # Checksum mismatch on download fails and leaves no model behind.
  rm -f "$model_path"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$cached_sha" \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    COMPME_QUALITY_CURL_BODY="wrong-model" \
    "$0" >/dev/null 2>"$tmp/sha-fail.err"; then
    echo "check-quality self-test failed: checksum failure was accepted" >&2
    return 1
  fi
  test ! -e "$model_path"

  # Missing model + download failure fails before any test run.
  rm -f "$model_path"
  : >"$tmp/cargo.log"
  : >"$tmp/curl.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_MODEL_GATE_PATH="$model_path" \
    COMPME_MODEL_GATE_URL="https://example.test/model.gguf" \
    COMPME_MODEL_GATE_SHA256="$downloaded_sha" \
    COMPME_QUALITY_CORPUS="$corpus_ok" \
    COMPME_QUALITY_CARGO_LOG="$tmp/cargo.log" \
    COMPME_QUALITY_CURL_LOG="$tmp/curl.log" \
    COMPME_QUALITY_CURL_FAIL=1 \
    "$0" >/dev/null 2>"$tmp/curl-fail.err"; then
    echo "check-quality self-test failed: curl failure was accepted" >&2
    return 1
  fi
  grep -q 'curl -L --fail --retry 3 --retry-delay 5 --output' "$tmp/curl.log"
  test ! -e "$model_path"
  test ! -s "$tmp/cargo.log"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "check-quality self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: .*check-quality\.sh \[--self-test\]$' "$tmp/self-test-argc.err"
  if "$0" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "check-quality self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: .*check-quality\.sh \[--self-test\]$' "$tmp/normal-argc.err"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    echo "usage: $0 [--self-test]" >&2
    exit 2
  fi
  # Scrub only the CI-provided vars (CI always exports GITHUB_ACTIONS); a
  # user-preset COMPME_* var must reach run_self_test's inherited-env guard
  # and be rejected there, so the self-test environment stays hermetic.
  unset GITHUB_ACTIONS GITHUB_REF_TYPE
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
  /*) gate_model="$model" ;;
  *) gate_model="$repo_root/$model" ;;
esac
case "$corpus" in
  /*) gate_corpus="$corpus" ;;
  *) gate_corpus="$repo_root/$corpus" ;;
esac

# Same env contract as run-model-gates.sh: CPU-forced, bounded context,
# fail-closed on missing model. COMPME_MODEL_GATE_PATH/COMPME_QUALITY_CORPUS
# point the test binary at the exact files verified above.
COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_MODEL_GATE_PATH="$gate_model" COMPME_QUALITY_CORPUS="$gate_corpus" cargo test --locked -p model_client --test quality -- --ignored --test-threads=1
