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
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
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
