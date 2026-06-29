#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

model="tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"
url="https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf"
expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"

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

COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1
(
  cd "$repo_root/tools/spike"
  COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1
)
