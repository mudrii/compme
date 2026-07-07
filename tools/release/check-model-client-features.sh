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
  # Static CPU-only llama off macOS until the real adapters land (ROADMAP
  # 1.1): "vulkan" needs the Vulkan SDK at build time (CI runners lack it)
  # and "dynamic-backends" hard-links shared libs in its build script with a
  # racy !exists()->hard_link().unwrap() that panics AlreadyExists under CI.
  assert_contains "$label" "$tree" 'llama-cpp-2 v' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "dynamic-backends"' || return 1
  assert_not_contains "$label" "$tree" 'llama-cpp-2 feature "vulkan"' || return 1
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
  non_macos_tree='llama-cpp-2 v0.1.146'
  non_macos_with_vulkan="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "vulkan"')"
  non_macos_with_dynamic_backends="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "dynamic-backends"')"

  check_macos_tree "self-test macOS" "$macos_tree" >/dev/null
  check_non_macos_tree "self-test non-macOS" "$non_macos_tree" >/dev/null

  macos_with_default="$(printf '%s\n%s\n' "$macos_tree" 'llama-cpp-2 feature "default"')"
  macos_with_dynamic_backends="$(printf '%s\n%s\n' "$macos_tree" 'llama-cpp-2 feature "dynamic-backends"')"
  macos_with_vulkan="$(printf '%s\n%s\n' "$macos_tree" 'llama-cpp-2 feature "vulkan"')"
  non_macos_with_metal="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "metal"')"
  non_macos_with_default="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "default"')"
  macos_with_openmp="$(printf '%s\n%s\n' "$macos_tree" 'llama-cpp-2 feature "openmp"')"
  macos_with_android="$(printf '%s\n%s\n' "$macos_tree" 'llama-cpp-2 feature "android"')"
  non_macos_with_openmp="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "openmp"')"
  non_macos_with_android="$(printf '%s\n%s\n' "$non_macos_tree" 'llama-cpp-2 feature "android"')"

  if check_macos_tree "self-test macOS forbidden default" "$macos_with_default" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: macOS default feature passed" >&2
    return 1
  fi
  if check_macos_tree "self-test macOS forbidden dynamic-backends" "$macos_with_dynamic_backends" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: macOS dynamic-backends feature passed" >&2
    return 1
  fi
  if check_macos_tree "self-test macOS forbidden vulkan" "$macos_with_vulkan" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: macOS Vulkan feature passed" >&2
    return 1
  fi
  if check_macos_tree "self-test macOS missing metal" 'llama-cpp-2 feature "accelerate"' >/dev/null 2>&1; then
    echo "model_client feature self-test failed: missing Metal feature passed" >&2
    return 1
  fi
  if check_macos_tree "self-test macOS forbidden openmp" "$macos_with_openmp" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: macOS OpenMP feature passed" >&2
    return 1
  fi
  if check_macos_tree "self-test macOS forbidden android" "$macos_with_android" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: macOS Android feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden vulkan" "$non_macos_with_vulkan" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS Vulkan feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden dynamic-backends" "$non_macos_with_dynamic_backends" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS dynamic-backends feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS missing llama-cpp-2" 'some-other-crate v1.0.0' >/dev/null 2>&1; then
    echo "model_client feature self-test failed: tree without llama-cpp-2 passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden metal" "$non_macos_with_metal" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS Metal feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden default" "$non_macos_with_default" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS default feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden openmp" "$non_macos_with_openmp" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS OpenMP feature passed" >&2
    return 1
  fi
  if check_non_macos_tree "self-test non-macOS forbidden android" "$non_macos_with_android" >/dev/null 2>&1; then
    echo "model_client feature self-test failed: non-macOS Android feature passed" >&2
    return 1
  fi

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-model-client-features.XXXXXX")"
  trap 'rm -rf "$tmp"' RETURN
  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "model_client feature self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-model-client-features\.sh \[--self-test\]$' "$tmp/self-test-argc.err"
  if "$0" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "model_client feature self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-model-client-features\.sh \[--self-test\]$' "$tmp/normal-argc.err"

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
