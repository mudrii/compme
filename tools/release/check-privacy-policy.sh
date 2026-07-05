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

lock = File.join(root, "Cargo.lock")
if File.exist?(lock)
  packages = File.read(lock).scan(/^name = "([^"]+)"/).flatten
  denied_packages.each do |name|
    abort("privacy policy check failed: denied telemetry package #{name}") if packages.include?(name)
  end
end

allowed_hosts = %w[
  127.0.0.1
  ai.google.dev
  compme
  cotypist.app
  docs.google.com
  example.com
  example.invalid
  example.test
  github.com
  huggingface.co
  localhost
  opensource.org
  www.apache.org
  www.llama.com
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
      next if candidate.include?("/docs/superpowers/")
      next unless candidate.match?(/\.(rs|sh|yml|yaml|md|rb|toml)\z/)
      paths << candidate
    end
  else
    paths << path
  end
end

paths.each do |path|
  text = File.read(path, invalid: :replace, undef: :replace)
  text.scan(%r{https?://([^/`"'\s)<>{}*]+)}) do |match|
    host = match.first.downcase.sub(/:\d+\z/, "").gsub("\\.", ".").split("@").last
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
  cat >"$tmp/good/crates/demo/src/lib.rs" <<'RS'
const MODEL_URL: &str = "https://huggingface.co/example/model.gguf";
const UPDATE_URL: &str = "https://github.com/mudrii/compme/releases/latest";
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
