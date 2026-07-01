#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
release_workflow="${1:-$repo_root/.github/workflows/release.yml}"
ci_workflow="$repo_root/.github/workflows/ci.yml"
gate_script="$repo_root/tools/release/run-model-gates.sh"
feature_script="$repo_root/tools/release/check-model-client-features.sh"
bundle_metadata_script="$repo_root/tools/bundle/check-bundle-metadata.sh"
make_app_script="$repo_root/tools/bundle/make-app.sh"
acceptance_doc="$repo_root/docs/ACCEPTANCE.md"
development_doc="$repo_root/docs/DEVELOPMENT.md"
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
    "CI bundle assembler self-test" => ["Bundle assembler self-test", "tools/bundle/make-app.sh --self-test"],
    "CI E2E self-test" => ["E2E runner self-test", "tools/acceptance/e2e-complete-me.sh --self-test"],
    "CI missing-model startup self-test" => ["Missing-model startup self-test", "tools/acceptance/missing-model-startup.sh --self-test"],
    "CI missing-model startup product smoke" => ["Missing-model startup product smoke", "tools/acceptance/missing-model-startup.sh"],
    "CI A1b self-test" => ["A1b runner self-test", "tools/acceptance/run-a1b-live-gates.sh --self-test"],
    "CI A2 self-test" => ["A2 compatibility runner self-test", "tools/acceptance/run-a2-compat-gates.sh --self-test"],
    "CI model client feature policy" => ["Model client feature policy", "tools/release/check-model-client-features.sh"],
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
    "release bundle assembler self-test" => ["Bundle assembler self-test", "tools/bundle/make-app.sh --self-test"],
    "release A1b self-test" => ["A1b runner self-test", "tools/acceptance/run-a1b-live-gates.sh --self-test"],
    "release A2 self-test" => ["A2 compatibility runner self-test", "tools/acceptance/run-a2-compat-gates.sh --self-test"],
    "release E2E self-test" => ["E2E runner self-test", "tools/acceptance/e2e-complete-me.sh --self-test"],
    "release missing-model startup self-test" => ["Missing-model startup self-test", "tools/acceptance/missing-model-startup.sh --self-test"],
    "release missing-model startup product smoke" => ["Missing-model startup product smoke", "tools/acceptance/missing-model-startup.sh"],
    "release model client feature policy" => ["Model client feature policy", "tools/release/check-model-client-features.sh"],
    "release policy check" => ["Release model gate policy", "bash tools/release/check-model-gates.sh"],
    "release cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
  }.each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(validate_steps, name, run)
  end

  release_needs = Array(release.fetch("needs"))
  abort("missing release gate: release job depends on validate") unless release_needs.include?("validate")
  release_steps = release.fetch("steps")
  abort("missing release gate: release tag metadata check") unless step?(
    release_steps,
    "Check release tag matches bundle metadata",
    "COMPME_EXPECTED_VERSION=\"${GITHUB_REF_NAME#v}\" tools/bundle/check-bundle-metadata.sh"
  )
' "$release_workflow" "$ci_workflow"

bash -n "$gate_script"
bash -n "$feature_script"
bash -n "$bundle_metadata_script"
bash -n "$make_app_script"
"$bundle_metadata_script" >/dev/null
"$make_app_script" --self-test >/dev/null

require_line "$feature_script" 'llama-cpp-2 feature "metal"' "model_client macOS Metal feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "dynamic-backends"' "model_client non-macOS dynamic backend assertion"
require_line "$feature_script" 'llama-cpp-2 feature "vulkan"' "model_client non-macOS Vulkan feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "default"' "model_client default feature denial"
require_line "$feature_script" 'spike macOS' "spike feature policy assertion"
require_line "$gate_script" '^model="tools/spike/models/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "pinned base GGUF model path"
require_line "$gate_script" '^url="https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "pinned base GGUF download URL"
require_line "$gate_script" '^expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"[[:space:]]*$' "pinned base GGUF sha256"
require_line "$gate_script" '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "serialized root ignored model tests"
require_line "$gate_script" '^  COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "serialized spike ignored model tests"
require_line "$acceptance_doc" '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized root ignored model tests"
require_line "$acceptance_doc" '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized spike ignored model tests"
require_line "$acceptance_doc" '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "acceptance docs bundle metadata check"
require_line "$acceptance_doc" '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "acceptance docs bundle assembler self-test"
require_line "$acceptance_doc" '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "acceptance docs E2E self-test"
require_line "$acceptance_doc" '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "acceptance docs missing-model startup self-test"
require_line "$acceptance_doc" '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "acceptance docs missing-model startup product smoke"
require_line "$acceptance_doc" '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "acceptance docs A1b self-test"
require_line "$acceptance_doc" '^--allow-manual[[:space:]]*$' "acceptance docs A1b allow-manual option"
require_line "$acceptance_doc" '^Use `--allow-manual` only after executing and recording the MANUAL checklist$' "acceptance docs A1b allow-manual policy"
require_line "$acceptance_doc" '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "acceptance docs A2 self-test"
require_line "$acceptance_doc" '^tools/release/check-model-client-features\.sh[[:space:]]*$' "acceptance docs model client feature policy"
require_line "$acceptance_doc" '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "acceptance docs cask updater self-test"
require_line "$releasing_doc" '^[[:space:]]*COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "release docs serialized root ignored model tests"
require_line "$releasing_doc" '^[[:space:]]*COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "release docs serialized spike ignored model tests"
require_line "$releasing_doc" 'tools/bundle/check-bundle-metadata\.sh' "release docs bundle metadata check"
require_line "$releasing_doc" 'tools/bundle/make-app\.sh --self-test' "release docs bundle assembler self-test"
require_line "$releasing_doc" 'tools/acceptance/missing-model-startup\.sh --self-test' "release docs missing-model startup self-test"
require_line "$releasing_doc" 'tools/acceptance/missing-model-startup\.sh`' "release docs missing-model startup product smoke"
require_line "$releasing_doc" 'tools/release/check-model-client-features\.sh' "release docs model client feature policy"
require_line "$releasing_doc" 'tools/release/update-cask\.sh --self-test' "release docs cask updater self-test"
require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "README bundle metadata check"
require_readme_gate_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "README bundle assembler self-test"
require_readme_gate_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "README E2E self-test"
require_readme_gate_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "README missing-model startup self-test"
require_readme_gate_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "README missing-model startup product smoke"
require_readme_gate_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "README A1b self-test"
require_readme_gate_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "README A2 self-test"
require_readme_gate_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "README model client feature policy"
require_readme_gate_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "README release gate policy check"
require_readme_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "README cask updater self-test"
require_readme_gate_line '^bash tools/release/run-model-gates\.sh[[:space:]]*$' "README model-backed release gate"
require_development_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "DEVELOPMENT bundle metadata check"
require_development_gate_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "DEVELOPMENT bundle assembler self-test"
require_development_gate_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "DEVELOPMENT E2E self-test"
require_development_gate_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "DEVELOPMENT missing-model startup self-test"
require_development_gate_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "DEVELOPMENT missing-model startup product smoke"
require_development_gate_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "DEVELOPMENT A1b self-test"
require_line "$development_doc" '^Use `--allow-manual` only after executing and recording the MANUAL checklist$' "DEVELOPMENT A1b allow-manual policy"
require_development_gate_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "DEVELOPMENT A2 self-test"
require_development_gate_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "DEVELOPMENT model client feature policy"
require_development_gate_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "DEVELOPMENT release gate policy check"
require_development_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "DEVELOPMENT cask updater self-test"
require_development_gate_line '^bash tools/release/run-model-gates\.sh[[:space:]]*$' "DEVELOPMENT model-backed release gate"
