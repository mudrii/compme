#!/usr/bin/env bash
# Assemble Compme.app from the release binary (A3 app-lifecycle).
#
# A real bundle is the unlock for: Launch Services registering the compme://
# scheme (CFBundleURLTypes), SMAppService launch-at-login, and a stable TCC
# identity (Accessibility/Screen Recording grants keyed on the bundle).
# Ad-hoc signed (-s -) for local use; real codesign/notarization is the
# A3 ship item and needs a Developer ID (human-gated).
#
# Usage: tools/bundle/make-app.sh [output-dir]   (default: target/bundle)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out_dir="${1:-"$repo_root/target/bundle"}"
app="$out_dir/Compme.app"

echo "building release binary…"
cargo build --release -p app --manifest-path "$repo_root/Cargo.toml"

echo "assembling $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$repo_root/tools/bundle/Info.plist" "$app/Contents/Info.plist"
cp "$repo_root/target/release/compme" "$app/Contents/MacOS/compme"

plutil -lint "$app/Contents/Info.plist"

echo "ad-hoc signing…"
codesign --force --sign - "$app"
codesign --verify "$app"

# Register the bundle (and its compme:// scheme) with Launch Services.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app" || true

echo "done: $app"
echo "smoke: COMPME_RUN_MS=1500 \"$app/Contents/MacOS/compme\""
