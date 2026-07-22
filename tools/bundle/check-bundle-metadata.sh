#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

usage() {
  echo "usage: check-bundle-metadata.sh [Info.plist Cargo.toml Cask.rb] | --self-test" >&2
}

run_self_test() {
  for name in COMPME_EXPECTED_VERSION COMPME_CASK_TAG_CANDIDATES; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "self-test FAILED: inherited $name" >&2
      return 1
    fi
  done
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-bundle-meta-test.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  cargo="$tmp/Cargo.toml"
  printf 'version = "1.2.3"\n' >"$cargo"

  write_plist() {
    min_version="${3:-14.0}"
    bundle_id="${4:-com.compme.app}"
    executable="${5:-compme}"
    version="${6:-1.2.3}"
    lsui="${7:-true}"
    bundle_version="${8:-$version}"
    case "$lsui" in
      true) lsui_xml='<key>LSUIElement</key><true/>' ;;
      false) lsui_xml='<key>LSUIElement</key><false/>' ;;
      missing) lsui_xml='' ;;
      *) echo "invalid test LSUIElement fixture: $lsui" >&2; exit 1 ;;
    esac
    cat >"$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>${bundle_id}</string>
  <key>CFBundleExecutable</key><string>${executable}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${bundle_version}</string>
  <key>LSMinimumSystemVersion</key><string>${min_version}</string>
  ${lsui_xml}
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
    floor="${3:-sonoma}"
    arch="${4:-arm64}"
    cat >"$1" <<CASK
cask "compme" do
  version "${2}"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  depends_on macos: :${floor}
  depends_on arch: :${arch}
end
CASK
  }

  good_plist="$tmp/good.plist"
  write_plist "$good_plist" compme
  bad_scheme_plist="$tmp/bad-scheme.plist"
  write_plist "$bad_scheme_plist" notcompme
  bad_min_plist="$tmp/bad-min-version.plist"
  write_plist "$bad_min_plist" compme 13.0
  bad_id_plist="$tmp/bad-id.plist"
  write_plist "$bad_id_plist" compme 14.0 com.example.compme
  bad_executable_plist="$tmp/bad-executable.plist"
  write_plist "$bad_executable_plist" compme 14.0 com.compme.app Compme
  bad_lsui_false_plist="$tmp/bad-lsui-false.plist"
  write_plist "$bad_lsui_false_plist" compme 14.0 com.compme.app compme 1.2.3 false
  bad_lsui_missing_plist="$tmp/bad-lsui-missing.plist"
  write_plist "$bad_lsui_missing_plist" compme 14.0 com.compme.app compme 1.2.3 missing
  bad_plist_version="$tmp/bad-plist-version.plist"
  write_plist "$bad_plist_version" compme 14.0 com.compme.app compme 9.9.9
  bad_bundle_version_plist="$tmp/bad-bundle-version.plist"
  write_plist "$bad_bundle_version_plist" compme 14.0 com.compme.app compme 1.2.3 true 9.9.9
  good_cask="$tmp/good.rb"
  write_cask "$good_cask" 1.2.3
  drift_cask="$tmp/drift.rb"
  write_cask "$drift_cask" 9.9.9
  tag_drift_cask="$tmp/tag-good.rb"
  write_cask "$tag_drift_cask" 1.2.3
  ventura_cask="$tmp/ventura.rb"
  write_cask "$ventura_cask" 1.2.3 ventura
  wrong_arch_cask="$tmp/wrong-arch.rb"
  write_cask "$wrong_arch_cask" 1.2.3 sonoma x86_64
  missing_arch_cask="$tmp/missing-arch.rb"
  write_cask "$missing_arch_cask" 1.2.3
  ruby -0pi -e 'sub(/^  depends_on arch: :arm64\n/, "")' "$missing_arch_cask"
  malformed_cask="$tmp/malformed.rb"
  printf 'cask "compme" do\n  version "1.2.3\nend\n' >"$malformed_cask"
  prerelease_plist="$tmp/prerelease.plist"
  write_plist "$prerelease_plist" compme 14.0 com.compme.app compme 1.2.3-rc.1 true 1.2.3-rc.1
  prerelease_cargo="$tmp/prerelease-Cargo.toml"
  printf 'version = "1.2.3-rc.1"\n' >"$prerelease_cargo"
  prerelease_cask="$tmp/prerelease.rb"
  write_cask "$prerelease_cask" 1.2.3-rc.1

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

  # In-flight release window: with stable release tags visible, the cask may
  # also name the latest release tag's version (the cask is rewritten only at
  # cask-finalization time), but never a third version.
  window_repo="$tmp/window-repo"
  mkdir -p "$window_repo"
  git init -q --initial-branch=main "$window_repo"
  git -C "$window_repo" -c user.name=t -c user.email=t@example.test \
    -c commit.gpgsign=false commit --allow-empty -m init >/dev/null
  git -C "$window_repo" -c tag.gpgSign=false tag v1.2.2
  lagging_cask="$window_repo/lagging.rb"
  write_cask "$lagging_cask" 1.2.2
  if ! out="$("$0" "$good_plist" "$cargo" "$lagging_cask" 2>&1)"; then
    echo "self-test FAILED: lagging cask in release window should pass, got: $out" >&2
    exit 1
  fi
  # A tag matching the app version (just pushed) does not shrink the window.
  git -C "$window_repo" -c tag.gpgSign=false tag v1.2.3
  if ! out="$("$0" "$good_plist" "$cargo" "$lagging_cask" 2>&1)"; then
    echo "self-test FAILED: lagging cask after tag push should pass, got: $out" >&2
    exit 1
  fi
  finalized_cask="$window_repo/finalized.rb"
  write_cask "$finalized_cask" 1.2.3
  if ! out="$("$0" "$good_plist" "$cargo" "$finalized_cask" 2>&1)"; then
    echo "self-test FAILED: finalized cask should pass, got: $out" >&2
    exit 1
  fi
  wrong_window_cask="$window_repo/wrong.rb"
  write_cask "$wrong_window_cask" 9.9.9
  if out="$("$0" "$good_plist" "$cargo" "$wrong_window_cask" 2>&1)"; then
    echo "self-test FAILED: third-version cask in release window should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"version drift"*) ;;
    *) echo "self-test FAILED: expected version-drift error, got: $out" >&2; exit 1 ;;
  esac
  two_behind_cask="$window_repo/two-behind.rb"
  write_cask "$two_behind_cask" 1.2.1
  if out="$("$0" "$good_plist" "$cargo" "$two_behind_cask" 2>&1)"; then
    echo "self-test FAILED: two-releases-behind cask should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"version drift"*) ;;
    *) echo "self-test FAILED: expected version-drift error, got: $out" >&2; exit 1 ;;
  esac

  # No visible tags (shallow checkout, fetch unavailable): strict cask==app.
  tagless_repo="$tmp/tagless-repo"
  mkdir -p "$tagless_repo"
  git init -q --initial-branch=main "$tagless_repo"
  git -C "$tagless_repo" -c user.name=t -c user.email=t@example.test \
    -c commit.gpgsign=false commit --allow-empty -m init >/dev/null
  tagless_cask="$tagless_repo/lagging.rb"
  write_cask "$tagless_cask" 1.2.2
  if out="$("$0" "$good_plist" "$cargo" "$tagless_cask" 2>&1)"; then
    echo "self-test FAILED: lagging cask without visible tags should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"version drift"*) ;;
    *) echo "self-test FAILED: expected version-drift error, got: $out" >&2; exit 1 ;;
  esac

  # Tag-checkout shape: actions/checkout on a tag push materializes only the
  # release tag itself, so the previous release tag exists only on the remote.
  # The best-effort fetch must pull it in for the cask window to engage.
  tag_origin="$tmp/tag-origin"
  git init -q --bare --initial-branch=main "$tag_origin"
  tag_seed="$tmp/tag-seed"
  git init -q --initial-branch=main "$tag_seed"
  git -C "$tag_seed" -c user.name=t -c user.email=t@example.test \
    -c commit.gpgsign=false commit --allow-empty -m init >/dev/null
  git -C "$tag_seed" -c tag.gpgSign=false tag v1.2.2
  git -C "$tag_seed" push -q "$tag_origin" main --tags
  tag_checkout_repo="$tmp/tag-checkout-repo"
  git init -q --initial-branch=main "$tag_checkout_repo"
  git -C "$tag_checkout_repo" -c user.name=t -c user.email=t@example.test \
    -c commit.gpgsign=false commit --allow-empty -m init >/dev/null
  git -C "$tag_checkout_repo" -c tag.gpgSign=false tag v1.2.3
  git -C "$tag_checkout_repo" remote add origin "$tag_origin"
  visible_tags="$(git -C "$tag_checkout_repo" tag --list 'v[0-9]*')"
  if [ "$visible_tags" != "v1.2.3" ]; then
    echo "self-test FAILED: tag-checkout fixture should see only v1.2.3, got: $visible_tags" >&2
    exit 1
  fi
  tag_checkout_cask="$tag_checkout_repo/lagging.rb"
  write_cask "$tag_checkout_cask" 1.2.2
  if ! out="$("$0" "$good_plist" "$cargo" "$tag_checkout_cask" 2>&1)"; then
    echo "self-test FAILED: lagging cask in tag-checkout shape should pass, got: $out" >&2
    exit 1
  fi

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

  if out="$(COMPME_EXPECTED_VERSION= "$0" "$good_plist" "$cargo" "$tag_drift_cask" 2>&1)"; then
    echo "self-test FAILED: empty release tag version should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"release tag version is empty"*) ;;
    *) echo "self-test FAILED: expected empty release-tag version error, got: $out" >&2; exit 1 ;;
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

  # (d) stale bundle macOS floor -> non-zero.
  if out="$("$0" "$bad_min_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: bundle macOS floor should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"LSMinimumSystemVersion"*) ;;
    *) echo "self-test FAILED: expected bundle macOS-floor error, got: $out" >&2; exit 1 ;;
  esac

  # (e) stale cask macOS floor -> non-zero.
  if out="$("$0" "$good_plist" "$cargo" "$ventura_cask" 2>&1)"; then
    echo "self-test FAILED: Ventura cask floor should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"macOS floor must be >= :sonoma"*) ;;
    *) echo "self-test FAILED: expected macOS-floor error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$good_plist" "$cargo" "$wrong_arch_cask" 2>&1)"; then
    echo "self-test FAILED: x86_64 cask architecture should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"architecture must be :arm64"*) ;;
    *) echo "self-test FAILED: expected cask architecture error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$good_plist" "$cargo" "$missing_arch_cask" 2>&1)"; then
    echo "self-test FAILED: missing cask architecture should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"architecture must be :arm64"*) ;;
    *) echo "self-test FAILED: expected missing cask architecture error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$good_plist" "$cargo" "$malformed_cask" 2>&1)"; then
    echo "self-test FAILED: malformed cask Ruby should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"invalid Ruby syntax"*) ;;
    *) echo "self-test FAILED: expected Ruby syntax error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$prerelease_plist" "$prerelease_cargo" "$prerelease_cask" 2>&1)"; then
    echo "self-test FAILED: prerelease bundle version should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"release version must be stable X.Y.Z"*) ;;
    *) echo "self-test FAILED: expected stable-version error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$bad_id_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: bad bundle identifier should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"CFBundleIdentifier"*) ;;
    *) echo "self-test FAILED: expected bundle identifier error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$bad_executable_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: bad bundle executable should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"CFBundleExecutable"*) ;;
    *) echo "self-test FAILED: expected bundle executable error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$bad_lsui_false_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: false LSUIElement should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"LSUIElement"*) ;;
    *) echo "self-test FAILED: expected LSUIElement error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$bad_lsui_missing_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: missing LSUIElement should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"LSUIElement"*) ;;
    *) echo "self-test FAILED: expected missing LSUIElement error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$bad_plist_version" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: bad plist version should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"CFBundleShortVersionString"*) ;;
    *) echo "self-test FAILED: expected plist version error, got: $out" >&2; exit 1 ;;
  esac

  if out="$("$0" "$bad_bundle_version_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: CFBundleVersion drift should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"CFBundleVersion"*) ;;
    *) echo "self-test FAILED: expected CFBundleVersion drift error, got: $out" >&2; exit 1 ;;
  esac

  # (f) all-consistent fixtures -> exits 0 with OK message.
  if ! out="$("$0" "$good_plist" "$cargo" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: consistent fixtures should pass, got: $out" >&2
    exit 1
  fi
  case "$out" in
    *"Bundle metadata OK"*) ;;
    *) echo "self-test FAILED: expected OK message, got: $out" >&2; exit 1 ;;
  esac

  # (g) workspace-inherited version resolves from the workspace root manifest.
  mkdir -p "$tmp/ws/crates/app"
  printf 'version.workspace = true\n' >"$tmp/ws/crates/app/Cargo.toml"
  printf '[workspace.package]\nversion = "1.2.3"\n' >"$tmp/ws/Cargo.toml"
  if ! out="$("$0" "$good_plist" "$tmp/ws/crates/app/Cargo.toml" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: workspace-inherited version should pass, got: $out" >&2
    exit 1
  fi
  case "$out" in
    *"Bundle metadata OK"*) ;;
    *) echo "self-test FAILED: expected OK message for inherited version, got: $out" >&2; exit 1 ;;
  esac

  # (h) inherited version with no workspace root version -> missing-version error.
  mkdir -p "$tmp/ws-broken/crates/app"
  printf 'version.workspace = true\n' >"$tmp/ws-broken/crates/app/Cargo.toml"
  printf '[workspace]\nmembers = []\n' >"$tmp/ws-broken/Cargo.toml"
  if out="$("$0" "$good_plist" "$tmp/ws-broken/crates/app/Cargo.toml" "$good_cask" 2>&1)"; then
    echo "self-test FAILED: unresolved inherited version should have failed" >&2
    echo "$out" >&2
    exit 1
  fi
  case "$out" in
    *"missing package version"*) ;;
    *) echo "self-test FAILED: expected missing-version error, got: $out" >&2; exit 1 ;;
  esac

  if "$0" "$good_plist" "$cargo" "$good_cask" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "self-test FAILED: extra normal argument was accepted" >&2
    exit 1
  fi
  grep -Fq "usage: check-bundle-metadata.sh" "$tmp/normal-argc.err"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "self-test FAILED: extra self-test argument was accepted" >&2
    exit 1
  fi
  grep -Fq "usage: check-bundle-metadata.sh" "$tmp/self-test-argc.err"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  # The checker invokes this self-test with poisoned knobs and requires it to
  # pass hermetically — unset them here rather than rejecting them.
  unset COMPME_EXPECTED_VERSION COMPME_CASK_TAG_CANDIDATES
  run_self_test
  exit 0
fi

if [ "$#" -ne 0 ] && [ "$#" -ne 3 ]; then
  usage
  exit 2
fi

info_plist="${1:-"$repo_root/tools/bundle/Info.plist"}"
app_manifest="${2:-"$repo_root/crates/app/Cargo.toml"}"
cask_file="${3:-"$repo_root/Casks/compme.rb"}"

# The cask is rewritten only at cask-finalization time, so mid-release it may
# still name the previous release. Discover stable release tags from the cask
# file's repository so the ruby check can allow that one-release lag. Refresh
# best-effort even when tags are already visible: actions/checkout on a tag
# push materializes only the release tag itself, never the previous ones the
# window needs (anonymous HTTPS fetch works on this public repo even with
# persist-credentials: false). When the fetch is unavailable (offline, no
# remote) the visible tags stand, which reduces to strict cask==app equality
# outside a release window. BatchMode and terminal-prompt denial keep the
# fetch non-interactive: an SSH remote without a loaded key or an unreachable
# network fails fast into the same fallback instead of blocking the gate.
cask_tag_candidates=""
cask_dir="$(dirname "$cask_file")"
if command -v git >/dev/null 2>&1 && git -C "$cask_dir" rev-parse --git-dir >/dev/null 2>&1; then
  GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=5" \
    git -C "$cask_dir" fetch --quiet --tags >/dev/null 2>&1 || true
  cask_tag_candidates="$(git -C "$cask_dir" tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname || true)"
fi
export COMPME_CASK_TAG_CANDIDATES="$cask_tag_candidates"

if ! ruby -c "$cask_file" >/dev/null; then
  echo "Casks/compme.rb: invalid Ruby syntax" >&2
  exit 1
fi

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

  cargo_text = File.read(cargo_path)
  cargo_version = cargo_text[/^version\s*=\s*"([^"]+)"/, 1]
  if cargo_version.nil? && cargo_text.match?(/^version\.workspace\s*=\s*true\s*$/)
    workspace_manifest = File.expand_path("../../Cargo.toml", File.dirname(cargo_path))
    if File.exist?(workspace_manifest)
      workspace_package = File.read(workspace_manifest)[/^\[workspace\.package\]\n(?:(?!\[).*\n?)*/]
      cargo_version = workspace_package[/^version\s*=\s*"([^"]+)"/, 1] if workspace_package
    end
  end
  cask_text = File.read(cask_path)
  cask_version = cask_text[/^\s*version\s+"([^"]+)"/, 1]
  cask_macos = cask_text[/^\s*depends_on\s+macos:\s+:(\w+)/, 1]
  cask_arch = cask_text[/^\s*depends_on\s+arch:\s+:(\w+)/, 1]
  plist_version = value_after.call("CFBundleShortVersionString")
  bundle_version = value_after.call("CFBundleVersion")
  expect.call("CFBundleVersion", bundle_version, plist_version)
  stable_version = /\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z/
  versions = [cargo_version, cask_version, plist_version, bundle_version]
  errors << "release version must be stable X.Y.Z" unless versions.all? { |version| version&.match?(stable_version) }
  errors << "crates/app Cargo.toml: missing package version" unless cargo_version
  errors << "Casks/compme.rb: missing cask version" unless cask_version
  errors << "Casks/compme.rb: macOS floor must be >= :sonoma" unless cask_macos == "sonoma"
  errors << "Casks/compme.rb: architecture must be :arm64" unless cask_arch == "arm64"
  if cargo_version && cask_version
    expect.call("CFBundleShortVersionString", plist_version, cargo_version)
    # Mid-release the cask intentionally lags one release (it is rewritten only
    # at cask-finalization time), so also allow the newest stable tag other than
    # the app version. With no visible tags this reduces to strict equality.
    latest_release_tag = ENV.fetch("COMPME_CASK_TAG_CANDIDATES", "").split("\n")
      .map { |tag| tag.sub(/\Av/, "") }
      .find { |tag_version| tag_version != cargo_version && tag_version.match?(stable_version) }
    if cask_version != cargo_version && cask_version != latest_release_tag
      drift_detail = latest_release_tag ? " or latest release tag #{latest_release_tag.inspect}" : ""
      errors << "version drift: cask #{cask_version.inspect} != app #{cargo_version.inspect}#{drift_detail}"
    end
  end
  if ENV.key?("COMPME_EXPECTED_VERSION")
    expected_version = ENV["COMPME_EXPECTED_VERSION"].to_s
    errors << "release tag version is empty" if expected_version.empty?
    if !expected_version.empty? && plist_version != expected_version
      errors << "release tag version drift: expected #{expected_version.inspect}, got #{plist_version.inspect}"
    end
  end

  unless errors.empty?
    warn("bundle metadata check failed:")
    errors.each { |error| warn("  - #{error}") }
    exit 1
  end

  puts "Bundle metadata OK: version=#{plist_version} id=com.compme.app executable=compme scheme=compme macos_min=14.0"
' "$info_plist" "$app_manifest" "$cask_file"
