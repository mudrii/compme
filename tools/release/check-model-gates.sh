#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
canonical_release_workflow="$repo_root/.github/workflows/release.yml"
release_workflow="${1:-$canonical_release_workflow}"
ci_workflow="$repo_root/.github/workflows/ci.yml"
audit_workflow="$repo_root/.github/workflows/audit.yml"
gate_script="$repo_root/tools/release/run-model-gates.sh"
feature_script="$repo_root/tools/release/check-model-client-features.sh"
privacy_script="$repo_root/tools/release/check-privacy-policy.sh"
bundle_metadata_script="$repo_root/tools/bundle/check-bundle-metadata.sh"
make_app_script="$repo_root/tools/bundle/make-app.sh"
make_icon_script="$repo_root/tools/bundle/make-icon.sh"
bundle_smoke_script="$repo_root/tools/bundle/bundle-smoke.sh"
finalize_cask_script="$repo_root/tools/release/finalize-cask.sh"
update_cask_script="$repo_root/tools/release/update-cask.sh"
notarize_script="$repo_root/tools/release/notarize-app.sh"
update_manifest_script="$repo_root/tools/release/write-update-manifest.sh"
version_validator_script="$repo_root/tools/release/validate-version.sh"
quality_script="$repo_root/tools/release/check-quality.sh"
version_docs_script="$repo_root/tools/release/check-version-docs.sh"
acceptance_doc="$repo_root/docs/ACCEPTANCE.md"
manual_validation_doc="$repo_root/docs/MANUAL-VALIDATION.md"
development_doc="$repo_root/docs/DEVELOPMENT.md"
releasing_doc="$repo_root/docs/RELEASING.md"
readme_doc="$repo_root/README.md"
roadmap_doc="$repo_root/docs/ROADMAP.md"
grammar_spec="$repo_root/docs/superpowers/specs/2026-07-01-grammar-fix-design.md"
cask_file="$repo_root/Casks/compme.rb"

require_line() {
  file="$1"
  pattern="$2"
  label="$3"
  if ! grep -Eq "$pattern" "$file"; then
    echo "missing release gate: $label" >&2
    return 1
  fi
}

reject_line() {
  file="$1"
  pattern="$2"
  label="$3"
  # Fail loud if the target moved/vanished: a negative assertion over a missing
  # file would otherwise pass silently (grep errors, the `if` is skipped), so the
  # guard goes dead-green when the code it protects is renamed away. Mirrors
  # require_line's loud behavior on an absent file.
  if [ ! -f "$file" ]; then
    echo "release gate target missing: $label ($file)" >&2
    return 1
  fi
  if grep -Eq "$pattern" "$file"; then
    echo "stale release gate: $label" >&2
    return 1
  fi
}

check_no_automated_a2_validation() {
  workflow_path="$1"
  workflow_label="$2"
  ruby -ryaml - "$workflow_path" "$workflow_label" <<'RUBY'
workflow = YAML.load_file(ARGV.fetch(0))
label = ARGV.fetch(1)
runner = "tools/acceptance/run-a2-compat-gates.sh"
ledger = "tools/release/check-a2-matrix-ledger.sh"
def active_shell_lines(run)
  lines = []
  dead_depth = 0
  function_depth = 0
  pending_function = false
  terminated = false
  run.to_s.lines.each do |raw|
    stripped = raw.strip
    next if stripped.empty? || stripped.start_with?("#")
    normalized = stripped.sub(/[[:space:]]+#.*$/, "")
    next if terminated
    if function_depth.positive?
      function_depth += normalized.count("{") - normalized.count("}")
      function_depth = 0 unless function_depth.positive?
      next
    end
    if pending_function
      if normalized == "{"
        function_depth = 1
        pending_function = false
        next
      end
      pending_function = false
    end
    if normalized.match?(/\A(?:function[[:space:]]+[A-Za-z_][A-Za-z0-9_]*(?:[[:space:]]*\(\))?|[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\(\))[[:space:]]*\z/)
      pending_function = true
      next
    end
    if normalized.match?(/\A(?:function[[:space:]]+)?[A-Za-z_][A-Za-z0-9_]*[[:space:]]*(?:\(\))?[[:space:]]*\{/)
      function_depth = [normalized.count("{") - normalized.count("}"), 0].max
      next
    end
    if dead_depth.positive?
      dead_depth += 1 if normalized.match?(/\Aif[[:space:]]+/)
      dead_depth -= 1 if normalized == "fi" || normalized.start_with?("fi ")
      next
    end
    if normalized.match?(/\Aif[[:space:]]+false(?:[;[:space:]]|\z)/)
      dead_depth = 1
      next
    end
    lines << normalized
    terminated = true if normalized.match?(/\A(?:exit|return)[[:space:]]+0\z/)
  end
  lines
end
reject_a2_env = lambda do |env, where|
  if env.keys.any? { |key| key.to_s.start_with?("COMPME_A2_") }
    abort("stale release gate: #{where} still injects automated A2 environment")
  end
end
reject_a2_env.call(workflow.fetch("env", {}), label)

workflow.fetch("jobs").each do |job_name, job|
  reject_a2_env.call(job.fetch("env", {}), "#{label} #{job_name}")
  Array(job["steps"]).each do |step|
    next unless step.is_a?(Hash)
    name = step["name"].to_s
    abort("stale release gate: #{label} #{job_name} still contains automated A2 step #{name}") if name.match?(/\bA2\b/i)
    env = step.fetch("env", {})
    reject_a2_env.call(env, "#{label} #{job_name}")
    run = step["run"].to_s
    if run.include?("bash -n tools/acceptance/*.sh") || run.include?("bash -n tools/release/*.sh")
      abort("stale release gate: #{label} #{job_name} syntax-checks A2 scripts through a wildcard")
    end
    run.lines.each do |raw|
      line = raw.strip
      next if line.empty? || line.start_with?("#")
      allowed_exclusion = [
        "! -path 'tools/acceptance/run-a2-compat-gates.sh' \\",
        "! -path 'tools/release/check-a2-matrix-ledger.sh' -print0 \\",
      ].include?(line)
      if (line.include?(runner) || line.include?(ledger)) && !allowed_exclusion
        abort("stale release gate: #{label} #{job_name} executes or validates A2 tooling")
      end
    end
    next if name == "Script syntax"
    if run.match?(%r{tools/(?:acceptance|release)/[^[:space:]\n]*\$[^[:space:]\n]*\.sh})
      abort("stale release gate: #{label} #{job_name} constructs a validation-tool path dynamically")
    end
    if run.match?(/\bfind[[:space:]]+tools\/(acceptance|release)\b/m) ||
       run.match?(/\b(xargs|find)[^\n]*(bash|sh)[[:space:]]+-n\b/m) ||
       run.match?(/\b(bash|sh)[[:space:]]+-n[^\n]*tools\/(acceptance|release)/m)
      abort("stale release gate: #{label} #{job_name} adds generic shell traversal that can reach local/manual A2 tooling")
    end
  end
end

syntax_steps = workflow.fetch("jobs").values.flat_map { |job| Array(job["steps"]) }
syntax_matches = syntax_steps.select { |step| step.is_a?(Hash) && step["name"] == "Script syntax" }
abort("missing release gate: #{label} keeps exactly one script syntax validation") unless syntax_matches.length == 1
syntax_lines = active_shell_lines(syntax_matches.first.fetch("run"))
expected_syntax_lines = [
  "find tools/acceptance tools/bundle tools/release -type f -name '*.sh' -print0 \\",
  "xargs -0 bash -n",
]
abort("missing release gate: #{label} script syntax is the approved traversal only") unless
  syntax_lines.length == 2 &&
  syntax_lines.fetch(0) == expected_syntax_lines.fetch(0) &&
  syntax_lines.fetch(1) == "| #{expected_syntax_lines.fetch(1)}"
RUBY
}

check_ci_integrity_controls() {
  ruby -ryaml - "$1" <<'RUBY'
workflow = YAML.load_file(ARGV.fetch(0))
trigger = workflow["on"] || workflow[true]
abort("missing release gate: CI keeps push/pull_request/dispatch triggers") unless trigger.keys.sort == ["pull_request", "push", "workflow_dispatch"]
push_trigger = trigger.fetch("push")
abort("missing release gate: CI push trigger is limited to main and spike branches") unless push_trigger.fetch("branches") == ["main", "spike/**"]
abort("missing release gate: CI push trigger skips only docs-only paths") unless push_trigger.fetch("paths-ignore") == ["docs/**", "*.md", "LICENSE"]
jobs = workflow.fetch("jobs")
checkout = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"
toolchain = "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30"
cache = "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32"
expected_actions = {
  "actionlint" => [[checkout, {"persist-credentials" => false}]],
  "check" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {"workspaces" => ".\ntools/spike\n", "cache-directories" => "tools/spike/models"}]],
  "spike" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {"workspaces" => "tools/spike"}]],
  "windows" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {}]],
  "linux" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {"cache-directories" => "~/.cargo/advisory-db"}]],
}
abort("missing release gate: exact CI job topology") unless jobs.keys.sort == expected_actions.keys.sort
expected_actions.each do |job_name, expected|
  actual = Array(jobs.fetch(job_name)["steps"]).each_with_object([]) do |step, actions|
    actions << [step.fetch("uses"), step.fetch("with", {})] if step.is_a?(Hash) && step.key?("uses")
  end
  abort("missing release gate: CI #{job_name} exact action and input topology") unless actual == expected
end
{"actionlint" => 10, "check" => 90, "spike" => 60, "windows" => 60, "linux" => 60}.each do |job_name, timeout|
  abort("missing release gate: CI #{job_name} exact timeout") unless jobs.fetch(job_name).fetch("timeout-minutes") == timeout
end
abort("missing release gate: CI check inherits read-only workflow permissions") if jobs.fetch("check").key?("permissions")
actionlint_step = jobs.fetch("actionlint").fetch("steps").find { |step| step["name"] == "Run actionlint" }
abort("missing release gate: CI actionlint runs the checksum-db pinned linter") unless
  actionlint_step && actionlint_step.fetch("run") == "go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 -color"
audit_step = jobs.fetch("linux").fetch("steps").find { |step| step["name"] == "Rust dependency audit" }
abort("missing release gate: CI dependency audit installs and runs pinned cargo-audit") unless
  audit_step && audit_step.fetch("run") == "cargo install cargo-audit --version 0.22.2 --locked\ncargo audit\n"
icon_step = jobs.fetch("check").fetch("steps").find { |step| step["name"] == "Bundle icon generator self-test" }
abort("missing release gate: CI runs the bundle icon generator self-test") unless
  icon_step && icon_step.fetch("run") == "tools/bundle/make-icon.sh --self-test"
RUBY
}

check_audit_integrity_controls() {
  ruby -ryaml - "$1" <<'RUBY'
workflow = YAML.load_file(ARGV.fetch(0))
triggers = workflow.fetch(true)
abort("missing release gate: dependency audit has exact weekly schedule") unless
  triggers.fetch("schedule") == [{"cron" => "17 6 * * 1"}]
abort("missing release gate: dependency audit supports manual dispatch") unless triggers.key?("workflow_dispatch")
abort("missing release gate: dependency audit has read-only contents permission") unless
  workflow.fetch("permissions") == {"contents" => "read"}
abort("missing release gate: dependency audit has exact concurrency policy") unless
  workflow.fetch("concurrency") == {"group" => "dependency-audit", "cancel-in-progress" => false}
jobs = workflow.fetch("jobs")
abort("missing release gate: dependency audit has isolated audit and governance jobs") unless jobs.keys.sort == ["audit", "governance"]
job = jobs.fetch("audit")
abort("missing release gate: dependency audit uses Linux") unless job.fetch("runs-on") == "ubuntu-latest"
abort("missing release gate: dependency audit exact timeout") unless job.fetch("timeout-minutes") == 20
steps = job.fetch("steps")
actions = steps.each_with_object([]) do |step, found|
  found << [step.fetch("uses"), step.fetch("with", {})] if step.key?("uses")
end
expected_actions = [
  ["actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0", {"persist-credentials" => false}],
  ["dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30", {}],
  ["Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32", {"cache-directories" => "~/.cargo/advisory-db"}],
]
abort("missing release gate: dependency audit exact action provenance") unless actions == expected_actions
audit_step = steps.find { |step| step["name"] == "Audit locked dependencies" }
abort("missing release gate: dependency audit installs and runs pinned cargo-audit") unless
  audit_step && audit_step.fetch("run") == "cargo install cargo-audit --version 0.22.2 --locked\ncargo audit\n"
governance = jobs.fetch("governance")
abort("missing release gate: governance check uses Linux") unless governance.fetch("runs-on") == "ubuntu-latest"
abort("missing release gate: governance check exact timeout") unless governance.fetch("timeout-minutes") == 10
abort("missing release gate: governance check has least-privilege permissions") unless
  governance.fetch("permissions") == {"contents" => "read", "issues" => "write"}
gov_actions = governance.fetch("steps").each_with_object([]) do |step, found|
  found << [step.fetch("uses"), step.fetch("with", {})] if step.key?("uses")
end
abort("missing release gate: governance check exact action provenance") unless gov_actions == [
  ["actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0", {"persist-credentials" => false}],
]
gov_step = governance.fetch("steps").find { |step| step["name"] == "Check GitHub governance (live, read-only)" }
abort("missing release gate: governance check runs the live read-only checker") unless
  gov_step && gov_step.fetch("run").include?('tools/release/check-github-governance.sh --repo "$GITHUB_REPOSITORY"') &&
  !gov_step.fetch("run").include?("|| true")
RUBY
}

check_release_integrity_controls() {
  ruby -ryaml - "$1" <<'RUBY'
def active_shell_lines(run)
  lines = []
  dead_depth = 0
  function_depth = 0
  pending_function = false
  terminated = false
  run.to_s.lines.each do |raw|
    stripped = raw.strip
    next if stripped.empty? || stripped.start_with?("#")
    normalized = stripped.sub(/[[:space:]]+#.*$/, "")
    next if terminated
    if function_depth.positive?
      function_depth += normalized.count("{") - normalized.count("}")
      function_depth = 0 unless function_depth.positive?
      next
    end
    if pending_function
      if normalized == "{"
        function_depth = 1
        pending_function = false
        next
      end
      pending_function = false
    end
    if normalized.match?(/\A(?:function[[:space:]]+[A-Za-z_][A-Za-z0-9_]*(?:[[:space:]]*\(\))?|[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\(\))[[:space:]]*\z/)
      pending_function = true
      next
    end
    if normalized.match?(/\A(?:function[[:space:]]+)?[A-Za-z_][A-Za-z0-9_]*[[:space:]]*(?:\(\))?[[:space:]]*\{/)
      function_depth = [normalized.count("{") - normalized.count("}"), 0].max
      next
    end
    if dead_depth.positive?
      dead_depth += 1 if normalized.match?(/\Aif[[:space:]]+/)
      dead_depth -= 1 if normalized == "fi" || normalized.start_with?("fi ")
      next
    end
    if normalized.match?(/\Aif[[:space:]]+false(?:[;[:space:]]|\z)/)
      dead_depth = 1
      next
    end
    lines << normalized
    terminated = true if normalized.match?(/\A(?:exit|return)[[:space:]]+0\z/)
  end
  lines
end

def require_run_fragment!(step, fragment, label)
  found = active_shell_lines(step.fetch("run")).any? do |line|
    next false unless line.include?(fragment)
    output_line = line.match?(/\A(echo|printf)[[:space:]]/)
    !output_line || fragment.match?(/\A(echo|printf)[[:space:]]/)
  end
  abort("missing release gate: #{label}") unless found
end

def require_exact_active_lines!(step, expected, label)
  actual = active_shell_lines(step.fetch("run"))
  abort("missing release gate: #{label} exact active command block") unless actual == expected
end

def reject_command_shadowing!(step, command_names, label)
  names = command_names.map { |name| Regexp.escape(name) }.join("|")
  function_pattern = /\A[[:space:]]*(?:function[[:space:]]+)?(?:#{names})(?:[[:space:]]*\(\))?[[:space:]]*(?:\{|\z)/
  alias_pattern = /\A[[:space:]]*alias[[:space:]]+(?:#{names})=/
  step.fetch("run").lines.each do |line|
    abort("missing release gate: #{label} forbids command shadowing") if line.match?(function_pattern) || line.match?(alias_pattern)
  end
end

workflow = YAML.load_file(ARGV.fetch(0))
jobs = workflow.fetch("jobs")
checkout = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"
toolchain = "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30"
cache = "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32"
upload = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
download = "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093"
attest = "actions/attest-build-provenance@0f67c3f4856b2e3261c31976d6725780e5e4c373"
expected_action_topology = {
  "preflight" => [[checkout, {"fetch-depth" => 0}]],
  "validate" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {"workspaces" => ".\ntools/spike\n", "cache-directories" => "~/.cargo/advisory-db"}]],
  "windows" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {}]],
  "linux" => [[checkout, {"persist-credentials" => false}], [toolchain, {"components" => "rustfmt, clippy"}], [cache, {}]],
  "prebuild" => [[checkout, {"fetch-depth" => 0}], [toolchain, {}], [upload, {"name" => "compme-prebuilt-binary", "if-no-files-found" => "error", "retention-days" => 3, "path" => "target/release/compme"}]],
  "build_release" => [[checkout, {"persist-credentials" => false}], [download, {"name" => "compme-prebuilt-binary", "path" => "target/release"}], [attest, {"subject-path" => "${{ steps.pkg.outputs.zip }}"}], [upload, {"name" => "compme-release-artifacts", "if-no-files-found" => "error", "retention-days" => 7, "path" => "${{ steps.pkg.outputs.zip }}\n${{ steps.pkg.outputs.zip }}.sha256\n"}]],
  "publish_release" => [[checkout, {"fetch-depth" => 0}], [download, {"name" => "compme-release-artifacts", "path" => "release-artifacts"}]],
  "finalize_cask" => [[checkout, {"fetch-depth" => 0}], [download, {"name" => "compme-release-artifacts", "path" => "release-artifacts"}]],
  "post_verify" => [],
}
abort("missing release gate: exact release job topology") unless jobs.keys.sort == expected_action_topology.keys.sort
expected_action_topology.each do |job_name, expected|
  actual = Array(jobs.fetch(job_name)["steps"]).each_with_object([]) do |step, actions|
    actions << [step.fetch("uses"), step.fetch("with", {})] if step.is_a?(Hash) && step.key?("uses")
  end
  abort("missing release gate: #{job_name} exact action and input topology") unless actual == expected
end
expected_timeouts = {
  "preflight" => 10, "validate" => 120, "windows" => 60, "linux" => 60,
  "prebuild" => 90, "build_release" => 360, "publish_release" => 20,
  "finalize_cask" => 20, "post_verify" => 30,
}
expected_timeouts.each do |job_name, timeout|
  abort("missing release gate: #{job_name} exact timeout") unless jobs.fetch(job_name).fetch("timeout-minutes") == timeout
end
expected_build_steps = [
  nil,
  "Check release tag matches bundle metadata",
  "Download prebuilt release binary",
  "Restore prebuilt binary executable bit",
  "Verify downloaded binary is arm64 only",
  "Register signing keychain cleanup path",
  "Import Developer ID certificate",
  "Build the .app bundle",
  "Notarize and staple the .app",
  "Delete signing keychain",
  "Package + checksum",
  "Verify packaged app signature and notarization",
  "Attest build provenance",
  "Upload release artifacts",
]
abort("missing release gate: signing job has exact shell-step topology") unless
  jobs.fetch("build_release").fetch("steps").map { |step| step["name"] } == expected_build_steps
abort("missing release gate: release validation inherits read-only workflow permissions") if jobs.fetch("validate").key?("permissions")
audit_step = jobs.fetch("validate").fetch("steps").find { |step| step["name"] == "Rust dependency audit" }
abort("missing release gate: release dependency audit installs and runs pinned cargo-audit") unless
  audit_step && audit_step.fetch("run") == "cargo install cargo-audit --version 0.22.2 --locked\ncargo audit\n"
icon_step = jobs.fetch("validate").fetch("steps").find { |step| step["name"] == "Bundle icon generator self-test" }
abort("missing release gate: release validation runs the bundle icon generator self-test") unless
  icon_step && icon_step.fetch("run") == "tools/bundle/make-icon.sh --self-test"
portable_steps = {
  "Clippy portable workspace (deny warnings)" => "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings",
  "Test portable workspace" => "cargo test --locked --workspace --exclude platform_macos",
  "Build app binary" => "cargo build --locked -p app",
}
%w[windows linux].each do |job_name|
  steps = jobs.fetch(job_name).fetch("steps")
  portable_steps.each do |name, command|
    matches = steps.select { |step| step["name"] == name && step["run"] == command }
    abort("missing release gate: #{job_name} portable parity #{name}") unless matches.length == 1
  end
end
serialized_workflow = workflow.to_s
abort("stale release gate: stable-only workflow contains prerelease branching") if
  serialized_workflow.include?("contains(github.ref_name") || serialized_workflow.match?(/\bprerelease\b/i)

preflight_steps = jobs.fetch("preflight").fetch("steps")
preflight = preflight_steps.find { |step| step["name"] == "Verify release tag is valid and at default-branch HEAD" }
abort("missing release gate: preflight validates release version and exact default-branch HEAD") unless preflight
abort("missing release gate: preflight exact protected-ref/default-branch environment") unless preflight.fetch("env") == {
  "DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}",
  "REF_PROTECTED" => "${{ github.ref_protected }}",
}
require_exact_active_lines!(preflight, [
  "set -euo pipefail",
  'if [ "$REF_PROTECTED" != "true" ]; then',
  'echo "release tag must match a protected v* ruleset" >&2',
  "exit 1",
  "fi",
  'version="${GITHUB_REF_NAME#v}"',
  'tools/release/validate-version.sh "$version"',
  'git fetch --force origin "refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH"',
  'default_sha="$(git rev-parse "origin/$DEFAULT_BRANCH")"',
  'if [ "$GITHUB_SHA" != "$default_sha" ]; then',
  'echo "release tag commit $GITHUB_SHA must equal current origin/$DEFAULT_BRANCH HEAD $default_sha" >&2',
  'exit 1',
  "fi",
], "preflight")

prebuild_steps = jobs.fetch("prebuild").fetch("steps")
abort("missing release gate: secretless prebuild has exact read-only permissions") unless jobs.fetch("prebuild").fetch("permissions") == {"contents" => "read"}
prebuild_head = prebuild_steps.find { |step| step["name"] == "Verify release tag is still at default-branch HEAD" }
abort("missing release gate: prebuild revalidates exact default-branch HEAD") unless prebuild_head
abort("missing release gate: prebuild exact default-branch environment") unless prebuild_head.fetch("env") == {"DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}"}
require_exact_active_lines!(prebuild_head, [
  "set -euo pipefail",
  'git fetch --force origin "refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH"',
  'default_sha="$(git rev-parse "origin/$DEFAULT_BRANCH")"',
  'if [ "$GITHUB_SHA" != "$default_sha" ]; then',
  'echo "release tag commit $GITHUB_SHA must still equal current origin/$DEFAULT_BRANCH HEAD $default_sha before prebuild" >&2',
  'exit 1',
  "fi",
], "prebuild exact HEAD")
prebuild_index = prebuild_steps.index { |step| step["name"] == "Prebuild release binary (no signing secrets in this job)" }
prebuild_arch_index = prebuild_steps.index { |step| step["name"] == "Verify prebuilt binary is arm64 only" }
prebuild_upload_index = prebuild_steps.index { |step| step["name"] == "Upload prebuilt release binary" }
abort("missing release gate: prebuild verifies arm64 after build and before upload") unless prebuild_index && prebuild_arch_index && prebuild_upload_index && prebuild_index < prebuild_arch_index && prebuild_arch_index < prebuild_upload_index
prebuild_arch = prebuild_steps.fetch(prebuild_arch_index)
[
  'archs="$(lipo -archs target/release/compme)"',
  'if [ "$archs" != "arm64" ]; then',
  'exit 1',
].each { |fragment| require_run_fragment!(prebuild_arch, fragment, "prebuild architecture #{fragment}") }

build_steps = jobs.fetch("build_release").fetch("steps")
download_index = build_steps.index { |step| step["name"] == "Download prebuilt release binary" }
chmod_index = build_steps.index { |step| step["name"] == "Restore prebuilt binary executable bit" }
download_arch_index = build_steps.index { |step| step["name"] == "Verify downloaded binary is arm64 only" }
register_index = build_steps.index { |step| step["name"] == "Register signing keychain cleanup path" }
import_index = build_steps.index { |step| step["name"] == "Import Developer ID certificate" }
abort("missing release gate: downloaded binary architecture and cleanup path are verified before secrets") unless download_index && chmod_index && download_arch_index && register_index && import_index && download_index < chmod_index && chmod_index < download_arch_index && download_arch_index < register_index && register_index < import_index
download_arch = build_steps.fetch(download_arch_index)
[
  'archs="$(lipo -archs target/release/compme)"',
  'if [ "$archs" != "arm64" ]; then',
  'exit 1',
].each { |fragment| require_run_fragment!(download_arch, fragment, "downloaded architecture #{fragment}") }
register = build_steps.fetch(register_index)
abort("missing release gate: signing keychain cleanup path is registered deterministically") unless register.fetch("run") == 'echo "COMPME_SIGNING_KEYCHAIN=$RUNNER_TEMP/compme-signing.keychain-db" >> "$GITHUB_ENV"'
import_run = build_steps.fetch(import_index).fetch("run")
assignment = 'keychain="${COMPME_SIGNING_KEYCHAIN:?signing keychain cleanup path was not registered}"'
create = 'security create-keychain -p "$keychain_password" "$keychain"'
key_import = 'security import "$p12" -k "$keychain" -P "$P12_PASSWORD" -T /usr/bin/codesign'
assignment_index = import_run.index(assignment)
abort("missing release gate: signing import consumes the registered cleanup path") unless assignment_index
[create, key_import].each do |command|
  command_index = import_run.index(command)
  abort("missing release gate: registered signing keychain path precedes #{command}") unless command_index && assignment_index < command_index
end

cleanup_index = build_steps.index { |step| step["name"] == "Delete signing keychain" }
abort("missing release gate: deletes signing keychain") unless cleanup_index
cleanup = build_steps.fetch(cleanup_index)
abort("missing release gate: signing keychain cleanup runs always") unless cleanup.fetch("if") == "always()"
cleanup_run = cleanup.fetch("run")
[
  'keychain="${COMPME_SIGNING_KEYCHAIN:-$RUNNER_TEMP/compme-signing.keychain-db}"',
  'cleanup_status=0',
  'if [ -e "$keychain" ] && ! security delete-keychain "$keychain"; then',
  'if [ -e "$keychain" ]; then',
  'cleanup_status=1',
  'unset COMPME_SIGNING_KEYCHAIN COMPME_CODESIGN_IDENTITY',
  'echo "COMPME_SIGNING_KEYCHAIN="',
  'echo "COMPME_CODESIGN_IDENTITY="',
  '>> "$GITHUB_ENV"',
  'exit "$cleanup_status"',
].each { |fragment| require_run_fragment!(cleanup, fragment, "signing keychain cleanup #{fragment}") }
abort("missing release gate: signing keychain deletion must not fail open") if active_shell_lines(cleanup_run).any? { |line| line.match?(/security delete-keychain.*\|\|[[:space:]]*true/) }

package_index = build_steps.index { |step| step["name"] == "Package + checksum" }
package_verify_index = build_steps.index { |step| step["name"] == "Verify packaged app signature and notarization" }
abort("missing release gate: package is reassessed before upload") unless
  package_index && package_verify_index && package_index < package_verify_index
package_verify = build_steps.fetch(package_verify_index)
reject_command_shadowing!(package_verify, %w[mktemp rm ditto find wc tr codesign xcrun spctl], "packaged-app reassessment")
abort("missing release gate: packaged-app reassessment uses exact ZIP output") unless
  package_verify.fetch("env") == {"ZIP" => "${{ steps.pkg.outputs.zip }}"}
[
  'ditto -x -k "$ZIP" "$verify_dir"',
  'if [ "$entry_count" -ne 1 ] || [ ! -d "$verify_dir/Compme.app" ]; then',
  'codesign --verify --deep --strict --verbose=2 "$app"',
  'xcrun stapler validate "$app"',
  'spctl --assess --type execute --verbose=4 "$app"',
].each { |fragment| require_run_fragment!(package_verify, fragment, "packaged-app reassessment #{fragment}") }

publish_steps = jobs.fetch("publish_release").fetch("steps")
abort("missing release gate: publish job has exact create-only publication step topology") unless publish_steps.map { |step| step["name"] } == [
  nil,
  "Download release artifacts",
  "Verify downloaded artifact checksum",
  "Verify artifact build provenance",
  "Verify release tag is still at default-branch HEAD before publication",
  "Write publication-time update manifest",
  "Create draft GitHub release",
  "Revalidate default-branch HEAD and undraft GitHub release",
]
checksum_index = publish_steps.index { |step| step["name"] == "Verify downloaded artifact checksum" }
head_index = publish_steps.index { |step| step["name"] == "Verify release tag is still at default-branch HEAD before publication" }
manifest_index = publish_steps.index { |step| step["name"] == "Write publication-time update manifest" }
create_index = publish_steps.index { |step| step["name"] == "Create draft GitHub release" }
undraft_index = publish_steps.index { |step| step["name"] == "Revalidate default-branch HEAD and undraft GitHub release" }
abort("missing release gate: checksum and exact default HEAD precede publication-time manifest, draft, and undraft") unless checksum_index && head_index && manifest_index && create_index && undraft_index && checksum_index < head_index && head_index < manifest_index && manifest_index < create_index && create_index < undraft_index
publish_head = publish_steps.fetch(head_index)
abort("missing release gate: pre-publication exact default-branch environment") unless publish_head.fetch("env") == {"DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}"}
require_exact_active_lines!(publish_head, [
  "set -euo pipefail",
  'git fetch --force origin "refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH"',
  'default_sha="$(git rev-parse "origin/$DEFAULT_BRANCH")"',
  'if [ "$GITHUB_SHA" != "$default_sha" ]; then',
  'echo "release tag commit $GITHUB_SHA must still equal current origin/$DEFAULT_BRANCH HEAD $default_sha before publication" >&2',
  'exit 1',
  "fi",
], "pre-publication exact HEAD")
manifest = publish_steps.fetch(manifest_index)
reject_command_shadowing!(manifest, %w[awk], "publication-time update manifest")
require_exact_active_lines!(manifest, [
  "set -euo pipefail",
  'VERSION="${GITHUB_REF_NAME#v}"',
  'ZIP="compme-${VERSION}-macos.zip"',
  'MANIFEST="compme-${VERSION}-update.json"',
  %q(SHA256="$(awk '{print $1}' "release-artifacts/$ZIP.sha256")"),
  "tools/release/write-update-manifest.sh \\",
  %q("$VERSION" "$ZIP" "$SHA256" > "release-artifacts/$MANIFEST"),
], "publication-time update manifest")
create = publish_steps.fetch(create_index)
reject_command_shadowing!(create, %w[gh], "draft release creation")
abort("missing release gate: draft creation uses GitHub token") unless create.fetch("env").fetch("GH_TOKEN") == "${{ github.token }}"
require_exact_active_lines!(create, [
  "set -euo pipefail",
  'VERSION="${GITHUB_REF_NAME#v}"',
  'ZIP="compme-${VERSION}-macos.zip"',
  'MANIFEST="compme-${VERSION}-update.json"',
  'gh release create "$GITHUB_REF_NAME" \\',
  '--verify-tag \\',
  '--draft \\',
  '--generate-notes \\',
  '"release-artifacts/$ZIP" \\',
  '"release-artifacts/$ZIP.sha256" \\',
  '"release-artifacts/$MANIFEST"',
], "draft release creation")
undraft = publish_steps.fetch(undraft_index)
reject_command_shadowing!(undraft, %w[git gh], "late undraft recheck")
abort("missing release gate: late undraft recheck uses exact default-branch/token environment") unless undraft.fetch("env") == {
  "DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}",
  "GH_TOKEN" => "${{ github.token }}",
}
require_exact_active_lines!(undraft, [
  "set -euo pipefail",
  'git fetch --force origin \\',
  '"refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH" \\',
  '"refs/tags/$GITHUB_REF_NAME:refs/tags/$GITHUB_REF_NAME"',
  'default_sha="$(git rev-parse "origin/$DEFAULT_BRANCH")"',
  'tag_sha="$(git rev-parse "refs/tags/$GITHUB_REF_NAME^{commit}")"',
  'if [ "$tag_sha" != "$GITHUB_SHA" ] || [ "$tag_sha" != "$default_sha" ]; then',
  'echo "release tag $GITHUB_REF_NAME must still resolve to workflow commit $GITHUB_SHA and current origin/$DEFAULT_BRANCH HEAD $default_sha before undraft; got $tag_sha" >&2',
  'if ! gh release delete "$GITHUB_REF_NAME" --yes; then',
  'echo "failed to delete stale draft release $GITHUB_REF_NAME" >&2',
  "fi",
  "exit 1",
  "fi",
  'gh release edit "$GITHUB_REF_NAME" --draft=false',
], "late default HEAD recheck and fail-closed undraft")

finalize = jobs.fetch("finalize_cask")
abort("missing release gate: cask finalization depends only on publication") unless Array(finalize.fetch("needs")) == ["publish_release"]
abort("missing release gate: cask finalization uses protected release environment") unless finalize.fetch("environment") == "release"
abort("missing release gate: cask finalization alone has contents write") unless finalize.fetch("permissions").fetch("contents") == "write"
finalize_steps = finalize.fetch("steps")
finalize_download = finalize_steps.index { |step| step["name"] == "Download release artifacts" }
finalize_checksum = finalize_steps.index { |step| step["name"] == "Verify downloaded artifact checksum" }
finalize_run = finalize_steps.index { |step| step["name"] == "Finalize Homebrew cask" }
abort("missing release gate: separate cask job downloads and verifies artifacts before finalization") unless finalize_download && finalize_checksum && finalize_run && finalize_download < finalize_checksum && finalize_checksum < finalize_run
abort("missing release gate: separate cask finalizer has exact branch/token environment") unless
  finalize_steps.fetch(finalize_run).fetch("env") == {
    "DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}",
    "GH_TOKEN" => "${{ github.token }}",
  }
require_run_fragment!(finalize_steps.fetch(finalize_run), 'tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"', "separate cask finalizer invocation")
RUBY
}

check_finalizer_helper_contract() {
  ruby - "$1" <<'RUBY'
path = ARGV.fetch(0)
source = File.read(path)

def function_body(source, name)
  start = source.index(/^#{Regexp.escape(name)}\(\) \{[[:space:]]*$/)
  abort("missing release gate: cask finalizer defines #{name}") unless start
  body_start = source.index("\n", start) + 1
  finish = source.index(/^\}[[:space:]]*$/, body_start)
  abort("missing release gate: cask finalizer closes #{name}") unless finish
  source[body_start...finish]
end

def active_shell_lines(body)
  lines = []
  dead_depth = 0
  terminated = false
  body.lines.each do |raw|
    stripped = raw.strip
    next if stripped.empty? || stripped.start_with?("#")
    next if terminated
    if dead_depth.positive?
      dead_depth += 1 if stripped.match?(/\Aif[[:space:]]+/)
      dead_depth -= 1 if stripped == "fi" || stripped.start_with?("fi ")
      next
    end
    if stripped.match?(/\Aif[[:space:]]+false(?:[;[:space:]]|\z)/)
      dead_depth = 1
      next
    end
    lines << stripped.sub(/[[:space:]]+#.*$/, "")
    terminated = true if stripped.sub(/[[:space:]]+#.*$/, "").match?(/\A(?:exit|return)[[:space:]]+0\z/)
  end
  lines
end

def require_active_fragment!(lines, fragment)
  found = lines.any? do |line|
    line.include?(fragment) && !line.match?(/\A(echo|printf)[[:space:]]/)
  end
  abort("missing release gate: cask finalizer helper contract #{fragment}") unless found
end

def require_active_line!(lines, expected)
  abort("missing release gate: cask finalizer helper contract #{expected}") unless
    lines.include?(expected)
end

freeze_lines = active_shell_lines(function_body(source, "freeze_release_helpers"))
published_body = function_body(source, "verify_published_artifact")
published_lines = active_shell_lines(published_body)
validate_lines = active_shell_lines(function_body(source, "validate_finalized_cask"))
finalize_lines = active_shell_lines(function_body(source, "finalize_cask"))
[
  'for helper in validate-version.sh update-cask.sh; do',
  'tag_sha="$2"',
  'destination="$frozen_root/tools/release/$helper"',
  'git -C "$repo_root" show "$tag_sha:tools/release/$helper" >"$destination"',
].each { |fragment| require_active_fragment!(freeze_lines, fragment) }
if freeze_lines.any? { |line| line.include?('cp "$repo_root/tools/release/$helper"') || line.include?('source_path="$repo_root/tools/release/$helper"') }
  abort("stale release gate: cask finalizer freezes helpers from the working tree")
end
[
  'artifact_name="compme-${version}-macos.zip"',
  'if [ "$(basename "$artifact_path")" != "$artifact_name" ]; then',
  'if ! release_ineligible="$(command gh release view "$tag" \\',
  '--json isDraft,isPrerelease \\',
  "--jq '.isDraft or .isPrerelease')\"; then",
  'if [ "$release_ineligible" != "false" ]; then',
  '--repo mudrii/compme',
  '--pattern "$checksum_name"',
  'local_sha="$(shasum -a 256 "$artifact_path"',
  'if [ "$local_sha" != "$published_sha" ]; then',
].each { |fragment| require_active_fragment!(published_lines, fragment) }
abort("missing release gate: cask finalizer actively downloads published checksum") unless
  published_lines.include?('if ! command gh release download "$tag" \\')
release_state_index = published_lines.index { |line| line == 'if ! release_ineligible="$(command gh release view "$tag" \\' }
download_checksum_index = published_lines.index { |line| line == 'if ! command gh release download "$tag" \\' }
abort("missing release gate: cask finalizer verifies stable published state before checksum download") unless
  release_state_index && download_checksum_index && release_state_index < download_checksum_index
abort("missing release gate: cask finalizer strictly parses published checksum") unless
  published_body.include?('match = /\A([0-9a-f]{64})  #{Regexp.escape(expected_name)}\n?\z/.match(content)')
[
  "expected_sha=\"$(shasum -a 256 \"$artifact_path\"",
  'ruby -c "$cask_path"',
  'require_exact_cask_line "$cask_path" "  version \"$version\"" "version"',
  'require_exact_cask_line "$cask_path" "  sha256 \"$expected_sha\"" "artifact sha256"',
  'url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"',
  'depends_on macos: :sonoma',
  'depends_on arch: :arm64',
].each { |fragment| require_active_fragment!(validate_lines, fragment) }
[
  'remote_branch_ref="refs/remotes/origin/$default_branch"',
  'verified_tag_ref="refs/compme-release/tags/$tag"',
  'if ! git fetch --no-tags origin',
  '"+refs/heads/$default_branch:$remote_branch_ref"',
  '"+refs/tags/$tag:$verified_tag_ref"; then',
  'freeze_release_helpers "$frozen_root" "$tag_sha"',
  'verify_published_artifact \\',
  'frozen_validator="$frozen_root/tools/release/validate-version.sh"',
  'frozen_updater="$frozen_root/tools/release/update-cask.sh"',
  '"$frozen_validator" "$version"',
  'COMPME_CASK_PATH="$cask_path"',
  'COMPME_CASK_ARTIFACT="$artifact_path"',
  '"$frozen_updater" "$tag"',
  'validate_finalized_cask "$cask_path" "$version" "$artifact_path"',
].each { |fragment| require_active_fragment!(finalize_lines, fragment) }
[
  'echo "failed to fetch release tag $tag and origin/$default_branch" >&2',
  'echo "failed to resolve fetched release tag $tag" >&2',
].each { |line| require_active_line!(finalize_lines, line) }

fetch_index = finalize_lines.index { |line| line.start_with?('if ! git fetch --no-tags origin') }
tag_sha_index = finalize_lines.index { |line| line.include?('tag_sha="$(git rev-parse "$verified_tag_ref^{commit}")"') }
tag_sha_verification_index = finalize_lines.index { |line| line.include?('if [ "$tag_sha" != "$GITHUB_SHA" ]; then') }
ancestry_index = finalize_lines.index { |line| line.include?('git merge-base --is-ancestor "$GITHUB_SHA" "$remote_branch_ref"') }
freeze_index = finalize_lines.index { |line| line.include?('freeze_release_helpers "$frozen_root" "$tag_sha"') }
published_index = finalize_lines.index { |line| line.include?('verify_published_artifact \\') }
checkout_index = finalize_lines.index { |line| line.include?('git checkout "$default_branch"') }
pull_index = finalize_lines.index { |line| line.include?('git pull --ff-only --no-tags origin "$default_branch"') }
updater_index = finalize_lines.index { |line| line.include?('"$frozen_updater" "$tag"') }
validation_index = finalize_lines.index { |line| line.include?('validate_finalized_cask "$cask_path" "$version" "$artifact_path"') }
git_add_index = finalize_lines.index { |line| line.include?('git add Casks/compme.rb') }
commit_index = finalize_lines.index { |line| line.include?('commit -m "chore(release): cask $tag"') }
push_index = finalize_lines.index { |line| line.include?('git push origin "HEAD:$default_branch"') }
abort("missing release gate: cask finalizer verifies tag provenance and published artifact before mutable branch operations") unless
  fetch_index && tag_sha_index && tag_sha_verification_index && ancestry_index && published_index && freeze_index && checkout_index && pull_index &&
  fetch_index < tag_sha_index && tag_sha_index < tag_sha_verification_index && tag_sha_verification_index < ancestry_index &&
  ancestry_index < freeze_index && freeze_index < published_index && published_index < checkout_index && checkout_index < pull_index
abort("missing release gate: cask finalizer validates the frozen-updater result before git publication") unless
  updater_index && validation_index && git_add_index && commit_index && push_index &&
  updater_index < validation_index && validation_index < git_add_index && validation_index < commit_index && validation_index < push_index

finalize_lines.each do |line|
  if line.match?(%r{(?:\A|[[:space:]])(?:"?\$repo_root/)?tools/release/update-cask\.sh["[:space:]]+"?\$tag"?})
    abort("stale release gate: cask finalizer executes update-cask.sh from the moving default branch")
  end
end
last_executable_line = source.lines.reverse.find do |raw|
  stripped = raw.strip
  !stripped.empty? && !stripped.start_with?("#")
end&.strip
abort("missing release gate: cask finalizer dispatches the reviewed finalize_cask function") unless
  last_executable_line == 'finalize_cask "$@"'
RUBY
}

check_manual_a2_summary() {
  local summary_file="$1"
  local summary_label="$2"
  require_line "$summary_file" '^A2 validation is local/manual-only' "$summary_label marks A2 local/manual-only"
  require_line "$summary_file" 'tools/acceptance/run-a2-compat-gates\.sh --self-test' "$summary_label retains the local A2 runner self-test"
  require_line "$summary_file" 'tools/release/check-a2-matrix-ledger\.sh --self-test' "$summary_label retains the local A2 ledger self-test"
  if grep -Fq '"$ledger"' "$summary_file" && ! grep -Eq '^ledger=' "$summary_file"; then
    echo "stale release gate: $summary_label uses an undefined A2 ledger variable" >&2
    return 1
  fi
}

require_test_symbol() {
  file="$1"
  symbol="$2"
  label="$3"
  if ! awk -v symbol="$symbol" '
    /^[[:space:]]*#\[(.*::)?test(\]|[[:space:]]*\()/ { pending_test = 1; next }
    pending_test && /^[[:space:]]*#/ { next }
    pending_test && $0 ~ "^[[:space:]]*(pub[[:space:]]+)?fn[[:space:]]+" symbol "\\(" { found = 1 }
    pending_test && $0 !~ /^[[:space:]]*$/ { pending_test = 0 }
    END { exit found ? 0 : 1 }
  ' "$file"; then
    echo "missing release gate: $label" >&2
    return 1
  fi
}

require_readme_gate_line() {
  pattern="$1"
  label="$2"
  if ! awk '
    /^## Current Validation Gates$/ { in_section = 1; next }
    in_section && /^## / { in_section = 0 }
    in_section { print }
  ' "$readme_doc" | grep -Eq "$pattern"; then
    echo "missing release gate: $label" >&2
    return 1
  fi
}

require_readme_homebrew_line() {
  pattern="$1"
  label="$2"
  if ! awk '
    /^### Homebrew \(macOS\)$/ { in_section = 1; next }
    in_section && /^### / { in_section = 0 }
    in_section { print }
  ' "$readme_doc" | grep -Eq "$pattern"; then
    echo "missing release gate: $label" >&2
    return 1
  fi
}

reject_readme_homebrew_line() {
  pattern="$1"
  label="$2"
  # Self-sufficient fail-loud on a missing README (same reasoning as reject_line):
  # a vanished file would make the awk|grep pipe no-match and pass silently. Today
  # earlier require_line "$readme_doc" calls abort first, but don't rely on ordering.
  if [ ! -f "$readme_doc" ]; then
    echo "release gate target missing: $label ($readme_doc)" >&2
    return 1
  fi
  if awk '
    /^### Homebrew \(macOS\)$/ { in_section = 1; next }
    in_section && /^### / { in_section = 0 }
    in_section { print }
  ' "$readme_doc" | grep -Eq "$pattern"; then
    echo "stale release gate: $label" >&2
    return 1
  fi
}

require_development_gate_line() {
  pattern="$1"
  label="$2"
  if ! awk '
    /^## Full Local Gate$/ { in_section = 1; next }
    in_section && /^## / { in_section = 0 }
    in_section { print }
  ' "$development_doc" | grep -Eq "$pattern"; then
    echo "missing release gate: $label" >&2
    return 1
  fi
}

require_grammar_spec_validation_line() {
  pattern="$1"
  label="$2"
  if ! awk '
    /^## Validation commands$/ { in_section = 1; next }
    in_section && /^## / { in_section = 0 }
    in_section { print }
  ' "$grammar_spec" | sed -E 's/^- `?//; s/`$//' | grep -Eq "$pattern"; then
    echo "missing release gate: $label" >&2
    return 1
  fi
}

check_self_test_env_file() {
  file="$1"
  shift
  ruby - "$file" "$@" <<'RUBY'
path = ARGV.shift
required = ARGV
lines = File.readlines(path)
start = lines.index { |line| line.match?(/^run_self_test\(\)[[:space:]]*\{[[:space:]]*$/) }
abort("missing release gate: #{path} defines run_self_test") unless start
unset_vars = lines.drop(start + 1).each_with_object([]) do |line, vars|
  stripped = line.strip
  vars.concat(stripped.split.drop(1)) if stripped.start_with?("unset ")
end
missing = required.reject { |name| unset_vars.include?(name) }
abort("missing release gate: #{path} self-test unsets #{missing.join(', ')}") unless missing.empty?
RUBY
}

check_all_self_test_env_contracts() {
  check_self_test_env_file "$gate_script" \
    GITHUB_ACTIONS GITHUB_REF_TYPE COMPME_ALLOW_MODEL_GATE_OVERRIDE \
    COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256 \
    COMPME_REQUIRE_LATENCY_BUDGET
  check_self_test_env_file "$notarize_script" \
    COMPME_NOTARYTOOL_KEYCHAIN_PROFILE COMPME_NOTARYTOOL_KEY_BASE64 \
    COMPME_NOTARYTOOL_KEY_PATH COMPME_NOTARYTOOL_KEY_ID \
    COMPME_NOTARYTOOL_ISSUER COMPME_NOTARYTOOL_APPLE_ID \
    COMPME_NOTARYTOOL_PASSWORD COMPME_NOTARYTOOL_TEAM_ID \
    COMPME_NOTARYTOOL_TEMP_KEY COMPME_NOTARYTOOL_TIMEOUT
  check_self_test_env_file "$make_app_script" \
    COMPME_BUNDLE_REPO_ROOT COMPME_BUNDLE_LSREGISTER CARGO_TARGET_DIR \
    COMPME_BUNDLE_SKIP_BUILD COMPME_CODESIGN_IDENTITY COMPME_CODESIGN_ENTITLEMENTS
  check_self_test_env_file "$update_cask_script" COMPME_CASK_PATH COMPME_CASK_ARTIFACT
  check_self_test_env_file "$update_manifest_script" COMPME_UPDATE_PUBLISHED_AT
  check_self_test_env_file "$bundle_metadata_script" COMPME_EXPECTED_VERSION COMPME_CASK_TAG_CANDIDATES
  check_self_test_env_file "$quality_script" \
    COMPME_ALLOW_MODEL_GATE_OVERRIDE \
    COMPME_MODEL_GATE_PATH COMPME_MODEL_GATE_URL COMPME_MODEL_GATE_SHA256 \
    COMPME_REQUIRE_MODEL_TESTS COMPME_REQUIRE_MODEL_CONTEXT \
    COMPME_QUALITY_CORPUS
  check_self_test_env_file "$version_docs_script" COMPME_VERSION_DOCS_ROOT
}

run_self_test() {
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/compme-check-model-gates.XXXXXX")"
  cleanup() {
    rm -rf "$tmp_dir"
  }

  check_all_self_test_env_contracts
  contract_index=0
  for spec in \
    "$gate_script|COMPME_REQUIRE_LATENCY_BUDGET" \
    "$notarize_script|COMPME_NOTARYTOOL_KEYCHAIN_PROFILE" \
    "$make_app_script|COMPME_BUNDLE_REPO_ROOT" \
    "$update_cask_script|COMPME_CASK_ARTIFACT" \
    "$update_manifest_script|COMPME_UPDATE_PUBLISHED_AT" \
    "$bundle_metadata_script|COMPME_EXPECTED_VERSION" \
    "$quality_script|COMPME_QUALITY_CORPUS" \
    "$version_docs_script|COMPME_VERSION_DOCS_ROOT"; do
    source_file="${spec%%|*}"
    poisoned_var="${spec#*|}"
    contract_fixture="$tmp_dir/env-contract-$contract_index.sh"
    contract_index=$((contract_index + 1))
    cp "$source_file" "$contract_fixture"
    ruby -e 'var, path = ARGV; text = File.read(path); File.write(path, text.gsub(/\b#{Regexp.escape(var)}\b/, ""))' \
      "$poisoned_var" "$contract_fixture"
    if check_self_test_env_file "$contract_fixture" "$poisoned_var" >/dev/null 2>&1; then
      echo "release gate self-test failed: missing $poisoned_var cleanup was accepted" >&2
      cleanup
      return 1
    fi
  done

  good_manual_doc="$tmp_dir/good-manual.md"
  cat >"$good_manual_doc" <<'MD'
A2 validation is local/manual-only.
- `tools/acceptance/run-a2-compat-gates.sh --self-test`
- `tools/release/check-a2-matrix-ledger.sh --self-test`
MD
  check_manual_a2_summary "$good_manual_doc" "fixture docs"

  bad_manual_doc="$tmp_dir/bad-manual.md"
  cat >"$bad_manual_doc" <<'MD'
A2 validation is local/manual-only.
- `tools/acceptance/run-a2-compat-gates.sh --self-test`
- `tools/release/check-a2-matrix-ledger.sh --self-test`
- `tools/release/check-a2-matrix-ledger.sh "$ledger"`
MD
  if check_manual_a2_summary "$bad_manual_doc" "fixture docs" >/dev/null 2>&1; then
    echo "release gate self-test failed: undefined manual A2 ledger variable was accepted" >&2
    cleanup
    return 1
  fi

  good_pipeline="$tmp_dir/good-pipeline.yml"
  cat >"$good_pipeline" <<'YAML'
jobs:
  check:
    steps:
      - name: Script syntax
        run: |
          find tools/acceptance tools/bundle tools/release -type f -name '*.sh' -print0 \
            | xargs -0 bash -n
YAML
  check_no_automated_a2_validation "$good_pipeline" "fixture"

  bad_pipeline="$tmp_dir/bad-pipeline.yml"
  cp "$good_pipeline" "$bad_pipeline"
  ruby -0pi -e 'sub(/(    steps:\n)/, "\\1      - name: A2 compatibility runner self-test\\n        run: tools/acceptance/run-a2-compat-gates.sh --self-test\\n")' "$bad_pipeline"
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: automated A2 runner was accepted" >&2
    cleanup
    return 1
  fi

  bad_pipeline="$tmp_dir/bad-generic-syntax.yml"
  cp "$good_pipeline" "$bad_pipeline"
  ruby -0pi -e 'sub(/find tools\/acceptance.*xargs -0 bash -n/m, "bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh")' "$bad_pipeline"
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: wildcard A2 syntax validation was accepted" >&2
    cleanup
    return 1
  fi

  bad_pipeline="$tmp_dir/bad-workflow-a2-env.yml"
  cp "$good_pipeline" "$bad_pipeline"
  ruby -0pi -e 'sub(/jobs:\n/, "env:\n  COMPME_A2_MATRIX_LEDGER: evidence.tsv\njobs:\n")' "$bad_pipeline"
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: workflow-level A2 environment was accepted" >&2
    cleanup
    return 1
  fi

  bad_pipeline="$tmp_dir/bad-job-a2-env.yml"
  cat >"$bad_pipeline" <<'YAML'
jobs:
  check:
    env:
      COMPME_A2_MATRIX_LEDGER: evidence.tsv
    steps:
      - name: Script syntax
        run: |
          find tools/acceptance tools/bundle tools/release -type f -name '*.sh' -print0 \
            | xargs -0 bash -n
YAML
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: job-level A2 environment was accepted" >&2
    cleanup
    return 1
  fi

  bad_pipeline="$tmp_dir/bad-exclusion-tail.yml"
  cat >"$bad_pipeline" <<'YAML'
jobs:
  check:
    steps:
      - name: Script syntax
        run: |
          find tools/acceptance tools/bundle tools/release -type f -name '*.sh' \
            ! -path 'tools/acceptance/run-a2-compat-gates.sh' \; tools/acceptance/run-a2-compat-gates.sh --self-test \
            ! -path 'tools/release/check-a2-matrix-ledger.sh' -print0 \
            | xargs -0 bash -n
YAML
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: A2 execution hidden after an exclusion was accepted" >&2
    cleanup
    return 1
  fi

  bad_pipeline="$tmp_dir/bad-extra-a2-traversal.yml"
  cp "$good_pipeline" "$bad_pipeline"
  ruby -0pi -e 'sub(/(    steps:\n)/, "\\1      - name: Extra shell validation\\n        run: find tools/acceptance -type f -exec bash -n {} +\\n")' "$bad_pipeline"
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: additional generic traversal reaching A2 tooling was accepted" >&2
    cleanup
    return 1
  fi

  bad_pipeline="$tmp_dir/bad-constructed-a2-path.yml"
  cp "$good_pipeline" "$bad_pipeline"
  ruby -0pi -e 'sub(/(    steps:\n)/, "\\1      - name: Constructed validation path\\n        run: kind=a2; tools/acceptance/run-\"$kind\"-compat-gates.sh --self-test\\n")' "$bad_pipeline"
  if check_no_automated_a2_validation "$bad_pipeline" "fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: dynamically constructed A2 runner path was accepted" >&2
    cleanup
    return 1
  fi

  old_grammar_spec="$grammar_spec"
  grammar_spec="$tmp_dir/good-spec.md"
  cat >"$grammar_spec" <<'MD'
# Fixture

## Validation commands
- `cargo fmt --all -- --check`

## Next section
MD
  require_grammar_spec_validation_line '^cargo fmt --all -- --check[[:space:]]*$' "fixture grammar spec fmt gate"

  grammar_spec="$tmp_dir/bad-spec.md"
  cat >"$grammar_spec" <<'MD'
# Fixture

## Validation commands
- `cargo fmt --workspace`

## Next section
MD
  if require_grammar_spec_validation_line '^cargo fmt --all -- --check[[:space:]]*$' "fixture grammar spec fmt gate" >/dev/null 2>&1; then
    echo "release gate self-test failed: malformed grammar spec validation command was accepted" >&2
    grammar_spec="$old_grammar_spec"
    cleanup
    return 1
  fi

  grammar_spec="$old_grammar_spec"

  old_readme_doc="$readme_doc"
  readme_doc="$tmp_dir/good-readme.md"
  cat >"$readme_doc" <<'MD'
# Fixture

## Current Validation Gates
tools/bundle/check-bundle-metadata.sh

## Next section
MD
  require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "fixture README gate line"

  readme_doc="$tmp_dir/bad-readme.md"
  cat >"$readme_doc" <<'MD'
# Fixture

tools/bundle/check-bundle-metadata.sh

## Current Validation Gates
- no gate commands listed here

## Next section
MD
  if require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "fixture README gate line" >/dev/null 2>&1; then
    echo "release gate self-test failed: README gate line outside the gates section was accepted" >&2
    readme_doc="$old_readme_doc"
    cleanup
    return 1
  fi

  readme_doc="$old_readme_doc"

  old_readme_doc="$readme_doc"
  readme_doc="$tmp_dir/good-homebrew-readme.md"
  cat >"$readme_doc" <<'MD'
# Fixture

### Homebrew (macOS)

Homebrew cask install is not available until the first signed `v*` release
publishes the artifact and finalizes the cask checksum. Until then, build from
source.

### From source
MD
  require_readme_homebrew_line 'Homebrew cask install is not available until the first signed' "fixture README Homebrew blocked status"
  require_readme_homebrew_line 'Until then, build from' "fixture README source fallback"

  readme_doc="$tmp_dir/bad-homebrew-readme.md"
  cat >"$readme_doc" <<'MD'
# Fixture

Homebrew cask install is not available until the first signed `v*` release.

### Homebrew (macOS)

brew install --cask compme

### From source
MD
  if require_readme_homebrew_line 'Homebrew cask install is not available until the first signed' "fixture README Homebrew blocked status" >/dev/null 2>&1; then
    echo "release gate self-test failed: README Homebrew blocked wording outside the Homebrew section was accepted" >&2
    readme_doc="$old_readme_doc"
    cleanup
    return 1
  fi

  readme_doc="$old_readme_doc"

  cask_url_fixture="$tmp_dir/good-cask.rb"
  cat >"$cask_url_fixture" <<'CASK'
cask "compme" do
  version "1.2.3"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
end
CASK
  require_line "$cask_url_fixture" '^  url "https://github\.com/mudrii/compme/releases/download/v#\{version\}/compme-#\{version\}-macos\.zip"$' "fixture cask GitHub release URL"

  cask_bad_url_fixture="$tmp_dir/bad-cask-url.rb"
  cat >"$cask_bad_url_fixture" <<'CASK'
cask "compme" do
  version "1.2.3"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  url "https://evil.example/compme.zip"
end
CASK
  if require_line "$cask_bad_url_fixture" '^  url "https://github\.com/mudrii/compme/releases/download/v#\{version\}/compme-#\{version\}-macos\.zip"$' "fixture cask GitHub release URL" >/dev/null 2>&1; then
    echo "release gate self-test failed: unexpected cask URL was accepted" >&2
    cleanup
    return 1
  fi

  stale_latency_fixture="$tmp_dir/stale-latency.rs"
  cat >"$stale_latency_fixture" <<'RS'
#[ignore = "requires the qwen2.5-0.5b GGUF model + Metal GPU; run with --ignored"]
fn release_enforced_model_test() {}
RS
  if reject_line "$stale_latency_fixture" 'Metal GPU' "fixture stale root latency GPU wording" >/dev/null 2>&1; then
    echo "release gate self-test failed: stale root latency GPU wording was accepted" >&2
    cleanup
    return 1
  fi

  # A negative assertion over a MISSING file must fail loud, not pass silently —
  # else renaming the guarded file turns the gate dead-green.
  if reject_line "$tmp_dir/reject-target-absent.rs" 'anything' "fixture missing reject target" >/dev/null 2>&1; then
    echo "release gate self-test failed: reject_line passed for a missing target file" >&2
    cleanup
    return 1
  fi

  rust_helper_fixture="$tmp_dir/helper-only.rs"
  cat >"$rust_helper_fixture" <<'RS'
fn accept_correction_emits_replace_range() {}
RS
  if require_test_symbol "$rust_helper_fixture" 'accept_correction_emits_replace_range' "fixture helper-only test symbol" >/dev/null 2>&1; then
    echo "release gate self-test failed: helper function was accepted as a test" >&2
    cleanup
    return 1
  fi

  rust_test_fixture="$tmp_dir/real-test.rs"
  cat >"$rust_test_fixture" <<'RS'
#[test]
fn accept_correction_emits_replace_range() {}
RS
  require_test_symbol "$rust_test_fixture" 'accept_correction_emits_replace_range' "fixture real test symbol"

  check_finalizer_fixture() {
    ruby -ryaml - "$1" <<'RUBY'
def active_shell_lines(run)
  run.lines.map do |line|
    stripped = line.strip
    next if stripped.empty? || stripped.start_with?("#")
    stripped.sub(/[[:space:]]+#.*$/, "")
  end.compact
end

def require_active_finalizer_command!(run, needle)
  found = active_shell_lines(run).any? do |line|
    line.include?(needle) &&
      !line.start_with?("echo ") &&
      !line.start_with?("printf ")
  end
  abort("missing release gate: finalizes Homebrew cask command #{needle}") unless found
end

workflow = YAML.load_file(ARGV.fetch(0))
release_steps = workflow.fetch("jobs").fetch("release").fetch("steps")
publish_index = release_steps.index { |step| step["name"] == "Create draft GitHub release" }
cask_index = release_steps.index { |step| step["name"] == "Finalize Homebrew cask" }
abort("missing release gate: publishes GitHub release") unless publish_index
abort("missing release gate: finalizes Homebrew cask") unless cask_index
abort("missing release gate: finalizes Homebrew cask after publishing release") unless cask_index > publish_index
cask_run = release_steps.fetch(cask_index).fetch("run")
require_active_finalizer_command!(cask_run, "tools/release/finalize-cask.sh \"$TAG\" \"$artifact_path\" \"$VERSION\" \"$DEFAULT_BRANCH\"")
RUBY
  }

  check_developer_id_fixture() {
    ruby -ryaml - "$1" <<'RUBY'
def contains_secret_reference?(value)
  case value
  when String
    value.include?("secrets.")
  when Hash
    value.any? { |_, child| contains_secret_reference?(child) }
  when Array
    value.any? { |child| contains_secret_reference?(child) }
  else
    false
  end
end

workflow = YAML.load_file(ARGV.fetch(0))
prebuild_job = workflow.fetch("jobs").fetch("prebuild")
abort("missing release gate: prebuild job carries no secret references") if contains_secret_reference?(prebuild_job)
prebuild_steps = prebuild_job.fetch("steps")
prebuild_index = prebuild_steps.index { |step| step["name"] == "Prebuild release binary (no signing secrets in this job)" }
abort("missing release gate: prebuilds release binary in secretless job") unless prebuild_index
release_steps = workflow.fetch("jobs").fetch("release").fetch("steps")
import_index = release_steps.index { |step| step["name"] == "Import Developer ID certificate" }
build_index = release_steps.index { |step| step["name"] == "Build the .app bundle" }
abort("missing release gate: imports Developer ID certificate") unless import_index
abort("missing release gate: builds app bundle") unless build_index
abort("missing release gate: imports Developer ID certificate before build") unless import_index < build_index
build_run = release_steps.fetch(build_index).fetch("run")
abort("missing release gate: bundle build skips cargo in signing job") unless build_run == %q(COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle")
release_steps.each_with_index do |step, idx|
  run = step["run"].to_s
  abort("missing release gate: no cargo command anywhere in signing job") if run.match?(/(^|[;&|[:space:]])cargo[[:space:]]/)
end
import_step = release_steps.fetch(import_index)
abort("missing release gate: Developer ID import is unconditional") if import_step.key?("if")
import_env = import_step.fetch("env")
{
  "P12_BASE64" => "secrets.COMPME_DEVELOPER_ID_P12_BASE64",
  "P12_PASSWORD" => "secrets.COMPME_DEVELOPER_ID_P12_PASSWORD",
  "SIGNING_IDENTITY" => "secrets.COMPME_CODESIGN_IDENTITY",
}.each do |key, needle|
  abort("missing release gate: Developer ID secret #{key}") unless import_env.fetch(key).include?(needle)
end
import_run = import_step.fetch("run")
[
  "for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY",
  "missing required release secret",
  "exit 1",
  "p12=\"$RUNNER_TEMP/developer-id.p12\"",
  "trap 'rm -f \"$p12\"' EXIT",
  "install -m 600 /dev/null \"$p12\"",
  "rm -f \"$p12\"",
  "trap - EXIT",
  "COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY",
].each do |needle|
  abort("missing release gate: Developer ID import policy") unless import_run.include?(needle)
end
RUBY
  }

  check_release_hardening_fixture() {
    ruby -ryaml - "$1" <<'RUBY'
workflow = YAML.load_file(ARGV.fetch(0))
abort("missing release gate: workflow defaults to read-only contents permission") unless workflow.fetch("permissions").fetch("contents") == "read"
jobs = workflow.fetch("jobs")
release = jobs.fetch("release")
abort("missing release gate: release job uses protected release environment") unless release.fetch("environment") == "release"
def approved_action_ref?(uses)
  [
    "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
    "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30",
    "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32",
    "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
    "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093",
  ].include?(uses)
end

def validate_action_inputs!(job_name, step)
  uses = step.fetch("uses")
  allowed = case uses.split("@", 2).first
            when "actions/checkout" then %w[fetch-depth persist-credentials]
            when "actions/upload-artifact" then %w[name path if-no-files-found]
            when "actions/download-artifact" then %w[name path]
            when "dtolnay/rust-toolchain" then %w[toolchain components]
            when "Swatinem/rust-cache" then []
            else []
            end
  actual = step.fetch("with", {}).keys.map(&:to_s)
  unexpected = actual - allowed
  abort("missing release gate: #{job_name} action has unapproved provenance/input keys #{unexpected.join(', ')}") unless unexpected.empty?
end
def contains_credential_reference?(value, parent_key = nil)
  case value
  when String
    value.include?("secrets.") || value.include?("github.token") ||
      (parent_key.to_s == "secrets" && value == "inherit")
  when Hash
    value.any? do |key, child|
      key_name = key.to_s
      credential_input = key_name != "persist-credentials" &&
        key_name.match?(/(?:^|[-_])(token|secret|password|credential|ssh-key)(?:s)?(?:$|[-_])/i)
      (credential_input && !child.to_s.empty?) || contains_credential_reference?(child, key_name)
    end
  when Array
    value.any? { |child| contains_credential_reference?(child, parent_key) }
  else
    false
  end
end
jobs.each do |job_name, job|
  abort("missing release gate: #{job_name} uses an unapproved reusable workflow") if job.key?("uses")
  Array(job["steps"]).each do |step|
    next unless step.key?("uses")
    abort("missing release gate: #{job_name} action uses an approved identity and commit SHA") unless approved_action_ref?(step["uses"])
    validate_action_inputs!(job_name, step)
  end
end
expected_actions = {
  "validate" => ["actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"],
  "windows" => ["dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30"],
  "linux" => ["Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32"],
  "prebuild" => [],
  "release" => ["actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"],
}
expected_actions.each do |job_name, expected|
  actual = Array(jobs.fetch(job_name)["steps"]).map { |step| step["uses"] }.compact
  abort("missing release gate: #{job_name} has the exact approved action sequence") unless actual == expected
end
if jobs.key?("prebuild")
  abort("missing release gate: prebuild job carries no credential references") if contains_credential_reference?(jobs.fetch("prebuild"))
end
needs = Array(release.fetch("needs"))
%w[validate windows linux].each do |job|
  abort("missing release gate: release job depends on #{job}") unless needs.include?(job)
end
abort("missing release gate: release job has contents write permission") unless release.fetch("permissions").fetch("contents") == "write"
checkout = release.fetch("steps").find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
abort("missing release gate: release checkout") unless checkout
abort("missing release gate: release checkout fetches full history") unless checkout.fetch("with").fetch("fetch-depth") == 0
abort("missing release gate: platform_windows release job") unless jobs.fetch("windows").fetch("runs-on") == "windows-latest"
abort("missing release gate: platform_linux release job") unless jobs.fetch("linux").fetch("runs-on") == "ubuntu-latest"
RUBY
  }

  good_release="$tmp_dir/good-release.yml"
  cat >"$good_release" <<'YAML'
jobs:
  release:
    steps:
      - name: Create draft GitHub release
        run: gh release create "$TAG" --draft
      - name: Finalize Homebrew cask
        run: |
          tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"
YAML
  check_finalizer_fixture "$good_release"

  commented_release="$tmp_dir/commented-release.yml"
  cat >"$commented_release" <<'YAML'
jobs:
  release:
    steps:
      - name: Create draft GitHub release
        run: gh release create "$TAG" --draft
      - name: Finalize Homebrew cask
        run: |
          # tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"
          echo 'tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"'
YAML
  if check_finalizer_fixture "$commented_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: commented/echoed cask finalizer commands were accepted" >&2
    cleanup
    return 1
  fi

  inline_commented_release="$tmp_dir/inline-commented-release.yml"
  cat >"$inline_commented_release" <<'YAML'
jobs:
  release:
    steps:
      - name: Create draft GitHub release
        run: gh release create "$TAG" --draft
      - name: Finalize Homebrew cask
        run: |
          : # tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"
YAML
  if check_finalizer_fixture "$inline_commented_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: inline-commented cask finalizer commands were accepted" >&2
    cleanup
    return 1
  fi

  reordered_release="$tmp_dir/reordered-release.yml"
  cat >"$reordered_release" <<'YAML'
jobs:
  release:
    steps:
      - name: Finalize Homebrew cask
        run: |
          tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"
      - name: Create draft GitHub release
        run: gh release create "$TAG" --draft
YAML
  if check_finalizer_fixture "$reordered_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: pre-publish cask finalizer was accepted" >&2
    cleanup
    return 1
  fi

  missing_finalizer_release="$tmp_dir/missing-finalizer-release.yml"
  cat >"$missing_finalizer_release" <<'YAML'
jobs:
  release:
    steps:
      - name: Create draft GitHub release
        run: gh release create "$TAG" --draft
      - name: Finalize Homebrew cask
        run: |
          echo "finalizer omitted"
YAML
  if check_finalizer_fixture "$missing_finalizer_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing cask finalizer command was accepted" >&2
    cleanup
    return 1
  fi

  good_developer_id_release="$tmp_dir/good-developer-id-release.yml"
  cat >"$good_developer_id_release" <<'YAML'
jobs:
  prebuild:
    steps:
      - name: Prebuild release binary (no signing secrets in this job)
        run: cargo build --locked --release -p app
  release:
    steps:
      - name: Import Developer ID certificate
        env:
          P12_BASE64: ${{ secrets.COMPME_DEVELOPER_ID_P12_BASE64 }}
          P12_PASSWORD: ${{ secrets.COMPME_DEVELOPER_ID_P12_PASSWORD }}
          SIGNING_IDENTITY: ${{ secrets.COMPME_CODESIGN_IDENTITY }}
        run: |
          for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY; do
            if [ -z "${!name:-}" ]; then
              echo "missing required release secret: $name" >&2
              exit 1
            fi
          done
          p12="$RUNNER_TEMP/developer-id.p12"
          trap 'rm -f "$p12"' EXIT
          install -m 600 /dev/null "$p12"
          printf '%s' "$P12_BASE64" | base64 --decode > "$p12"
          rm -f "$p12"
          trap - EXIT
          echo "COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY" >> "$GITHUB_ENV"
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  check_developer_id_fixture "$good_developer_id_release"

  conditional_developer_id_release="$tmp_dir/conditional-developer-id-release.yml"
  cp "$good_developer_id_release" "$conditional_developer_id_release"
  ruby -0pi -e 'sub(/(      - name: Import Developer ID certificate\n)/, "\\1        if: false\n")' "$conditional_developer_id_release"
  if check_developer_id_fixture "$conditional_developer_id_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: conditional Developer ID import was accepted" >&2
    cleanup
    return 1
  fi

  late_chmod_developer_id_release="$tmp_dir/late-chmod-developer-id-release.yml"
  cp "$good_developer_id_release" "$late_chmod_developer_id_release"
  ruby -0pi -e '
    sub(/          install -m 600 \/dev\/null "\$p12"\n/, "")
    sub(/(          printf .*base64 --decode > "\$p12"\n)/) do |decode|
      decode + %q(          chmod 600 "$p12") + "\n"
    end
  ' "$late_chmod_developer_id_release"
  grep -Fq 'chmod 600 "$p12"' "$late_chmod_developer_id_release"
  if check_developer_id_fixture "$late_chmod_developer_id_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: P12 permissions applied only after decode were accepted" >&2
    cleanup
    return 1
  fi

  cargo_in_signing_job_release="$tmp_dir/cargo-in-signing-job-release.yml"
  cat >"$cargo_in_signing_job_release" <<'YAML'
jobs:
  prebuild:
    steps:
      - name: Prebuild release binary (no signing secrets in this job)
        run: cargo build --locked --release -p app
  release:
    steps:
      - name: Import Developer ID certificate
        env:
          P12_BASE64: ${{ secrets.COMPME_DEVELOPER_ID_P12_BASE64 }}
          P12_PASSWORD: ${{ secrets.COMPME_DEVELOPER_ID_P12_PASSWORD }}
          SIGNING_IDENTITY: ${{ secrets.COMPME_CODESIGN_IDENTITY }}
        run: |
          for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY; do
            if [ -z "${!name:-}" ]; then
              echo "missing required release secret: $name" >&2
              exit 1
            fi
          done
          echo "COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY" >> "$GITHUB_ENV"
      - name: Rebuild release binary
        run: cargo build --locked --release -p app
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  if check_developer_id_fixture "$cargo_in_signing_job_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: cargo inside the signing job was accepted" >&2
    cleanup
    return 1
  fi

  secret_in_prebuild_release="$tmp_dir/secret-in-prebuild-release.yml"
  cat >"$secret_in_prebuild_release" <<'YAML'
jobs:
  prebuild:
    steps:
      - name: Prebuild release binary (no signing secrets in this job)
        env:
          LEAK: ${{ secrets.COMPME_NOTARYTOOL_PASSWORD }}
        run: cargo build --locked --release -p app
  release:
    steps:
      - name: Import Developer ID certificate
        env:
          P12_BASE64: ${{ secrets.COMPME_DEVELOPER_ID_P12_BASE64 }}
          P12_PASSWORD: ${{ secrets.COMPME_DEVELOPER_ID_P12_PASSWORD }}
          SIGNING_IDENTITY: ${{ secrets.COMPME_CODESIGN_IDENTITY }}
        run: |
          for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY; do
            if [ -z "${!name:-}" ]; then
              echo "missing required release secret: $name" >&2
              exit 1
            fi
          done
          echo "COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY" >> "$GITHUB_ENV"
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  if check_developer_id_fixture "$secret_in_prebuild_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: secret reference in prebuild job was accepted" >&2
    cleanup
    return 1
  fi

  missing_identity_export_release="$tmp_dir/missing-identity-export-release.yml"
  cat >"$missing_identity_export_release" <<'YAML'
jobs:
  prebuild:
    steps:
      - name: Prebuild release binary (no signing secrets in this job)
        run: cargo build --locked --release -p app
  release:
    steps:
      - name: Import Developer ID certificate
        env:
          P12_BASE64: ${{ secrets.COMPME_DEVELOPER_ID_P12_BASE64 }}
          P12_PASSWORD: ${{ secrets.COMPME_DEVELOPER_ID_P12_PASSWORD }}
          SIGNING_IDENTITY: ${{ secrets.COMPME_CODESIGN_IDENTITY }}
        run: |
          for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY; do
            if [ -z "${!name:-}" ]; then
              echo "missing required release secret: $name" >&2
              exit 1
            fi
          done
          echo "COMPME_SIGNING_KEYCHAIN=$keychain" >> "$GITHUB_ENV"
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  if check_developer_id_fixture "$missing_identity_export_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing Developer ID identity export was accepted" >&2
    cleanup
    return 1
  fi

  good_hardened_release="$tmp_dir/good-hardened-release.yml"
  cat >"$good_hardened_release" <<'YAML'
permissions:
  contents: read
jobs:
  validate:
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
  windows:
    runs-on: windows-latest
    steps:
      - uses: dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30
  linux:
    runs-on: ubuntu-latest
    steps:
      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32
  prebuild:
    steps: []
  release:
    needs: [validate, windows, linux]
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
YAML
  check_release_hardening_fixture "$good_hardened_release"

  mutable_action_release="$tmp_dir/mutable-action-release.yml"
  cat >"$mutable_action_release" <<'YAML'
permissions:
  contents: read
jobs:
  validate:
    steps: []
  windows:
    runs-on: windows-latest
    steps: []
  linux:
    runs-on: ubuntu-latest
    steps: []
  release:
    needs: [validate, windows, linux]
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
YAML
  if check_release_hardening_fixture "$mutable_action_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: mutable release action ref was accepted" >&2
    cleanup
    return 1
  fi

  mutable_prereq_action_release="$tmp_dir/mutable-prereq-action-release.yml"
  cat >"$mutable_prereq_action_release" <<'YAML'
permissions:
  contents: read
jobs:
  validate:
    steps:
      - uses: actions/checkout@v4
  windows:
    runs-on: windows-latest
    steps: []
  linux:
    runs-on: ubuntu-latest
    steps: []
  release:
    needs: [validate, windows, linux]
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
YAML
  if check_release_hardening_fixture "$mutable_prereq_action_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: mutable prerequisite action ref was accepted" >&2
    cleanup
    return 1
  fi

  attacker_action_release="$tmp_dir/attacker-action-release.yml"
  cp "$good_hardened_release" "$attacker_action_release"
  ruby -0pi -e 'sub(%r{actions/checkout@[0-9a-f]{40}}, "attacker/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5")' "$attacker_action_release"
  if check_release_hardening_fixture "$attacker_action_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: attacker-owned pinned action was accepted" >&2
    cleanup
    return 1
  fi

  reusable_workflow_release="$tmp_dir/reusable-workflow-release.yml"
  cp "$good_hardened_release" "$reusable_workflow_release"
  ruby -0pi -e 'sub(/jobs:\n/, "jobs:\n  attacker_job:\n    uses: attacker/repo/.github/workflows/release.yml@34e114876b0b11c390a56381ad16ebd13914f8d5\n")' "$reusable_workflow_release"
  if check_release_hardening_fixture "$reusable_workflow_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: job-level attacker reusable workflow was accepted" >&2
    cleanup
    return 1
  fi

  checkout_override_release="$tmp_dir/checkout-override-release.yml"
  cp "$good_hardened_release" "$checkout_override_release"
  ruby -0pi -e 'sub(/(          fetch-depth: 0\n)/, "\\1          repository: attacker/repo\n")' "$checkout_override_release"
  if check_release_hardening_fixture "$checkout_override_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: checkout repository override was accepted" >&2
    cleanup
    return 1
  fi

  artifact_override_release="$tmp_dir/artifact-override-release.yml"
  cp "$good_hardened_release" "$artifact_override_release"
  ruby -0pi -e 'sub(/(  validate:\n    steps:\n)/, "\\1      - uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093\\n        with:\n          name: compme-prebuilt-binary\\n          path: target/release\\n          run-id: 1234\\n")' "$artifact_override_release"
  if check_release_hardening_fixture "$artifact_override_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: cross-run artifact provenance override was accepted" >&2
    cleanup
    return 1
  fi

  token_in_prebuild_release="$tmp_dir/token-in-prebuild-release.yml"
  cp "$good_hardened_release" "$token_in_prebuild_release"
  ruby -0pi -e 'sub(/(  prebuild:\n)/, "\\1    env:\n      GH_TOKEN: ${{ github.token }}\\n")' "$token_in_prebuild_release"
  if check_release_hardening_fixture "$token_in_prebuild_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: explicit GitHub token in secretless prebuild was accepted" >&2
    cleanup
    return 1
  fi

  inherited_secrets_prebuild_release="$tmp_dir/inherited-secrets-prebuild-release.yml"
  cp "$good_hardened_release" "$inherited_secrets_prebuild_release"
  ruby -0pi -e 'sub(/(  prebuild:\n)/, "\\1    secrets: inherit\\n")' "$inherited_secrets_prebuild_release"
  if check_release_hardening_fixture "$inherited_secrets_prebuild_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: inherited secrets in secretless prebuild were accepted" >&2
    cleanup
    return 1
  fi

  shallow_release="$tmp_dir/shallow-release.yml"
  cat >"$shallow_release" <<'YAML'
permissions:
  contents: read
jobs:
  validate:
    steps: []
  windows:
    runs-on: windows-latest
    steps: []
  linux:
    runs-on: ubuntu-latest
    steps: []
  release:
    needs: [validate, windows, linux]
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
YAML
  if check_release_hardening_fixture "$shallow_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: shallow release checkout was accepted" >&2
    cleanup
    return 1
  fi

  broad_write_release="$tmp_dir/broad-write-release.yml"
  cat >"$broad_write_release" <<'YAML'
permissions:
  contents: write
jobs:
  validate:
    steps: []
  windows:
    runs-on: windows-latest
    steps: []
  linux:
    runs-on: ubuntu-latest
    steps: []
  release:
    needs: [validate, windows, linux]
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
YAML
  if check_release_hardening_fixture "$broad_write_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: broad workflow write permission was accepted" >&2
    cleanup
    return 1
  fi

  missing_matrix_release="$tmp_dir/missing-matrix-release.yml"
  cat >"$missing_matrix_release" <<'YAML'
permissions:
  contents: read
jobs:
  validate:
    steps: []
  release:
    needs: validate
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
YAML
  if check_release_hardening_fixture "$missing_matrix_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing release Windows/Linux jobs were accepted" >&2
    cleanup
    return 1
  fi

  check_split_artifact_fixture() {
    ruby -ryaml - "$1" <<'RUBY'
def active_shell_lines(run)
  run.lines.map do |line|
    stripped = line.strip
    next if stripped.empty? || stripped.start_with?("#")
    stripped.sub(/[[:space:]]+#.*$/, "")
  end.compact
end

workflow = YAML.load_file(ARGV.fetch(0))
trigger = workflow["on"] || workflow[true]
abort("missing release gate: release workflow push trigger is limited to v* tags") unless trigger.fetch("push").fetch("tags") == ["v*"]
jobs = workflow.fetch("jobs")
build = jobs.fetch("build_release")
publish = jobs.fetch("publish_release")
quote = 39.chr
tag_guard = "${{ github.ref_type == #{quote}tag#{quote} && startsWith(github.ref_name, #{quote}v#{quote}) }}"
abort("missing release gate: build_release is limited to v* tag refs") unless build.fetch("if") == tag_guard
abort("missing release gate: publish_release is limited to v* tag refs") unless publish.fetch("if") == tag_guard
abort("missing release gate: build_release uses protected release environment") unless build.fetch("environment") == "release"
abort("missing release gate: publish_release uses protected release environment") unless publish.fetch("environment") == "release"
build_steps = build.fetch("steps")
publish_steps = publish.fetch("steps")
publish_checkout = publish_steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
abort("missing release gate: publish_release checkout fetches full history") unless publish_checkout&.fetch("with")&.fetch("fetch-depth") == 0
build_index = build_steps.index { |step| step["name"] == "Build the .app bundle" }
notarize_index = build_steps.index { |step| step["name"] == "Notarize and staple the .app" }
cleanup_index = build_steps.index { |step| step["name"] == "Delete signing keychain" }
package_index = build_steps.index { |step| step["name"] == "Package + checksum" }
manifest_index = build_steps.index { |step| step["name"] == "Write update manifest" }
upload_index = build_steps.index { |step| step["name"] == "Upload release artifacts" }
abort("missing release gate: release artifact chain is build -> notarize -> cleanup -> package -> manifest -> upload") unless build_index && notarize_index && cleanup_index && package_index && manifest_index && upload_index && build_index < notarize_index && notarize_index < cleanup_index && cleanup_index < package_index && package_index < manifest_index && manifest_index < upload_index
cleanup_step = build_steps.fetch(cleanup_index)
abort("missing release gate: signing keychain cleanup runs always") unless cleanup_step.fetch("if") == "always()"
cleanup_run = cleanup_step.fetch("run")
abort("missing release gate: signing keychain cleanup deletes keychain") unless cleanup_run.include?("security delete-keychain \"$COMPME_SIGNING_KEYCHAIN\"")
package_run = build_steps.fetch(package_index).fetch("run")
["ditto -c -k --keepParent", "shasum -a 256", "echo \"version=$version\"", "echo \"zip=$zip\"", "echo \"sha256=$sha\""].each do |needle|
  abort("missing release gate: package step #{needle}") unless package_run.include?(needle)
end
manifest_step = build_steps.fetch(manifest_index)
manifest_env = manifest_step.fetch("env")
abort("missing release gate: manifest consumes package output VERSION") unless manifest_env.fetch("VERSION") == "${{ steps.pkg.outputs.version }}"
abort("missing release gate: manifest consumes package output ZIP") unless manifest_env.fetch("ZIP") == "${{ steps.pkg.outputs.zip }}"
abort("missing release gate: manifest consumes package output SHA256") unless manifest_env.fetch("SHA256") == "${{ steps.pkg.outputs.sha256 }}"
manifest_run = manifest_step.fetch("run")
abort("missing release gate: manifest writes update manifest") unless manifest_run.include?("tools/release/write-update-manifest.sh \"$VERSION\" \"$ZIP\" \"$SHA256\" > \"$manifest\"")
abort("missing release gate: manifest emits manifest output") unless manifest_run.include?("echo \"manifest=$manifest\" >> \"$GITHUB_OUTPUT\"")
upload_path = build_steps.fetch(upload_index).fetch("with").fetch("path").to_s
abort("missing release gate: upload includes manifest output") unless upload_path.include?("${{ steps.manifest.outputs.manifest }}")
download_index = publish_steps.index { |step| step["name"] == "Download release artifacts" }
checksum_index = publish_steps.index { |step| step["name"] == "Verify downloaded artifact checksum" }
publish_index = publish_steps.index { |step| step["name"] == "Create draft GitHub release" }
cask_index = publish_steps.index { |step| step["name"] == "Finalize Homebrew cask" }
abort("missing release gate: verifies downloaded artifact checksum before publishing release") unless download_index && checksum_index && publish_index && download_index < checksum_index && checksum_index < publish_index
abort("missing release gate: finalizes Homebrew cask after publishing release") unless cask_index && publish_index < cask_index
checksum_run = active_shell_lines(publish_steps.fetch(checksum_index).fetch("run"))
["cd release-artifacts", "test -f \"$ZIP\"", "test -f \"$ZIP.sha256\"", "shasum -a 256 -c \"$ZIP.sha256\""].each do |needle|
  abort("missing release gate: verifies downloaded artifact checksum #{needle}") unless checksum_run.include?(needle)
end
publish_files = publish_steps.fetch(publish_index).fetch("with").fetch("files").to_s
["release-artifacts/compme-*-macos.zip", "release-artifacts/compme-*-macos.zip.sha256", "release-artifacts/compme-*-update.json"].each do |needle|
  abort("missing release gate: publishes downloaded artifact #{needle}") unless publish_files.include?(needle)
end
cask_lines = active_shell_lines(publish_steps.fetch(cask_index).fetch("run"))
abort("missing release gate: derives cask ZIP from release version") unless cask_lines.include?("ZIP=\"compme-${VERSION}-macos.zip\"")
abort("missing release gate: finalizes cask from downloaded release artifact") unless cask_lines.include?("artifact_path=\"$PWD/release-artifacts/$ZIP\"")
RUBY
  }

  good_split_release="$tmp_dir/good-split-release.yml"
  cat >"$good_split_release" <<'YAML'
on:
  push:
    tags: ["v*"]
jobs:
  build_release:
    if: ${{ github.ref_type == 'tag' && startsWith(github.ref_name, 'v') }}
    environment: release
    steps:
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
      - name: Notarize and staple the .app
        run: tools/release/notarize-app.sh "$RUNNER_TEMP/bundle/Compme.app"
      - name: Delete signing keychain
        if: always()
        run: security delete-keychain "$COMPME_SIGNING_KEYCHAIN"
      - name: Package + checksum
        run: |
          version="${GITHUB_REF_NAME#v}"
          zip="compme-${version}-macos.zip"
          ditto -c -k --keepParent "$RUNNER_TEMP/bundle/Compme.app" "$zip"
          sha="$(shasum -a 256 "$zip" | awk '{print $1}')"
          echo "version=$version" >> "$GITHUB_OUTPUT"
          echo "zip=$zip" >> "$GITHUB_OUTPUT"
          echo "sha256=$sha" >> "$GITHUB_OUTPUT"
      - name: Write update manifest
        env:
          VERSION: ${{ steps.pkg.outputs.version }}
          ZIP: ${{ steps.pkg.outputs.zip }}
          SHA256: ${{ steps.pkg.outputs.sha256 }}
        run: |
          manifest="compme-${VERSION}-update.json"
          tools/release/write-update-manifest.sh "$VERSION" "$ZIP" "$SHA256" > "$manifest"
          echo "manifest=$manifest" >> "$GITHUB_OUTPUT"
      - name: Upload release artifacts
        with:
          path: |
            ${{ steps.pkg.outputs.zip }}
            ${{ steps.pkg.outputs.zip }}.sha256
            ${{ steps.manifest.outputs.manifest }}
  publish_release:
    if: ${{ github.ref_type == 'tag' && startsWith(github.ref_name, 'v') }}
    environment: release
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
      - name: Download release artifacts
        with:
          path: release-artifacts
      - name: Verify downloaded artifact checksum
        run: |
          VERSION="${GITHUB_REF_NAME#v}"
          ZIP="compme-${VERSION}-macos.zip"
          cd release-artifacts
          test -f "$ZIP"
          test -f "$ZIP.sha256"
          shasum -a 256 -c "$ZIP.sha256"
      - name: Create draft GitHub release
        with:
          files: |
            release-artifacts/compme-*-macos.zip
            release-artifacts/compme-*-macos.zip.sha256
            release-artifacts/compme-*-update.json
      - name: Finalize Homebrew cask
        run: |
          ZIP="compme-${VERSION}-macos.zip"
          artifact_path="$PWD/release-artifacts/$ZIP"
          tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"
YAML
  check_split_artifact_fixture "$good_split_release"

  bad_split="$tmp_dir/bad-split.yml"
  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/\n      - name: Notarize and staple the \.app\n        run: tools\/release\/notarize-app\.sh "\$RUNNER_TEMP\/bundle\/Compme\.app"\n/, "\n")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing split notarization was accepted" >&2
    cleanup
    return 1
  fi

  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/VERSION: \$\{\{ steps\.pkg\.outputs\.version \}\}/, "VERSION: 9.9.9")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: manifest env drift was accepted" >&2
    cleanup
    return 1
  fi

  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/\n            \$\{\{ steps\.manifest\.outputs\.manifest \}\}/, "")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: upload missing manifest was accepted" >&2
    cleanup
    return 1
  fi

  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/\n      - name: Verify downloaded artifact checksum\n        run: \|\n          VERSION="\$\{GITHUB_REF_NAME#v\}"\n          ZIP="compme-\$\{VERSION\}-macos\.zip"\n          cd release-artifacts\n          test -f "\$ZIP"\n          test -f "\$ZIP\.sha256"\n          shasum -a 256 -c "\$ZIP\.sha256"\n/, "\n")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing downloaded artifact checksum verification was accepted" >&2
    cleanup
    return 1
  fi

  cat >"$bad_split" <<'YAML'
on:
  push:
    tags: ["v*"]
jobs:
  build_release:
    if: ${{ github.ref_type == 'tag' && startsWith(github.ref_name, 'v') }}
    environment: release
    steps:
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
      - name: Notarize and staple the .app
        run: tools/release/notarize-app.sh "$RUNNER_TEMP/bundle/Compme.app"
      - name: Delete signing keychain
        if: always()
        run: security delete-keychain "$COMPME_SIGNING_KEYCHAIN"
      - name: Package + checksum
        run: |
          version="${GITHUB_REF_NAME#v}"
          zip="compme-${version}-macos.zip"
          ditto -c -k --keepParent "$RUNNER_TEMP/bundle/Compme.app" "$zip"
          sha="$(shasum -a 256 "$zip" | awk '{print $1}')"
          echo "version=$version" >> "$GITHUB_OUTPUT"
          echo "zip=$zip" >> "$GITHUB_OUTPUT"
          echo "sha256=$sha" >> "$GITHUB_OUTPUT"
      - name: Write update manifest
        env:
          VERSION: ${{ steps.pkg.outputs.version }}
          ZIP: ${{ steps.pkg.outputs.zip }}
          SHA256: ${{ steps.pkg.outputs.sha256 }}
        run: |
          manifest="compme-${VERSION}-update.json"
          tools/release/write-update-manifest.sh "$VERSION" "$ZIP" "$SHA256" > "$manifest"
          echo "manifest=$manifest" >> "$GITHUB_OUTPUT"
      - name: Upload release artifacts
        with:
          path: |
            ${{ steps.pkg.outputs.zip }}
            ${{ steps.pkg.outputs.zip }}.sha256
            ${{ steps.manifest.outputs.manifest }}
  publish_release:
    if: ${{ github.ref_type == 'tag' && startsWith(github.ref_name, 'v') }}
    environment: release
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
      - name: Download release artifacts
        with:
          path: release-artifacts
      - name: Verify downloaded artifact checksum
        run: |
          VERSION="${GITHUB_REF_NAME#v}"
          ZIP="compme-${VERSION}-macos.zip"
          # cd release-artifacts
          # test -f "$ZIP"
          # test -f "$ZIP.sha256"
          echo 'shasum -a 256 -c "$ZIP.sha256"'
      - name: Create draft GitHub release
        with:
          files: |
            release-artifacts/compme-*-macos.zip
            release-artifacts/compme-*-macos.zip.sha256
            release-artifacts/compme-*-update.json
      - name: Finalize Homebrew cask
        run: |
          ZIP="compme-${VERSION}-macos.zip"
          artifact_path="$PWD/release-artifacts/$ZIP"
          tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"
YAML
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: commented/echoed checksum verification was accepted" >&2
    cleanup
    return 1
  fi

  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/release-artifacts\/compme-\*-macos\.zip\.sha256/, "release-artifacts/wrong.sha256")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: wrong publish artifact files were accepted" >&2
    cleanup
    return 1
  fi

  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/artifact_path="\$PWD\/release-artifacts\/\$ZIP"/, "artifact_path=\"$PWD/$ZIP\"")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: wrong cask artifact path was accepted" >&2
    cleanup
    return 1
  fi

  cp "$good_split_release" "$bad_split"
  ruby -0pi -e 'sub(/fetch-depth: 0/, "fetch-depth: 1")' "$bad_split"
  if check_split_artifact_fixture "$bad_split" >/dev/null 2>&1; then
    echo "release gate self-test failed: shallow publish checkout was accepted" >&2
    cleanup
    return 1
  fi

  # The current workflow is the public policy seam for coordinated release
  # integrity controls. Each mutation below preserves valid YAML while removing
  # or weakening one security property; the checker must reject every variant.
  check_ci_integrity_controls "$ci_workflow"
  ci_integrity_fixture="$tmp_dir/ci-integrity.yml"

  cp "$ci_workflow" "$ci_integrity_fixture"
  ruby -0pi -e 'sub(/cargo-audit --version 0\.22\.2 --locked/, "cargo-audit --version 0.22.1 --locked")' "$ci_integrity_fixture"
  if check_ci_integrity_controls "$ci_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: changed CI cargo-audit version was accepted" >&2
    cleanup
    return 1
  fi

  cp "$ci_workflow" "$ci_integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("check")["timeout-minutes"] = 91
    File.write(path, YAML.dump(workflow))
  ' "$ci_integrity_fixture"
  if check_ci_integrity_controls "$ci_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: changed CI timeout was accepted" >&2
    cleanup
    return 1
  fi

  cp "$ci_workflow" "$ci_integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("check")["permissions"] = {"contents" => "read", "checks" => "write"}
    File.write(path, YAML.dump(workflow))
  ' "$ci_integrity_fixture"
  if check_ci_integrity_controls "$ci_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: unnecessary CI checks permission was accepted" >&2
    cleanup
    return 1
  fi

  cp "$ci_workflow" "$ci_integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("check").fetch("steps")
    steps.reject! { |step| step["name"] == "Bundle icon generator self-test" }
    File.write(path, YAML.dump(workflow))
  ' "$ci_integrity_fixture"
  if check_ci_integrity_controls "$ci_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing CI icon generator self-test was accepted" >&2
    cleanup
    return 1
  fi

  check_audit_integrity_controls "$audit_workflow"
  audit_integrity_fixture="$tmp_dir/audit-integrity.yml"

  cp "$audit_workflow" "$audit_integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch(true).delete("schedule")
    File.write(path, YAML.dump(workflow))
  ' "$audit_integrity_fixture"
  if check_audit_integrity_controls "$audit_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: dependency audit without a schedule was accepted" >&2
    cleanup
    return 1
  fi

  cp "$audit_workflow" "$audit_integrity_fixture"
  ruby -0pi -e 'sub(/cargo-audit --version 0\.22\.2 --locked/, "cargo-audit")' "$audit_integrity_fixture"
  if check_audit_integrity_controls "$audit_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: unpinned scheduled cargo-audit was accepted" >&2
    cleanup
    return 1
  fi

  cp "$audit_workflow" "$audit_integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("permissions")["contents"] = "write"
    File.write(path, YAML.dump(workflow))
  ' "$audit_integrity_fixture"
  if check_audit_integrity_controls "$audit_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: scheduled audit with write permission was accepted" >&2
    cleanup
    return 1
  fi

  cp "$audit_workflow" "$audit_integrity_fixture"
  ruby -0pi -e 'sub(%q(actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0), %q(actions/checkout@v7))' "$audit_integrity_fixture"
  if check_audit_integrity_controls "$audit_integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: mutable scheduled-audit checkout action was accepted" >&2
    cleanup
    return 1
  fi

  check_release_integrity_controls "$canonical_release_workflow"
  integrity_fixture="$tmp_dir/release-integrity.yml"

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("build_release")["timeout-minutes"] = 60
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shortened signing/notary timeout was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("build_release").fetch("steps")
    import_index = steps.index { |step| step["name"] == "Import Developer ID certificate" }
    steps.insert(import_index + 1, {"name" => "Injected signing step", "run" => "codesign --version"})
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: injected signing-job shell step was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    step = workflow.fetch("jobs").fetch("publish_release").fetch("steps").find { |candidate| candidate["name"] == "Write publication-time update manifest" }
    step["run"] = "awk() { printf shadowed; }\n" + step.fetch("run")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shadowed publication-manifest awk was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("windows").fetch("steps")
    steps.find { |step| step["name"] == "Test portable workspace" }["run"] = "cargo test --locked -p platform_windows"
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: narrowed release portability test was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("build_release").fetch("steps")
    steps.reject! { |step| step["name"] == "Verify packaged app signature and notarization" }
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing packaged-app reassessment was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(codesign --verify --deep --strict --verbose=2 "$app"), %q(codesign --verify "$app"))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: weakened packaged-app signature reassessment was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("build_release").fetch("steps")
    step = steps.find { |candidate| candidate["name"] == "Verify packaged app signature and notarization" }
    step["run"] = "codesign() { :; }\nxcrun() { :; }\nspctl() { :; }\n" + step.fetch("run")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shadowed packaged-app assessment commands were accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(gh release delete "$GITHUB_REF_NAME" --yes), %q(echo stale-draft))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: stale draft without deletion was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("publish_release").fetch("steps")
    step = steps.find { |candidate| candidate["name"] == "Revalidate default-branch HEAD and undraft GitHub release" }
    step["run"] = "gh() { :; }\n" + step.fetch("run")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shadowed late-undraft gh command was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(tools/release/validate-version.sh "$version"), %q(echo "$version"))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing shared version validation was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(          tools/release/validate-version.sh "$version"), %q(          if false; then\n            tools/release/validate-version.sh "$version"\n          fi))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shared version validation retained only in a dead branch was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(          tools/release/validate-version.sh "$version"), %q(          exit 0
          tools/release/validate-version.sh "$version"))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shared version validation after unconditional success was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(          tools/release/validate-version.sh "$version"), %q(          exit 0 # success
          tools/release/validate-version.sh "$version"))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shared version validation after commented unconditional success was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(          tools/release/validate-version.sh "$version"), %q(          never_called() {
            tools/release/validate-version.sh "$version"
          }))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shared version validation in an unused function was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(          tools/release/validate-version.sh "$version"), %q(          never_called()
          {
            tools/release/validate-version.sh "$version"
          }))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: shared version validation in a split-style unused function was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(if [ "$GITHUB_SHA" != "$default_sha" ]; then), %q(if false; then))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: non-exact preflight default HEAD check was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -e '
    path = ARGV.fetch(0)
    text = File.read(path)
    needle = %q(if [ "$GITHUB_SHA" != "$default_sha" ]; then)
    first = text.index(needle)
    second = first && text.index(needle, first + needle.length)
    abort "missing second exact-HEAD fixture" unless second
    text[second, needle.length] = "if false; then"
    File.write(path, text)
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: non-exact prebuild default HEAD check was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(if [ "$archs" != "arm64" ]; then), %q(if [ -z "$archs" ]; then))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: weakened prebuild architecture check was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("build_release").fetch("steps")
    arch = steps.index { |step| step["name"] == "Verify downloaded binary is arm64 only" }
    import = steps.index { |step| step["name"] == "Import Developer ID certificate" }
    steps.insert(import + 1, steps.delete_at(arch))
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: post-secret architecture verification was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("build_release").fetch("steps")
    steps.reject! { |step| step["name"] == "Register signing keychain cleanup path" }
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing pre-import keychain registration was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(security delete-keychain "$keychain"; then), %q(security delete-keychain "$keychain" || true; then))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: fail-open signing keychain deletion was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(keychain="${COMPME_SIGNING_KEYCHAIN:-$RUNNER_TEMP/compme-signing.keychain-db}"), %q(keychain="${COMPME_SIGNING_KEYCHAIN:-}"))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cleanup without deterministic keychain fallback was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(if [ -e "$keychain" ]; then), %q(if false; then))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cleanup without keychain absence verification was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("publish_release").fetch("steps")
    steps.reject! { |step| step["name"] == "Verify release tag is still at default-branch HEAD before publication" }
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing pre-publication exact-HEAD check was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q( --verify-tag), %q())' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: draft creation without tag verification was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q("release-artifacts/$ZIP.sha256"), %q("release-artifacts/wrong.sha256"))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: draft creation with wrong checksum asset was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("finalize_cask").delete("environment")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: unprotected separate cask finalizer job was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("finalize_cask").fetch("steps")
    steps.find { |step| step["name"] == "Finalize Homebrew cask" }.fetch("env").delete("GH_TOKEN")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: unauthenticated published-checksum download was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("finalize_cask")["needs"] = ["build_release"]
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask finalizer not serialized after publication was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("prebuild").fetch("permissions")["id-token"] = "write"
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: prebuild OIDC write permission was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("publish_release").fetch("steps")
    steps.find { |step| step["name"] == "Verify release tag is still at default-branch HEAD before publication" }.delete("env")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: pre-publication HEAD check without default branch env was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    steps = workflow.fetch("jobs").fetch("publish_release").fetch("steps")
    create = steps.find { |step| step["name"] == "Create draft GitHub release" }
    create["run"] = "gh release upload \"$GITHUB_REF_NAME\" stale.zip --clobber\n" + create.fetch("run")
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: extra clobbering release command was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -0pi -e 'sub(%q(tools/release/validate-version.sh "$version"), %q(: '\''tools/release/validate-version.sh "$version"'\''))' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: no-op wrapped version validation was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs").fetch("preflight").fetch("steps").first.fetch("with")["fetch-depth"] = 1
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: wrong allowed checkout input value was accepted" >&2
    cleanup
    return 1
  fi

  cp "$canonical_release_workflow" "$integrity_fixture"
  ruby -ryaml -e '
    path = ARGV.fetch(0)
    workflow = YAML.load_file(path)
    workflow.fetch("jobs")["extra_approved_action_job"] = {"runs-on" => "ubuntu-latest", "steps" => [{"uses" => "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"}]}
    File.write(path, YAML.dump(workflow))
  ' "$integrity_fixture"
  if check_release_integrity_controls "$integrity_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: extra approved-action job was accepted" >&2
    cleanup
    return 1
  fi

  finalizer_fixture="$tmp_dir/finalize-cask.sh"

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(    echo "failed to fetch release tag $tag and origin/$default_branch" >&2), %q(    # echo "failed to fetch release tag $tag and origin/$default_branch" >&2))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: commented cask finalizer fetch diagnostic was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(git fetch --no-tags origin), %q(git fetch origin))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask finalizer with implicit tag fetching was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q("+refs/heads/$default_branch:$remote_branch_ref"), %q("$default_branch"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: FETCH_HEAD-only cask finalizer branch fetch was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q("+refs/tags/$tag:$verified_tag_ref"), %q("refs/tags/$tag:refs/tags/$tag"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask finalizer local-tag overwrite was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(/^  if ! release_ineligible=.*$/, "  if ! release_ineligible=false; then")' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask finalizer without stable published-state check was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(/^  verify_published_artifact .*$/, "  echo skipped")' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask finalizer without published checksum verification was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(if ! command gh release download "$tag" \\), %q(if ! echo gh release download "$tag" \\))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: inert published-checksum download was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(if [ "$local_sha" != "$published_sha" ]; then), %q(if false; then))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: disabled published-checksum comparison was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(    "$frozen_updater" "$tag"), %q(    tools/release/update-cask.sh "$tag"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: moving-default-branch cask updater was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  freeze_release_helpers "$frozen_root" "$tag_sha"), %q(  # freeze_release_helpers "$frozen_root" "$tag_sha"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: commented frozen-helper invocation was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  freeze_release_helpers "$frozen_root" "$tag_sha"), %q(  return 0
  freeze_release_helpers "$frozen_root" "$tag_sha"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: frozen-helper path after early return was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  freeze_release_helpers "$frozen_root" "$tag_sha"), ""); sub(%q(  git pull --ff-only --no-tags origin "$default_branch"), %q(  git pull --ff-only --no-tags origin "$default_branch"
  freeze_release_helpers "$frozen_root" "$tag_sha"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: helper freeze after mutable branch pull was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(git -C "$repo_root" show "$tag_sha:tools/release/$helper" >"$destination"), %q(cp "$repo_root/tools/release/$helper" "$destination"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: working-tree helper copy was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  freeze_release_helpers "$frozen_root" "$tag_sha"), ""); sub(%q(  if [ "$tag_sha" != "$GITHUB_SHA" ]; then), %q(  freeze_release_helpers "$frozen_root" "$tag_sha"
  if [ "$tag_sha" != "$GITHUB_SHA" ]; then))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: helper freeze before tag SHA verification was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  freeze_release_helpers "$frozen_root" "$tag_sha"), %q(  freeze_release_helpers "$frozen_root" HEAD))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: helper freeze from HEAD was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  expected_sha="$(shasum -a 256 "$artifact_path" | awk '\''{print $1}'\'')"), %q(  expected_sha="fixed"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask SHA detached from artifact was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(  validate_finalized_cask "$cask_path" "$version" "$artifact_path"), ""); sub(%q(  git push origin "HEAD:$default_branch"), %q(  git push origin "HEAD:$default_branch"
  validate_finalized_cask "$cask_path" "$version" "$artifact_path"))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: post-push finalized-cask validation was accepted" >&2
    cleanup
    return 1
  fi

  cp "$finalize_cask_script" "$finalizer_fixture"
  ruby -0pi -e 'sub(%q(
finalize_cask "$@"
), %q(
cat <<SH
finalize_cask "$@"
SH
echo "dispatch omitted"
))' "$finalizer_fixture"
  if check_finalizer_helper_contract "$finalizer_fixture" >/dev/null 2>&1; then
    echo "release gate self-test failed: cask finalizer dispatch retained only in a heredoc was accepted" >&2
    cleanup
    return 1
  fi

  cleanup
}

if [[ "${1:-}" == "--self-test" ]]; then
  if [ "$#" -ne 1 ]; then
    echo "usage: $0 [--self-test] [release-workflow.yml]" >&2
    exit 2
  fi
  run_self_test
  echo "Self-test passed"
  exit 0
fi
if [ "$#" -gt 1 ]; then
  echo "usage: $0 [--self-test] [release-workflow.yml]" >&2
  exit 2
fi

check_no_automated_a2_validation "$ci_workflow" "CI"
check_no_automated_a2_validation "$release_workflow" "release"
check_ci_integrity_controls "$ci_workflow"
check_audit_integrity_controls "$audit_workflow"
check_release_integrity_controls "$release_workflow"
check_all_self_test_env_contracts
check_manual_a2_summary "$readme_doc" "README"
check_manual_a2_summary "$development_doc" "DEVELOPMENT"
reject_line "$0" '^[[:space:]]*a2_matrix_ledger_script=' "release policy checker binds the manual A2 ledger tool"
reject_line "$0" '^[[:space:]]*bash -n "\$a2_matrix_ledger_script"' "release policy checker syntax-checks the manual A2 ledger tool"
reject_line "$0" '^[[:space:]]*"\$a2_matrix_ledger_script"' "release policy checker executes the manual A2 ledger tool"
reject_line "$0" '^[[:space:]]*(check_live_a2|require_live_a2)' "release policy checker validates A2 evidence"
reject_line "$0" '^[[:space:]]*(bash[[:space:]]+)?tools/(acceptance/run-a2-compat-gates|release/check-a2-matrix-ledger)\.sh' "release policy checker executes A2 tooling directly"
run_self_test >/dev/null

ruby -ryaml -e '
  def step?(steps, name, run)
    steps.any? do |step|
      step.is_a?(Hash) &&
        step["name"] == name &&
        step["run"] == run
    end
  end

  def require_step!(jobs, job, name, run, label)
    steps = jobs.fetch(job).fetch("steps")
    abort("missing release gate: #{label}") unless step?(steps, name, run)
  end

  def active_shell_lines(run)
    lines = []
    dead_depth = 0
    function_depth = 0
    pending_function = false
    terminated = false
    run.to_s.lines.each do |raw|
      stripped = raw.strip
      next if stripped.empty? || stripped.start_with?("#")
      normalized = stripped.sub(/[[:space:]]+#.*$/, "")
      next if terminated
      if function_depth.positive?
        function_depth += normalized.count("{") - normalized.count("}")
        function_depth = 0 unless function_depth.positive?
        next
      end
      if pending_function
        if normalized == "{"
          function_depth = 1
          pending_function = false
          next
        end
        pending_function = false
      end
      if normalized.match?(/\A(?:function[[:space:]]+[A-Za-z_][A-Za-z0-9_]*(?:[[:space:]]*\(\))?|[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\(\))[[:space:]]*\z/)
        pending_function = true
        next
      end
      if normalized.match?(/\A(?:function[[:space:]]+)?[A-Za-z_][A-Za-z0-9_]*[[:space:]]*(?:\(\))?[[:space:]]*\{/)
        function_depth = [normalized.count("{") - normalized.count("}"), 0].max
        next
      end
      if dead_depth.positive?
        dead_depth += 1 if normalized.match?(/\Aif[[:space:]]+/)
        dead_depth -= 1 if normalized == "fi" || normalized.start_with?("fi ")
        next
      end
      if normalized.match?(/\Aif[[:space:]]+false(?:[;[:space:]]|\z)/)
        dead_depth = 1
        next
      end
      lines << normalized
      terminated = true if normalized.match?(/\A(?:exit|return)[[:space:]]+0\z/)
    end
    lines
  end

  def require_active_finalizer_command!(run, needle)
    found = active_shell_lines(run).any? do |line|
      line.include?(needle) &&
        !line.start_with?("echo ") &&
        !line.start_with?("printf ")
    end
    abort("missing release gate: finalizes Homebrew cask command #{needle}") unless found
  end

  def contains_secret_reference?(value, parent_key = nil)
    case value
    when String
      value.include?("secrets.") || value.include?("github.token") ||
        (parent_key.to_s == "secrets" && value == "inherit")
    when Hash
      value.any? do |key, child|
        key_name = key.to_s
        credential_input = key_name != "persist-credentials" &&
          key_name.match?(/(?:^|[-_])(token|secret|password|credential|ssh-key)(?:s)?(?:$|[-_])/i)
        (credential_input && !child.to_s.empty?) || contains_secret_reference?(child, key_name)
      end
    when Array
      value.any? { |child| contains_secret_reference?(child, parent_key) }
    else
      false
    end
  end

  release_workflow = YAML.load_file(ARGV.fetch(0))
  ci_workflow = YAML.load_file(ARGV.fetch(1))

  def rust_toolchain_step_valid?(step)
    step["uses"].to_s.start_with?("dtolnay/rust-toolchain@") &&
      (step.fetch("with", {}).keys - ["components"]).empty?
  end

  jobs = ci_workflow.fetch("jobs")
  abort("missing release gate: CI workflow defaults to read-only contents permission") unless ci_workflow.fetch("permissions").fetch("contents") == "read"
  def approved_action_ref?(uses)
    [
      "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
      "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30",
      "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32",
      "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
      "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093",
      "actions/attest-build-provenance@0f67c3f4856b2e3261c31976d6725780e5e4c373",
    ].include?(uses)
  end
  def validate_action_inputs!(job_name, step)
    uses = step.fetch("uses")
    actual = step.fetch("with", {})
    approved = case uses.split("@", 2).first
               when "actions/checkout"
                 [{}, {"fetch-depth" => 0}, {"persist-credentials" => false}]
               when "dtolnay/rust-toolchain"
                 [
                   {},
                   {"components" => "rustfmt, clippy"},
                 ]
               when "Swatinem/rust-cache"
                 [
                   {},
                   {"workspaces" => "tools/spike"},
                   {"cache-directories" => "~/.cargo/advisory-db"},
                   {"cache-directories" => "tools/spike/models"},
                   {"workspaces" => ".\ntools/spike\n", "cache-directories" => "tools/spike/models"},
                   {"workspaces" => ".\ntools/spike\n", "cache-directories" => "~/.cargo/advisory-db"},
                 ]
               when "actions/upload-artifact"
                 [
                   {"name" => "compme-prebuilt-binary", "if-no-files-found" => "error", "retention-days" => 3, "path" => "target/release/compme"},
                   {"name" => "compme-release-artifacts", "if-no-files-found" => "error", "retention-days" => 7, "path" => "${{ steps.pkg.outputs.zip }}\n${{ steps.pkg.outputs.zip }}.sha256\n"},
                 ]
               when "actions/download-artifact"
                 [
                   {"name" => "compme-prebuilt-binary", "path" => "target/release"},
                   {"name" => "compme-release-artifacts", "path" => "release-artifacts"},
                 ]
               when "actions/attest-build-provenance"
                 [{"subject-path" => "${{ steps.pkg.outputs.zip }}"}]
               else
                 []
               end
    abort("missing release gate: #{job_name} action has exact approved provenance inputs") unless approved.include?(actual)
  end
  def validate_actions!(workflow, label)
    workflow.fetch("jobs").each do |job_name, job|
      abort("missing release gate: #{label} #{job_name} uses an unapproved reusable workflow") if job.key?("uses")
      Array(job["steps"]).each do |step|
        next unless step.is_a?(Hash) && step.key?("uses")
        abort("missing release gate: #{label} #{job_name} action uses an approved identity and commit SHA") unless approved_action_ref?(step["uses"])
        validate_action_inputs!("#{label} #{job_name}", step)
      end
    end
  end
  def require_exact_actions!(workflow, expected_by_job, label)
    jobs = workflow.fetch("jobs")
    abort("missing release gate: #{label} has the exact approved job topology") unless jobs.keys.sort == expected_by_job.keys.sort
    expected_by_job.each do |job_name, expected|
      actual = Array(jobs.fetch(job_name)["steps"]).map { |step| step["uses"] }.compact
      abort("missing release gate: #{label} #{job_name} has the exact approved action sequence") unless actual == expected
    end
  end
  validate_actions!(ci_workflow, "CI")
  ci_action_sequence = [
    "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
    "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30",
    "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32",
  ]
  require_exact_actions!(ci_workflow, {
    "actionlint" => ["actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"],
    "check" => ci_action_sequence,
    "spike" => ci_action_sequence,
    "windows" => ci_action_sequence,
    "linux" => ci_action_sequence,
  }, "CI")
  expected_ci_timeouts = {"actionlint" => 10, "check" => 90, "spike" => 60, "windows" => 60, "linux" => 60}
  expected_ci_timeouts.each do |job_name, timeout|
    abort("missing release gate: CI #{job_name} exact timeout") unless jobs.fetch(job_name).fetch("timeout-minutes") == timeout
  end
  abort("missing release gate: CI check inherits read-only workflow permissions") if jobs.fetch("check").key?("permissions")
  jobs.each do |job_name, job|
    next unless %w[check spike windows linux].include?(job_name)
    abort("missing release gate: CI #{job_name} pins Rust toolchain") unless Array(job["steps"]).any? { |step| step.is_a?(Hash) && rust_toolchain_step_valid?(step) }
  end
  ci_steps = jobs.fetch("check").fetch("steps")
  # Gate steps required verbatim in BOTH the CI check job and the release
  # validate job; per-workflow extras are merged in at each call site.
  shared_gate_steps = {
    "bundle metadata" => ["Bundle metadata", "tools/bundle/check-bundle-metadata.sh"],
    "bundle metadata self-test" => ["Bundle metadata self-test", "tools/bundle/check-bundle-metadata.sh --self-test"],
    "bundle assembler self-test" => ["Bundle assembler self-test", "tools/bundle/make-app.sh --self-test"],
    "bundle icon generator self-test" => ["Bundle icon generator self-test", "tools/bundle/make-icon.sh --self-test"],
    "bundle smoke" => ["Bundle smoke", "tools/bundle/bundle-smoke.sh"],
    "bundle smoke self-test" => ["Bundle smoke self-test", "tools/bundle/bundle-smoke.sh --self-test"],
    "E2E self-test" => ["E2E runner self-test", "tools/acceptance/e2e-complete-me.sh --self-test"],
    "missing-model startup self-test" => ["Missing-model startup self-test", "tools/acceptance/missing-model-startup.sh --self-test"],
    "missing-model startup product smoke" => ["Missing-model startup product smoke", "tools/acceptance/missing-model-startup.sh"],
    "UI-assisted session self-test" => ["UI-assisted session self-test", "tools/acceptance/run-ui-assisted-session.sh --self-test"],
    "A1b self-test" => ["A1b runner self-test", "tools/acceptance/run-a1b-live-gates.sh --self-test"],
    "model client feature policy" => ["Model client feature policy", "tools/release/check-model-client-features.sh"],
    "model client feature policy self-test" => ["Model client feature policy self-test", "tools/release/check-model-client-features.sh --self-test"],
    "agent brief alignment" => ["Agent brief alignment", "tools/release/check-agent-briefs.sh"],
    "agent brief alignment self-test" => ["Agent brief alignment self-test", "tools/release/check-agent-briefs.sh --self-test"],
    "privacy policy" => ["Privacy policy", "tools/release/check-privacy-policy.sh"],
    "privacy policy self-test" => ["Privacy policy self-test", "tools/release/check-privacy-policy.sh --self-test"],
    "GitHub governance checker self-test" => ["GitHub governance checker self-test", "tools/release/check-github-governance.sh --self-test"],
    "model gate policy" => ["Release model gate policy", "bash tools/release/check-model-gates.sh"],
    "model gate self-test" => ["Release model gate self-test", "tools/release/run-model-gates.sh --self-test"],
    "cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
    "cask finalizer" => ["Release cask finalizer self-test", "tools/release/finalize-cask.sh --self-test"],
    "notarization helper" => ["Notarization helper self-test", "tools/release/notarize-app.sh --self-test"],
    "update manifest" => ["Update manifest self-test", "tools/release/write-update-manifest.sh --self-test"],
    "version validator" => ["Release version validator self-test", "tools/release/validate-version.sh --self-test"],
    "quality gate self-test" => ["Quality gate self-test", "tools/release/check-quality.sh --self-test"],
    "version docs check self-test" => ["Version docs check self-test", "tools/release/check-version-docs.sh --self-test"],
  }
  {
    "CI root format" => ["Format", "cargo fmt --all -- --check"],
    "CI root clippy" => ["Clippy (deny warnings)", "cargo clippy --locked --workspace --all-targets -- -D warnings"],
    "CI root test" => ["Test", "cargo test --locked --workspace --all-targets -- --test-threads=1"],
    "CI root build" => ["Build", "cargo build --locked --workspace --all-targets"],
    "CI platform_macos examples build" => ["Build macOS acceptance examples", "cargo build --locked -p platform_macos --examples"],
    "CI rustdoc" => ["Rustdoc (deny warnings)", "RUSTDOCFLAGS=\"-D warnings\" cargo doc --no-deps --workspace"],
    "CI version docs check" => ["Version docs check", "tools/release/check-version-docs.sh"],
    "CI model smoke gate" => ["Model-backed smoke gate", "bash tools/release/run-model-gates.sh"],
  }.merge(shared_gate_steps.transform_keys { |key| "CI #{key}" }).each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(ci_steps, name, run)
  end
  smoke_step = ci_steps.find { |step| step["name"] == "Model-backed smoke gate" }
  abort("missing release gate: CI model smoke gate skips only the latency budget") unless
    smoke_step && smoke_step.fetch("env", {})["COMPME_REQUIRE_LATENCY_BUDGET"].to_s == "0"
  # The pinned dependency audit lives in the Linux job (platform-independent;
  # keeps the premium macOS lane off the cargo-audit compile).
  abort("missing release gate: CI pinned dependency audit") unless step?(
    jobs.fetch("linux").fetch("steps"),
    "Rust dependency audit",
    "cargo install cargo-audit --version 0.22.2 --locked\ncargo audit\n"
  )

  # CI windows/linux jobs gate the whole portable workspace (everything but
  # the Apple-only platform_macos crate) plus the app binary through its
  # fail-closed shell facade. Tag releases require the same portable coverage.
  windows = jobs.fetch("windows")
  abort("missing release gate: platform_windows runs on Windows") unless windows.fetch("runs-on") == "windows-latest"
  require_step!(jobs, "windows", "Clippy portable workspace (deny warnings)", "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings", "platform_windows clippy job")
  require_step!(jobs, "windows", "Test portable workspace", "cargo test --locked --workspace --exclude platform_macos", "platform_windows test job")
  require_step!(jobs, "windows", "Build app binary", "cargo build --locked -p app", "platform_windows build job")

  linux = jobs.fetch("linux")
  abort("missing release gate: platform_linux runs on Linux") unless linux.fetch("runs-on") == "ubuntu-latest"
  require_step!(jobs, "linux", "Clippy portable workspace (deny warnings)", "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings", "platform_linux clippy job")
  require_step!(jobs, "linux", "Test portable workspace", "cargo test --locked --workspace --exclude platform_macos", "platform_linux test job")
  require_step!(jobs, "linux", "Build app binary", "cargo build --locked -p app", "platform_linux build job")
  shellcheck_step = linux.fetch("steps").find { |step| step["name"] == "Shellcheck (errors only)" }
  abort("missing release gate: CI linux shellchecks tool scripts at error severity") unless
    shellcheck_step && shellcheck_step.fetch("run").include?("shellcheck --severity=error") &&
    !shellcheck_step.fetch("run").include?("|| true")

  workflow = release_workflow
  trigger = workflow["on"] || workflow[true]
  abort("missing release gate: release workflow is triggered only by push tags") unless trigger.is_a?(Hash) && trigger.keys == ["push"]
  push_trigger = trigger.fetch("push")
  abort("missing release gate: release workflow push trigger is limited to v* tags") unless push_trigger.is_a?(Hash) && push_trigger.keys == ["tags"] && push_trigger.fetch("tags") == ["v*"]
  abort("missing release gate: workflow defaults to read-only contents permission") unless workflow.fetch("permissions").fetch("contents") == "read"
  concurrency = workflow.fetch("concurrency")
  abort("missing release gate: release workflow serializes every tag run") unless concurrency.fetch("group") == "release"
  abort("missing release gate: release workflow does not cancel in-progress release") unless concurrency.fetch("cancel-in-progress") == false
  release_jobs = workflow.fetch("jobs")
  preflight = release_jobs.fetch("preflight")
  abort("missing release gate: release preflight runs before expensive jobs") unless preflight.fetch("runs-on") == "ubuntu-latest"
  preflight_steps = preflight.fetch("steps")
  preflight_checkout = preflight_steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
  abort("missing release gate: preflight checkout fetches full history") unless preflight_checkout && preflight_checkout.fetch("with").fetch("fetch-depth") == 0
  preflight_tag = preflight_steps.find { |step| step["name"] == "Verify release tag is valid and at default-branch HEAD" }
  abort("missing release gate: preflight verifies release version and exact default-branch HEAD") unless preflight_tag
  preflight_run = preflight_tag.fetch("run")
  ["version=\"${GITHUB_REF_NAME#v}\"", "tools/release/validate-version.sh \"$version\"", "git fetch --force origin \"refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH\"", "default_sha=\"$(git rev-parse \"origin/$DEFAULT_BRANCH\")\"", "if [ \"$GITHUB_SHA\" != \"$default_sha\" ]; then"].each do |needle|
    abort("missing release gate: preflight #{needle}") unless preflight_run.include?(needle)
  end
  abort("missing release gate: preflight checks release tag metadata") unless step?(
    preflight_steps,
    "Check release tag matches bundle metadata",
    "COMPME_EXPECTED_VERSION=\"${GITHUB_REF_NAME#v}\" tools/bundle/check-bundle-metadata.sh"
  )
  %w[validate windows linux].each do |job|
    abort("missing release gate: #{job} waits for release preflight") unless Array(release_jobs.fetch(job).fetch("needs")).include?("preflight")
  end
  validate_steps = release_jobs.fetch("validate").fetch("steps")
  %w[validate windows linux prebuild].each do |job_name|
    abort("missing release gate: release #{job_name} pins Rust toolchain") unless Array(release_jobs.fetch(job_name).fetch("steps")).any? { |step| step.is_a?(Hash) && rust_toolchain_step_valid?(step) }
  end
  windows = release_jobs.fetch("windows")
  abort("missing release gate: release platform_windows runs on Windows") unless windows.fetch("runs-on") == "windows-latest"
  require_step!(release_jobs, "windows", "Clippy portable workspace (deny warnings)", "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings", "release platform_windows clippy job")
  require_step!(release_jobs, "windows", "Test portable workspace", "cargo test --locked --workspace --exclude platform_macos", "release platform_windows test job")
  require_step!(release_jobs, "windows", "Build app binary", "cargo build --locked -p app", "release platform_windows build job")
  linux = release_jobs.fetch("linux")
  abort("missing release gate: release platform_linux runs on Linux") unless linux.fetch("runs-on") == "ubuntu-latest"
  require_step!(release_jobs, "linux", "Clippy portable workspace (deny warnings)", "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings", "release platform_linux clippy job")
  require_step!(release_jobs, "linux", "Test portable workspace", "cargo test --locked --workspace --exclude platform_macos", "release platform_linux test job")
  require_step!(release_jobs, "linux", "Build app binary", "cargo build --locked -p app", "release platform_linux build job")
  prebuild = release_jobs.fetch("prebuild")
  build_release = release_jobs.fetch("build_release")
  publish_release = release_jobs.fetch("publish_release")
  finalize_cask = release_jobs.fetch("finalize_cask")
  post_verify = release_jobs.fetch("post_verify")
  quote = 39.chr
  tag_job_guard = "${{ github.ref_type == #{quote}tag#{quote} && startsWith(github.ref_name, #{quote}v#{quote}) }}"
  abort("missing release gate: prebuild is limited to v* tag refs") unless prebuild.fetch("if") == tag_job_guard
  abort("missing release gate: build_release is limited to v* tag refs") unless build_release.fetch("if") == tag_job_guard
  abort("missing release gate: publish_release is limited to v* tag refs") unless publish_release.fetch("if") == tag_job_guard
  abort("missing release gate: finalize_cask is limited to v* tag refs") unless finalize_cask.fetch("if") == tag_job_guard
  abort("missing release gate: post_verify is limited to v* tag refs") unless post_verify.fetch("if") == tag_job_guard
  serialized_release_workflow = workflow.to_s
  abort("stale release gate: stable-only workflow contains prerelease branching") if serialized_release_workflow.include?("contains(github.ref_name") || serialized_release_workflow.match?(/\bprerelease\b/i)
  validate_actions!(workflow, "release")
  checkout = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0"
  toolchain = "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30"
  cache = "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32"
  upload = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
  download = "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093"
  attest = "actions/attest-build-provenance@0f67c3f4856b2e3261c31976d6725780e5e4c373"
  require_exact_actions!(workflow, {
    "preflight" => [checkout],
    "validate" => [checkout, toolchain, cache],
    "windows" => [checkout, toolchain, cache],
    "linux" => [checkout, toolchain, cache],
    "prebuild" => [checkout, toolchain, upload],
    "build_release" => [checkout, download, attest, upload],
    "publish_release" => [checkout, download],
    "finalize_cask" => [checkout, download],
    "post_verify" => [],
  }, "release")
  expected_release_timeouts = {
    "preflight" => 10, "validate" => 120, "windows" => 60, "linux" => 60,
    "prebuild" => 90, "build_release" => 360, "publish_release" => 20,
    "finalize_cask" => 20, "post_verify" => 30,
  }
  expected_release_timeouts.each do |job_name, timeout|
    abort("missing release gate: release #{job_name} exact timeout") unless release_jobs.fetch(job_name).fetch("timeout-minutes") == timeout
  end
  abort("missing release gate: release validate inherits read-only workflow permissions") if release_jobs.fetch("validate").key?("permissions")

  {
    "release root format" => ["Root format", "cargo fmt --all -- --check"],
    "release root clippy" => ["Root clippy", "cargo clippy --locked --workspace --all-targets -- -D warnings"],
    "release root test" => ["Root tests", "cargo test --locked --workspace --all-targets -- --test-threads=1"],
    "release root build" => ["Root build", "cargo build --locked --workspace --all-targets"],
    "release platform_macos examples build" => ["Build macOS acceptance examples", "cargo build --locked -p platform_macos --examples"],
    "release quality gate" => ["Model-quality gate", "bash tools/release/check-quality.sh"],
    "release version docs check" => ["Version docs check", "tools/release/check-version-docs.sh"],
    "release workflow invokes model gate script" => ["Model-backed release gates", "bash tools/release/run-model-gates.sh"],
    "release pinned dependency audit" => ["Rust dependency audit", "cargo install cargo-audit --version 0.22.2 --locked\ncargo audit\n"],
  }.merge(shared_gate_steps.transform_keys { |key| "release #{key}" }).each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(validate_steps, name, run)
  end
  model_gate_step = validate_steps.find { |step| step["name"] == "Model-backed release gates" }
  abort("missing release gate: hosted-runner model gates skip only the latency budget") unless model_gate_step && model_gate_step.fetch("env", {})["COMPME_REQUIRE_LATENCY_BUDGET"].to_s == "0"
  prebuild_needs = Array(prebuild.fetch("needs"))
  %w[validate windows linux].each do |job|
    abort("missing release gate: prebuild job depends on #{job}") unless prebuild_needs.include?(job)
  end
  build_release_needs = Array(build_release.fetch("needs"))
  abort("missing release gate: build_release job depends on prebuild") unless build_release_needs.include?("prebuild")
  publish_release_needs = Array(publish_release.fetch("needs"))
  abort("missing release gate: publish_release job depends only on build_release") unless publish_release_needs == ["build_release"]
  finalize_cask_needs = Array(finalize_cask.fetch("needs"))
  abort("missing release gate: finalize_cask job depends only on publish_release") unless finalize_cask_needs == ["publish_release"]
  post_verify_needs = Array(post_verify.fetch("needs"))
  abort("missing release gate: post_verify job depends only on finalize_cask") unless post_verify_needs == ["finalize_cask"]
  # The prebuild job compiles third-party code (build.rs, proc-macros) and must
  # therefore stay completely secretless: no protected environment, no secret
  # references anywhere in the job.
  abort("missing release gate: prebuild job must not use a protected environment") unless prebuild["environment"].nil?
  abort("missing release gate: prebuild job carries no secret references") if contains_secret_reference?(prebuild)
  abort("missing release gate: prebuild job has read-only contents permission") unless prebuild.fetch("permissions").fetch("contents") == "read"
  abort("missing release gate: build_release uses protected release environment") unless build_release.fetch("environment") == "release"
  abort("missing release gate: publish_release uses protected release environment") unless publish_release.fetch("environment") == "release"
  abort("missing release gate: finalize_cask uses protected release environment") unless finalize_cask.fetch("environment") == "release"
  abort("missing release gate: build_release job has read-only contents permission") unless build_release.fetch("permissions").fetch("contents") == "read"
  abort("missing release gate: publish_release job has contents write permission") unless publish_release.fetch("permissions").fetch("contents") == "write"
  abort("missing release gate: finalize_cask job has contents write permission") unless finalize_cask.fetch("permissions").fetch("contents") == "write"
  abort("missing release gate: post_verify job has read-only contents permission") unless post_verify.fetch("permissions").fetch("contents") == "read"
  abort("missing release gate: post_verify job must not use a protected environment") unless post_verify["environment"].nil?
  abort("missing release gate: post_verify job references no repository secrets") if post_verify.to_s.include?("secrets.")
  post_verify_steps = post_verify.fetch("steps")
  {
    "Download published assets and verify checksum" => ["gh release download", "shasum -a 256 -c"],
    "Install the published cask" => ["brew tap mudrii/compme", "brew install --cask compme"],
    "Assess installed app" => ["codesign --verify --deep --strict", "xcrun stapler validate", "spctl --assess"],
    "Bounded startup smoke" => ["COMPME_RUN_MS=5000", "another instance is already running", "compme: running (acceptance_pid=None stub=true run_ms=Some(5000))"],
  }.each do |name, needles|
    step = post_verify_steps.find { |candidate| candidate["name"] == name }
    abort("missing release gate: post_verify keeps step #{name}") unless step
    needles.each do |needle|
      abort("missing release gate: post_verify #{name} runs #{needle}") unless step.fetch("run").include?(needle)
    end
  end
  preflight_steps = release_jobs.fetch("preflight").fetch("steps")
  protected_tag_step = preflight_steps.find { |step| step["name"] == "Verify release tag is valid and at default-branch HEAD" }
  abort("missing release gate: preflight validates protected release tag") unless protected_tag_step
  abort("missing release gate: protected tag check receives github.ref_protected") unless protected_tag_step.fetch("env").fetch("REF_PROTECTED") == "${{ github.ref_protected }}"
  protected_tag_run = protected_tag_step.fetch("run")
  abort("missing release gate: protected tag check fails closed") unless protected_tag_run.include?(%q([ "$REF_PROTECTED" != "true" ])) && protected_tag_run.include?("release tag must match a protected v* ruleset")
  prebuild_steps = prebuild.fetch("steps")
  build_steps = build_release.fetch("steps")
  publish_steps = publish_release.fetch("steps")
  prebuild_checkout = prebuild_steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
  abort("missing release gate: prebuild checkout") unless prebuild_checkout
  abort("missing release gate: prebuild checkout fetches full history") unless prebuild_checkout.fetch("with").fetch("fetch-depth") == 0
  checkout = build_steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
  abort("missing release gate: build_release checkout") unless checkout
  abort("missing release gate: build_release checkout does not persist credentials") unless checkout.fetch("with").fetch("persist-credentials") == false
  publish_checkout = publish_steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
  abort("missing release gate: publish_release checkout") unless publish_checkout
  abort("missing release gate: publish_release checkout fetches full history") unless publish_checkout.fetch("with").fetch("fetch-depth") == 0
  ancestry_index = prebuild_steps.index { |step| step["name"] == "Verify release tag is still at default-branch HEAD" }
  scrub_index = prebuild_steps.index { |step| step["name"] == "Scrub persisted git credentials" }
  rust_index = prebuild_steps.index { |step| step["name"] == "Install Rust (stable)" }
  prebuild_metadata_index = prebuild_steps.index { |step| step["name"] == "Check release tag matches bundle metadata" }
  prebuild_index = prebuild_steps.index { |step| step["name"] == "Prebuild release binary (no signing secrets in this job)" }
  prebuild_arch_index = prebuild_steps.index { |step| step["name"] == "Verify prebuilt binary is arm64 only" }
  prebuild_upload_index = prebuild_steps.index { |step| step["name"] == "Upload prebuilt release binary" }
  metadata_index = build_steps.index { |step| step["name"] == "Check release tag matches bundle metadata" }
  download_binary_index = build_steps.index { |step| step["name"] == "Download prebuilt release binary" }
  chmod_index = build_steps.index { |step| step["name"] == "Restore prebuilt binary executable bit" }
  download_arch_index = build_steps.index { |step| step["name"] == "Verify downloaded binary is arm64 only" }
  register_keychain_index = build_steps.index { |step| step["name"] == "Register signing keychain cleanup path" }
  import_index = build_steps.index { |step| step["name"] == "Import Developer ID certificate" }
  build_index = build_steps.index { |step| step["name"] == "Build the .app bundle" }
  notarize_index = build_steps.index { |step| step["name"] == "Notarize and staple the .app" }
  cleanup_index = build_steps.index { |step| step["name"] == "Delete signing keychain" }
  package_index = build_steps.index { |step| step["name"] == "Package + checksum" }
  package_verify_index = build_steps.index { |step| step["name"] == "Verify packaged app signature and notarization" }
  manifest_index = publish_steps.index { |step| step["name"] == "Write publication-time update manifest" }
  upload_index = build_steps.index { |step| step["name"] == "Upload release artifacts" }
  expected_build_step_names = [
    nil,
    "Check release tag matches bundle metadata",
    "Download prebuilt release binary",
    "Restore prebuilt binary executable bit",
    "Verify downloaded binary is arm64 only",
    "Register signing keychain cleanup path",
    "Import Developer ID certificate",
    "Build the .app bundle",
    "Notarize and staple the .app",
    "Delete signing keychain",
    "Package + checksum",
    "Verify packaged app signature and notarization",
    "Attest build provenance",
    "Upload release artifacts",
  ]
  abort("missing release gate: signing job has exact shell-step topology") unless
    build_steps.map { |step| step["name"] } == expected_build_step_names
  abort("missing release gate: verifies tag ancestry in prebuild job") unless ancestry_index
  abort("missing release gate: scrubs persisted git credentials") unless scrub_index
  abort("missing release gate: installs Rust in prebuild") unless rust_index
  abort("missing release gate: checks release tag metadata in prebuild") unless prebuild_metadata_index
  abort("missing release gate: prebuilds release binary in secretless job") unless prebuild_index
  abort("missing release gate: verifies prebuilt release binary architecture") unless prebuild_arch_index
  abort("missing release gate: uploads prebuilt release binary") unless prebuild_upload_index
  abort("missing release gate: checks release tag metadata") unless metadata_index
  abort("missing release gate: downloads prebuilt release binary") unless download_binary_index
  abort("missing release gate: restores prebuilt binary executable bit") unless chmod_index
  abort("missing release gate: verifies downloaded release binary architecture") unless download_arch_index
  abort("missing release gate: registers signing keychain cleanup path") unless register_keychain_index
  abort("missing release gate: imports Developer ID certificate") unless import_index
  abort("missing release gate: builds app bundle") unless build_index
  abort("missing release gate: notarizes and staples app") unless notarize_index
  abort("stale release gate: unsigned release switch remains") if build_release.fetch("env", {}).key?("COMPME_HAVE_SIGNING")
  abort("missing release gate: Developer ID import is unconditional") if build_steps.fetch(import_index).key?("if")
  abort("missing release gate: notarization is unconditional") if build_steps.fetch(notarize_index).key?("if")
  abort("missing release gate: deletes signing keychain") unless cleanup_index
  abort("missing release gate: packages release artifact") unless package_index
  abort("missing release gate: reassesses packaged app") unless package_verify_index
  abort("missing release gate: writes publication-time update manifest") unless manifest_index
  abort("missing release gate: uploads release artifacts from read-only build job") unless upload_index
  abort("missing release gate: verifies tag ancestry before third-party build code") unless ancestry_index < prebuild_index
  abort("missing release gate: scrubs persisted git credentials after ancestry check") unless ancestry_index < scrub_index
  abort("missing release gate: scrubs persisted git credentials before Rust/build code") unless scrub_index < rust_index
  abort("missing release gate: checks release tag metadata before prebuild") unless prebuild_metadata_index < prebuild_index
  abort("missing release gate: verifies arm64 prebuilt binary before artifact upload") unless prebuild_index < prebuild_arch_index && prebuild_arch_index < prebuild_upload_index
  abort("missing release gate: checks release tag metadata before Developer ID secrets") unless metadata_index < import_index
  abort("missing release gate: downloads prebuilt binary before Developer ID import") unless download_binary_index < import_index
  abort("missing release gate: verifies downloaded arm64 binary and registers cleanup before secrets") unless download_binary_index < chmod_index && chmod_index < download_arch_index && download_arch_index < register_keychain_index && register_keychain_index < import_index
  abort("missing release gate: imports Developer ID certificate before build") unless import_index < build_index
  scrub_run = prebuild_steps.fetch(scrub_index).fetch("run")
  abort("missing release gate: scrub removes checkout extraheader") unless scrub_run.include?("git config --local --unset-all http.https://github.com/.extraheader")
  abort("missing release gate: prebuild job runs a cold build (no rust-cache)") if prebuild_steps.any? { |step| step["uses"].to_s.include?("rust-cache") }
  prebuild_step = prebuild_steps.fetch(prebuild_index)
  abort("missing release gate: prebuild compiles the release app binary") unless prebuild_step.fetch("run") == "cargo build --locked --release -p app"
  prebuild_upload_step = prebuild_steps.fetch(prebuild_upload_index)
  abort("missing release gate: prebuilt binary uploaded with pinned upload-artifact action") unless prebuild_upload_step.fetch("uses").match?(/\Aactions\/upload-artifact@[0-9a-f]{40}\z/)
  prebuild_upload_with = prebuild_upload_step.fetch("with")
  abort("missing release gate: prebuilt binary artifact is named compme-prebuilt-binary") unless prebuild_upload_with.fetch("name") == "compme-prebuilt-binary"
  abort("missing release gate: prebuilt binary upload fails when binary is missing") unless prebuild_upload_with.fetch("if-no-files-found") == "error"
  abort("missing release gate: prebuilt binary upload path is target/release/compme") unless prebuild_upload_with.fetch("path") == "target/release/compme"
  # The signing job must never compile anything: no cargo command in any step,
  # no Rust toolchain install, no rust-cache. Third-party build code only runs
  # in the secretless prebuild job.
  build_steps.each_with_index do |step, idx|
    run = step["run"].to_s
    abort("missing release gate: no cargo command anywhere in signing job (step #{step["name"] || idx})") if run.match?(/(^|[;&|[:space:]])cargo[[:space:]]/)
    uses = step["uses"].to_s
    abort("missing release gate: no Rust toolchain install in signing job") if uses.start_with?("dtolnay/rust-toolchain@")
    abort("missing release gate: no rust-cache in signing job") if uses.include?("rust-cache")
  end
  abort("missing release gate: release artifact chain is build -> notarize -> cleanup -> package -> reassess -> upload") unless build_index < notarize_index && notarize_index < cleanup_index && cleanup_index < package_index && package_index < package_verify_index && package_verify_index < upload_index
  build_steps.each_with_index do |step, idx|
    next unless contains_secret_reference?(step)
    abort("missing release gate: checks release tag metadata before secret-bearing build step #{step["name"] || idx}") unless metadata_index < idx
    abort("missing release gate: downloads prebuilt binary before secret-bearing build step #{step["name"] || idx}") unless download_binary_index < idx
  end
  ancestry_run = prebuild_steps.fetch(ancestry_index).fetch("run")
  ["git fetch --force origin \"refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH\"", "default_sha=\"$(git rev-parse \"origin/$DEFAULT_BRANCH\")\"", "if [ \"$GITHUB_SHA\" != \"$default_sha\" ]; then"].each do |needle|
    abort("missing release gate: early exact default-branch HEAD check") unless ancestry_run.include?(needle)
  end
  download_binary_step = build_steps.fetch(download_binary_index)
  abort("missing release gate: prebuilt binary downloaded with pinned download-artifact action") unless download_binary_step.fetch("uses").match?(/\Aactions\/download-artifact@[0-9a-f]{40}\z/)
  download_binary_with = download_binary_step.fetch("with")
  abort("missing release gate: signing job downloads compme-prebuilt-binary artifact") unless download_binary_with.fetch("name") == "compme-prebuilt-binary"
  abort("missing release gate: prebuilt binary lands in target/release") unless download_binary_with.fetch("path") == "target/release"
  chmod_step = build_steps.fetch(chmod_index)
  abort("missing release gate: restores executable bit on downloaded binary") unless chmod_step.fetch("run") == "chmod +x target/release/compme"
  import_step = build_steps.fetch(import_index)
  import_env = import_step.fetch("env")
  {
    "P12_BASE64" => "secrets.COMPME_DEVELOPER_ID_P12_BASE64",
    "P12_PASSWORD" => "secrets.COMPME_DEVELOPER_ID_P12_PASSWORD",
    "SIGNING_IDENTITY" => "secrets.COMPME_CODESIGN_IDENTITY",
  }.each do |key, needle|
    abort("missing release gate: Developer ID secret #{key}") unless import_env.fetch(key).include?(needle)
  end
  import_run = import_step.fetch("run")
  [
    "for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY",
    "missing required release secret",
    "exit 1",
    "p12=\"$RUNNER_TEMP/developer-id.p12\"",
    "trap #{39.chr}rm -f \"$p12\"#{39.chr} EXIT",
    "install -m 600 /dev/null \"$p12\"",
    "rm -f \"$p12\"",
    "trap - EXIT",
  ].each do |needle|
    abort("missing release gate: Developer ID missing-secret failure loop") unless import_run.include?(needle)
  end
  abort("missing release gate: Developer ID identity exported to bundle build") unless import_run.include?("COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY")
  build_step = build_steps.fetch(build_index)
  abort("missing release gate: builds release app with prebuilt bundle assembler") unless build_step.fetch("run") == %q(COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle")
  notarize_step = build_steps.fetch(notarize_index)
  abort("missing release gate: notarizes built app bundle") unless notarize_step.fetch("run") == %q(tools/release/notarize-app.sh "$RUNNER_TEMP/bundle/Compme.app")
  cleanup_step = build_steps.fetch(cleanup_index)
  abort("missing release gate: signing keychain cleanup runs always") unless cleanup_step.fetch("if") == "always()"
  cleanup_run = cleanup_step.fetch("run")
  ["keychain=\"${COMPME_SIGNING_KEYCHAIN:-$RUNNER_TEMP/compme-signing.keychain-db}\"", "security delete-keychain \"$keychain\"", "if [ -e \"$keychain\" ]; then", "exit \"$cleanup_status\"", "unset COMPME_SIGNING_KEYCHAIN COMPME_CODESIGN_IDENTITY", "COMPME_SIGNING_KEYCHAIN=", "COMPME_CODESIGN_IDENTITY=", ">> \"$GITHUB_ENV\""].each do |needle|
    abort("missing release gate: signing keychain cleanup #{needle}") unless cleanup_run.include?(needle)
  end
  package_step = build_steps.fetch(package_index)
  abort("missing release gate: package step exposes pkg outputs") unless package_step.fetch("id") == "pkg"
  package_run = package_step.fetch("run")
  [
    "version=\"${GITHUB_REF_NAME#v}\"",
    "zip=\"compme-${version}-macos.zip\"",
    "ditto -c -k --keepParent \"$RUNNER_TEMP/bundle/Compme.app\" \"$zip\"",
    "shasum -a 256 \"$zip\"",
    "printf ",
    "\"$sha\" \"$zip\" > \"$zip.sha256\"",
    "echo \"version=$version\"",
    "echo \"zip=$zip\"",
    "echo \"sha256=$sha\"",
  ].each do |needle|
    abort("missing release gate: package step #{needle}") unless package_run.include?(needle)
  end
  package_verify_step = build_steps.fetch(package_verify_index)
  abort("missing release gate: packaged-app reassessment consumes package ZIP output") unless
    package_verify_step.fetch("env") == {"ZIP" => "${{ steps.pkg.outputs.zip }}"}
  package_verify_run = active_shell_lines(package_verify_step.fetch("run"))
  [
    "verify_dir=\"$(mktemp -d \"$RUNNER_TEMP/compme-package-verify.XXXXXX\")\"",
    "trap #{39.chr}rm -rf \"$verify_dir\"#{39.chr} EXIT",
    "ditto -x -k \"$ZIP\" \"$verify_dir\"",
    "entry_count=\"$(find \"$verify_dir\" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d #{39.chr} #{39.chr})\"",
    "if [ \"$entry_count\" -ne 1 ] || [ ! -d \"$verify_dir/Compme.app\" ]; then",
    "app=\"$verify_dir/Compme.app\"",
    "codesign --verify --deep --strict --verbose=2 \"$app\"",
    "xcrun stapler validate \"$app\"",
    "spctl --assess --type execute --verbose=4 \"$app\"",
  ].each do |needle|
    abort("missing release gate: packaged-app reassessment #{needle}") unless package_verify_run.include?(needle)
  end
  manifest_step = publish_steps.fetch(manifest_index)
  manifest_run = manifest_step.fetch("run")
  abort("missing release gate: publication-time manifest forbids awk command shadowing") if
    manifest_run.lines.any? { |line| line.match?(/\A[[:space:]]*(?:function[[:space:]]+)?awk(?:[[:space:]]*\(\))?[[:space:]]*(?:\{|\z)/) }
  [
    "set -euo pipefail",
    "VERSION=\"${GITHUB_REF_NAME#v}\"",
    "ZIP=\"compme-${VERSION}-macos.zip\"",
    "MANIFEST=\"compme-${VERSION}-update.json\"",
    "SHA256=\"$(awk #{39.chr}{print $1}#{39.chr} \"release-artifacts/$ZIP.sha256\")\"",
    "tools/release/write-update-manifest.sh \\",
    "\"$VERSION\" \"$ZIP\" \"$SHA256\" > \"release-artifacts/$MANIFEST\"",
  ].each do |needle|
    abort("missing release gate: publication-time manifest step #{needle}") unless manifest_run.include?(needle)
  end
  upload_step = build_steps.fetch(upload_index)
  upload_with = upload_step.fetch("with")
  abort("missing release gate: uploads artifacts with pinned upload-artifact action") unless upload_step.fetch("uses").match?(/\Aactions\/upload-artifact@[0-9a-f]{40}\z/)
  abort("missing release gate: uploads named release artifact bundle") unless upload_with.fetch("name") == "compme-release-artifacts"
  abort("missing release gate: upload fails if release artifact is missing") unless upload_with.fetch("if-no-files-found") == "error"
  upload_path = upload_with.fetch("path").to_s
  [
    "${{ steps.pkg.outputs.zip }}",
    "${{ steps.pkg.outputs.zip }}.sha256",
  ].each do |needle|
    abort("missing release gate: upload includes #{needle}") unless upload_path.include?(needle)
  end

  download_index = publish_steps.index { |step| step["name"] == "Download release artifacts" }
  checksum_index = publish_steps.index { |step| step["name"] == "Verify downloaded artifact checksum" }
  publish_head_index = publish_steps.index { |step| step["name"] == "Verify release tag is still at default-branch HEAD before publication" }
  publish_manifest_index = publish_steps.index { |step| step["name"] == "Write publication-time update manifest" }
  create_index = publish_steps.index { |step| step["name"] == "Create draft GitHub release" }
  undraft_index = publish_steps.index { |step| step["name"] == "Revalidate default-branch HEAD and undraft GitHub release" }
  abort("missing release gate: publish job downloads, verifies, rechecks HEAD, writes manifest, creates draft, then undrafts") unless download_index && checksum_index && publish_head_index && publish_manifest_index && create_index && undraft_index && download_index < checksum_index && checksum_index < publish_head_index && publish_head_index < publish_manifest_index && publish_manifest_index < create_index && create_index < undraft_index
  download_step = publish_steps.fetch(download_index)
  download_with = download_step.fetch("with")
  abort("missing release gate: downloads artifacts with pinned download-artifact action") unless download_step.fetch("uses").match?(/\Aactions\/download-artifact@[0-9a-f]{40}\z/)
  abort("missing release gate: downloads named release artifact bundle") unless download_with.fetch("name") == "compme-release-artifacts"
  abort("missing release gate: downloads release artifacts into release-artifacts") unless download_with.fetch("path") == "release-artifacts"
  checksum_lines = active_shell_lines(publish_steps.fetch(checksum_index).fetch("run"))
  ["cd release-artifacts", "test -f \"$ZIP\"", "test -f \"$ZIP.sha256\"", "shasum -a 256 -c \"$ZIP.sha256\""].each do |needle|
    abort("missing release gate: verifies downloaded artifact checksum #{needle}") unless checksum_lines.include?(needle)
  end
  publish_head = publish_steps.fetch(publish_head_index)
  publish_head_run = active_shell_lines(publish_head.fetch("run"))
  ["git fetch --force origin \"refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH\"", "default_sha=\"$(git rev-parse \"origin/$DEFAULT_BRANCH\")\"", "if [ \"$GITHUB_SHA\" != \"$default_sha\" ]; then", "exit 1"].each do |needle|
    abort("missing release gate: pre-publication exact default HEAD #{needle}") unless publish_head_run.any? { |line| line.include?(needle) && !line.match?(/\A(echo|printf)[[:space:]]/) }
  end
  create_step = publish_steps.fetch(create_index)
  abort("missing release gate: draft creation uses GitHub token") unless create_step.fetch("env").fetch("GH_TOKEN") == "${{ github.token }}"
  create_lines = active_shell_lines(create_step.fetch("run"))
  [
    "VERSION=\"${GITHUB_REF_NAME#v}\"",
    "ZIP=\"compme-${VERSION}-macos.zip\"",
    "MANIFEST=\"compme-${VERSION}-update.json\"",
    "gh release create \"$GITHUB_REF_NAME\" \\",
    "--verify-tag \\",
    "--draft \\",
    "--generate-notes \\",
    "\"release-artifacts/$ZIP\" \\",
    "\"release-artifacts/$ZIP.sha256\" \\",
    "\"release-artifacts/$MANIFEST\"",
  ].each do |needle|
    abort("missing release gate: exact fail-closed draft creation #{needle}") unless create_lines.include?(needle)
  end
  undraft_step = publish_steps.fetch(undraft_index)
  abort("missing release gate: late undraft recheck exact environment") unless undraft_step.fetch("env") == {
    "DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}",
    "GH_TOKEN" => "${{ github.token }}",
  }
  undraft_lines = active_shell_lines(undraft_step.fetch("run"))
  [
    "git fetch --force origin \\",
    "\"refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH\" \\",
    "\"refs/tags/$GITHUB_REF_NAME:refs/tags/$GITHUB_REF_NAME\"",
    "default_sha=\"$(git rev-parse \"origin/$DEFAULT_BRANCH\")\"",
    "tag_sha=\"$(git rev-parse \"refs/tags/$GITHUB_REF_NAME^{commit}\")\"",
    "if [ \"$tag_sha\" != \"$GITHUB_SHA\" ] || [ \"$tag_sha\" != \"$default_sha\" ]; then",
    "if ! gh release delete \"$GITHUB_REF_NAME\" --yes; then",
    "exit 1",
    "gh release edit \"$GITHUB_REF_NAME\" --draft=false",
  ].each do |needle|
    abort("missing release gate: late undraft recheck #{needle}") unless undraft_lines.include?(needle)
  end

  finalize_steps = finalize_cask.fetch("steps")
  finalize_checkout = finalize_steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
  abort("missing release gate: finalize_cask checkout fetches full history") unless finalize_checkout&.fetch("with")&.fetch("fetch-depth") == 0
  finalize_download_index = finalize_steps.index { |step| step["name"] == "Download release artifacts" }
  finalize_checksum_index = finalize_steps.index { |step| step["name"] == "Verify downloaded artifact checksum" }
  cask_index = finalize_steps.index { |step| step["name"] == "Finalize Homebrew cask" }
  abort("missing release gate: separate cask job downloads and verifies artifacts before finalization") unless finalize_download_index && finalize_checksum_index && cask_index && finalize_download_index < finalize_checksum_index && finalize_checksum_index < cask_index
  finalize_download_with = finalize_steps.fetch(finalize_download_index).fetch("with")
  abort("missing release gate: finalize_cask downloads named release artifact bundle") unless finalize_download_with == {"name" => "compme-release-artifacts", "path" => "release-artifacts"}
  finalize_checksum_lines = active_shell_lines(finalize_steps.fetch(finalize_checksum_index).fetch("run"))
  ["cd release-artifacts", "test -f \"$ZIP\"", "test -f \"$ZIP.sha256\"", "shasum -a 256 -c \"$ZIP.sha256\""].each do |needle|
    abort("missing release gate: finalize_cask verifies downloaded artifact checksum #{needle}") unless finalize_checksum_lines.include?(needle)
  end
  cask_step = finalize_steps.fetch(cask_index)
  abort("missing release gate: cask finalizer has exact branch/token environment") unless cask_step.fetch("env") == {
    "DEFAULT_BRANCH" => "${{ github.event.repository.default_branch }}",
    "GH_TOKEN" => "${{ github.token }}",
  }
  cask_run = cask_step.fetch("run")
  cask_lines = active_shell_lines(cask_run)
  abort("missing release gate: derives cask ZIP from release version") unless cask_lines.include?("ZIP=\"compme-${VERSION}-macos.zip\"")
  abort("missing release gate: finalizes cask from downloaded release artifact") unless cask_lines.include?("artifact_path=\"$PWD/release-artifacts/$ZIP\"")
  require_active_finalizer_command!(cask_run, %q(tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"))
  abort("missing release gate: release tag metadata check") unless step?(
    build_steps,
    "Check release tag matches bundle metadata",
    "COMPME_EXPECTED_VERSION=\"${GITHUB_REF_NAME#v}\" tools/bundle/check-bundle-metadata.sh"
  )
' "$release_workflow" "$ci_workflow"

workspace_members_count="$(cargo metadata --format-version 1 --no-deps | ruby -rjson -e 'puts JSON.parse(STDIN.read).fetch("workspace_members").length')"
workspace_test_count="$(cargo test --locked --workspace --all-targets -- --list | awk '/: test$/ { count++ } END { print count + 0 }')"
workspace_test_count_commas="$(ruby -e 'puts ARGV.fetch(0).reverse.gsub(/(\d{3})(?=\d)/, "\\1,").reverse' "$workspace_test_count")"

bash -n "$gate_script"
bash -n "$feature_script"
bash -n "$privacy_script"
bash -n "$bundle_metadata_script"
bash -n "$make_app_script"
bash -n "$make_icon_script"
bash -n "$finalize_cask_script"
bash -n "$update_cask_script"
bash -n "$notarize_script"
bash -n "$update_manifest_script"
bash -n "$version_validator_script"
"$bundle_metadata_script" >/dev/null
COMPME_EXPECTED_VERSION=9.9.9 "$bundle_metadata_script" --self-test >/dev/null
COMPME_BUNDLE_REPO_ROOT=/tmp/compme-poisoned-root COMPME_BUNDLE_LSREGISTER=/tmp/poisoned-lsregister CARGO_TARGET_DIR=/tmp/compme-poisoned-target COMPME_BUNDLE_SKIP_BUILD=1 COMPME_CODESIGN_IDENTITY=poisoned COMPME_CODESIGN_ENTITLEMENTS=/tmp/poisoned.entitlements "$make_app_script" --self-test >/dev/null
"$make_icon_script" --self-test >/dev/null
GITHUB_ACTIONS=true GITHUB_REF_TYPE=tag COMPME_ALLOW_MODEL_GATE_OVERRIDE=1 COMPME_MODEL_GATE_PATH=/tmp/compme-poisoned-model.gguf COMPME_MODEL_GATE_URL=https://invalid.example/poison.gguf COMPME_MODEL_GATE_SHA256=0000000000000000000000000000000000000000000000000000000000000000 COMPME_REQUIRE_LATENCY_BUDGET=0 "$gate_script" --self-test >/dev/null
"$gate_script" --self-test >/dev/null
"$privacy_script" >/dev/null
"$privacy_script" --self-test >/dev/null
if ! "$finalize_cask_script" --self-test >/dev/null; then
  echo "release gate failed: finalize-cask self-test" >&2
  exit 1
fi
COMPME_CASK_PATH=/tmp/compme-poisoned-cask.rb COMPME_CASK_ARTIFACT=/tmp/compme-poisoned.zip "$update_cask_script" --self-test >/dev/null
COMPME_NOTARYTOOL_KEYCHAIN_PROFILE=poisoned COMPME_NOTARYTOOL_KEY_BASE64=poisoned COMPME_NOTARYTOOL_KEY_PATH=/tmp/poisoned.p8 COMPME_NOTARYTOOL_KEY_ID=poisoned COMPME_NOTARYTOOL_ISSUER=poisoned COMPME_NOTARYTOOL_APPLE_ID=poisoned@example.invalid COMPME_NOTARYTOOL_PASSWORD=poisoned COMPME_NOTARYTOOL_TEAM_ID=poisoned COMPME_NOTARYTOOL_TEMP_KEY=/tmp/poisoned-temp.p8 COMPME_NOTARYTOOL_TIMEOUT=1 "$notarize_script" --self-test >/dev/null
COMPME_UPDATE_PUBLISHED_AT=not-a-date "$update_manifest_script" --self-test >/dev/null
"$version_validator_script" --self-test >/dev/null
for invalid_release_version in 1.2.3-rc.1 1.2.3-alpha 1.2.3-beta.2 1.2.3+build.7; do
  if "$version_validator_script" "$invalid_release_version" >/dev/null 2>&1; then
    echo "release gate failed: prerelease/build version accepted: $invalid_release_version" >&2
    exit 1
  fi
done
check_finalizer_helper_contract "$finalize_cask_script"

require_line "$readme_doc" "workspace of ${workspace_members_count}$" "README workspace member count"
require_line "$development_doc" "workspace with ${workspace_members_count}[[:space:]]+members" "DEVELOPMENT workspace member count"
require_line "$readme_doc" "roughly ${workspace_test_count_commas}[[:space:]]+tests" "README workspace test count"
require_line "$development_doc" "~${workspace_test_count}[[:space:]]+tests" "DEVELOPMENT workspace test count"
require_line "$roadmap_doc" "≈${workspace_test_count}[[:space:]]+workspace tests" "ROADMAP workspace test count"
require_line "$roadmap_doc" "current workspace count is recorded in the" "ROADMAP readiness count source"
require_line "$grammar_spec" "≈${workspace_test_count}[[:space:]]+tests green" "grammar spec prerequisite test count"
if grep -Eq 'sha256 "0{64}"' "$cask_file"; then
  require_line "$roadmap_doc" 'first real release' "ROADMAP first release pending status"
  require_readme_homebrew_line 'Homebrew cask install is not available until the first signed `v\*` release' "README Homebrew pre-release blocked status"
  require_readme_homebrew_line 'Until then, build from' "README Homebrew source fallback"
else
  reject_readme_homebrew_line 'Homebrew cask install is not available until the first signed `v\*` release' "README Homebrew pre-release blocked status after first tag"
  reject_readme_homebrew_line 'Until then, build from' "README Homebrew source fallback after first tag"
fi
require_line "$cask_file" '^  url "https://github\.com/mudrii/compme/releases/download/v#\{version\}/compme-#\{version\}-macos\.zip"$' "Homebrew cask GitHub release URL"
require_line "$cask_file" '^  depends_on arch: :arm64$' "Homebrew cask Apple Silicon architecture constraint"
require_line "$grammar_spec" 'grammar_fix_prompt_is_single_line_and_includes_word_and_left_context' "grammar spec G1 prompt RED-first test"
require_line "$grammar_spec" 'vet_correction_accepts_one_edit_and_preserves_case' "grammar spec G1 vet accept RED-first test"
require_line "$grammar_spec" 'vet_correction_rejects_empty_identical_multi_word_large_edit_and_non_ascii' "grammar spec G1 vet reject RED-first test"
require_line "$grammar_spec" 'vet_correction_rejects_alot_to_a_lot_for_single_word_mode' "grammar spec single-word autocorrect RED-first test"
require_line "$grammar_spec" 'grammar_fix_request_bypasses_screen_wait_context_personalization_and_complete_n' "grammar spec worker bypass RED-first test"
require_line "$grammar_spec" 'grammar_fix_request_preserves_range_and_vets_model_output' "grammar spec worker range RED-first test"
require_line "$grammar_spec" 'grammar_fix_rejected_output_returns_no_correction' "grammar spec worker reject RED-first test"
require_line "$grammar_spec" 'on_correction_shows_correction_with_range_anchor' "grammar spec engine show RED-first test"
require_line "$grammar_spec" 'stale_correction_result_is_ignored_after_text_changes' "grammar spec engine invalidation RED-first test"
require_line "$grammar_spec" 'accept_correction_emits_replace_range' "grammar spec engine accept RED-first test"
require_line "$grammar_spec" 'preview_accept_correction_exposes_suggestion_and_range_while_showing' "grammar spec engine preview RED-first test"
require_line "$grammar_spec" 'accept_full_and_word_do_not_commit_correction_presentation' "grammar spec engine accept isolation RED-first test"
require_line "$grammar_spec" 'word_at_caret_returns_whole_word_and_scalar_range_at_end' "grammar spec word-at-caret end RED-first test"
require_line "$grammar_spec" 'word_at_caret_returns_whole_word_and_scalar_range_mid_word' "grammar spec word-at-caret middle RED-first test"
require_line "$grammar_spec" 'word_at_caret_handles_astral_prefix_without_utf16_offset_drift' "grammar spec word-at-caret astral RED-first test"
require_line "$grammar_spec" 'word_at_caret_returns_previous_word_at_boundary_and_none_for_empty_field' "grammar spec word-at-caret empty RED-first test"
require_line "$grammar_spec" 'correction_range_splice_replaces_midword_without_left_fragment_leak' "grammar spec platform replace RED-first test"
require_line "$grammar_spec" 'scalar_correction_range_to_utf16_range_accounts_for_astral_scalars' "grammar spec platform scalar conversion RED-first test"
require_line "$grammar_spec" 'correction_range_expected_text_guard_rejects_changed_live_text' "grammar spec platform stale range guard RED-first test"
require_line "$grammar_spec" 'grammar_fix_enabled_inherits_global_default_without_app' "grammar spec prefs default RED-first test"
require_line "$grammar_spec" 'grammar_fix_enabled_respects_per_app_override' "grammar spec prefs override RED-first test"
require_line "$grammar_spec" 'set_app_policy_field_writes_grammar_fix' "grammar spec prefs write RED-first test"
require_line "$grammar_spec" 'grammar_trigger_dispatches_word_at_caret_scalar_range' "grammar spec run-loop dispatch RED-first test"
require_line "$grammar_spec" 'grammar_detection_blocks_without_fresh_browser_domain_when_domain_rules_exist' "grammar spec run-loop domain RED-first test"
require_line "$grammar_spec" 'grammar_detection_refresh_drops_stale_allowed_browser_domain' "grammar spec run-loop domain refresh RED-first test"
require_line "$grammar_spec" 'grammar_detection_respects_enable_per_app_snooze_and_axset' "grammar spec run-loop gate RED-first test"
require_line "$grammar_spec" 'grammar_detection_rejects_non_empty_selection' "grammar spec run-loop selection RED-first test"
require_line "$grammar_spec" 'config_parses_grammar_check_and_grammar_accept_keys' "grammar spec config keys RED-first test"
require_line "$grammar_spec" 'grammar_check_shortcut_routes_to_detection' "grammar spec shortcut dispatch RED-first test"
require_line "$grammar_spec" 'grammar_accept_action_routes_to_accept_correction_not_full' "grammar spec correction accept RED-first test"
require_test_symbol "$repo_root/crates/model_client/src/lib.rs" 'grammar_fix_prompt_is_single_line_and_includes_word_and_left_context' "model_client grammar prompt test"
require_test_symbol "$repo_root/crates/grammar/src/lib.rs" 'vet_correction_accepts_one_edit_and_preserves_case' "grammar vet accept test"
require_test_symbol "$repo_root/crates/grammar/src/lib.rs" 'vet_correction_rejects_empty_identical_multi_word_large_edit_and_non_ascii' "grammar vet reject test"
require_test_symbol "$repo_root/crates/app/src/inference.rs" 'grammar_fix_request_bypasses_screen_wait_context_personalization_and_complete_n' "inference grammar bypass test"
require_test_symbol "$repo_root/crates/app/src/inference.rs" 'grammar_fix_request_preserves_range_and_vets_model_output' "inference grammar range test"
require_test_symbol "$repo_root/crates/app/src/inference.rs" 'grammar_fix_rejected_output_returns_no_correction' "inference grammar reject test"
require_test_symbol "$repo_root/crates/engine/src/lib.rs" 'on_correction_shows_correction_with_range_anchor' "engine correction show test"
require_test_symbol "$repo_root/crates/engine/src/lib.rs" 'accept_correction_emits_replace_range' "engine correction accept test"
require_test_symbol "$repo_root/crates/engine_core/src/lib.rs" 'accept_full_and_word_do_not_commit_correction_presentation' "engine_core correction accept isolation test"
require_test_symbol "$repo_root/crates/context/src/lib.rs" 'word_at_caret_returns_whole_word_and_scalar_range_at_end' "context word-at-caret end test"
require_test_symbol "$repo_root/crates/context/src/lib.rs" 'word_at_caret_returns_whole_word_and_scalar_range_mid_word' "context word-at-caret midword test"
require_test_symbol "$repo_root/crates/context/src/lib.rs" 'word_at_caret_handles_astral_prefix_without_utf16_offset_drift' "context word-at-caret astral test"
require_test_symbol "$repo_root/crates/platform_macos/src/lib.rs" 'correction_range_splice_replaces_midword_without_left_fragment_leak' "platform_macos range splice test"
require_test_symbol "$repo_root/crates/platform_macos/src/lib.rs" 'correction_range_expected_text_guard_rejects_changed_live_text' "platform_macos stale range guard test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'grammar_trigger_dispatches_word_at_caret_scalar_range' "run_loop grammar dispatch test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'grammar_detection_blocks_without_fresh_browser_domain_when_domain_rules_exist' "run_loop grammar domain test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'grammar_detection_refresh_drops_stale_allowed_browser_domain' "run_loop grammar domain refresh test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'grammar_detection_respects_enable_per_app_snooze_and_axset' "run_loop grammar gate test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'grammar_detection_rejects_non_empty_selection' "run_loop grammar selection test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'config_parses_grammar_check_and_grammar_accept_keys' "run_loop grammar config test"
require_test_symbol "$repo_root/crates/app/src/run_loop.rs" 'grammar_accept_action_routes_to_accept_correction_not_full' "run_loop grammar accept routing test"
require_line "$make_app_script" 'COMPME_BUNDLE_LSREGISTER=' "bundle self-test launch services override"
require_line "$make_app_script" 'grep -Fq "lsregister -f \$app" "\$log"' "bundle self-test asserts Launch Services registration"
require_line "$make_app_script" 'COMPME_BUNDLE_LSREGISTER="\$fake_bin/lsregister_fail"' "bundle self-test asserts Launch Services registration failure"
require_line "$make_app_script" 'lsregister failure was accepted' "bundle self-test rejects masked Launch Services registration failure"
require_line "$make_app_script" '^[[:space:]]*"\$lsregister" -f "\$app"[[:space:]]*$' "bundle Launch Services registration fails closed"
require_line "$bundle_smoke_script" '^[[:space:]]*env -i[[:space:]]*$' "bundle smoke clears inherited product environment"
require_line "$bundle_smoke_script" '"COMPME_CONFIG=\$runtime_dir/config.env"' "bundle smoke isolates packaged app config"
require_line "$bundle_smoke_script" '"COMPME_RUN_MS=\$run_ms"' "bundle smoke bounds packaged app runtime"
require_line "$bundle_smoke_script" '"COMPME_STUB_COMPLETION=\${COMPME_STUB_COMPLETION:- smoke}"' "bundle smoke enables deterministic completion"
require_line "$bundle_smoke_script" 'COMPME_ACCEPTANCE_PID=444' "bundle smoke self-test poisons the inherited product environment"
require_line "$bundle_smoke_script" 'hostile product environment leaked into app' "bundle smoke self-test rejects inherited product environment"
require_line "$bundle_smoke_script" 'bundle smoke failed: isolated app exited as a duplicate instance' "bundle smoke rejects instance-lock collisions"
require_line "$bundle_smoke_script" 'app never reached the bounded stub runtime' "bundle smoke verifies bounded stub startup"
require_line "$bundle_smoke_script" 'COMPME_BUNDLE_SMOKE_MAKE_APP' "bundle smoke make-app override"
require_line "$bundle_smoke_script" 'COMPME_BUNDLE_SMOKE_APP_EXIT=42' "bundle smoke self-test rejects app failure"
require_line "$feature_script" 'llama-cpp-2 feature "metal"' "model_client macOS Metal feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "dynamic-backends"' "model_client non-macOS dynamic backend assertion"
require_line "$feature_script" 'llama-cpp-2 feature "vulkan"' "model_client non-macOS Vulkan feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "default"' "model_client default feature denial"
require_line "$feature_script" 'spike macOS' "spike feature policy assertion"
require_line "$privacy_script" 'sentry' "privacy policy denied package assertion"
require_line "$privacy_script" 'segment\.io' "privacy policy denied host self-test"
require_development_gate_line '^tools/release/check-privacy-policy\.sh[[:space:]]*$' "DEVELOPMENT privacy policy gate"
require_development_gate_line '^tools/release/check-privacy-policy\.sh --self-test[[:space:]]*$' "DEVELOPMENT privacy policy self-test gate"
require_line "$bundle_metadata_script" 'release tag version is empty' "bundle metadata empty release-tag version rejection"
require_line "$bundle_metadata_script" 'ruby -c "\$cask_file"' "bundle metadata validates cask Ruby syntax"
require_line "$bundle_metadata_script" 'Casks/compme\.rb: invalid Ruby syntax' "bundle metadata rejects invalid cask Ruby syntax"
require_line "$bundle_metadata_script" 'Casks/compme\.rb: architecture must be :arm64' "bundle metadata enforces cask arm64 architecture"
require_line "$gate_script" '^default_model="tools/spike/models/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "pinned base GGUF model path"
require_line "$gate_script" '^default_url="https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "pinned base GGUF download URL"
require_line "$gate_script" '^default_expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"[[:space:]]*$' "pinned base GGUF sha256"
require_line "$gate_script" 'COMPME_ALLOW_MODEL_GATE_OVERRIDE' "release-context model gate override escape hatch"
require_line "$gate_script" 'refusing \$name override in GitHub release context' "release-context model gate override rejection"
require_line "$gate_script" 'COMPME_MODEL_GATE_CURL_BODY="wrong-model"' "model gate checksum failure self-test"
require_line "$gate_script" 'latency=1 gpu=0 ctx_tokens=256 spike_model= args=test --locked -p model_client --test latency' "model gate root env self-test"
require_line "$gate_script" 'tools/spike env=1 ctx= latency=1 gpu= ctx_tokens= spike_model=\$model_path args=test --locked --test model_integration' "model gate spike env self-test"
require_line "$quality_script" '^default_model="tools/spike/models/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "quality gate pinned base GGUF model path"
require_line "$quality_script" '^default_url="https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "quality gate pinned base GGUF download URL"
require_line "$quality_script" '^default_expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"[[:space:]]*$' "quality gate pinned base GGUF sha256"
require_line "$quality_script" '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_MODEL_GATE_PATH="\$gate_model" COMPME_QUALITY_CORPUS="\$gate_corpus" cargo test --locked -p model_client --test quality -- --ignored --test-threads=1[[:space:]]*$' "quality gate serialized env test invocation"
require_line "$version_docs_script" 'Latest published artifact' "version docs check covers README status line"
require_line "$version_docs_script" 'supported release is' "version docs check covers SECURITY supported release"
require_line "$version_docs_script" 'remains the latest published artifact' "version docs check covers ROADMAP header"
require_line "$version_docs_script" 'latest published artifact is' "version docs check covers RELEASING boundary note"
require_line "$version_docs_script" 'version-docs check failed' "version docs check names stale files"
require_line "$version_docs_script" 'points to' "version docs check covers DEVELOPMENT tag-pin note"
require_line "$version_docs_script" 'latest published artifact' "version docs check covers ACCEPTANCE header"
require_line "$version_docs_script" 'Release boundary' "version docs check covers ARCHITECTURE boundary"
require_line "$version_docs_script" 'Validate the latest published' "version docs check covers MANUAL-VALIDATION boundary"
reject_line "$repo_root/crates/model_client/tests/latency.rs" 'Metal GPU' "root model-client ignored tests stale GPU wording"
require_line "$finalize_cask_script" 'git fetch --no-tags origin' "cask finalizer disables implicit tag fetches"
require_line "$finalize_cask_script" '\+refs/heads/\$default_branch:\$remote_branch_ref' "cask finalizer refreshes the remote branch explicitly"
require_line "$finalize_cask_script" '\+refs/tags/\$tag:\$verified_tag_ref' "cask finalizer fetches the release tag into a private ref"
require_line "$finalize_cask_script" 'git merge-base --is-ancestor "\$GITHUB_SHA" "\$remote_branch_ref"' "cask finalizer ancestry check"
require_line "$finalize_cask_script" 'tag/version mismatch' "cask finalizer tag/version guard"
require_line "$finalize_cask_script" 'refusing to publish a stale or out-of-order cask update' "cask finalizer stale version refusal"
require_line "$finalize_cask_script" 'git push origin "HEAD:\$default_branch"' "cask finalizer push"
require_line "$gate_script" '^require_latency_budget="\$\{COMPME_REQUIRE_LATENCY_BUDGET:-1\}"[[:space:]]*$' "latency budget defaults on, CI opt-out only"
require_line "$gate_script" '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET="\$require_latency_budget" cargo test --locked -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "serialized root ignored model tests"
require_line "$gate_script" '^  COMPME_SPIKE_MODEL_PATH="\$spike_model" COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_LATENCY_BUDGET="\$require_latency_budget" cargo test --locked --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "serialized spike ignored model tests"
require_line "$acceptance_doc" '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized root ignored model tests"
require_line "$acceptance_doc" '^COMPME_SPIKE_MODEL_PATH="\$PWD/models/qwen2\.5-0\.5b-q4_k_m\.gguf" COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized spike ignored model tests"
require_line "$acceptance_doc" 'DEVELOPMENT\.md#full-local-gate' "acceptance docs link the canonical full local gate"
require_line "$acceptance_doc" 'overlay-correction-presenter' "acceptance docs correction overlay gate"
require_line "$acceptance_doc" 'Apps policy grid' "acceptance docs Apps policy LOOK gate"
require_line "$acceptance_doc" 'Personalization pane' "acceptance docs Personalization LOOK gate"
require_line "$acceptance_doc" '^--allow-manual[[:space:]]*$' "acceptance docs A1b allow-manual option"
require_line "$acceptance_doc" '^Use `--allow-manual` only after executing and recording the MANUAL checklist$' "acceptance docs A1b allow-manual policy"
require_line "$releasing_doc" 'push to `main` / `spike/\*\*`, PR, or `workflow_dispatch`' "release docs CI trigger truth"
require_line "$releasing_doc" '^[[:space:]]*bash tools/release/run-model-gates\.sh[[:space:]]*$' "release docs model gate wrapper"
require_line "$releasing_doc" 'COMPME_MODEL_GATE_PATH' "release docs model gate path override"
require_line "$releasing_doc" 'COMPME_MODEL_GATE_URL' "release docs model gate URL override"
require_line "$releasing_doc" 'COMPME_MODEL_GATE_SHA256' "release docs model gate SHA override"
require_line "$releasing_doc" 'COMPME_ALLOW_MODEL_GATE_OVERRIDE' "release docs model gate override escape hatch"
require_line "$releasing_doc" 'COMPME_SPIKE_MODEL_PATH' "release docs spike model path override"
require_line "$releasing_doc" 'verifies the downloaded zip checksum' "release docs publish checksum verification"
require_line "$releasing_doc" 'same-name release assets are never overwritten; a collision fails closed' "release docs asset collision policy"
require_line "$releasing_doc" 'tools/bundle/check-bundle-metadata\.sh' "release docs bundle metadata check"
require_line "$releasing_doc" 'tools/bundle/check-bundle-metadata\.sh --self-test' "release docs bundle metadata self-test"
require_line "$releasing_doc" 'tools/release/run-model-gates\.sh --self-test' "release docs model gate self-test"
require_line "$releasing_doc" 'tools/release/finalize-cask\.sh --self-test' "release docs cask finalizer self-test"
require_line "$releasing_doc" 'tools/bundle/make-app\.sh --self-test' "release docs bundle assembler self-test"
require_line "$releasing_doc" 'tools/bundle/bundle-smoke\.sh' "release docs bundle smoke"
require_line "$releasing_doc" 'tools/bundle/bundle-smoke\.sh --self-test' "release docs bundle smoke self-test"
require_line "$releasing_doc" 'tools/acceptance/run-ui-assisted-session\.sh --self-test' "release docs UI-assisted session self-test"
require_line "$releasing_doc" 'tools/acceptance/missing-model-startup\.sh --self-test' "release docs missing-model startup self-test"
require_line "$releasing_doc" 'tools/acceptance/missing-model-startup\.sh`' "release docs missing-model startup product smoke"
require_line "$releasing_doc" 'tools/release/check-model-client-features\.sh' "release docs model client feature policy"
require_line "$releasing_doc" 'tools/release/check-model-client-features\.sh --self-test' "release docs model client feature policy self-test"
require_line "$releasing_doc" 'tools/release/check-agent-briefs\.sh' "release docs agent brief alignment"
require_line "$releasing_doc" 'tools/release/check-agent-briefs\.sh --self-test' "release docs agent brief alignment self-test"
require_line "$releasing_doc" 'tools/release/update-cask\.sh --self-test' "release docs cask updater self-test"
require_line "$releasing_doc" 'tools/release/notarize-app\.sh --self-test' "release docs notarization helper self-test"
require_line "$releasing_doc" 'tools/release/write-update-manifest\.sh --self-test' "release docs update manifest self-test"
require_line "$releasing_doc" 'cargo build --locked -p platform_macos --examples' "release docs platform_macos examples build"
require_line "$releasing_doc" 'git pull --ff-only origin main' "release docs require up-to-date default branch before tag"
require_line "$releasing_doc" 'tag commit is not an ancestor' "release docs cask finalizer ancestry guard"
require_line "$repo_root/tools/acceptance/run-a1b-live-gates.sh" 'overlay-correction-presenter' "A1b runner correction overlay gate"
require_line "$readme_doc" '^A2 validation is local/manual-only' "README marks A2 validation local/manual-only"
require_line "$development_doc" '^A2 validation is local/manual-only' "DEVELOPMENT marks A2 validation local/manual-only"
require_line "$acceptance_doc" '^A2 validation is local/manual-only' "acceptance docs mark A2 validation local/manual-only"
require_line "$releasing_doc" '^A2 validation is local/manual-only' "release docs mark A2 validation local/manual-only"
require_line "$roadmap_doc" '^A2 compatibility validation is now local/manual-only' "roadmap marks A2 validation local/manual-only"
require_line "$acceptance_doc" '^tools/acceptance/run-a2-compat-gates\.sh <kind>[[:space:]]*$' "acceptance docs retain manual A2 runner"
require_line "$acceptance_doc" '^tools/release/check-a2-matrix-ledger\.sh "\$ledger"[[:space:]]*$' "acceptance docs retain manual A2 ledger checker"
reject_line "$readme_doc" '^bash -n tools/acceptance/\*\.sh tools/bundle/\*\.sh tools/release/\*\.sh' "README wildcard syntax-checks local/manual A2 scripts"
reject_line "$development_doc" '^bash -n tools/acceptance/\*\.sh tools/bundle/\*\.sh tools/release/\*\.sh' "DEVELOPMENT wildcard syntax-checks local/manual A2 scripts"
reject_line "$acceptance_doc" '^bash -n tools/acceptance/\*\.sh tools/bundle/\*\.sh tools/release/\*\.sh' "acceptance docs wildcard syntax-check local/manual A2 scripts"
reject_line "$grammar_spec" 'bash -n tools/acceptance/\*\.sh tools/bundle/\*\.sh tools/release/\*\.sh' "grammar spec wildcard syntax-checks local/manual A2 scripts"
for gate in \
  apps-policy-toggle-look \
  personalization-pane-look \
  menu-bar-icon-look \
  shortcuts-recorder-look \
  always-on-hotkeys-physical-look \
  setup-model-picker-look \
  nine-tab-settings-walkthrough \
  caret-marker-chromium-forks-calibration \
  caret-marker-chrome-marker \
  caret-marker-chromium-marker \
  caret-marker-electron-marker \
  encrypted-memory-all-monitored-live \
  grammar-fix-textedit-look \
  mirror-window-firefox-zen-look \
  setup-needed-docs-arc-onboarding \
  multi-candidate-cycle-physical-look \
  input-monitoring-revoked-carbon-accept; do
  require_line "$repo_root/tools/acceptance/run-a1b-live-gates.sh" "$gate" "A1b runner emits manual gate $gate"
  require_line "$acceptance_doc" "^- \`$gate\`[[:space:]]*$" "acceptance docs list manual gate $gate"
  require_line "$manual_validation_doc" "\`$gate\`" "manual validation docs list manual gate $gate"
done
require_readme_gate_line 'docs/DEVELOPMENT\.md#full-local-gate' "README gates section links the canonical full local gate"
require_development_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "DEVELOPMENT bundle metadata check"
require_development_gate_line '^tools/bundle/check-bundle-metadata\.sh --self-test[[:space:]]*$' "DEVELOPMENT bundle metadata self-test"
require_development_gate_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "DEVELOPMENT bundle assembler self-test"
require_development_gate_line '^tools/bundle/bundle-smoke\.sh[[:space:]]*$' "DEVELOPMENT bundle smoke"
require_development_gate_line '^tools/bundle/bundle-smoke\.sh --self-test[[:space:]]*$' "DEVELOPMENT bundle smoke self-test"
require_development_gate_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "DEVELOPMENT E2E self-test"
require_development_gate_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "DEVELOPMENT missing-model startup self-test"
require_development_gate_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "DEVELOPMENT missing-model startup product smoke"
require_development_gate_line '^tools/acceptance/run-ui-assisted-session\.sh --self-test[[:space:]]*$' "DEVELOPMENT UI-assisted session self-test"
require_development_gate_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "DEVELOPMENT A1b self-test"
require_line "$development_doc" '^Use `--allow-manual` only after executing and recording the MANUAL checklist$' "DEVELOPMENT A1b allow-manual policy"
require_development_gate_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "DEVELOPMENT model client feature policy"
require_development_gate_line '^tools/release/check-model-client-features\.sh --self-test[[:space:]]*$' "DEVELOPMENT model client feature policy self-test"
require_development_gate_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "DEVELOPMENT release gate policy check"
require_development_gate_line '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "DEVELOPMENT model gate self-test"
require_development_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "DEVELOPMENT cask updater self-test"
require_development_gate_line '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "DEVELOPMENT cask finalizer self-test"
require_development_gate_line '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "DEVELOPMENT notarization helper self-test"
require_development_gate_line '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "DEVELOPMENT update manifest self-test"
require_development_gate_line '^cargo build --locked -p platform_macos --examples[[:space:]]*$' "DEVELOPMENT platform_macos examples build"
require_development_gate_line '^bash tools/release/run-model-gates\.sh[[:space:]]*$' "DEVELOPMENT model-backed release gate"
require_development_gate_line '^find tools/acceptance tools/bundle tools/release -type f -name .\*\.sh. -print0 \| xargs -0 bash -n[[:space:]]*$' "DEVELOPMENT script syntax gate"
require_development_gate_line '^find tools -type f -name .\*\.sh. -print0 \| xargs -0 shellcheck --severity=error[[:space:]]*$' "DEVELOPMENT shellcheck gate"
require_development_gate_line '^RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace[[:space:]]*$' "DEVELOPMENT rustdoc gate"
require_development_gate_line '^tools/release/check-version-docs\.sh --self-test[[:space:]]*$' "DEVELOPMENT version docs check self-test"
require_development_gate_line '^tools/release/check-quality\.sh --self-test[[:space:]]*$' "DEVELOPMENT quality gate self-test"
require_development_gate_line '^tools/release/check-version-docs\.sh[[:space:]]*$' "DEVELOPMENT version docs check"
require_development_gate_line '^bash tools/release/check-quality\.sh[[:space:]]*$' "DEVELOPMENT model-quality gate"
require_grammar_spec_validation_line '^cargo fmt --all -- --check[[:space:]]*$' "grammar spec fmt gate"
require_grammar_spec_validation_line '^cargo clippy --locked --workspace --all-targets -- -D warnings[[:space:]]*$' "grammar spec clippy gate"
require_grammar_spec_validation_line '^cargo test --locked --workspace --all-targets -- --test-threads=1[[:space:]]*$' "grammar spec workspace test gate"
require_grammar_spec_validation_line '^cargo build --locked --workspace --all-targets[[:space:]]*$' "grammar spec workspace build gate"
require_grammar_spec_validation_line '^cargo build --locked -p platform_macos --examples[[:space:]]*$' "grammar spec platform_macos examples build gate"
require_line "$grammar_spec" 'find tools/acceptance tools/bundle tools/release -type f -name.*-print0.*xargs -0 bash -n' "grammar spec script syntax gate"
require_grammar_spec_validation_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "grammar spec bundle metadata gate"
require_grammar_spec_validation_line '^tools/bundle/check-bundle-metadata\.sh --self-test[[:space:]]*$' "grammar spec bundle metadata self-test"
require_grammar_spec_validation_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "grammar spec bundle assembler self-test"
require_grammar_spec_validation_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "grammar spec E2E self-test"
require_grammar_spec_validation_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "grammar spec missing-model self-test"
require_grammar_spec_validation_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "grammar spec missing-model product smoke"
require_grammar_spec_validation_line '^tools/acceptance/run-ui-assisted-session\.sh --self-test[[:space:]]*$' "grammar spec UI-assisted session self-test"
require_grammar_spec_validation_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "grammar spec A1b self-test"
require_grammar_spec_validation_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "grammar spec model client feature policy"
require_grammar_spec_validation_line '^tools/release/check-model-client-features\.sh --self-test[[:space:]]*$' "grammar spec model client feature policy self-test"
require_grammar_spec_validation_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "grammar spec release policy check"
require_grammar_spec_validation_line '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "grammar spec model gate self-test"
require_grammar_spec_validation_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "grammar spec cask updater self-test"
require_grammar_spec_validation_line '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "grammar spec cask finalizer self-test"
require_grammar_spec_validation_line '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "grammar spec notarization helper self-test"
require_grammar_spec_validation_line '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "grammar spec update manifest self-test"
require_grammar_spec_validation_line '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "grammar spec root ignored model tests"
require_grammar_spec_validation_line '^cd tools/spike && cargo fmt -- --check[[:space:]]*$' "grammar spec spike fmt gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo clippy --locked --all-targets -- -D warnings[[:space:]]*$' "grammar spec spike clippy gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo test --locked[[:space:]]*$' "grammar spec spike test gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo build --locked --bins[[:space:]]*$' "grammar spec spike build gate"
require_grammar_spec_validation_line '^cd tools/spike && COMPME_SPIKE_MODEL_PATH="\$PWD/models/qwen2\.5-0\.5b-q4_k_m\.gguf" COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "grammar spec spike ignored model tests"
