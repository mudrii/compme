#!/usr/bin/env bash
# Assemble Compme.app from the release binary (A3 app-lifecycle).
#
# A real bundle is the unlock for: Launch Services registering the compme://
# scheme (CFBundleURLTypes), SMAppService launch-at-login, and a stable TCC
# identity (Accessibility/Screen Recording grants keyed on the bundle).
# Ad-hoc signed (-s -) by default for local use. Set
# COMPME_CODESIGN_IDENTITY to a Developer ID Application identity to produce a
# hardened-runtime, timestamped release signature; notarization is handled by
# tools/release/notarize-app.sh after packaging.
#
# Usage: tools/bundle/make-app.sh [output-dir]   (default: target/bundle)
#        tools/bundle/make-app.sh --self-test
set -euo pipefail

script_repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_root="${COMPME_BUNDLE_REPO_ROOT:-$script_repo_root}"
lsregister="${COMPME_BUNDLE_LSREGISTER:-/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister}"

run_self_test() {
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/compme-make-app-self-test.XXXXXX")"
  cleanup() {
    rm -rf "$tmp_dir"
  }
  trap cleanup EXIT

  fake_bin="$tmp_dir/bin"
  fixture_root="$tmp_dir/repo"
  out_dir="$tmp_dir/out"
  log="$tmp_dir/commands.log"
  mkdir -p "$fake_bin"
  mkdir -p "$fixture_root/tools/bundle"
  cp "$repo_root/tools/bundle/Info.plist" "$fixture_root/tools/bundle/Info.plist"

  cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
mkdir -p "$COMPME_BUNDLE_REPO_ROOT/target/release"
printf '#!/usr/bin/env bash\nexit 0\n' >"$COMPME_BUNDLE_REPO_ROOT/target/release/compme"
chmod +x "$COMPME_BUNDLE_REPO_ROOT/target/release/compme"
SH
  cat >"$fake_bin/plutil" <<'SH'
#!/usr/bin/env bash
printf 'plutil %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
SH
  cat >"$fake_bin/codesign" <<'SH'
#!/usr/bin/env bash
printf 'codesign %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
SH
  cat >"$fake_bin/lsregister" <<'SH'
#!/usr/bin/env bash
printf 'lsregister %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
SH
  cat >"$fake_bin/lsregister_fail" <<'SH'
#!/usr/bin/env bash
printf 'lsregister_fail %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
exit 23
SH
  chmod +x "$fake_bin/cargo" "$fake_bin/plutil" "$fake_bin/codesign" "$fake_bin/lsregister" "$fake_bin/lsregister_fail"

  PATH="$fake_bin:$PATH" \
    COMPME_BUNDLE_SELF_TEST_LOG="$log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister" \
    "$0" "$out_dir" >"$tmp_dir/stdout"

  app="$out_dir/Compme.app"
  test -d "$app/Contents/MacOS"
  test -d "$app/Contents/Resources"
  cmp "$fixture_root/tools/bundle/Info.plist" "$app/Contents/Info.plist" >/dev/null
  test -x "$app/Contents/MacOS/compme"
  grep -Fq "cargo build --release -p app --manifest-path $fixture_root/Cargo.toml" "$log"
  grep -Fq "plutil -lint $app/Contents/Info.plist" "$log"
  grep -Fq "codesign --force --sign - $app" "$log"
  grep -Fq "codesign --verify --strict $app" "$log"
  grep -Fq "lsregister -f $app" "$log"

  signed_out="$tmp_dir/out-signed"
  PATH="$fake_bin:$PATH" \
    COMPME_BUNDLE_SELF_TEST_LOG="$log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister" \
    COMPME_CODESIGN_IDENTITY="Developer ID Application: Compme Test (TEAMID)" \
    COMPME_CODESIGN_ENTITLEMENTS="$fixture_root/tools/bundle/release.entitlements" \
    "$0" "$signed_out" >"$tmp_dir/stdout-signed"
  grep -Fq "codesign --force --sign Developer ID Application: Compme Test (TEAMID) --options runtime --timestamp --entitlements $fixture_root/tools/bundle/release.entitlements $signed_out/Compme.app" "$log"

  if PATH="$fake_bin:$PATH" \
    COMPME_BUNDLE_SELF_TEST_LOG="$log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister_fail" \
    "$0" "$tmp_dir/out-fail" >"$tmp_dir/stdout-fail" 2>"$tmp_dir/stderr-fail"; then
    echo "lsregister failure was accepted" >&2
    return 1
  fi
  grep -Fq "lsregister_fail -f $tmp_dir/out-fail/Compme.app" "$log"
  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  run_self_test
  exit 0
fi

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

codesign_identity="${COMPME_CODESIGN_IDENTITY:--}"
codesign_args=(--force --sign "$codesign_identity")
if [[ "$codesign_identity" == "-" ]]; then
  echo "ad-hoc signing…"
else
  echo "Developer-ID signing…"
  codesign_args+=(--options runtime --timestamp)
fi
if [[ -n "${COMPME_CODESIGN_ENTITLEMENTS:-}" ]]; then
  codesign_args+=(--entitlements "$COMPME_CODESIGN_ENTITLEMENTS")
fi
codesign "${codesign_args[@]}" "$app"
codesign --verify --strict "$app"

# Register the bundle (and its compme:// scheme) with Launch Services.
if [[ "${COMPME_BUNDLE_SKIP_LSREGISTER:-0}" != "1" ]]; then
  "$lsregister" -f "$app"
fi

echo "done: $app"
echo "smoke: COMPME_RUN_MS=1500 \"$app/Contents/MacOS/compme\""
