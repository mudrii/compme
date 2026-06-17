#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

assert_contains() {
  label="$1"
  haystack="$2"
  needle="$3"
  if ! grep -Fq "$needle" <<<"$haystack"; then
    echo "model_client feature check failed: $label missing $needle" >&2
    return 1
  fi
}

assert_not_contains() {
  label="$1"
  haystack="$2"
  needle="$3"
  if grep -Fq "$needle" <<<"$haystack"; then
    echo "model_client feature check failed: $label unexpectedly contains $needle" >&2
    return 1
  fi
}

tree_for() {
  target="$1"
  if [ "$target" = "host" ]; then
    cargo tree -p model_client -e features
  else
    cargo tree -p model_client --target "$target" -e features
  fi
}

check_non_macos_target() {
  label="$1"
  target="$2"
  tree="$(tree_for "$target")"
  assert_contains "$label" "$tree" 'llama-cpp-2 feature "dynamic-backends"'
  assert_contains "$label" "$tree" 'llama-cpp-2 feature "vulkan"'
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "metal"'
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "default"'
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "openmp"'
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "android"'
  echo "model_client feature check passed: $label"
}

check_spike_macos_features() {
  tree="$(cd tools/spike && cargo tree -e features)"
  assert_contains "spike macOS" "$tree" 'llama-cpp-2 feature "metal"'
  assert_not_contains "spike macOS" "$tree" 'llama-cpp-2 feature "dynamic-backends"'
  assert_not_contains "spike macOS" "$tree" 'llama-cpp-2 feature "vulkan"'
  assert_not_contains "spike macOS" "$tree" 'llama-cpp-2 feature "default"'
  assert_not_contains "spike macOS" "$tree" 'llama-cpp-2 feature "openmp"'
  assert_not_contains "spike macOS" "$tree" 'llama-cpp-2 feature "android"'
  echo "model_client feature check passed: spike macOS"
}

host_triple="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [[ "$host_triple" == *apple-darwin ]]; then
  host_tree="$(tree_for host)"
  assert_contains "host macOS" "$host_tree" 'llama-cpp-2 feature "metal"'
  assert_not_contains "host macOS" "$host_tree" 'llama-cpp-2 feature "dynamic-backends"'
  assert_not_contains "host macOS" "$host_tree" 'llama-cpp-2 feature "vulkan"'
  assert_not_contains "host macOS" "$host_tree" 'llama-cpp-2 feature "default"'
  echo "model_client feature check passed: host macOS"
  check_spike_macos_features
else
  echo "model_client feature check skipped: host is $host_triple, not macOS"
fi

check_non_macos_target "linux x86_64" "x86_64-unknown-linux-gnu"
check_non_macos_target "windows x86_64" "x86_64-pc-windows-msvc"
