#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DEFAULT_REPOSITORY="mudrii/compme"
DEFAULT_BRANCH="main"
DEFAULT_ENVIRONMENT="release"
TAG_RULESET_NAME="protected-release-tags"

usage() {
  cat <<'EOF'
Usage:
  tools/release/check-github-governance.sh [--repo OWNER/REPO]
  tools/release/check-github-governance.sh --self-test

Read-only check of the live GitHub controls around main, release deployment,
Actions policy, and release tags. Requires an authenticated `gh` with metadata
read access. The script never changes repository settings.
EOF
}

validate() {
  ruby -rjson - "$@" <<'RUBY'
branch_path, environment_path, actions_path, tag_ruleset_path = ARGV
branch = JSON.parse(File.read(branch_path))
environment = JSON.parse(File.read(environment_path))
actions = JSON.parse(File.read(actions_path))
tag_ruleset = JSON.parse(File.read(tag_ruleset_path))

failures = []

if branch["missing"]
  failures << "main branch protection is missing"
else
  status = branch.fetch("required_status_checks", {})
  checks = Array(status["checks"]) + Array(status["contexts"])
  failures << "main must require strict status checks" unless status["strict"] == true
  failures << "main must require at least one status check" if checks.empty?

  reviews = branch.fetch("required_pull_request_reviews", {})
  approvals = reviews.fetch("required_approving_review_count", 0).to_i
  failures << "main must require at least one approving review" if approvals < 1
  failures << "main must enforce protections for administrators" unless branch.dig("enforce_admins", "enabled") == true
end

review_rule = Array(environment["protection_rules"]).find { |rule| rule["type"] == "required_reviewers" }
if review_rule.nil?
  failures << "release environment must require a reviewer"
else
  failures << "release environment must prevent self-review" unless review_rule["prevent_self_review"] == true
  failures << "release environment reviewer list must not be empty" if Array(review_rule["reviewers"]).empty?
end
failures << "release environment must disable administrator bypass" unless environment["can_admins_bypass"] == false

deployment = environment["deployment_branch_policy"]
unless deployment.is_a?(Hash) &&
       (deployment["protected_branches"] == true || deployment["custom_branch_policies"] == true)
  failures << "release environment must restrict deployment branches/tags"
end

failures << "GitHub Actions must stay enabled" unless actions["enabled"] == true
failures << "GitHub Actions must use the selected-actions allowlist" unless actions["allowed_actions"] == "selected"
failures << "GitHub Actions must require full-SHA pinning" unless actions["sha_pinning_required"] == true

if tag_ruleset["missing"]
  failures << "protected-release-tags ruleset is missing"
else
  failures << "release-tag ruleset must be active" unless tag_ruleset["enforcement"] == "active"
  failures << "release-tag ruleset must target tags" unless tag_ruleset["target"] == "tag"
  includes = Array(tag_ruleset.dig("conditions", "ref_name", "include"))
  failures << "release-tag ruleset must include refs/tags/v*" unless includes.include?("refs/tags/v*")
  rule_types = Array(tag_ruleset["rules"]).map { |rule| rule["type"] }.compact
  %w[creation update deletion non_fast_forward].each do |required|
    failures << "release-tag ruleset must restrict #{required.tr("_", " ")}" unless rule_types.include?(required)
  end
  failures << "release-tag ruleset must not have bypass actors" unless Array(tag_ruleset["bypass_actors"]).empty?
end

if failures.empty?
  puts "GitHub governance check passed"
  exit 0
end

warn "GitHub governance check failed:"
failures.each { |failure| warn "  - #{failure}" }
exit 1
RUBY
}

branch_protection_missing_error() {
  grep -Eq 'Branch not protected|HTTP 404' "$1"
}

self_test() {
  local tmp
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-governance-self-test.XXXXXX")"
  trap 'rm -rf "$tmp"' RETURN

  cat >"$tmp/branch-good.json" <<'JSON'
{"required_status_checks":{"strict":true,"checks":[{"context":"CI"}]},"required_pull_request_reviews":{"required_approving_review_count":1},"enforce_admins":{"enabled":true}}
JSON
  cat >"$tmp/environment-good.json" <<'JSON'
{"can_admins_bypass":false,"protection_rules":[{"type":"required_reviewers","prevent_self_review":true,"reviewers":[{"type":"User"}]}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}
JSON
  cat >"$tmp/actions-good.json" <<'JSON'
{"enabled":true,"allowed_actions":"selected","sha_pinning_required":true}
JSON
  cat >"$tmp/ruleset-good.json" <<'JSON'
{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v*"]}},"rules":[{"type":"creation"},{"type":"update"},{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}
JSON

  validate \
    "$tmp/branch-good.json" \
    "$tmp/environment-good.json" \
    "$tmp/actions-good.json" \
    "$tmp/ruleset-good.json" >/dev/null

  printf '%s\n' '{"missing":true}' >"$tmp/bad.json"
  if validate "$tmp/bad.json" "$tmp/bad.json" "$tmp/bad.json" "$tmp/bad.json" >/dev/null 2>&1; then
    echo "self-test failed: insecure fixtures unexpectedly passed" >&2
    return 1
  fi

  printf '%s\n' 'gh: Branch not protected (HTTP 404)' >"$tmp/branch-missing.err"
  branch_protection_missing_error "$tmp/branch-missing.err" || {
    echo "self-test failed: missing branch protection was not recognized" >&2
    return 1
  }
  printf '%s\n' 'gh: API rate limit exceeded (HTTP 403)' >"$tmp/branch-failed.err"
  if branch_protection_missing_error "$tmp/branch-failed.err"; then
    echo "self-test failed: non-404 branch API error looked unprotected" >&2
    return 1
  fi

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  [[ $# -eq 1 ]] || {
    usage >&2
    exit 2
  }
  self_test
  exit
fi

repository="${COMPME_GITHUB_REPOSITORY:-$DEFAULT_REPOSITORY}"
if [[ "${1:-}" == "--repo" ]]; then
  [[ $# -eq 2 && -n "${2:-}" ]] || {
    usage >&2
    exit 2
  }
  repository="$2"
elif [[ $# -ne 0 ]]; then
  usage >&2
  exit 2
fi

command -v gh >/dev/null 2>&1 || {
  echo "gh is required" >&2
  exit 1
}

tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-governance.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

if ! gh api "repos/$repository/branches/$DEFAULT_BRANCH/protection" \
  >"$tmp/branch.json" 2>"$tmp/branch.err"; then
  if branch_protection_missing_error "$tmp/branch.err"; then
    printf '%s\n' '{"missing":true}' >"$tmp/branch.json"
  else
    echo "unable to read main branch protection:" >&2
    cat "$tmp/branch.err" >&2
    exit 1
  fi
fi

gh api "repos/$repository/environments/$DEFAULT_ENVIRONMENT" >"$tmp/environment.json"
gh api "repos/$repository/actions/permissions" >"$tmp/actions.json"
gh api "repos/$repository/rulesets" >"$tmp/rulesets.json"

ruleset_id="$(
  ruby -rjson -e '
    rulesets = JSON.parse(File.read(ARGV.fetch(0)))
    found = rulesets.find do |ruleset|
      ruleset["name"] == ARGV.fetch(1) &&
        ruleset["target"] == "tag" &&
        ruleset["enforcement"] == "active"
    end
    print(found["id"]) if found
  ' "$tmp/rulesets.json" "$TAG_RULESET_NAME"
)"

if [[ -z "$ruleset_id" ]]; then
  printf '%s\n' '{"missing":true}' >"$tmp/tag-ruleset.json"
else
  gh api "repos/$repository/rulesets/$ruleset_id" >"$tmp/tag-ruleset.json"
fi

validate \
  "$tmp/branch.json" \
  "$tmp/environment.json" \
  "$tmp/actions.json" \
  "$tmp/tag-ruleset.json"
