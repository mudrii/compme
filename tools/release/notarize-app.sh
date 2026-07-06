#!/usr/bin/env bash
# Submit a Developer-ID signed Compme.app to Apple's notarization service, wait
# for approval, staple the ticket, and validate the staple.
#
# Authentication can use either:
#   COMPME_NOTARYTOOL_KEYCHAIN_PROFILE
#   COMPME_NOTARYTOOL_KEY_PATH + COMPME_NOTARYTOOL_KEY_ID + COMPME_NOTARYTOOL_ISSUER
#   COMPME_NOTARYTOOL_KEY_BASE64 + COMPME_NOTARYTOOL_KEY_ID + COMPME_NOTARYTOOL_ISSUER
#   COMPME_NOTARYTOOL_APPLE_ID + COMPME_NOTARYTOOL_PASSWORD + COMPME_NOTARYTOOL_TEAM_ID
#
# Usage: tools/release/notarize-app.sh path/to/Compme.app
#        tools/release/notarize-app.sh --self-test
set -euo pipefail

usage() {
  echo "usage: notarize-app.sh path/to/Compme.app | --self-test" >&2
}

notary_auth_args=()

build_notary_auth_args() {
  notary_auth_args=()
  if [[ -n "${COMPME_NOTARYTOOL_KEYCHAIN_PROFILE:-}" ]]; then
    notary_auth_args+=(--keychain-profile "$COMPME_NOTARYTOOL_KEYCHAIN_PROFILE")
    return 0
  fi

  if [[ -n "${COMPME_NOTARYTOOL_KEY_BASE64:-}" ]]; then
    if [[ -z "${COMPME_NOTARYTOOL_KEY_ID:-}" || -z "${COMPME_NOTARYTOOL_ISSUER:-}" ]]; then
      echo "COMPME_NOTARYTOOL_KEY_BASE64 requires COMPME_NOTARYTOOL_KEY_ID and COMPME_NOTARYTOOL_ISSUER" >&2
      return 1
    fi
    local key_file="${COMPME_NOTARYTOOL_TEMP_KEY:-}"
    if [[ -z "$key_file" ]]; then
      echo "COMPME_NOTARYTOOL_TEMP_KEY must be set by the caller before decoding a base64 key" >&2
      return 1
    fi
    printf '%s' "$COMPME_NOTARYTOOL_KEY_BASE64" | base64 --decode >"$key_file"
    chmod 600 "$key_file"
    notary_auth_args+=(--key "$key_file" --key-id "$COMPME_NOTARYTOOL_KEY_ID" --issuer "$COMPME_NOTARYTOOL_ISSUER")
    return 0
  fi

  if [[ -n "${COMPME_NOTARYTOOL_KEY_PATH:-}" ]]; then
    if [[ -z "${COMPME_NOTARYTOOL_KEY_ID:-}" || -z "${COMPME_NOTARYTOOL_ISSUER:-}" ]]; then
      echo "COMPME_NOTARYTOOL_KEY_PATH requires COMPME_NOTARYTOOL_KEY_ID and COMPME_NOTARYTOOL_ISSUER" >&2
      return 1
    fi
    notary_auth_args+=(--key "$COMPME_NOTARYTOOL_KEY_PATH" --key-id "$COMPME_NOTARYTOOL_KEY_ID" --issuer "$COMPME_NOTARYTOOL_ISSUER")
    return 0
  fi

  if [[ -n "${COMPME_NOTARYTOOL_APPLE_ID:-}" || -n "${COMPME_NOTARYTOOL_PASSWORD:-}" || -n "${COMPME_NOTARYTOOL_TEAM_ID:-}" ]]; then
    if [[ -z "${COMPME_NOTARYTOOL_APPLE_ID:-}" || -z "${COMPME_NOTARYTOOL_PASSWORD:-}" || -z "${COMPME_NOTARYTOOL_TEAM_ID:-}" ]]; then
      echo "Apple-ID notarization requires COMPME_NOTARYTOOL_APPLE_ID, COMPME_NOTARYTOOL_PASSWORD, and COMPME_NOTARYTOOL_TEAM_ID" >&2
      return 1
    fi
    notary_auth_args+=(--apple-id "$COMPME_NOTARYTOOL_APPLE_ID" --password "$COMPME_NOTARYTOOL_PASSWORD" --team-id "$COMPME_NOTARYTOOL_TEAM_ID")
    return 0
  fi

  echo "missing notarization credentials; set COMPME_NOTARYTOOL_KEYCHAIN_PROFILE, App Store Connect API key vars, or Apple-ID vars" >&2
  return 1
}

run_self_test() {
  local fake_bin app log
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-notarize-self-test.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  fake_bin="$tmp/bin"
  app="$tmp/Compme.app"
  log="$tmp/commands.log"
  mkdir -p "$fake_bin" "$app/Contents/MacOS"
  printf '<plist version="1.0"><dict></dict></plist>\n' >"$app/Contents/Info.plist"
  printf '#!/usr/bin/env bash\n' >"$app/Contents/MacOS/compme"
  chmod +x "$app/Contents/MacOS/compme"

  cat >"$fake_bin/ditto" <<'SH'
#!/usr/bin/env bash
printf 'ditto %s\n' "$*" >>"$COMPME_NOTARIZE_SELF_TEST_LOG"
out="${@: -1}"
printf 'zip\n' >"$out"
SH
  cat >"$fake_bin/xcrun" <<'SH'
#!/usr/bin/env bash
printf 'xcrun %s\n' "$*" >>"$COMPME_NOTARIZE_SELF_TEST_LOG"
case "${COMPME_NOTARIZE_XCRUN_FAIL:-}" in
  notarytool)
    [ "${1:-}" = "notarytool" ] && exit 41
    ;;
  staple)
    [ "${1:-}" = "stapler" ] && [ "${2:-}" = "staple" ] && exit 42
    ;;
  validate)
    [ "${1:-}" = "stapler" ] && [ "${2:-}" = "validate" ] && exit 43
    ;;
esac
exit 0
SH
  chmod +x "$fake_bin/ditto" "$fake_bin/xcrun"

  PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$app" >"$tmp/stdout"

  grep -Fq "ditto -c -k --keepParent $app " "$log"
  grep -Fq "Compme-notary.zip" "$log"
  grep -Fq "xcrun notarytool submit --wait --timeout 30m --keychain-profile compme-release" "$log"
  grep -Fq "xcrun stapler staple $app" "$log"
  grep -Fq "xcrun stapler validate $app" "$log"

  local key_file="$tmp/AuthKey_TEST.p8"
  PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_TEMP_KEY="$key_file" \
    COMPME_NOTARYTOOL_KEY_BASE64="$(printf 'PRIVATE KEY\n' | base64)" \
    COMPME_NOTARYTOOL_KEY_ID="ABC123DEFG" \
    COMPME_NOTARYTOOL_ISSUER="00000000-0000-0000-0000-000000000000" \
    COMPME_NOTARYTOOL_TIMEOUT="45m" \
    "$0" "$app" >"$tmp/stdout-api"
  grep -Fq "xcrun notarytool submit --wait --timeout 45m --key $key_file --key-id ABC123DEFG --issuer 00000000-0000-0000-0000-000000000000" "$log"
  grep -Fq "PRIVATE KEY" "$key_file"

  notary_fail_log="$tmp/notary-fail.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$notary_fail_log" \
    COMPME_NOTARIZE_XCRUN_FAIL=notarytool \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$app" >"$tmp/notary-fail.out" 2>"$tmp/notary-fail.err"; then
    echo "self-test FAILED: notarytool failure should fail" >&2
    return 1
  fi
  grep -Fq "xcrun notarytool submit" "$notary_fail_log"
  if grep -Fq "xcrun stapler staple $app" "$notary_fail_log"; then
    echo "self-test FAILED: stapler ran after notarytool failure" >&2
    return 1
  fi

  staple_fail_log="$tmp/staple-fail.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$staple_fail_log" \
    COMPME_NOTARIZE_XCRUN_FAIL=staple \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$app" >"$tmp/staple-fail.out" 2>"$tmp/staple-fail.err"; then
    echo "self-test FAILED: stapler staple failure should fail" >&2
    return 1
  fi
  grep -Fq "xcrun stapler staple $app" "$staple_fail_log"
  if grep -Fq "xcrun stapler validate $app" "$staple_fail_log"; then
    echo "self-test FAILED: validate ran after stapler staple failure" >&2
    return 1
  fi

  validate_fail_log="$tmp/validate-fail.log"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$validate_fail_log" \
    COMPME_NOTARIZE_XCRUN_FAIL=validate \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$app" >"$tmp/validate-fail.out" 2>"$tmp/validate-fail.err"; then
    echo "self-test FAILED: stapler validate failure should fail" >&2
    return 1
  fi
  grep -Fq "xcrun stapler validate $app" "$validate_fail_log"

  if PATH="$fake_bin:$PATH" "$0" "$app" >"$tmp/no-creds.out" 2>"$tmp/no-creds.err"; then
    echo "self-test FAILED: missing credentials should fail" >&2
    return 1
  fi
  grep -Fq "missing notarization credentials" "$tmp/no-creds.err"

  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEY_BASE64="$(printf 'PRIVATE KEY\n' | base64)" \
    "$0" "$app" >"$tmp/b64-missing-ids.out" 2>"$tmp/b64-missing-ids.err"; then
    echo "self-test FAILED: base64 key without key id/issuer should fail" >&2
    return 1
  fi
  grep -Fq "COMPME_NOTARYTOOL_KEY_BASE64 requires COMPME_NOTARYTOOL_KEY_ID and COMPME_NOTARYTOOL_ISSUER" "$tmp/b64-missing-ids.err"

  if (
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE=""
    COMPME_NOTARYTOOL_KEY_BASE64="$(printf 'PRIVATE KEY\n' | base64)"
    COMPME_NOTARYTOOL_KEY_ID="ABC123DEFG"
    COMPME_NOTARYTOOL_ISSUER="00000000-0000-0000-0000-000000000000"
    unset COMPME_NOTARYTOOL_TEMP_KEY
    build_notary_auth_args
  ) >"$tmp/no-temp-key.out" 2>"$tmp/no-temp-key.err"; then
    echo "self-test FAILED: base64 key without a caller-set temp key should fail" >&2
    return 1
  fi
  grep -Fq "COMPME_NOTARYTOOL_TEMP_KEY must be set by the caller before decoding a base64 key" "$tmp/no-temp-key.err"

  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEY_PATH="$tmp/AuthKey_TEST.p8" \
    "$0" "$app" >"$tmp/key-path-missing-ids.out" 2>"$tmp/key-path-missing-ids.err"; then
    echo "self-test FAILED: key path without key id/issuer should fail" >&2
    return 1
  fi
  grep -Fq "COMPME_NOTARYTOOL_KEY_PATH requires COMPME_NOTARYTOOL_KEY_ID and COMPME_NOTARYTOOL_ISSUER" "$tmp/key-path-missing-ids.err"

  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_APPLE_ID="release@example.test" \
    "$0" "$app" >"$tmp/partial-apple-id.out" 2>"$tmp/partial-apple-id.err"; then
    echo "self-test FAILED: partial Apple-ID credentials should fail" >&2
    return 1
  fi
  grep -Fq "Apple-ID notarization requires COMPME_NOTARYTOOL_APPLE_ID, COMPME_NOTARYTOOL_PASSWORD, and COMPME_NOTARYTOOL_TEAM_ID" "$tmp/partial-apple-id.err"

  printf 'not a bundle\n' >"$tmp/not-an-app"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$tmp/not-an-app" >"$tmp/not-an-app.out" 2>"$tmp/not-an-app.err"; then
    echo "self-test FAILED: non-directory app path should fail" >&2
    return 1
  fi
  grep -Fq "not an app bundle: $tmp/not-an-app" "$tmp/not-an-app.err"

  plain_dir="$tmp/plain-dir"
  mkdir -p "$plain_dir"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$plain_dir" >"$tmp/plain-dir.out" 2>"$tmp/plain-dir.err"; then
    echo "self-test FAILED: plain directory should fail" >&2
    return 1
  fi
  grep -Fq "not a Compme.app bundle: $plain_dir" "$tmp/plain-dir.err"

  wrong_name="$tmp/Foo.app"
  mkdir -p "$wrong_name/Contents/MacOS"
  printf '<plist version="1.0"><dict></dict></plist>\n' >"$wrong_name/Contents/Info.plist"
  printf '#!/usr/bin/env bash\n' >"$wrong_name/Contents/MacOS/compme"
  chmod +x "$wrong_name/Contents/MacOS/compme"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$wrong_name" >"$tmp/wrong-name.out" 2>"$tmp/wrong-name.err"; then
    echo "self-test FAILED: wrong bundle name should fail" >&2
    return 1
  fi
  grep -Fq "not a Compme.app bundle: $wrong_name" "$tmp/wrong-name.err"

  missing_plist="$tmp/MissingPlist.app"
  mkdir -p "$missing_plist/Contents/MacOS"
  printf '#!/usr/bin/env bash\n' >"$missing_plist/Contents/MacOS/compme"
  chmod +x "$missing_plist/Contents/MacOS/compme"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$missing_plist" >"$tmp/missing-plist.out" 2>"$tmp/missing-plist.err"; then
    echo "self-test FAILED: missing Info.plist should fail" >&2
    return 1
  fi
  grep -Fq "not a Compme.app bundle: $missing_plist" "$tmp/missing-plist.err"

  missing_executable="$tmp/MissingExecutable.app"
  mkdir -p "$missing_executable/Contents/MacOS"
  printf '<plist version="1.0"><dict></dict></plist>\n' >"$missing_executable/Contents/Info.plist"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$missing_executable" >"$tmp/missing-executable.out" 2>"$tmp/missing-executable.err"; then
    echo "self-test FAILED: missing compme executable should fail" >&2
    return 1
  fi
  grep -Fq "not a Compme.app bundle: $missing_executable" "$tmp/missing-executable.err"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "self-test FAILED: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: notarize-app.sh" "$tmp/self-test-argc.err"
  if PATH="$fake_bin:$PATH" \
    COMPME_NOTARIZE_SELF_TEST_LOG="$log" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE="compme-release" \
    "$0" "$app" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "self-test FAILED: extra normal argument was accepted" >&2
    return 1
  fi
  grep -Fq "usage: notarize-app.sh" "$tmp/normal-argc.err"

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

app="${1:-}"
if [[ -z "$app" ]]; then
  usage
  exit 2
fi
if [[ ! -d "$app" ]]; then
  echo "not an app bundle: $app" >&2
  exit 1
fi
if [[ "$(basename "$app")" != "Compme.app" || "$app" != *.app || ! -f "$app/Contents/Info.plist" || ! -x "$app/Contents/MacOS/compme" ]]; then
  echo "not a Compme.app bundle: $app" >&2
  exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-notary.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/notary"
archive="$tmp/notary/Compme-notary.zip"
: "${COMPME_NOTARYTOOL_TEMP_KEY:="$tmp/AuthKey.p8"}"
export COMPME_NOTARYTOOL_TEMP_KEY

echo "creating notarization archive..."
ditto -c -k --keepParent "$app" "$archive"

notary_args=(--wait --timeout "${COMPME_NOTARYTOOL_TIMEOUT:-30m}")
build_notary_auth_args

echo "submitting to Apple notarization..."
xcrun notarytool submit "${notary_args[@]}" "${notary_auth_args[@]}" "$archive"

echo "stapling notarization ticket..."
xcrun stapler staple "$app"
xcrun stapler validate "$app"

echo "notarized and stapled: $app"
