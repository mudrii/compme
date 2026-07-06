#!/usr/bin/env bash
# Write the GitHub-release update manifest: informational release metadata
# published alongside the artifact for tooling and humans. Nothing consumes it
# in-app yet (the app's Check for Updates just opens the releases page); any
# future auto-updater must add signature verification before trusting it.
#
# Usage: tools/release/write-update-manifest.sh VERSION ZIP SHA256 > manifest.json
#        tools/release/write-update-manifest.sh --self-test
set -euo pipefail

repo="mudrii/compme"
min_macos="14.0"

usage() {
  echo "usage: write-update-manifest.sh VERSION ZIP SHA256 | --self-test" >&2
}

validate_version() {
  local version="$1"
  [[ "$version" =~ ^[0-9]+[.][0-9]+[.][0-9]+([.-][0-9A-Za-z]+)*$ ]]
}

validate_sha() {
  local sha="$1"
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]]
}

validate_published_at() {
  local published_at="$1"
  [[ "$published_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]
}

json_escape() {
  ruby -rjson -e 'print ARGV[0].to_json' "$1"
}

write_manifest() {
  local version="$1"
  local zip="$2"
  local sha="$3"
  local tag="v${version}"
  local base="https://github.com/${repo}/releases/download/${tag}"
  local pub_date="${COMPME_UPDATE_PUBLISHED_AT:-$(date -u '+%Y-%m-%dT%H:%M:%SZ')}"

  validate_version "$version" || {
    echo "invalid version: $version" >&2
    return 1
  }
  validate_sha "$sha" || {
    echo "invalid sha256: $sha" >&2
    return 1
  }
  validate_published_at "$pub_date" || {
    echo "invalid published_at: $pub_date" >&2
    return 1
  }
  case "$zip" in
    compme-"$version"-macos.zip) ;;
    *)
      echo "zip filename must be compme-${version}-macos.zip: $zip" >&2
      return 1
      ;;
  esac

  cat <<JSON
{
  "version": $(json_escape "$version"),
  "published_at": $(json_escape "$pub_date"),
  "minimum_system_version": $(json_escape "$min_macos"),
  "url": $(json_escape "$base/$zip"),
  "sha256": $(json_escape "$sha"),
  "release_notes_url": $(json_escape "https://github.com/${repo}/releases/tag/${tag}")
}
JSON
}

run_self_test() {
  local sha
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-update-manifest-test.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  sha="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  COMPME_UPDATE_PUBLISHED_AT="2026-07-02T00:00:00Z" "$0" 1.2.3 compme-1.2.3-macos.zip "$sha" >"$tmp/manifest.json"
  ruby -rjson -e '
    data = JSON.parse(File.read(ARGV[0]))
    abort "version" unless data["version"] == "1.2.3"
    abort "published_at" unless data["published_at"] == "2026-07-02T00:00:00Z"
    abort "minimum_system_version" unless data["minimum_system_version"] == "14.0"
    abort "url" unless data["url"] == "https://github.com/mudrii/compme/releases/download/v1.2.3/compme-1.2.3-macos.zip"
    abort "sha256" unless data["sha256"] == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    abort "notes" unless data["release_notes_url"] == "https://github.com/mudrii/compme/releases/tag/v1.2.3"
  ' "$tmp/manifest.json"
  if "$0" bad compme-bad-macos.zip "$sha" >"$tmp/bad.out" 2>"$tmp/bad.err"; then
    echo "self-test FAILED: bad version passed" >&2
    return 1
  fi
  grep -Fq "invalid version" "$tmp/bad.err"
  if COMPME_UPDATE_PUBLISHED_AT="not-a-date" "$0" 1.2.3 compme-1.2.3-macos.zip "$sha" >"$tmp/bad-date.out" 2>"$tmp/bad-date.err"; then
    echo "self-test FAILED: bad published_at passed" >&2
    return 1
  fi
  grep -Fq "invalid published_at" "$tmp/bad-date.err"
  upper_sha="$(printf '%s' "$sha" | tr '[:lower:]' '[:upper:]')"
  if "$0" 1.2.3 compme-1.2.3-macos.zip "$upper_sha" >"$tmp/upper-sha.out" 2>"$tmp/upper-sha.err"; then
    echo "self-test FAILED: uppercase sha passed" >&2
    return 1
  fi
  grep -Fq "invalid sha256" "$tmp/upper-sha.err"
  if "$0" 1.2.3 compme-1.2.3-macos.zip "${sha:0:63}" >"$tmp/short-sha.out" 2>"$tmp/short-sha.err"; then
    echo "self-test FAILED: truncated sha passed" >&2
    return 1
  fi
  grep -Fq "invalid sha256" "$tmp/short-sha.err"
  if "$0" 1.2.3 compme-1.2.4-macos.zip "$sha" >"$tmp/bad-zip.out" 2>"$tmp/bad-zip.err"; then
    echo "self-test FAILED: mismatched zip version passed" >&2
    return 1
  fi
  grep -Fq "zip filename must be compme-1.2.3-macos.zip" "$tmp/bad-zip.err"
  if "$0" 1.2.3 compme-1.2.3-macos.zip >"$tmp/argc.out" 2>"$tmp/argc.err"; then
    echo "self-test FAILED: wrong argument count passed" >&2
    return 1
  fi
  grep -Fq "usage: write-update-manifest.sh" "$tmp/argc.err"
  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "self-test FAILED: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: write-update-manifest.sh" "$tmp/self-test-argc.err"
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

if [[ "$#" -ne 3 ]]; then
  usage
  exit 2
fi

write_manifest "$1" "$2" "$3"
