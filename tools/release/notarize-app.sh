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
  mkdir -p "$fake_bin" "$app"

  cat >"$fake_bin/ditto" <<'SH'
#!/usr/bin/env bash
printf 'ditto %s\n' "$*" >>"$COMPME_NOTARIZE_SELF_TEST_LOG"
out="${@: -1}"
printf 'zip\n' >"$out"
SH
  cat >"$fake_bin/xcrun" <<'SH'
#!/usr/bin/env bash
printf 'xcrun %s\n' "$*" >>"$COMPME_NOTARIZE_SELF_TEST_LOG"
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

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  run_self_test
  exit 0
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
