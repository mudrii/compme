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

The main-ruleset, release-environment, and release-tag-ruleset checks always
run and hard-fail on regressions from the documented baseline; the accepted
gaps tracked in docs/RELEASING.md print as warnings without failing. The
classic branch-protection and Actions-permissions endpoints are admin-gated:
when the token cannot read them (HTTP 401/403 — GITHUB_TOKEN holds no
Administration:read permission) those checks are skipped with a ::warning::
line naming the skipped check instead of failing. Exits 0 when every evaluable
hard-baseline check passes (warnings allowed), 1 when an evaluable check
fails, 2 on usage errors.
EOF
}

validate() {
  ruby -rjson - "$@" <<'RUBY'
branch_path, environment_path, actions_path, tag_ruleset_path, main_ruleset_path = ARGV
branch = JSON.parse(File.read(branch_path))
environment = JSON.parse(File.read(environment_path))
actions = JSON.parse(File.read(actions_path))
tag_ruleset = JSON.parse(File.read(tag_ruleset_path))
main_ruleset = JSON.parse(File.read(main_ruleset_path))

failures = []
warnings = []

# main is accident-protected by either classic branch protection (the full
# review model, checked strictly when present) or an active default-branch
# ruleset blocking deletion and force pushes — the mechanism chosen for the
# documented direct-to-main workflow. The branch fixture uses "missing" for
# definitely-unprotected and "unknown" for an unreadable (admin-gated)
# endpoint; when unknown, only the ruleset verdict can be evaluated.
if branch["missing"] && main_ruleset["missing"]
  failures << "main has neither classic branch protection nor an active default-branch ruleset"
end
if branch["unknown"]
  warnings << "main classic branch protection is unreadable (admin-gated endpoint); its strict checks were skipped"
  if main_ruleset["missing"]
    failures << "main has no active default-branch ruleset and classic branch protection is unreadable"
  end
end
unless branch["missing"] || branch["unknown"]
  status = branch.fetch("required_status_checks", {})
  checks = Array(status["checks"]) + Array(status["contexts"])
  failures << "main must require strict status checks" unless status["strict"] == true
  failures << "main must require at least one status check" if checks.empty?

  reviews = branch.fetch("required_pull_request_reviews", {})
  approvals = reviews.fetch("required_approving_review_count", 0).to_i
  failures << "main must require at least one approving review" if approvals < 1
  failures << "main must enforce protections for administrators" unless branch.dig("enforce_admins", "enabled") == true
end
unless main_ruleset["missing"]
  failures << "main ruleset must be active" unless main_ruleset["enforcement"] == "active"
  failures << "main ruleset must target branches" unless main_ruleset["target"] == "branch"
  includes = Array(main_ruleset.dig("conditions", "ref_name", "include"))
  failures << "main ruleset must include the default branch" unless
    (includes & ["~DEFAULT_BRANCH", "refs/heads/main", "~ALL"]).any?
  rule_types = Array(main_ruleset["rules"]).map { |rule| rule["type"] }.compact
  %w[deletion non_fast_forward].each do |required|
    failures << "main ruleset must restrict #{required.tr("_", " ")}" unless rule_types.include?(required)
  end
  failures << "main ruleset must not have bypass actors" unless Array(main_ruleset["bypass_actors"]).empty?
end

review_rule = Array(environment["protection_rules"]).find { |rule| rule["type"] == "required_reviewers" }
if review_rule.nil?
  failures << "release environment must require a reviewer"
else
  warnings << "release environment allows reviewer self-approval (owner decision pending)" unless review_rule["prevent_self_review"] == true
  failures << "release environment reviewer list must not be empty" if Array(review_rule["reviewers"]).empty?
end
warnings << "release environment allows administrator bypass (owner decision pending)" unless environment["can_admins_bypass"] == false

deployment = environment["deployment_branch_policy"]
unless deployment.is_a?(Hash) &&
       (deployment["protected_branches"] == true || deployment["custom_branch_policies"] == true)
  warnings << "release environment does not restrict deployment branches/tags (owner decision pending)"
end

if actions["unknown"]
  warnings << "GitHub Actions policy is unreadable (admin-gated endpoint); its checks were skipped"
else
  failures << "GitHub Actions must stay enabled" unless actions["enabled"] == true
  warnings << "GitHub Actions does not use the selected-actions allowlist (owner decision pending)" unless actions["allowed_actions"] == "selected"
  warnings << "GitHub Actions does not require full-SHA pinning (owner decision pending)" unless actions["sha_pinning_required"] == true
end

if tag_ruleset["missing"]
  # The live discovery filter only matches enforcement == "active", so a
  # disabled ruleset also reaches this branch.
  failures << "protected-release-tags ruleset is missing or not active"
else
  failures << "release-tag ruleset must be active" unless tag_ruleset["enforcement"] == "active"
  failures << "release-tag ruleset must target tags" unless tag_ruleset["target"] == "tag"
  includes = Array(tag_ruleset.dig("conditions", "ref_name", "include"))
  failures << "release-tag ruleset must include refs/tags/v*" unless includes.include?("refs/tags/v*")
  rule_types = Array(tag_ruleset["rules"]).map { |rule| rule["type"] }.compact
  %w[update deletion non_fast_forward].each do |required|
    failures << "release-tag ruleset must restrict #{required.tr("_", " ")}" unless rule_types.include?(required)
  end
  warnings << "release-tag ruleset does not restrict creation (owner decision pending)" unless rule_types.include?("creation")
  failures << "release-tag ruleset must not have bypass actors" unless Array(tag_ruleset["bypass_actors"]).empty?
end

unless warnings.empty?
  warn "GitHub governance accepted caveats:"
  warnings.each { |warning| warn "  - #{warning}" }
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

# The branch-protection and Actions-permissions endpoints require
# Administration:read (which GITHUB_TOKEN cannot hold); 401 or a
# permission-denied 403 there means "unreadable", not "absent". Other 403s
# (e.g. rate limiting) fail loud instead of silently skipping checks.
admin_endpoint_unreadable_error() {
  if grep -Eq 'HTTP 401' "$1"; then
    return 0
  fi
  grep -Eq 'Resource not accessible' "$1" && grep -Eq 'HTTP 403' "$1"
}

self_test() {
  if printenv COMPME_GITHUB_REPOSITORY >/dev/null 2>&1; then
    echo "self-test failed: inherited COMPME_GITHUB_REPOSITORY" >&2
    return 1
  fi
  unset COMPME_GITHUB_REPOSITORY
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
  cat >"$tmp/main-ruleset-good.json" <<'JSON'
{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}
JSON
  # The live tag ruleset: creation is an accepted caveat (restricting it with
  # no bypass actors would lock the owner out of cutting releases).
  cat >"$tmp/ruleset-caveat.json" <<'JSON'
{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v*"]}},"rules":[{"type":"update"},{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}
JSON

  validate \
    "$tmp/branch-good.json" \
    "$tmp/environment-good.json" \
    "$tmp/actions-good.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/main-ruleset-good.json" >/dev/null

  # Live-mirror of the documented baseline: no classic protection, an active
  # default-branch ruleset, and the accepted environment/Actions caveats —
  # passes with warnings, so the weekly job only fails on real regressions.
  printf '%s\n' '{"missing":true}' >"$tmp/branch-missing.json"
  cat >"$tmp/environment-caveat.json" <<'JSON'
{"can_admins_bypass":true,"protection_rules":[{"type":"required_reviewers","prevent_self_review":false,"reviewers":[{"type":"User"}]}],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":false}}
JSON
  cat >"$tmp/actions-caveat.json" <<'JSON'
{"enabled":true,"allowed_actions":"all","sha_pinning_required":false}
JSON
  validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/actions-caveat.json" \
    "$tmp/ruleset-caveat.json" \
    "$tmp/main-ruleset-good.json" >/dev/null

  printf '%s\n' '{"missing":true}' >"$tmp/bad.json"
  if validate "$tmp/bad.json" "$tmp/bad.json" "$tmp/bad.json" "$tmp/bad.json" "$tmp/bad.json" >/dev/null 2>&1; then
    echo "self-test failed: insecure fixtures unexpectedly passed" >&2
    return 1
  fi

  # main ruleset without the force-push rule must fail even with caveats green.
  cat >"$tmp/main-ruleset-no-ff.json" <<'JSON'
{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"]}},"rules":[{"type":"deletion"}],"bypass_actors":[]}
JSON
  if validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/actions-caveat.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/main-ruleset-no-ff.json" >/dev/null 2>&1; then
    echo "self-test failed: main ruleset missing non_fast_forward was accepted" >&2
    return 1
  fi

  # main ruleset with a bypass actor must fail.
  cat >"$tmp/main-ruleset-bypass.json" <<'JSON'
{"target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"]}},"rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[{"actor_id":1,"actor_type":"User","bypass_mode":"always"}]}
JSON
  if validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/actions-caveat.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/main-ruleset-bypass.json" >/dev/null 2>&1; then
    echo "self-test failed: main ruleset with bypass actor was accepted" >&2
    return 1
  fi

  # Hard baseline regressions fail even when every caveat stays accepted.
  printf '%s\n' '{"can_admins_bypass":true,"protection_rules":[],"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":false}}' \
    >"$tmp/environment-no-review.json"
  if validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-no-review.json" \
    "$tmp/actions-caveat.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/main-ruleset-good.json" >/dev/null 2>&1; then
    echo "self-test failed: release environment without a reviewer requirement was accepted" >&2
    return 1
  fi
  printf '%s\n' '{"enabled":false,"allowed_actions":"all","sha_pinning_required":false}' \
    >"$tmp/actions-disabled.json"
  if validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/actions-disabled.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/main-ruleset-good.json" >/dev/null 2>&1; then
    echo "self-test failed: disabled GitHub Actions was accepted" >&2
    return 1
  fi

  # A tag ruleset missing a hard restriction (e.g. deletion) fails even with
  # the creation caveat accepted.
  cat >"$tmp/ruleset-no-deletion.json" <<'JSON'
{"target":"tag","enforcement":"active","conditions":{"ref_name":{"include":["refs/tags/v*"]}},"rules":[{"type":"update"},{"type":"non_fast_forward"}],"bypass_actors":[]}
JSON
  if validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/actions-caveat.json" \
    "$tmp/ruleset-no-deletion.json" \
    "$tmp/main-ruleset-good.json" >/dev/null 2>&1; then
    echo "self-test failed: tag ruleset missing deletion restriction was accepted" >&2
    return 1
  fi

  # Admin-gated endpoints unreadable (HTTP 401/403): classic protection and
  # Actions checks are skipped with warnings while the always-evaluable
  # ruleset/environment checks still decide the exit code.
  printf '%s\n' '{"unknown":true}' >"$tmp/admin-unknown.json"
  out="$(validate \
    "$tmp/admin-unknown.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/admin-unknown.json" \
    "$tmp/ruleset-caveat.json" \
    "$tmp/main-ruleset-good.json" 2>&1)" || {
    echo "self-test failed: unreadable admin endpoints should not fail the check" >&2
    echo "$out" >&2
    return 1
  }
  case "$out" in
    *"classic branch protection is unreadable"*"Actions policy is unreadable"*) ;;
    *) echo "self-test failed: expected skip warnings for unreadable admin endpoints, got: $out" >&2; return 1 ;;
  esac

  # A ruleset regression still fails when the admin endpoints are unreadable.
  if validate \
    "$tmp/admin-unknown.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/admin-unknown.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/main-ruleset-no-ff.json" >/dev/null 2>&1; then
    echo "self-test failed: main ruleset regression under unreadable admin endpoints was accepted" >&2
    return 1
  fi

  # No main ruleset at all also fails: with classic protection unreadable, the
  # ruleset is the only verifiable accident protection on main.
  if validate \
    "$tmp/admin-unknown.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/admin-unknown.json" \
    "$tmp/ruleset-good.json" \
    "$tmp/bad.json" >/dev/null 2>&1; then
    echo "self-test failed: missing main ruleset under unreadable admin endpoints was accepted" >&2
    return 1
  fi

  # A disabled tag ruleset is invisible to the enforcement==active discovery
  # filter, so the failure must read "missing or not active".
  if out="$(validate \
    "$tmp/branch-missing.json" \
    "$tmp/environment-caveat.json" \
    "$tmp/actions-caveat.json" \
    "$tmp/bad.json" \
    "$tmp/main-ruleset-good.json" 2>&1)"; then
    echo "self-test failed: disabled tag ruleset was accepted" >&2
    return 1
  fi
  case "$out" in
    *"protected-release-tags ruleset is missing or not active"*) ;;
    *) echo "self-test failed: expected missing-or-not-active message, got: $out" >&2; return 1 ;;
  esac

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
  printf '%s\n' 'gh: Resource not accessible by integration (HTTP 403)' >"$tmp/admin-forbidden.err"
  admin_endpoint_unreadable_error "$tmp/admin-forbidden.err" || {
    echo "self-test failed: admin-gated 403 was not recognized as unreadable" >&2
    return 1
  }
  printf '%s\n' 'gh: Bad credentials (HTTP 401)' >"$tmp/admin-unauthorized.err"
  admin_endpoint_unreadable_error "$tmp/admin-unauthorized.err" || {
    echo "self-test failed: 401 was not recognized as unreadable" >&2
    return 1
  }
  if admin_endpoint_unreadable_error "$tmp/branch-missing.err"; then
    echo "self-test failed: unprotected branch looked admin-unreadable" >&2
    return 1
  fi
  # Rate limiting (also 403, but not a permission denial) must fail loud
  # rather than masquerade as an unreadable admin-gated endpoint.
  if admin_endpoint_unreadable_error "$tmp/branch-failed.err"; then
    echo "self-test failed: rate-limit 403 looked admin-unreadable" >&2
    return 1
  fi

  # The inherited-env guard must reject a preset COMPME_GITHUB_REPOSITORY.
  if COMPME_GITHUB_REPOSITORY=poisoned/repo "$0" --self-test >/dev/null 2>&1; then
    echo "self-test failed: inherited COMPME_GITHUB_REPOSITORY was accepted" >&2
    return 1
  fi

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  [[ $# -eq 1 ]] || {
    usage >&2
    exit 2
  }
  # No entrypoint scrub: the inherited-env guard in self_test rejects a
  # preset COMPME_GITHUB_REPOSITORY loudly, keeping the guard reachable.
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

# Probe the admin-gated endpoints first: tokens without Administration:read
# (GITHUB_TOKEN cannot hold it) get HTTP 401/403 from them, so their checks
# degrade to skipped-with-warning. The environment and ruleset endpoints are
# readable on this public repo; their checks always run and hard-fail.
if ! gh api "repos/$repository/branches/$DEFAULT_BRANCH/protection" \
  >"$tmp/branch.json" 2>"$tmp/branch.err"; then
  if branch_protection_missing_error "$tmp/branch.err"; then
    printf '%s\n' '{"missing":true}' >"$tmp/branch.json"
  elif admin_endpoint_unreadable_error "$tmp/branch.err"; then
    echo "::warning::main branch protection endpoint is unreadable (admin-gated); skipping classic protection checks"
    printf '%s\n' '{"unknown":true}' >"$tmp/branch.json"
  else
    echo "unable to read main branch protection:" >&2
    cat "$tmp/branch.err" >&2
    exit 1
  fi
fi

if ! gh api "repos/$repository/actions/permissions" \
  >"$tmp/actions.json" 2>"$tmp/actions.err"; then
  if admin_endpoint_unreadable_error "$tmp/actions.err"; then
    echo "::warning::Actions permissions endpoint is unreadable (admin-gated); skipping Actions policy checks"
    printf '%s\n' '{"unknown":true}' >"$tmp/actions.json"
  else
    echo "unable to read Actions permissions:" >&2
    cat "$tmp/actions.err" >&2
    exit 1
  fi
fi

gh api "repos/$repository/environments/$DEFAULT_ENVIRONMENT" >"$tmp/environment.json"
gh api "repos/$repository/rulesets?per_page=100" >"$tmp/rulesets.json"

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

# Find the active branch ruleset covering the default branch (the list
# endpoint does not reliably expand conditions, so fetch each candidate).
printf '%s\n' '{"missing":true}' >"$tmp/main-ruleset.json"
for candidate_id in $(
  ruby -rjson -e '
    rulesets = JSON.parse(File.read(ARGV.fetch(0)))
    ids = rulesets.select { |ruleset| ruleset["target"] == "branch" && ruleset["enforcement"] == "active" }.map { |ruleset| ruleset["id"] }
    puts ids
  ' "$tmp/rulesets.json"
); do
  gh api "repos/$repository/rulesets/$candidate_id" >"$tmp/candidate-ruleset.json"
  if ruby -rjson -e '
    ruleset = JSON.parse(File.read(ARGV.fetch(0)))
    includes = Array(ruleset.dig("conditions", "ref_name", "include"))
    exit((includes & ["~DEFAULT_BRANCH", "refs/heads/main", "~ALL"]).any? ? 0 : 1)
  ' "$tmp/candidate-ruleset.json"; then
    cp "$tmp/candidate-ruleset.json" "$tmp/main-ruleset.json"
    break
  fi
done

validate \
  "$tmp/branch.json" \
  "$tmp/environment.json" \
  "$tmp/actions.json" \
  "$tmp/tag-ruleset.json" \
  "$tmp/main-ruleset.json"
