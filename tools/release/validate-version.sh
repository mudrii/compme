#!/usr/bin/env bash
# Validate the stable release version supported by Apple bundle metadata,
# Compme tags, artifact names, and the Homebrew cask.
# Usage: validate-version.sh VERSION
#        validate-version.sh --self-test
set -euo pipefail

usage() {
  echo "usage: validate-version.sh VERSION | --self-test" >&2
}

validate_version() {
  local version="$1"

  # CFBundleShortVersionString and CFBundleVersion accept numeric components,
  # not SemVer prerelease/build suffixes. Keep one stable X.Y.Z contract across
  # every release surface instead of publishing invalid macOS metadata.
  if [[ ! "$version" =~ ^(0|[1-9][0-9]*)[.](0|[1-9][0-9]*)[.](0|[1-9][0-9]*)$ ]]; then
    echo "invalid version: $version" >&2
    return 1
  fi
}

run_self_test() {
  local version
  local -a valid=(
    0.0.0
    1.2.3
  )
  local -a invalid=(
    ""
    01.2.3
    1.02.3
    1.2.03
    1.2.3-rc.1
    1.2.3-rc.01
    1.2.3+build
    1.2.3.4
    v1.2.3
  )

  for version in "${valid[@]}"; do
    "$0" "$version"
  done
  for version in "${invalid[@]}"; do
    if "$0" "$version" >/dev/null 2>&1; then
      echo "self-test FAILED: invalid version passed: $version" >&2
      return 1
    fi
  done
  if "$0" >/dev/null 2>&1; then
    echo "self-test FAILED: missing version passed" >&2
    return 1
  fi
  if "$0" 1.2.3 unexpected-extra >/dev/null 2>&1; then
    echo "self-test FAILED: extra argument passed" >&2
    return 1
  fi

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

if [[ "$#" -ne 1 ]]; then
  usage
  exit 2
fi

validate_version "$1"
