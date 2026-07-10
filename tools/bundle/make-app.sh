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
# Set COMPME_BUNDLE_SKIP_BUILD=1 to assemble/sign from an already-built
# target/release/compme without invoking cargo (release workflow imports the
# Developer-ID identity only after prebuilding).
#
# Usage: tools/bundle/make-app.sh [output-dir]   (default: target/bundle)
#        tools/bundle/make-app.sh --self-test
set -euo pipefail

script_repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_root="${COMPME_BUNDLE_REPO_ROOT:-$script_repo_root}"
lsregister="${COMPME_BUNDLE_LSREGISTER:-/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister}"

run_self_test() {
  for name in \
    COMPME_BUNDLE_REPO_ROOT COMPME_BUNDLE_LSREGISTER CARGO_TARGET_DIR \
    COMPME_BUNDLE_SKIP_BUILD COMPME_CODESIGN_IDENTITY \
    COMPME_CODESIGN_ENTITLEMENTS; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "self-test FAILED: inherited $name" >&2
      return 1
    fi
  done
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
  cp "$script_repo_root/tools/bundle/Info.plist" "$fixture_root/tools/bundle/Info.plist"
  # A stand-in icon so the required-icon copy has something to move; content is
  # irrelevant to the assembly test.
  printf 'icns-fixture' >"$fixture_root/tools/bundle/AppIcon.icns"

  cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
target_dir="${CARGO_TARGET_DIR:-$COMPME_BUNDLE_REPO_ROOT/target}"
marker="${COMPME_BUNDLE_BINARY_MARKER:-default-binary}"
mkdir -p "$target_dir/release"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" %q\n' "$marker" >"$target_dir/release/compme"
chmod +x "$target_dir/release/compme"
SH
  cat >"$fake_bin/plutil" <<'SH'
#!/usr/bin/env bash
printf 'plutil %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
SH
  cat >"$fake_bin/codesign" <<'SH'
#!/usr/bin/env bash
printf 'codesign %s\n' "$*" >>"$COMPME_BUNDLE_SELF_TEST_LOG"
case "${COMPME_BUNDLE_CODESIGN_FAIL:-}" in
  sign)
    case " $* " in
      *" --force "*) exit 31 ;;
    esac
    ;;
  verify)
    case " $* " in
      *" --verify "*) exit 32 ;;
    esac
    ;;
esac
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
  cmp "$fixture_root/tools/bundle/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns" >/dev/null
  test -x "$app/Contents/MacOS/compme"
  grep -Fq "cargo build --locked --release -p app --manifest-path $fixture_root/Cargo.toml" "$log"
  grep -Fq "plutil -lint $app/Contents/Info.plist" "$log"
  grep -Fq "codesign --force --sign - $app" "$log"
  grep -Fq "codesign --verify --strict $app" "$log"
  grep -Fq "lsregister -f $app" "$log"

  custom_target="$tmp_dir/custom-target"
  custom_out="$tmp_dir/out-custom-target"
  PATH="$fake_bin:$PATH" \
    CARGO_TARGET_DIR="$custom_target" \
    COMPME_BUNDLE_BINARY_MARKER="custom-target-binary" \
    COMPME_BUNDLE_SELF_TEST_LOG="$log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister" \
    "$0" "$custom_out" >"$tmp_dir/stdout-custom-target"
  "$custom_out/Compme.app/Contents/MacOS/compme" | grep -Fxq "custom-target-binary"

  prebuilt_target="$tmp_dir/prebuilt-target"
  prebuilt_out="$tmp_dir/out-prebuilt"
  prebuilt_log="$tmp_dir/prebuilt.log"
  mkdir -p "$prebuilt_target/release"
  printf '#!/usr/bin/env bash\nprintf "prebuilt-binary\\n"\n' >"$prebuilt_target/release/compme"
  chmod +x "$prebuilt_target/release/compme"
  PATH="$fake_bin:$PATH" \
    CARGO_TARGET_DIR="$prebuilt_target" \
    COMPME_BUNDLE_SKIP_BUILD=1 \
    COMPME_BUNDLE_SELF_TEST_LOG="$prebuilt_log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister" \
    "$0" "$prebuilt_out" >"$tmp_dir/stdout-prebuilt"
  "$prebuilt_out/Compme.app/Contents/MacOS/compme" | grep -Fxq "prebuilt-binary"
  if grep -Fq "cargo build" "$prebuilt_log"; then
    echo "self-test FAILED: prebuilt bundle mode invoked cargo" >&2
    return 1
  fi

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

  sign_fail_log="$tmp_dir/sign-fail.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_BUNDLE_SELF_TEST_LOG="$sign_fail_log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister" \
    COMPME_BUNDLE_CODESIGN_FAIL=sign \
    "$0" "$tmp_dir/out-sign-fail" >"$tmp_dir/stdout-sign-fail" 2>"$tmp_dir/stderr-sign-fail"; then
    echo "codesign signing failure was accepted" >&2
    return 1
  fi
  grep -Fq "codesign --force --sign - $tmp_dir/out-sign-fail/Compme.app" "$sign_fail_log"
  if grep -Fq "lsregister -f $tmp_dir/out-sign-fail/Compme.app" "$sign_fail_log"; then
    echo "self-test FAILED: lsregister ran after codesign signing failure" >&2
    return 1
  fi

  verify_fail_log="$tmp_dir/verify-fail.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_BUNDLE_SELF_TEST_LOG="$verify_fail_log" \
    COMPME_BUNDLE_REPO_ROOT="$fixture_root" \
    COMPME_BUNDLE_LSREGISTER="$fake_bin/lsregister" \
    COMPME_BUNDLE_CODESIGN_FAIL=verify \
    "$0" "$tmp_dir/out-verify-fail" >"$tmp_dir/stdout-verify-fail" 2>"$tmp_dir/stderr-verify-fail"; then
    echo "codesign verify failure was accepted" >&2
    return 1
  fi
  grep -Fq "codesign --verify --strict $tmp_dir/out-verify-fail/Compme.app" "$verify_fail_log"
  if grep -Fq "lsregister -f $tmp_dir/out-verify-fail/Compme.app" "$verify_fail_log"; then
    echo "self-test FAILED: lsregister ran after codesign verify failure" >&2
    return 1
  fi

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp_dir/self-test-argc.err"; then
    echo "self-test FAILED: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: tools/bundle/make-app.sh" "$tmp_dir/self-test-argc.err"
  if "$0" "$tmp_dir/out-extra" unexpected-extra >/dev/null 2>"$tmp_dir/normal-argc.err"; then
    echo "self-test FAILED: extra normal argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: tools/bundle/make-app.sh" "$tmp_dir/normal-argc.err"

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  if [[ "$#" -ne 1 ]]; then
    echo "usage: tools/bundle/make-app.sh [output-dir] | --self-test" >&2
    exit 2
  fi
  unset COMPME_BUNDLE_REPO_ROOT COMPME_BUNDLE_LSREGISTER CARGO_TARGET_DIR
  unset COMPME_BUNDLE_SKIP_BUILD COMPME_CODESIGN_IDENTITY COMPME_CODESIGN_ENTITLEMENTS
  run_self_test
  exit 0
fi

if [[ "$#" -gt 1 ]]; then
  echo "usage: tools/bundle/make-app.sh [output-dir] | --self-test" >&2
  exit 2
fi

out_dir="${1:-"$repo_root/target/bundle"}"
app="$out_dir/Compme.app"

echo "building release binary…"
bundle_target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "${COMPME_BUNDLE_SKIP_BUILD:-0}" == "1" ]]; then
  echo "using prebuilt release binary…"
  if [[ ! -x "$bundle_target_dir/release/compme" ]]; then
    echo "missing prebuilt release binary: $bundle_target_dir/release/compme" >&2
    exit 1
  fi
else
  CARGO_TARGET_DIR="$bundle_target_dir" cargo build --locked --release -p app --manifest-path "$repo_root/Cargo.toml"
fi

echo "assembling $app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$repo_root/tools/bundle/Info.plist" "$app/Contents/Info.plist"
cp "$bundle_target_dir/release/compme" "$app/Contents/MacOS/compme"

# App icon (CFBundleIconFile=AppIcon). Required: a bundle without it shows the
# generic placeholder in Finder/Dock. Regenerate with tools/bundle/make-icon.sh.
icon_src="$repo_root/tools/bundle/AppIcon.icns"
if [[ ! -f "$icon_src" ]]; then
  echo "missing app icon: $icon_src (run tools/bundle/make-icon.sh)" >&2
  exit 1
fi
cp "$icon_src" "$app/Contents/Resources/AppIcon.icns"

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
"$lsregister" -f "$app"

echo "done: $app"
echo "smoke: COMPME_RUN_MS=1500 \"$app/Contents/MacOS/compme\""
