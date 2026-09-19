#!/usr/bin/env bash
# Hermetic packaging orchestration tests; real ELF/desktop/AppStream validation
# and an extracted-artifact smoke run remain part of native release validation.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-appimage-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/input space" "$tmp/out"
printf '#!/bin/sh\nexit 0\n' >"$tmp/input space/compme"
chmod +x "$tmp/input space/compme"
printf 'runtime' >"$tmp/input space/runtime"
export COMPME_APPIMAGE_TEST_ROOT="$tmp"
cat >"$tmp/bin/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) exit 2 ;; esac
SH
cat >"$tmp/bin/readelf" <<'SH'
#!/bin/sh
case "$1" in
  -h) echo 'Machine: Advanced Micro Devices X86-64' ;;
  -l) if test -f "$COMPME_APPIMAGE_TEST_ROOT/nix"; then echo /nix/store/loader; fi ;;
  *) exit 2 ;;
esac
SH
cat >"$tmp/bin/linuxdeploy" <<'SH'
#!/bin/sh
test "$1" = --appdir && test "$3" = --executable && test "$5" = --desktop-file && test "$7" = --icon-file || exit 2
test -f "$4" && test -f "$6" && test -f "$8" || exit 3
test ! -f "$COMPME_APPIMAGE_TEST_ROOT/fail-deploy" || exit 17
mkdir -p "$2/usr/bin"
cp "$4" "$2/usr/bin/compme"
SH
cat >"$tmp/bin/appimagetool" <<'SH'
#!/bin/sh
test "$ARCH" = x86_64 && test "$1" = --runtime-file && test -s "$2" || exit 2
test -x "$3/usr/bin/compme" || exit 3
test ! -f "$COMPME_APPIMAGE_TEST_ROOT/fail-image" || exit 18
cp "$3/usr/bin/compme" "$4"
SH
for validator in desktop-file-validate appstreamcli; do
  printf '#!/bin/sh\nexit 0\n' >"$tmp/bin/$validator"
done
chmod +x "$tmp/bin/"*
export PATH="$tmp/bin:$PATH"
build() {
  bash "$repo_root/tools/bundle/make-appimage.sh" \
    --binary "$tmp/input space/compme" --runtime "$tmp/input space/runtime" --output "$tmp/out/$1"
}
build success.AppImage
test -x "$tmp/out/success.AppImage"
cp "$tmp/out/success.AppImage" "$tmp/original"
if build success.AppImage; then echo 'Overwrite accepted' >&2; exit 1; fi
cmp "$tmp/original" "$tmp/out/success.AppImage"
for failure in fail-deploy fail-image nix; do
  touch "$tmp/$failure"
  if build "$failure.AppImage"; then echo "Failure accepted: $failure" >&2; exit 1; fi
  test ! -e "$tmp/out/$failure.AppImage"
  rm "$tmp/$failure"
done
if bash "$repo_root/tools/bundle/make-appimage.sh" --binary; then echo 'Missing argument accepted' >&2; exit 1; fi
echo 'AppImage packaging self-test passed'
