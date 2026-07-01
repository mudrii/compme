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
roadmap_doc="$repo_root/docs/ROADMAP.md"
grammar_spec="$repo_root/docs/superpowers/specs/2026-07-01-grammar-fix-design.md"

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

run_self_test() {
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/compme-check-model-gates.XXXXXX")"
  cleanup() {
    rm -rf "$tmp_dir"
  }

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
  cleanup
}

if [[ "${1:-}" == "--self-test" ]]; then
  run_self_test
  echo "Self-test passed"
  exit 0
fi

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

  release_workflow = YAML.load_file(ARGV.fetch(0))
  ci_workflow = YAML.load_file(ARGV.fetch(1))

  jobs = ci_workflow.fetch("jobs")
  ci_steps = jobs.fetch("check").fetch("steps")
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
    "CI platform_macos examples build" => ["Build macOS acceptance examples", "cargo build -p platform_macos --examples"],
  }.each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(ci_steps, name, run)
  end

  windows = jobs.fetch("windows")
  abort("missing release gate: platform_windows runs on Windows") unless windows.fetch("runs-on") == "windows-latest"
  require_step!(jobs, "windows", "Format", "cargo fmt -p platform_windows -- --check", "platform_windows fmt job")
  require_step!(jobs, "windows", "Clippy (deny warnings)", "cargo clippy -p platform_windows --all-targets -- -D warnings", "platform_windows clippy job")
  require_step!(jobs, "windows", "Test", "cargo test -p platform_windows", "platform_windows test job")
  require_step!(jobs, "windows", "Build", "cargo build -p platform_windows", "platform_windows build job")

  linux = jobs.fetch("linux")
  abort("missing release gate: platform_linux runs on Linux") unless linux.fetch("runs-on") == "ubuntu-latest"
  require_step!(jobs, "linux", "Format", "cargo fmt -p platform_linux -- --check", "platform_linux fmt job")
  require_step!(jobs, "linux", "Clippy (deny warnings)", "cargo clippy -p platform_linux --all-targets -- -D warnings", "platform_linux clippy job")
  require_step!(jobs, "linux", "Test", "cargo test -p platform_linux", "platform_linux test job")
  require_step!(jobs, "linux", "Build", "cargo build -p platform_linux", "platform_linux build job")

  workflow = release_workflow
  release_jobs = workflow.fetch("jobs")
  validate_steps = release_jobs.fetch("validate").fetch("steps")
  release = release_jobs.fetch("release")

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

workspace_members_count="$(cargo metadata --format-version 1 --no-deps | ruby -rjson -e 'puts JSON.parse(STDIN.read).fetch("workspace_members").length')"
workspace_test_count="$(cargo test --workspace --all-targets -- --list | awk '/: test$/ { count++ } END { print count + 0 }')"
workspace_test_count_commas="$(ruby -e 'puts ARGV.fetch(0).reverse.gsub(/(\d{3})(?=\d)/, "\\1,").reverse' "$workspace_test_count")"

bash -n "$gate_script"
bash -n "$feature_script"
bash -n "$bundle_metadata_script"
bash -n "$make_app_script"
"$bundle_metadata_script" >/dev/null
"$make_app_script" --self-test >/dev/null

require_line "$readme_doc" "workspace of ${workspace_members_count}$" "README workspace member count"
require_line "$development_doc" "workspace with ${workspace_members_count}[[:space:]]+members" "DEVELOPMENT workspace member count"
require_line "$readme_doc" "roughly ${workspace_test_count_commas}[[:space:]]+tests" "README workspace test count"
require_line "$development_doc" "~${workspace_test_count}[[:space:]]+tests" "DEVELOPMENT workspace test count"
require_line "$roadmap_doc" "≈${workspace_test_count}[[:space:]]+workspace tests" "ROADMAP workspace test count"
require_line "$roadmap_doc" "${workspace_test_count}[[:space:]]+tests, clippy clean" "ROADMAP readiness test count"
require_line "$grammar_spec" "≈${workspace_test_count}[[:space:]]+tests green" "grammar spec prerequisite test count"
require_line "$grammar_spec" 'grammar_fix_prompt_is_single_line_and_includes_word_and_left_context' "grammar spec G1 prompt RED-first test"
require_line "$grammar_spec" 'vet_correction_accepts_one_edit_and_preserves_case' "grammar spec G1 vet accept RED-first test"
require_line "$grammar_spec" 'vet_correction_rejects_empty_identical_multi_word_large_edit_and_non_ascii' "grammar spec G1 vet reject RED-first test"
require_line "$grammar_spec" 'grammar_autocorrect_prepass_rejects_multi_word_table_entries' "grammar spec autocorrect prepass RED-first test"
require_line "$grammar_spec" 'vet_correction_rejects_alot_to_a_lot_for_single_word_mode' "grammar spec single-word autocorrect RED-first test"
require_line "$grammar_spec" 'grammar_fix_request_bypasses_screen_wait_context_personalization_and_complete_n' "grammar spec worker bypass RED-first test"
require_line "$grammar_spec" 'grammar_fix_request_preserves_range_and_vets_model_output' "grammar spec worker range RED-first test"
require_line "$grammar_spec" 'grammar_fix_rejected_output_returns_no_correction' "grammar spec worker reject RED-first test"
require_line "$grammar_spec" 'offer_correction_shows_correction_with_exact_range' "grammar spec engine show RED-first test"
require_line "$grammar_spec" 'on_correction_shows_correction_with_range_and_invalidates_on_text_changed' "grammar spec engine invalidation RED-first test"
require_line "$grammar_spec" 'accept_correction_emits_replace_range_with_exact_range' "grammar spec engine exact range RED-first test"
require_line "$grammar_spec" 'accept_full_and_word_do_not_commit_correction_presentation' "grammar spec engine accept isolation RED-first test"
require_line "$grammar_spec" 'word_at_caret_returns_whole_word_and_scalar_range_at_end' "grammar spec word-at-caret end RED-first test"
require_line "$grammar_spec" 'word_at_caret_returns_whole_word_and_scalar_range_mid_word' "grammar spec word-at-caret middle RED-first test"
require_line "$grammar_spec" 'word_at_caret_handles_astral_prefix_without_utf16_offset_drift' "grammar spec word-at-caret astral RED-first test"
require_line "$grammar_spec" 'word_at_caret_returns_none_at_boundary_or_empty_field' "grammar spec word-at-caret empty RED-first test"
require_line "$grammar_spec" 'platform_seam_replaces_midword_range_without_left_fragment_leak' "grammar spec platform replace RED-first test"
require_line "$grammar_spec" 'platform_seam_text_range_rect_converts_scalar_range_and_fails_closed' "grammar spec platform rect RED-first test"
require_line "$grammar_spec" 'grammar_fix_enabled_inherits_global_default_without_app' "grammar spec prefs default RED-first test"
require_line "$grammar_spec" 'grammar_fix_enabled_respects_per_app_override' "grammar spec prefs override RED-first test"
require_line "$grammar_spec" 'set_app_policy_field_writes_grammar_fix' "grammar spec prefs write RED-first test"
require_line "$grammar_spec" 'grammar_trigger_dispatches_word_at_caret_scalar_range' "grammar spec run-loop dispatch RED-first test"
require_line "$grammar_spec" 'grammar_detection_blocks_without_fresh_browser_domain_when_domain_rules_exist' "grammar spec run-loop domain RED-first test"
require_line "$grammar_spec" 'grammar_detection_respects_enable_per_app_snooze_and_axset' "grammar spec run-loop gate RED-first test"
require_line "$grammar_spec" 'grammar_detection_rejects_non_empty_selection' "grammar spec run-loop selection RED-first test"
require_line "$grammar_spec" 'grammar_detection_rejects_non_axset_before_model_request' "grammar spec run-loop AxSet RED-first test"
require_line "$grammar_spec" 'config_parses_grammar_check_and_grammar_accept_keys' "grammar spec config keys RED-first test"
require_line "$grammar_spec" 'grammar_check_shortcut_routes_to_detection' "grammar spec shortcut dispatch RED-first test"
require_line "$grammar_spec" 'grammar_accept_action_routes_to_accept_correction_not_full' "grammar spec correction accept RED-first test"
require_line "$make_app_script" 'COMPME_BUNDLE_LSREGISTER=' "bundle self-test launch services override"
require_line "$make_app_script" 'grep -Fq "lsregister -f \$app" "\$log"' "bundle self-test asserts Launch Services registration"
require_line "$make_app_script" 'COMPME_BUNDLE_LSREGISTER="\$fake_bin/lsregister_fail"' "bundle self-test asserts Launch Services registration failure"
require_line "$make_app_script" 'lsregister failure was accepted' "bundle self-test rejects masked Launch Services registration failure"
require_line "$make_app_script" '^[[:space:]]*"\$lsregister" -f "\$app"[[:space:]]*$' "bundle Launch Services registration fails closed"
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
require_grammar_spec_validation_line '^cargo fmt --all -- --check[[:space:]]*$' "grammar spec fmt gate"
require_grammar_spec_validation_line '^cargo clippy --workspace --all-targets -- -D warnings[[:space:]]*$' "grammar spec clippy gate"
require_grammar_spec_validation_line '^cargo test --workspace --all-targets -- --test-threads=1[[:space:]]*$' "grammar spec workspace test gate"
require_grammar_spec_validation_line '^cargo build --workspace --all-targets[[:space:]]*$' "grammar spec workspace build gate"
require_grammar_spec_validation_line '^cargo build -p platform_macos --examples[[:space:]]*$' "grammar spec platform_macos examples build gate"
require_grammar_spec_validation_line '^bash -n tools/acceptance/\*\.sh tools/bundle/\*\.sh tools/release/\*\.sh[[:space:]]*$' "grammar spec script syntax gate"
require_grammar_spec_validation_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "grammar spec bundle metadata gate"
require_grammar_spec_validation_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "grammar spec bundle assembler self-test"
require_grammar_spec_validation_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "grammar spec E2E self-test"
require_grammar_spec_validation_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "grammar spec missing-model self-test"
require_grammar_spec_validation_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "grammar spec missing-model product smoke"
require_grammar_spec_validation_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "grammar spec A1b self-test"
require_grammar_spec_validation_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "grammar spec A2 self-test"
require_grammar_spec_validation_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "grammar spec model client feature policy"
require_grammar_spec_validation_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "grammar spec release policy check"
require_grammar_spec_validation_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "grammar spec cask updater self-test"
require_grammar_spec_validation_line '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "grammar spec root ignored model tests"
require_grammar_spec_validation_line '^cd tools/spike && cargo fmt -- --check[[:space:]]*$' "grammar spec spike fmt gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo clippy --all-targets -- -D warnings[[:space:]]*$' "grammar spec spike clippy gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo test[[:space:]]*$' "grammar spec spike test gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo build --bins[[:space:]]*$' "grammar spec spike build gate"
require_grammar_spec_validation_line '^cd tools/spike && COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "grammar spec spike ignored model tests"
