#!/usr/bin/env bash
# Finalize Casks/compme.rb for a published release: download the release zip,
# compute its sha256, and rewrite the cask's `version` + `sha256` lines.
#
# The cask sha256 can only be known after the Release workflow builds the
# artifact. The tag workflow runs this automatically from the just-built zip;
# run it manually only to recover or verify a release cask update.
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
  local identifier
  local -a prerelease_identifiers
  if [[ ! "$version" =~ ^(0|[1-9][0-9]*)[.](0|[1-9][0-9]*)[.](0|[1-9][0-9]*)(-([0-9A-Za-z-]+[.])*[0-9A-Za-z-]+)?$ ]]; then
    echo "invalid version: $version" >&2
    return 1
  fi
  if [[ "$version" == *-* ]]; then
    IFS=. read -r -a prerelease_identifiers <<<"${version#*-}"
    for identifier in "${prerelease_identifiers[@]}"; do
      if [[ "$identifier" =~ ^[0-9]+$ && "$identifier" == 0* && "$identifier" != "0" ]]; then
        echo "invalid version: $version" >&2
        return 1
      fi
    done
  fi
}

rewrite_cask() {
  cask_path="$1"
  version="$2"
  sha="$3"
  expected_url='  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"'

  if ! grep -Eq '^  version "[^"]+"$' "$cask_path"; then
    echo "missing rewritable version line in $cask_path" >&2
    return 1
  fi
  if ! grep -Eq '^  sha256 "[0-9a-f]{64}"$' "$cask_path"; then
    echo "missing rewritable sha256 line in $cask_path" >&2
    return 1
  fi
  if ! grep -Fxq "$expected_url" "$cask_path"; then
    echo "missing expected GitHub release url line in $cask_path" >&2
    return 1
  fi

  # Portable in-place sed: BSD sed parses -i'' as -i with the NEXT arg (-E) as
  # the backup suffix, silently disabling ERE and littering a "-E" backup file.
  # -i.bak + rm is the form both BSD and GNU parse identically; one invocation
  # with two -e expressions keeps the .bak cleanup failure-safe under set -e.
  sed -i.bak -E \
    -e "s/^  version \".*\"/  version \"${version}\"/" \
    -e "s/^  sha256 \"[0-9a-f]*\"/  sha256 \"${sha}\"/" \
    "$cask_path" || { rm -f "${cask_path}.bak"; return 1; }
  rm -f "${cask_path}.bak"
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
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  printf 'fixture artifact\n' >"$artifact"
  expected_sha="$(shasum -a 256 "$artifact" | awk '{print $1}')"

  COMPME_CASK_PATH="$fixture" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 >"$tmp/out.log"

  grep -q 'version "9.8.7"' "$fixture"
  grep -q "sha256 \"$expected_sha\"" "$fixture"
  grep -q "version=9.8.7 sha256=$expected_sha" "$tmp/out.log"

  rc_artifact="$tmp/compme-9.8.7-rc.1-macos.zip"
  rc_fixture="$tmp/rc.rb"
  cat >"$rc_fixture" <<'CASK'
cask "compme" do
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  printf 'rc fixture artifact\n' >"$rc_artifact"
  COMPME_CASK_PATH="$rc_fixture" COMPME_CASK_ARTIFACT="$rc_artifact" "$0" v9.8.7-rc.1 >"$tmp/rc.out"
  grep -q 'version "9.8.7-rc.1"' "$rc_fixture"

  if COMPME_CASK_PATH="$fixture" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7.6 >"$tmp/four-part.out" 2>"$tmp/four-part.err"; then
    echo "four-part version unexpectedly passed" >&2
    return 1
  fi
  grep -q 'invalid version: 9.8.7.6' "$tmp/four-part.err"
  if COMPME_CASK_PATH="$fixture" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7+build >"$tmp/build-metadata.out" 2>"$tmp/build-metadata.err"; then
    echo "build-metadata version unexpectedly passed" >&2
    return 1
  fi
  grep -q 'invalid version: 9.8.7+build' "$tmp/build-metadata.err"
  leading_zero_artifact="$tmp/compme-9.8.7-rc.01-macos.zip"
  printf 'invalid prerelease artifact\n' >"$leading_zero_artifact"
  if COMPME_CASK_PATH="$fixture" COMPME_CASK_ARTIFACT="$leading_zero_artifact" "$0" v9.8.7-rc.01 >"$tmp/leading-zero-prerelease.out" 2>"$tmp/leading-zero-prerelease.err"; then
    echo "numeric prerelease identifier with a leading zero unexpectedly passed" >&2
    return 1
  fi
  grep -q 'invalid version: 9.8.7-rc.01' "$tmp/leading-zero-prerelease.err"

  mismatched_artifact="$tmp/compme-0.9.9-macos.zip"
  printf 'old artifact\n' >"$mismatched_artifact"
  mismatch_fixture="$tmp/mismatch.rb"
  cat >"$mismatch_fixture" <<'CASK'
cask "compme" do
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  mismatch_golden="$tmp/mismatch.golden"
  cp "$mismatch_fixture" "$mismatch_golden"
  if COMPME_CASK_PATH="$mismatch_fixture" COMPME_CASK_ARTIFACT="$mismatched_artifact" "$0" v9.8.7 >"$tmp/mismatch-artifact.log" 2>&1; then
    echo "mismatched local artifact unexpectedly passed" >&2
    return 1
  fi
  grep -q 'artifact filename mismatch' "$tmp/mismatch-artifact.log"
  cmp "$mismatch_fixture" "$mismatch_golden"

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
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
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
  invalid_log="$tmp/invalid.log"
  if COMPME_CASK_PATH="$malformed" COMPME_CASK_ARTIFACT="$artifact" "$0" '1.2.3"; system("bad") #' >"$invalid_log" 2>&1; then
    echo "malformed version unexpectedly passed" >&2
    return 1
  fi
  grep -q 'invalid version' "$invalid_log"
  grep -q 'version "1.2.3"' "$malformed"
  grep -q 'example.invalid/old.zip' "$malformed"

  extra_arg="$tmp/extra-arg.rb"
  cat >"$extra_arg" <<'CASK'
cask "compme" do
  version "1.2.3"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  url "https://example.invalid/old.zip"
end
CASK
  extra_arg_golden="$tmp/extra-arg.golden"
  cp "$extra_arg" "$extra_arg_golden"
  if COMPME_CASK_PATH="$extra_arg" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 unexpected-extra >"$tmp/extra-arg.log" 2>&1; then
    echo "extra positional arg unexpectedly passed" >&2
    return 1
  fi
  grep -q 'usage: update-cask.sh' "$tmp/extra-arg.log"
  cmp "$extra_arg" "$extra_arg_golden"

  no_check="$tmp/no-check.rb"
  cat >"$no_check" <<'CASK'
cask "compme" do
  version "1.2.3"
  sha256 :no_check
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  no_check_golden="$tmp/no-check.golden"
  cp "$no_check" "$no_check_golden"
  if COMPME_CASK_PATH="$no_check" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 >"$tmp/no-check.log" 2>&1; then
    echo "sha256 :no_check cask unexpectedly passed" >&2
    return 1
  fi
  grep -q 'missing rewritable sha256 line' "$tmp/no-check.log"
  cmp "$no_check" "$no_check_golden"

  no_sha="$tmp/no-sha.rb"
  cat >"$no_sha" <<'CASK'
cask "compme" do
  version "1.2.3"
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  no_sha_golden="$tmp/no-sha.golden"
  cp "$no_sha" "$no_sha_golden"
  if COMPME_CASK_PATH="$no_sha" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 >"$tmp/no-sha.log" 2>&1; then
    echo "missing sha256 cask unexpectedly passed" >&2
    return 1
  fi
  grep -q 'missing rewritable sha256 line' "$tmp/no-sha.log"
  cmp "$no_sha" "$no_sha_golden"

  no_version="$tmp/no-version.rb"
  cat >"$no_version" <<'CASK'
cask "compme" do
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  no_version_golden="$tmp/no-version.golden"
  cp "$no_version" "$no_version_golden"
  if COMPME_CASK_PATH="$no_version" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 >"$tmp/no-version.log" 2>&1; then
    echo "missing version cask unexpectedly passed" >&2
    return 1
  fi
  grep -q 'missing rewritable version line' "$tmp/no-version.log"
  cmp "$no_version" "$no_version_golden"

  hostile_url="$tmp/hostile-url.rb"
  cat >"$hostile_url" <<'CASK'
cask "compme" do
  version "1.2.3"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  url "https://evil.example/compme.zip"
end
CASK
  hostile_url_golden="$tmp/hostile-url.golden"
  cp "$hostile_url" "$hostile_url_golden"
  if COMPME_CASK_PATH="$hostile_url" COMPME_CASK_ARTIFACT="$artifact" "$0" v9.8.7 >"$tmp/hostile-url.log" 2>&1; then
    echo "unexpected cask URL unexpectedly passed" >&2
    return 1
  fi
  grep -q 'missing expected GitHub release url line' "$tmp/hostile-url.log"
  cmp "$hostile_url" "$hostile_url_golden"

  # Regression pin for the historical `sed -i''` mis-parse: BSD sed still
  # substituted correctly (the patterns are BRE-compatible) so content greps
  # can't catch it — the stray `<file>-E` backup is the only observable
  # symptom. Assert no backup litter of any kind remains.
  stray="$(find "$tmp" -name '*.rb?*' -print)"
  if [ -n "$stray" ]; then
    echo "self-test FAILED: stray sed backup files: $stray" >&2
    exit 1
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

if [ "$#" -ne 1 ]; then
  usage
  exit 2
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
artifact_name="$(basename "$artifact")"
if [ "$artifact_name" != "$zip" ]; then
  echo "artifact filename mismatch: expected $zip, got $artifact_name" >&2
  exit 1
fi
if [ ! -f "$artifact" ]; then
  echo "missing artifact: $artifact" >&2
  exit 1
fi
sha="$(shasum -a 256 "$artifact" | awk '{print $1}')"
echo "version=$version sha256=$sha"

rewrite_cask "$cask" "$version" "$sha"

echo "updated $cask:"
grep -E '^\s+(version|sha256) ' "$cask"
echo "next: commit Casks/compme.rb if you are running this outside the release workflow"
