#!/usr/bin/env bash
# Finalize Casks/compme.rb for a published release: download the release zip,
# compute its sha256, and rewrite the cask's `version` + `sha256` lines.
#
# The cask sha256 can only be known after the Release workflow builds and
# uploads the artifact, so this is a deliberate post-release step (the workflow
# prints the same values to its job summary). Run it, then commit + push.
#
# Usage: tools/release/update-cask.sh vX.Y.Z   (or X.Y.Z)
#        tools/release/update-cask.sh --self-test
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

usage() {
  echo "usage: update-cask.sh vX.Y.Z | --self-test" >&2
}

validate_version() {
  local version="$1"
  if [[ ! "$version" =~ ^[0-9]+[.][0-9]+[.][0-9]+([.-][0-9A-Za-z]+)*$ ]]; then
    echo "invalid version: $version" >&2
    return 1
  fi
}

rewrite_cask() {
  cask_path="$1"
  version="$2"
  sha="$3"

  # Portable in-place sed (BSD/macOS needs the empty -i arg; GNU tolerates -i'').
  sed -i'' -E "s/^  version \".*\"/  version \"${version}\"/" "$cask_path"
  sed -i'' -E "s/^  sha256 \"[0-9a-f]*\"/  sha256 \"${sha}\"/" "$cask_path"
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-cask-test.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  fixture="$tmp/compme.rb"
  artifact="$tmp/compme-9.8.7-macos.zip"
  cat >"$fixture" <<'CASK'
cask "compme" do
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
end
CASK
  printf 'fixture artifact\n' >"$artifact"
  expected_sha="$(shasum -a 256 "$artifact" | awk '{print $1}')"

  COMPME_CASK_PATH="$fixture" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 >"$tmp/out.log"

  grep -q 'version "9.8.7"' "$fixture"
  grep -q "sha256 \"$expected_sha\"" "$fixture"
  grep -q "version=9.8.7 sha256=$expected_sha" "$tmp/out.log"

  # Assert the constructed artifact URL: fake curl on PATH captures its -o source
  # URL so we pin the v-prefixed tag + compme-<version>-macos.zip filename.
  fake_bin="$tmp/bin"
  mkdir -p "$fake_bin"
  cat >"$fake_bin/curl" <<'SH'
#!/usr/bin/env bash
out=""
url=""
prev=""
for arg in "$@"; do
  case "$prev" in
    -o) out="$arg" ;;
  esac
  case "$arg" in
    https://*) url="$arg" ;;
  esac
  prev="$arg"
done
printf '%s\n' "$url" >"$COMPME_CASK_TEST_URL_LOG"
printf 'fixture artifact\n' >"$out"
SH
  chmod +x "$fake_bin/curl"

  url_fixture="$tmp/url.rb"
  cat >"$url_fixture" <<'CASK'
cask "compme" do
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
end
CASK

  PATH="$fake_bin:$PATH" \
    COMPME_CASK_PATH="$url_fixture" \
    COMPME_CASK_TEST_URL_LOG="$tmp/url.log" \
    "$0" v9.8.7 >"$tmp/url-out.log"

  expected_url="https://github.com/mudrii/compme/releases/download/v9.8.7/compme-9.8.7-macos.zip"
  actual_url="$(cat "$tmp/url.log")"
  if [ "$actual_url" != "$expected_url" ]; then
    echo "self-test FAILED: artifact URL mismatch" >&2
    echo "  expected: $expected_url" >&2
    echo "  actual:   $actual_url" >&2
    exit 1
  fi

  malformed="$tmp/malformed.rb"
  cat >"$malformed" <<'CASK'
cask "compme" do
  version "1.2.3"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  url "https://example.invalid/old.zip"
end
CASK
  if COMPME_CASK_PATH="$malformed" "$0" '1.2.3"; system("bad") #' "$artifact" >/tmp/compme-update-cask-invalid.log 2>&1; then
    echo "malformed version unexpectedly passed" >&2
    return 1
  fi
  grep -q 'version "1.2.3"' "$malformed"
  grep -q 'example.invalid/old.zip' "$malformed"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  run_self_test
  exit 0
fi

raw="${1:-}"
if [ -z "$raw" ]; then
  usage
  exit 2
fi

version="${raw#v}"
validate_version "$version"
cask="${COMPME_CASK_PATH:-"$repo_root/Casks/compme.rb"}"
zip="compme-${version}-macos.zip"
url="https://github.com/mudrii/compme/releases/download/v${version}/${zip}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ -n "${COMPME_CASK_ARTIFACT:-}" ]; then
  artifact="$COMPME_CASK_ARTIFACT"
  echo "using local artifact $artifact"
else
  artifact="$tmp/$zip"
  echo "downloading $url"
  curl -fsSL "$url" -o "$artifact"
fi
sha="$(shasum -a 256 "$artifact" | awk '{print $1}')"
echo "version=$version sha256=$sha"

rewrite_cask "$cask" "$version" "$sha"

echo "updated $cask:"
grep -E '^\s+(version|sha256) ' "$cask"
echo "next: git add Casks/compme.rb && git commit -m 'chore(release): cask v${version}' && git push"
