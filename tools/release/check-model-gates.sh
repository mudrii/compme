#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
release_workflow="${1:-$repo_root/.github/workflows/release.yml}"
ci_workflow="$repo_root/.github/workflows/ci.yml"
gate_script="$repo_root/tools/release/run-model-gates.sh"
bundle_metadata_script="$repo_root/tools/bundle/check-bundle-metadata.sh"
acceptance_doc="$repo_root/docs/ACCEPTANCE.md"
releasing_doc="$repo_root/docs/RELEASING.md"
readme_doc="$repo_root/README.md"

require_line() {
  file="$1"
  pattern="$2"
  label="$3"
  if ! grep -Eq "$pattern" "$file"; then
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

ruby -ryaml -e '
  def step?(steps, name, run)
    steps.any? do |step|
      step.is_a?(Hash) &&
        step["name"] == name &&
        step["run"] == run
    end
  end

  release_workflow = YAML.load_file(ARGV.fetch(0))
  ci_workflow = YAML.load_file(ARGV.fetch(1))

  ci_steps = ci_workflow.fetch("jobs").fetch("check").fetch("steps")
  {
    "CI script syntax" => ["Script syntax", "bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh"],
    "CI bundle metadata" => ["Bundle metadata", "tools/bundle/check-bundle-metadata.sh"],
    "CI E2E self-test" => ["E2E runner self-test", "tools/acceptance/e2e-complete-me.sh --self-test"],
    "CI A1b self-test" => ["A1b runner self-test", "tools/acceptance/run-a1b-live-gates.sh --self-test"],
    "CI A2 self-test" => ["A2 compatibility runner self-test", "tools/acceptance/run-a2-compat-gates.sh --self-test"],
    "CI release policy" => ["Release model gate policy", "bash tools/release/check-model-gates.sh"],
    "CI cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
  }.each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(ci_steps, name, run)
  end

  workflow = release_workflow
  jobs = workflow.fetch("jobs")
  validate_steps = jobs.fetch("validate").fetch("steps")
  release = jobs.fetch("release")

  {
    "release workflow invokes model gate script" => ["Model-backed release gates", "bash tools/release/run-model-gates.sh"],
    "release script syntax" => ["Script syntax", "bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh"],
    "release bundle metadata" => ["Bundle metadata", "tools/bundle/check-bundle-metadata.sh"],
    "release A1b self-test" => ["A1b runner self-test", "tools/acceptance/run-a1b-live-gates.sh --self-test"],
    "release A2 self-test" => ["A2 compatibility runner self-test", "tools/acceptance/run-a2-compat-gates.sh --self-test"],
    "release E2E self-test" => ["E2E runner self-test", "tools/acceptance/e2e-complete-me.sh --self-test"],
    "release policy check" => ["Release model gate policy", "bash tools/release/check-model-gates.sh"],
    "release cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
  }.each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(validate_steps, name, run)
  end

  release_needs = Array(release.fetch("needs"))
  abort("missing release gate: release job depends on validate") unless release_needs.include?("validate")
' "$release_workflow" "$ci_workflow"

bash -n "$gate_script"
bash -n "$bundle_metadata_script"
"$bundle_metadata_script" >/dev/null

require_line "$gate_script" '^url="https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/main/qwen2\.5-0\.5b-q4_k_m\.gguf\?download=true"[[:space:]]*$' "pinned GGUF download URL"
require_line "$gate_script" '^expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"[[:space:]]*$' "pinned GGUF sha256"
require_line "$gate_script" '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "serialized root ignored model tests"
require_line "$gate_script" '^  COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "serialized spike ignored model tests"
require_line "$acceptance_doc" '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized root ignored model tests"
require_line "$acceptance_doc" '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized spike ignored model tests"
require_line "$acceptance_doc" '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "acceptance docs bundle metadata check"
require_line "$acceptance_doc" '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "acceptance docs cask updater self-test"
require_line "$releasing_doc" '^[[:space:]]*COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "release docs serialized root ignored model tests"
require_line "$releasing_doc" '^[[:space:]]*COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "release docs serialized spike ignored model tests"
require_line "$releasing_doc" 'tools/bundle/check-bundle-metadata\.sh' "release docs bundle metadata check"
require_line "$releasing_doc" 'tools/release/update-cask\.sh --self-test' "release docs cask updater self-test"
require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "README bundle metadata check"
require_readme_gate_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "README release gate policy check"
require_readme_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "README cask updater self-test"
require_readme_gate_line '^bash tools/release/run-model-gates\.sh[[:space:]]*$' "README model-backed release gate"
