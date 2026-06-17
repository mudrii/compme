#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

model="tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"
url="https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_k_m.gguf"
expected="74a4da8c9fdbcd15bd1f6d01d621410d31c6fc00986f5eb687824e7b93d7a9db"

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

COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1
(
  cd "$repo_root/tools/spike"
  COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1
)
