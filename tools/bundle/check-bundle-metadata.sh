#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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
  cask_version = File.read(cask_path)[/^\s*version\s+"([^"]+)"/, 1]
  plist_version = value_after.call("CFBundleShortVersionString")
  errors << "crates/app Cargo.toml: missing package version" unless cargo_version
  errors << "Casks/compme.rb: missing cask version" unless cask_version
  if cargo_version && cask_version
    expect.call("CFBundleShortVersionString", plist_version, cargo_version)
    errors << "version drift: cask #{cask_version.inspect} != app #{cargo_version.inspect}" unless cask_version == cargo_version
  end

  unless errors.empty?
    warn("bundle metadata check failed:")
    errors.each { |error| warn("  - #{error}") }
    exit 1
  end

  puts "Bundle metadata OK: version=#{plist_version} id=com.compme.app executable=compme scheme=compme"
' "$info_plist" "$app_manifest" "$cask_file"
