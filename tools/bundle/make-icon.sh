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

run_self_test() {
  tmp_test="$(mktemp -d "${TMPDIR:-/tmp}/compme-make-icon-self-test.XXXXXX")"
  cleanup_self_test() {
    rm -rf "$tmp_test"
  }
  trap cleanup_self_test EXIT

  fake_bin="$tmp_test/bin"
  log="$tmp_test/commands.log"
  source_png="$tmp_test/source.png"
  icon_before="$tmp_test/AppIcon.icns.before"
  mkdir -p "$fake_bin"
  printf 'png fixture' >"$source_png"
  cp "$here/AppIcon.icns" "$icon_before"

  for tool in swift sips iconutil; do
    cat >"$fake_bin/$tool" <<'SH'
#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >>"$COMPME_ICON_SELF_TEST_LOG"
SH
    chmod +x "$fake_bin/$tool"
  done

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

out="$here/AppIcon.icns"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-icon.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

src="${ICON_SRC:-}"
if [[ -z "$src" ]]; then
  # No supplied art — render the placeholder squircle to a 1024px PNG.
  src="$tmp/icon-1024.png"
  swift "$here/make-icon.swift" "$src"
fi

iconset="$tmp/AppIcon.iconset"
mkdir -p "$iconset"
# Apple's required iconset sizes (pt @1x/@2x).
for spec in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x 256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
  px="${spec%%:*}"
  name="${spec##*:}"
  sips -z "$px" "$px" "$src" --out "$iconset/icon_${name}.png" >/dev/null
done

iconutil -c icns "$iconset" -o "$out"
echo "wrote $out"
