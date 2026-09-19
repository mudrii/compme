#!/usr/bin/env bash
# Build an experimental Linux AppImage from a prebuilt native release binary.
# No tool/runtime downloads: callers supply reviewed linuxdeploy/appimagetool
# binaries on PATH and an architecture-matched AppImage runtime file.
set -euo pipefail
export LC_ALL=C
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

usage() {
  echo "usage: $0 --binary PATH --runtime PATH --output PATH" >&2
  echo "       $0 --self-test" >&2
}

if [[ "${1:-}" == --self-test && $# == 1 ]]; then
  exec bash "$repo_root/tools/bundle/test-appimage.sh"
fi
binary= runtime= output=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary|--runtime|--output)
      [[ $# -ge 2 && -n "$2" ]] || { usage; exit 2; }
      case "$1" in
        --binary) binary="$2" ;;
        --runtime) runtime="$2" ;;
        --output) output="$2" ;;
      esac
      shift 2 ;;
    --help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
[[ -n "$binary" && -n "$runtime" && -n "$output" ]] || { usage; exit 2; }
[[ "$(uname -s)" == Linux ]] || { echo 'Linux packaging requires Linux' >&2; exit 1; }
[[ -f "$binary" && -x "$binary" && -s "$runtime" ]] || {
  echo 'A native executable and a nonempty AppImage runtime are required' >&2; exit 1;
}
[[ ! -e "$output" && ! -L "$output" ]] || { echo 'Refusing to overwrite output' >&2; exit 1; }
for tool in linuxdeploy appimagetool readelf desktop-file-validate appstreamcli; do
  command -v "$tool" >/dev/null || { echo "Missing packaging tool: $tool" >&2; exit 1; }
done
# Nix's absolute loader paths do not survive transfer to another distribution.
# Also fail before staging on non-ELF or foreign-architecture input.
headers="$(readelf -h "$binary")"
case "$(uname -m)" in
  x86_64) architecture=x86_64; machine='Advanced Micro Devices X86-64' ;;
  aarch64) architecture=aarch64; machine='AArch64' ;;
  *) echo 'Only x86_64 and aarch64 AppImages are supported' >&2; exit 1 ;;
esac
grep -Fq "$machine" <<<"$headers" || { echo 'Binary architecture does not match this host' >&2; exit 1; }
runtime_headers="$(readelf -h "$runtime")"
grep -Fq "$machine" <<<"$runtime_headers" || { echo 'Runtime architecture does not match this host' >&2; exit 1; }
program_headers="$(readelf -l "$binary")"
if grep -Fq /nix/store/ <<<"$program_headers"; then
  echo 'Build on the oldest supported distribution; Nix loader paths are not portable' >&2
  exit 1
fi
desktop-file-validate "$repo_root/tools/bundle/com.compme.app.desktop"
appstreamcli validate --no-net "$repo_root/tools/bundle/com.compme.app.metainfo.xml"
mkdir -p "$(dirname "$output")"
output="$(cd "$(dirname "$output")" && pwd)/$(basename "$output")"
stage="$(mktemp -d "$(dirname "$output")/.compme-appimage.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
appdir="$stage/Compme.AppDir"
mkdir -p "$appdir/usr/share/metainfo" "$stage/icons"
cp "$repo_root/tools/bundle/com.compme.app.metainfo.xml" "$appdir/usr/share/metainfo/"
cp "$repo_root/crates/platform_macos/assets/tray-icon.svg" "$stage/icons/com.compme.app.svg"
linuxdeploy --appdir "$appdir" --executable "$binary" \
  --desktop-file "$repo_root/tools/bundle/com.compme.app.desktop" \
  --icon-file "$stage/icons/com.compme.app.svg"
ARCH="$architecture" appimagetool --runtime-file "$runtime" "$appdir" "$stage/Compme.AppImage"
[[ -s "$stage/Compme.AppImage" && -x "$stage/Compme.AppImage" ]] || {
  echo 'AppImage tool did not produce an executable artifact' >&2; exit 1;
}
# Hard-link publication fails rather than overwriting an output created by
# another process since the initial check. Staging is on the same filesystem.
ln "$stage/Compme.AppImage" "$output"
echo "Experimental AppImage: $output"
