#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-bundle-meta-test.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  cargo="$tmp/Cargo.toml"
  printf 'version = "1.2.3"\n' >"$cargo"

  write_plist() {
    cat >"$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>com.compme.app</string>
  <key>CFBundleExecutable</key><string>compme</string>
  <key>CFBundleShortVersionString</key><string>1.2.3</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLSchemes</key>
      <array><string>${2}</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
  }

  write_cask() {
    cat >"$1" <<CASK
cask "compme" do
  version "${2}"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  depends_on macos: ">= :sonoma"
end
CASK
  }

  good_plist="$tmp/good.plist"
  write_plist "$good_plist" compme
  bad_scheme_plist="$tmp/bad-scheme.plist"
  write_plist "$bad_scheme_plist" notcompme
  good_cask="$tmp/good.rb"
  write_cask "$good_cask" 1.2.3
  drift_cask="$tmp/drift.rb"
  write_cask "$drift_cask" 9.9.9
  tag_drift_cask="$tmp/tag-good.rb"
  write_cask "$tag_drift_cask" 1.2.3

  # (a) version drift: cask version != Cargo.toml version -> non-zero + drift error.
  if out="$("$0" "$good_plist" "$cargo" "$drift_cask" 2>&1)"; then
    echo "self-test FAILED: drift cask should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"version drift"*) ;;
    *) echo "self-test FAILED: expected version-drift error, got: $out" >&2; exit 1 ;;
  esac

  # (b) release tag drift: expected version != bundle metadata -> non-zero.
  if out="$(COMPME_EXPECTED_VERSION=9.9.9 "$0" "$good_plist" "$cargo" "$tag_drift_cask" 2>&1)"; then
    echo "self-test FAILED: tag drift should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"release tag version drift"*) ;;
    *) echo "self-test FAILED: expected release-tag drift error, got: $out" >&2; exit 1 ;;
  esac

  # (c) missing 'compme' CFBundleURLScheme -> non-zero.
  if out="$("$0" "$bad_scheme_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: missing scheme should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"CFBundleURLSchemes: missing compme"*) ;;
    *) echo "self-test FAILED: expected missing-scheme error, got: $out" >&2; exit 1 ;;
  esac

  # (d) all-consistent fixtures -> exits 0 with OK message.
  if ! out="$("$0" "$good_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: consistent fixtures should pass, got: $out" >&2
    exit 1
  fi
  case "$out" in
    *"Bundle metadata OK"*) ;;
    *) echo "self-test FAILED: expected OK message, got: $out" >&2; exit 1 ;;
  esac

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  run_self_test
  exit 0
fi

info_plist="${1:-"$repo_root/tools/bundle/Info.plist"}"
app_manifest="${2:-"$repo_root/crates/app/Cargo.toml"}"
cask_file="${3:-"$repo_root/Casks/compme.rb"}"

ruby -rrexml/document -e '
  info_path, cargo_path, cask_path = ARGV
  info = REXML::Document.new(File.read(info_path))
  dict = info.root.elements["dict"]
  abort("missing bundle metadata: plist dict") unless dict

  elements = dict.elements.to_a
  value_after = lambda do |key|
    idx = elements.find_index { |element| element.name == "key" && element.text == key }
    abort("missing bundle metadata: #{key}") unless idx && elements[idx + 1]
    value = elements[idx + 1]
    case value.name
    when "string" then value.text
    when "true" then true
    when "false" then false
    else value
    end
  end

  errors = []
  expect = lambda do |label, actual, expected|
    errors << "#{label}: expected #{expected.inspect}, got #{actual.inspect}" unless actual == expected
  end

  expect.call("CFBundleIdentifier", value_after.call("CFBundleIdentifier"), "com.compme.app")
  expect.call("CFBundleExecutable", value_after.call("CFBundleExecutable"), "compme")
  expect.call("LSMinimumSystemVersion", value_after.call("LSMinimumSystemVersion"), "14.0")
  expect.call("LSUIElement", value_after.call("LSUIElement"), true)

  schemes = []
  collect_schemes = lambda do |parent|
    children = parent.elements.to_a
    children.each_with_index do |element, idx|
      if element.name == "key" && element.text == "CFBundleURLSchemes"
        array = children[idx + 1]
        schemes.concat(array.elements.to_a("string").map(&:text)) if array&.name == "array"
      else
        collect_schemes.call(element)
      end
    end
  end
  collect_schemes.call(dict)
  errors << "CFBundleURLSchemes: missing compme" unless schemes.include?("compme")

  cargo_version = File.read(cargo_path)[/^version\s*=\s*"([^"]+)"/, 1]
  cask_text = File.read(cask_path)
  cask_version = cask_text[/^\s*version\s+"([^"]+)"/, 1]
  cask_macos = cask_text[/^\s*depends_on\s+macos:\s+">=\s*:(\w+)"/, 1]
  plist_version = value_after.call("CFBundleShortVersionString")
  errors << "crates/app Cargo.toml: missing package version" unless cargo_version
  errors << "Casks/compme.rb: missing cask version" unless cask_version
  errors << "Casks/compme.rb: macOS floor must be >= :sonoma" unless cask_macos == "sonoma"
  if cargo_version && cask_version
    expect.call("CFBundleShortVersionString", plist_version, cargo_version)
    errors << "version drift: cask #{cask_version.inspect} != app #{cargo_version.inspect}" unless cask_version == cargo_version
  end
  expected_version = ENV["COMPME_EXPECTED_VERSION"]
  if expected_version && !expected_version.empty? && plist_version != expected_version
    errors << "release tag version drift: expected #{expected_version.inspect}, got #{plist_version.inspect}"
  end

  unless errors.empty?
    warn("bundle metadata check failed:")
    errors.each { |error| warn("  - #{error}") }
    exit 1
  end

  puts "Bundle metadata OK: version=#{plist_version} id=com.compme.app executable=compme scheme=compme macos_min=14.0"
' "$info_plist" "$app_manifest" "$cask_file"
