#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

usage() {
  echo "usage: check-model-client-features.sh [--self-test]" >&2
}

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

check_non_macos_tree() {
  label="$1"
  tree="$2"
  assert_contains "$label" "$tree" 'llama-cpp-2 feature "dynamic-backends"' || return 1
  assert_contains "$label" "$tree" 'llama-cpp-2 feature "vulkan"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "metal"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "default"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "openmp"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "android"' || return 1
  echo "model_client feature check passed: $label"
}

check_non_macos_target() {
  label="$1"
  target="$2"
  tree="$(tree_for "$target")"
  check_non_macos_tree "$label" "$tree"
}

check_macos_tree() {
  label="$1"
  tree="$2"
  assert_contains "$label" "$tree" 'llama-cpp-2 feature "metal"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "dynamic-backends"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "vulkan"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "default"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "openmp"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "android"' || return 1
  echo "model_client feature check passed: $label"
}

check_spike_macos_features() {
  tree="$(cd tools/spike && cargo tree -e features)"
  check_macos_tree "spike macOS" "$tree"
}

run_self_test() {
  macos_tree='llama-cpp-2 feature "metal"'
  non_macos_tree='llama-cpp-2 feature "dynamic-backends"
llama-cpp-2 feature "vulkan"'

  check_macos_tree "self-test macOS" "$macos_tree" >/dev/null
  check_non_macos_tree "self-test non-macOS" "$non_macos_tree" >/dev/null

  macos_with_default="$(printf '%s\n%s\n' "$macos_tree" 'llama-cpp-2 feature "default"')"
  non_macos_with_metal="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "metal"')"

  if check_macos_tree "self-test macOS forbidden default" "$macos_with_default" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: macOS default feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS missing vulkan" 'llama-cpp-2 feature "dynamic-backends"' >/dev/null 2>&1; then
    echo "model_client feature self-test failed: missing Vulkan feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden metal" "$non_macos_with_metal" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS Metal feature passed" >&2
    return 1
  fi

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi

if [ "$#" -ne 0 ]; then
  usage
  exit 2
fi

host_triple="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [[ "$host_triple" == *apple-darwin ]]; then
  host_tree="$(tree_for host)"
  check_macos_tree "host macOS" "$host_tree"
  check_spike_macos_features
else
  echo "model_client feature check skipped: host is $host_triple, not macOS"
fi

check_non_macos_target "linux x86_64" "x86_64-unknown-linux-gnu"
check_non_macos_target "windows x86_64" "x86_64-pc-windows-msvc"
