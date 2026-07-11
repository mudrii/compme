#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

usage() {
  echo "usage: check-privacy-policy.sh [--self-test] [repo-root]" >&2
}

check_repo() {
  root="$1"
  ruby - "$root" <<'RUBY'
root = ARGV.fetch(0)

denied_packages = %w[
  amplitude
  datadog
  mixpanel
  newrelic
  opentelemetry
  opentelemetry_sdk
  posthog
  segment
  sentry
  sentry-core
  telemetry
]

lockfiles = []
root_lock = File.join(root, "Cargo.lock")
if File.exist?(root_lock)
  lockfiles << root_lock
elsif File.exist?(File.join(root, "Cargo.toml"))
  abort("privacy policy check failed: Cargo.toml present but Cargo.lock missing in #{root}")
end
%w[crates tools .github Casks docs].each do |entry|
  path = File.join(root, entry)
  next unless File.directory?(path)
  Dir.glob(File.join(path, "**", "Cargo.lock"), File::FNM_DOTMATCH).each do |candidate|
    next unless File.file?(candidate)
    next if candidate.include?("/target/") || candidate.include?("/tools/acceptance/logs/")
    lockfiles << candidate
  end
end

# Any Cargo.toml not covered by a workspace lockfile must have a sibling
# Cargo.lock, or the denied-package scan silently skips its dependencies.
cargo_tomls = []
root_toml = File.join(root, "Cargo.toml")
cargo_tomls << root_toml if File.file?(root_toml)
%w[crates tools .github Casks docs].each do |entry|
  path = File.join(root, entry)
  next unless File.directory?(path)
  Dir.glob(File.join(path, "**", "Cargo.toml"), File::FNM_DOTMATCH).each do |candidate|
    next unless File.file?(candidate)
    next if candidate.include?("/target/") || candidate.include?("/tools/acceptance/logs/")
    cargo_tomls << candidate
  end
end

workspaces = {}
cargo_tomls.each do |toml|
  text = File.read(toml)
  next unless text.match?(/^\[workspace\]/)
  dir = File.dirname(toml)
  unless File.file?(File.join(dir, "Cargo.lock"))
    rel = File.dirname(toml.delete_prefix(root + "/"))
    abort("privacy policy check failed: Cargo.toml present but Cargo.lock missing in #{rel}")
  end
  section = text[/^\[workspace\](?:(?!^\[).)*/m] || ""
  members = (section[/^members\s*=\s*\[(.*?)\]/m, 1] || "").scan(/"([^"]+)"/).flatten
  excludes = (section[/^exclude\s*=\s*\[(.*?)\]/m, 1] || "").scan(/"([^"]+)"/).flatten
  workspaces[dir] = [members, excludes]
end
cargo_tomls.each do |toml|
  text = File.read(toml)
  next unless text.match?(/^\[package\]/)
  dir = File.dirname(toml)
  next if File.file?(File.join(dir, "Cargo.lock"))
  covered = workspaces.any? do |ws_dir, (members, excludes)|
    next false unless dir.start_with?(ws_dir + "/")
    rel = dir.delete_prefix(ws_dir + "/")
    members.any? { |m| File.fnmatch(m, rel, File::FNM_PATHNAME) } &&
      excludes.none? { |e| File.fnmatch(e, rel, File::FNM_PATHNAME) }
  end
  next if covered
  rel = File.dirname(toml.delete_prefix(root + "/"))
  abort("privacy policy check failed: Cargo.toml present but Cargo.lock missing in #{rel}")
end

lockfiles.each do |lock|
  packages = File.read(lock).scan(/^name = "([^"]+)"/).flatten
  denied_packages.each do |name|
    if packages.include?(name)
      rel = lock.delete_prefix(root + "/")
      abort("privacy policy check failed: denied telemetry package #{name} in #{rel}")
    end
  end
end

allowed_hosts = %w[
  127.0.0.1
  ai.google.dev
  compme
  cotypist.app
  developer.apple.com
  docs.google.com
  docs.rs
  example.com
  example.invalid
  example.test
  github.com
  huggingface.co
  localhost
  opensource.org
  v2.tauri.app
  www.apache.org
  www.apple.com
  www.llama.com
  www.w3.org
  x.com
]
denied_host_patterns = [
  /(^|\.)amplitude\.com\z/i,
  /(^|\.)datadoghq\.com\z/i,
  /(^|\.)google-analytics\.com\z/i,
  /(^|\.)googletagmanager\.com\z/i,
  /(^|\.)mixpanel\.com\z/i,
  /(^|\.)newrelic\.com\z/i,
  /(^|\.)posthog\.com\z/i,
  /(^|\.)segment\.com\z/i,
  /(^|\.)segment\.io\z/i,
  /(^|\.)sentry\.io\z/i,
]

paths = []
%w[crates tools .github Casks README.md docs].each do |entry|
  path = File.join(root, entry)
  next unless File.exist?(path)
  if File.directory?(path)
    Dir.glob(File.join(path, "**", "*"), File::FNM_DOTMATCH).each do |candidate|
      next unless File.file?(candidate)
      next if candidate.include?("/target/") || candidate.include?("/tools/acceptance/logs/")
      # Scan every text file; only skip known-binary formats. An extension
      # allowlist here would let a telemetry URL hide in a .json/.plist.
      next if candidate.match?(/\.(png|jpe?g|gif|icns|ico|pdf|zip|gz|tar|gguf|bin|dylib|car|o|a|DS_Store)\z/i)
      paths << candidate
    end
  else
    paths << path
  end
end

paths.each do |path|
  text = File.read(path, invalid: :replace, undef: :replace)
  text.scan(%r{https?://([^/`"'\s)<>{}*]+)}) do |match|
    host = match.first.downcase.sub(/:\d+\z/, "").gsub("\\.", ".").split("@").last.sub(/\.\z/, "")
    if denied_host_patterns.any? { |pattern| pattern.match?(host) }
      rel = path.delete_prefix(root + "/")
      abort("privacy policy check failed: denied telemetry host #{host} in #{rel}")
    end
    next if allowed_hosts.include?(host) ||
      host.match?(/\A(192\.0\.2|198\.51\.100|203\.0\.113)\.[0-9]+\z/) ||
      host.end_with?(".example") ||
      host.end_with?(".example.com") ||
      host.end_with?(".example.test") ||
      host.end_with?(".githubusercontent.com")
    rel = path.delete_prefix(root + "/")
    abort("privacy policy check failed: unreviewed network host #{host} in #{rel}")
  end
end
RUBY
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-privacy-policy.XXXXXX")"
  cleanup() {
    rm -rf "$tmp"
  }
  trap cleanup EXIT

  mkdir -p "$tmp/good/crates/demo/src" "$tmp/good/tools/release" "$tmp/good/.github/workflows" "$tmp/good/docs" "$tmp/good/Casks"
  cat >"$tmp/good/Cargo.lock" <<'LOCK'
[[package]]
name = "serde"
version = "1.0.0"
LOCK
  printf '[workspace]\nmembers = ["crates/demo"]\n' >"$tmp/good/Cargo.toml"
  printf '[package]\nname = "demo"\n' >"$tmp/good/crates/demo/Cargo.toml"
  cat >"$tmp/good/crates/demo/src/lib.rs" <<'RS'
const MODEL_URL: &str = "https://huggingface.co/example/model.gguf";
const UPDATE_URL: &str = "https://github.com/mudrii/compme/releases/latest";
const ABSOLUTE_DNS_URL: &str = "https://example.test./absolute";
RS
  check_repo "$tmp/good"

  cp -R "$tmp/good" "$tmp/denied-package"
  cat >>"$tmp/denied-package/Cargo.lock" <<'LOCK'
[[package]]
name = "sentry"
version = "1.0.0"
LOCK
  if check_repo "$tmp/denied-package" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: denied telemetry package was accepted" >&2
    return 1
  fi

  cp -R "$tmp/good" "$tmp/denied-nested-package"
  mkdir -p "$tmp/denied-nested-package/tools/spike"
  cat >"$tmp/denied-nested-package/tools/spike/Cargo.lock" <<'LOCK'
[[package]]
name = "sentry"
version = "1.0.0"
LOCK
  if check_repo "$tmp/denied-nested-package" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: denied telemetry package in nested lockfile was accepted" >&2
    return 1
  fi

  cp -R "$tmp/good" "$tmp/missing-lock"
  rm "$tmp/missing-lock/Cargo.lock"
  printf '[workspace]\n' >"$tmp/missing-lock/Cargo.toml"
  if check_repo "$tmp/missing-lock" >/dev/null 2>"$tmp/missing-lock.err"; then
    echo "privacy policy self-test failed: Cargo.toml without Cargo.lock was accepted" >&2
    return 1
  fi
  grep -q 'Cargo.toml present but Cargo.lock missing' "$tmp/missing-lock.err"

  cp -R "$tmp/good" "$tmp/deleted-nested-lock"
  mkdir -p "$tmp/deleted-nested-lock/tools/spike"
  printf '[workspace]\n' >"$tmp/deleted-nested-lock/tools/spike/Cargo.toml"
  if check_repo "$tmp/deleted-nested-lock" >/dev/null 2>"$tmp/deleted-nested-lock.err"; then
    echo "privacy policy self-test failed: nested workspace Cargo.toml without Cargo.lock was accepted" >&2
    return 1
  fi
  grep -q 'Cargo.toml present but Cargo.lock missing in tools/spike' "$tmp/deleted-nested-lock.err"

  cp -R "$tmp/good" "$tmp/uncovered-package"
  mkdir -p "$tmp/uncovered-package/tools/spike"
  printf '[package]\nname = "spike"\n' >"$tmp/uncovered-package/tools/spike/Cargo.toml"
  printf '[workspace]\nmembers = ["crates/demo"]\nexclude = ["tools/spike"]\n' >"$tmp/uncovered-package/Cargo.toml"
  if check_repo "$tmp/uncovered-package" >/dev/null 2>"$tmp/uncovered-package.err"; then
    echo "privacy policy self-test failed: excluded package Cargo.toml without Cargo.lock was accepted" >&2
    return 1
  fi
  grep -q 'Cargo.toml present but Cargo.lock missing in tools/spike' "$tmp/uncovered-package.err"

  cp -R "$tmp/good" "$tmp/denied-host"
  {
    printf '%s' 'const ANALYTICS_URL: &str = "https:'
    printf '%s' '//api.'
    printf '%s\n' 'segment.io/v1/track";'
  } >"$tmp/denied-host/crates/demo/src/lib.rs"
  if check_repo "$tmp/denied-host" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: denied analytics host was accepted" >&2
    return 1
  fi

  cp -R "$tmp/good" "$tmp/denied-spec-host"
  mkdir -p "$tmp/denied-spec-host/docs/superpowers/specs"
  {
    printf '%s' 'Design note: https:'
    printf '%s' '//api.'
    printf '%s\n' 'segment.io/v1/track'
  } >"$tmp/denied-spec-host/docs/superpowers/specs/network.md"
  if check_repo "$tmp/denied-spec-host" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: denied docs/superpowers host was accepted" >&2
    return 1
  fi

  cp -R "$tmp/good" "$tmp/denied-json-host"
  {
    printf '%s' '{"dsn": "https:'
    printf '%s' '//api.'
    printf '%s\n' 'segment.io/v1/track"}'
  } >"$tmp/denied-json-host/crates/demo/telemetry.json"
  if check_repo "$tmp/denied-json-host" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: denied host in a .json file was accepted" >&2
    return 1
  fi

  cp -R "$tmp/good" "$tmp/evil-host"
  {
    printf '%s' 'const EVIL_URL: &str = "https:'
    printf '%s\n' '//evil.com/collect";'
    cat <<'RS'
const USERINFO_FIXTURE_URL: &str = "https://evil.com@bank.example/login";
RS
  } >"$tmp/evil-host/crates/demo/src/lib.rs"
  if check_repo "$tmp/evil-host" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: unreviewed evil.com host was accepted" >&2
    return 1
  fi

  cp -R "$tmp/good" "$tmp/userinfo-fixture"
  cat >"$tmp/userinfo-fixture/crates/demo/src/lib.rs" <<'RS'
const USERINFO_FIXTURE_URL: &str = "https://evil.com@bank.example/login";
RS
  check_repo "$tmp/userinfo-fixture"

  cp -R "$tmp/good" "$tmp/unreviewed-host"
  {
    printf '%s' 'const UNKNOWN_URL: &str = "https:'
    printf '%s\n' '//metrics.invalid/collect";'
  } >"$tmp/unreviewed-host/crates/demo/src/lib.rs"
  if check_repo "$tmp/unreviewed-host" >/dev/null 2>&1; then
    echo "privacy policy self-test failed: unreviewed host was accepted" >&2
    return 1
  fi

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "privacy policy self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-privacy-policy\.sh \[--self-test\] \[repo-root\]$' "$tmp/self-test-argc.err"

  if "$0" "$tmp/good" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "privacy policy self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-privacy-policy\.sh \[--self-test\] \[repo-root\]$' "$tmp/normal-argc.err"

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
if [[ "$#" -gt 1 ]]; then
  usage
  exit 2
fi

check_repo "${1:-$repo_root}"
