#!/usr/bin/env bash
# Finalize Casks/compme.rb for a published release: download the release zip,
# compute its sha256, and rewrite the cask's `version` + `sha256` lines.
#
# The cask sha256 can only be known after the Release workflow builds and
# uploads the artifact, so this is a deliberate post-release step (the workflow
# prints the same values to its job summary). Run it, then commit + push.
#
# Usage: tools/release/update-cask.sh vX.Y.Z   (or X.Y.Z)
set -euo pipefail

raw="${1:?usage: update-cask.sh vX.Y.Z}"
version="${raw#v}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cask="$repo_root/Casks/compme.rb"
zip="compme-${version}-macos.zip"
url="https://github.com/mudrii/compme/releases/download/v${version}/${zip}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $url"
curl -fsSL "$url" -o "$tmp/$zip"
sha="$(shasum -a 256 "$tmp/$zip" | awk '{print $1}')"
echo "version=$version sha256=$sha"

# Portable in-place sed (BSD/macOS needs the empty -i arg; GNU tolerates -i'').
sed -i'' -E "s/^  version \".*\"/  version \"${version}\"/" "$cask"
sed -i'' -E "s/^  sha256 \"[0-9a-f]*\"/  sha256 \"${sha}\"/" "$cask"

echo "updated $cask:"
grep -E '^\s+(version|sha256) ' "$cask"
echo "next: git add Casks/compme.rb && git commit -m 'chore(release): cask v${version}' && git push"
