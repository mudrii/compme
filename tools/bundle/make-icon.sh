#!/usr/bin/env bash
# Regenerate tools/bundle/AppIcon.icns — the Compme app icon.
#
# Placeholder art: a rounded-square indigo→violet gradient squircle with a
# white "C". Run once and COMMIT the resulting AppIcon.icns; make-app.sh only
# copies it, so the release pipeline needs no swift/iconutil step. Re-run this
# to swap the art (drop in a real 1024px design at $ICON_SRC to skip the
# generated glyph).
#
# Usage: tools/bundle/make-icon.sh   (writes tools/bundle/AppIcon.icns)
#        tools/bundle/make-icon.sh --self-test
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  echo "usage: tools/bundle/make-icon.sh | --self-test" >&2
}

generate_icon() (
  set -euo pipefail
  out="$1"
  src="$2"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-icon.XXXXXX")"
  generated=""
  trap 'rm -rf "$tmp"; if [[ -n "$generated" ]]; then rm -f "$generated"; fi' EXIT

  if [[ -z "$src" ]]; then
    src="$tmp/icon-1024.png"
    if ! swift "$here/make-icon.swift" "$src"; then
      echo "Swift icon rendering failed" >&2
      return 1
    fi
  fi

  iconset="$tmp/AppIcon.iconset"
  mkdir -p "$iconset"
  for spec in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x 256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
    px="${spec%%:*}"
    name="${spec##*:}"
    if ! sips -z "$px" "$px" "$src" --out "$iconset/icon_${name}.png" >/dev/null; then
      echo "sips failed for ${name}" >&2
      return 1
    fi
  done

  out_dir="$(dirname "$out")"
  generated="$(mktemp "$out_dir/.AppIcon.icns.XXXXXX")"
  if ! iconutil -c icns "$iconset" -o "$generated"; then
    echo "iconutil failed" >&2
    return 1
  fi
  if [[ ! -s "$generated" ]]; then
    echo "iconutil did not produce a non-empty icon" >&2
    return 1
  fi
  chmod 0644 "$generated"
  mv "$generated" "$out"
  echo "wrote $out"
)

run_self_test() {
  tmp_test="$(mktemp -d "${TMPDIR:-/tmp}/compme-make-icon-self-test.XXXXXX")"
  cleanup_self_test() {
    rm -rf "$tmp_test"
  }
  trap cleanup_self_test EXIT

  fake_bin="$tmp_test/bin"
  log="$tmp_test/commands.log"
  work_tmp="$tmp_test/work-tmp"
  output_dir="$tmp_test/output"
  source_png="$output_dir/source.png"
  generated_icon="$output_dir/generated.icns"
  sips_count="$tmp_test/sips.count"
  icon_before="$tmp_test/AppIcon.icns.before"
  mkdir -p "$fake_bin" "$work_tmp" "$output_dir"
  printf 'png fixture' >"$source_png"
  printf '0\n' >"$sips_count"
  cp "$here/AppIcon.icns" "$icon_before"

  cat >"$fake_bin/swift" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s %s\n' "$(basename "$0")" "$*" >>"$COMPME_ICON_SELF_TEST_LOG"
printf 'generated png\n' >"$2"
SH
  cat >"$fake_bin/sips" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s %s\n' "$(basename "$0")" "$*" >>"$COMPME_ICON_SELF_TEST_LOG"
count="$(cat "$COMPME_ICON_SELF_TEST_SIPS_COUNT")"
case "$count" in
  0) expected="16:16x16" ;;
  1) expected="32:16x16@2x" ;;
  2) expected="32:32x32" ;;
  3) expected="64:32x32@2x" ;;
  4) expected="128:128x128" ;;
  5) expected="256:128x128@2x" ;;
  6) expected="256:256x256" ;;
  7) expected="512:256x256@2x" ;;
  8) expected="512:512x512" ;;
  9) expected="1024:512x512@2x" ;;
  *) exit 3 ;;
esac
expected_px="${expected%%:*}"
expected_name="${expected##*:}"
[[ "$#" -eq 6 && "$1" == "-z" && "$2" == "$expected_px" && "$3" == "$expected_px" && "$5" == "--out" ]]
[[ "$(basename "$6")" == "icon_${expected_name}.png" ]]
if [[ -n "${COMPME_ICON_SELF_TEST_EXPECTED_SRC:-}" ]]; then
  [[ "$4" == "$COMPME_ICON_SELF_TEST_EXPECTED_SRC" ]]
else
  [[ "$(basename "$4")" == "icon-1024.png" ]]
fi
printf '%s\n' "$((count + 1))" >"$COMPME_ICON_SELF_TEST_SIPS_COUNT"
while [[ "$#" -gt 0 ]]; do
  if [[ "$1" == "--out" ]]; then
    printf 'resized png\n' >"$2"
    exit 0
  fi
  shift
done
exit 2
SH
  cat >"$fake_bin/iconutil" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s %s\n' "$(basename "$0")" "$*" >>"$COMPME_ICON_SELF_TEST_LOG"
[[ "$#" -eq 5 && "$1" == "-c" && "$2" == "icns" && "$(basename "$3")" == "AppIcon.iconset" && "$4" == "-o" ]]
for name in 16x16 16x16@2x 32x32 32x32@2x 128x128 128x128@2x 256x256 256x256@2x 512x512 512x512@2x; do
  [[ -s "$3/icon_${name}.png" ]]
done
while [[ "$#" -gt 0 ]]; do
  if [[ "$1" == "-o" ]]; then
    printf 'generated icns\n' >"$2"
    if [[ "${COMPME_ICON_SELF_TEST_FAIL_ICONUTIL:-0}" == "1" ]]; then
      exit 41
    fi
    exit 0
  fi
  shift
done
exit 2
SH
  chmod +x "$fake_bin/swift" "$fake_bin/sips" "$fake_bin/iconutil"

  set +e
  PATH="$fake_bin:$PATH" \
  ICON_SRC="$source_png" \
  COMPME_ICON_SELF_TEST_LOG="$log" \
    "$0" unexpected-extra >"$tmp_test/stdout" 2>"$tmp_test/stderr"
  status=$?
  set -e
  if [[ "$status" -ne 2 ]]; then
    echo "make-icon self-test failed: unexpected argument exited $status, expected 2" >&2
    return 1
  fi
  if [[ "$(cat "$tmp_test/stderr")" != "usage: tools/bundle/make-icon.sh | --self-test" ]]; then
    echo "make-icon self-test failed: unexpected argument did not print exact usage" >&2
    return 1
  fi
  if [[ -s "$log" ]]; then
    echo "make-icon self-test failed: a tool ran after argument rejection" >&2
    return 1
  fi
  if ! cmp -s "$icon_before" "$here/AppIcon.icns"; then
    echo "make-icon self-test failed: rejected invocation modified AppIcon.icns" >&2
    return 1
  fi

  : >"$log"
  printf '0\n' >"$sips_count"
  PATH="$fake_bin:$PATH" COMPME_ICON_SELF_TEST_LOG="$log" \
    COMPME_ICON_SELF_TEST_SIPS_COUNT="$sips_count" TMPDIR="$work_tmp" \
    generate_icon "$generated_icon" "" >"$tmp_test/generated.out"
  if [[ "$(grep -c '^swift ' "$log")" -ne 1 ]] ||
    [[ "$(grep -c '^sips ' "$log")" -ne 10 ]] ||
    [[ "$(grep -c '^iconutil ' "$log")" -ne 1 ]]; then
    echo "make-icon self-test failed: generated-source command contract drifted" >&2
    return 1
  fi
  if [[ "$(cat "$sips_count")" -ne 10 ]]; then
    echo "make-icon self-test failed: generated-source size sequence drifted" >&2
    return 1
  fi
  if [[ "$(cat "$generated_icon")" != "generated icns" ]]; then
    echo "make-icon self-test failed: generated-source icon was not installed" >&2
    return 1
  fi
  if [[ "$(stat -f '%Lp' "$generated_icon")" != "644" ]]; then
    echo "make-icon self-test failed: generated icon permissions are not 0644" >&2
    return 1
  fi

  : >"$log"
  printf '0\n' >"$sips_count"
  PATH="$fake_bin:$PATH" COMPME_ICON_SELF_TEST_LOG="$log" \
    COMPME_ICON_SELF_TEST_SIPS_COUNT="$sips_count" \
    COMPME_ICON_SELF_TEST_EXPECTED_SRC="$source_png" TMPDIR="$work_tmp" \
    generate_icon "$generated_icon" "$source_png" >"$tmp_test/supplied.out"
  if grep -q '^swift ' "$log" || [[ "$(grep -c '^sips ' "$log")" -ne 10 ]] ||
    [[ "$(grep -c '^iconutil ' "$log")" -ne 1 ]]; then
    echo "make-icon self-test failed: supplied-source command contract drifted" >&2
    return 1
  fi

  printf 'last good icon\n' >"$generated_icon"
  : >"$log"
  printf '0\n' >"$sips_count"
  if PATH="$fake_bin:$PATH" COMPME_ICON_SELF_TEST_LOG="$log" \
    COMPME_ICON_SELF_TEST_SIPS_COUNT="$sips_count" \
    COMPME_ICON_SELF_TEST_EXPECTED_SRC="$source_png" \
    COMPME_ICON_SELF_TEST_FAIL_ICONUTIL=1 TMPDIR="$work_tmp" \
    generate_icon "$generated_icon" "$source_png" >"$tmp_test/fail.out" 2>"$tmp_test/fail.err"; then
    echo "make-icon self-test failed: iconutil failure was accepted" >&2
    return 1
  fi
  if [[ "$(cat "$generated_icon")" != "last good icon" ]]; then
    echo "make-icon self-test failed: iconutil failure replaced the last good icon" >&2
    return 1
  fi
  if find "$work_tmp" -maxdepth 1 -type d -name 'compme-icon.*' | grep -q .; then
    echo "make-icon self-test failed: generator temporary directory leaked" >&2
    return 1
  fi
  if find "$output_dir" -maxdepth 1 -type f -name '.AppIcon.icns.*' | grep -q .; then
    echo "make-icon self-test failed: atomic output temporary file leaked" >&2
    return 1
  fi

  assert_swift_rejects() {
    label="$1"
    shift
    set +e
    "$real_swift" "$here/make-icon.swift" "$@" >"$tmp_test/swift-$label.out" 2>"$tmp_test/swift-$label.err"
    status=$?
    set -e
    if [[ "$status" -ne 2 ]] ||
      [[ "$(cat "$tmp_test/swift-$label.err")" != "usage: swift make-icon.swift <output.png>" ]]; then
      echo "make-icon self-test failed: Swift helper accepted invalid arguments" >&2
      return 1
    fi
  }
  real_swift="$(command -v swift)"
  assert_swift_rejects missing
  assert_swift_rejects extra "$tmp_test/unexpected.png" extra
  if [[ -e "$tmp_test/unexpected.png" ]]; then
    echo "make-icon self-test failed: Swift helper wrote output for invalid arguments" >&2
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

if [[ "$#" -ne 0 ]]; then
  usage
  exit 2
fi

generate_icon "$here/AppIcon.icns" "${ICON_SRC:-}"
