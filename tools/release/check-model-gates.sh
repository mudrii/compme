#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
release_workflow="${1:-$repo_root/.github/workflows/release.yml}"
gate_script="$repo_root/tools/release/run-model-gates.sh"

require_line() {
  file="$1"
  pattern="$2"
  label="$3"
  if ! grep -Eq "$pattern" "$file"; then
    echo "missing release model gate: $label" >&2
    return 1
  fi
}

ruby -ryaml -e '
  workflow = YAML.load_file(ARGV.fetch(0))
  jobs = workflow.fetch("jobs")
  validate_steps = jobs.fetch("validate").fetch("steps")
  release = jobs.fetch("release")

  model_gate = validate_steps.any? do |step|
    step.is_a?(Hash) &&
      step["name"] == "Model-backed release gates" &&
      step["run"] == "bash tools/release/run-model-gates.sh"
  end
  abort("missing release model gate: release workflow invokes model gate script") unless model_gate

  release_needs = Array(release.fetch("needs"))
  abort("missing release model gate: release job depends on validate") unless release_needs.include?("validate")
' "$release_workflow"

bash -n "$gate_script"

require_line "$gate_script" '^url="https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/main/qwen2\.5-0\.5b-q4_k_m\.gguf\?download=true"[[:space:]]*$' "pinned GGUF download URL"
require_line "$gate_script" '^expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"[[:space:]]*$' "pinned GGUF sha256"
require_line "$gate_script" '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored[[:space:]]*$' "root ignored model tests"
require_line "$gate_script" '^  COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored[[:space:]]*$' "spike ignored model tests"
