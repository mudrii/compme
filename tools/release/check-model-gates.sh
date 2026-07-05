#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
release_workflow="${1:-$repo_root/.github/workflows/release.yml}"
ci_workflow="$repo_root/.github/workflows/ci.yml"
gate_script="$repo_root/tools/release/run-model-gates.sh"
feature_script="$repo_root/tools/release/check-model-client-features.sh"
bundle_metadata_script="$repo_root/tools/bundle/check-bundle-metadata.sh"
make_app_script="$repo_root/tools/bundle/make-app.sh"
finalize_cask_script="$repo_root/tools/release/finalize-cask.sh"
notarize_script="$repo_root/tools/release/notarize-app.sh"
update_manifest_script="$repo_root/tools/release/write-update-manifest.sh"
acceptance_doc="$repo_root/docs/ACCEPTANCE.md"
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
publish_index = release_steps.index { |step| step["name"] == "Publish GitHub release" }
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
workflow = YAML.load_file(ARGV.fetch(0))
release_steps = workflow.fetch("jobs").fetch("release").fetch("steps")
import_index = release_steps.index { |step| step["name"] == "Import Developer ID certificate" }
build_index = release_steps.index { |step| step["name"] == "Build the .app bundle" }
abort("missing release gate: imports Developer ID certificate") unless import_index
abort("missing release gate: builds app bundle") unless build_index
abort("missing release gate: imports Developer ID certificate before build") unless import_index < build_index
import_step = release_steps.fetch(import_index)
import_env = import_step.fetch("env")
{
  "P12_BASE64" => "secrets.COMPME_DEVELOPER_ID_P12_BASE64",
  "P12_PASSWORD" => "secrets.COMPME_DEVELOPER_ID_P12_PASSWORD",
  "SIGNING_IDENTITY" => "secrets.COMPME_CODESIGN_IDENTITY",
}.each do |key, needle|
  abort("missing release gate: Developer ID secret #{key}") unless import_env.fetch(key).include?(needle)
end
import_run = import_step.fetch("run")
["for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY", "missing required release secret", "exit 1", "COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY"].each do |needle|
  abort("missing release gate: Developer ID import policy") unless import_run.include?(needle)
end
RUBY
  }

  good_release="$tmp_dir/good-release.yml"
  cat >"$good_release" <<'YAML'
jobs:
  release:
    steps:
      - name: Publish GitHub release
        uses: softprops/action-gh-release@v2
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
      - name: Publish GitHub release
        uses: softprops/action-gh-release@v2
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
      - name: Publish GitHub release
        uses: softprops/action-gh-release@v2
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
      - name: Publish GitHub release
        uses: softprops/action-gh-release@v2
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
      - name: Publish GitHub release
        uses: softprops/action-gh-release@v2
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
        run: tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  check_developer_id_fixture "$good_developer_id_release"

  missing_identity_export_release="$tmp_dir/missing-identity-export-release.yml"
  cat >"$missing_identity_export_release" <<'YAML'
jobs:
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
        run: tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  if check_developer_id_fixture "$missing_identity_export_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: missing Developer ID identity export was accepted" >&2
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

  release_workflow = YAML.load_file(ARGV.fetch(0))
  ci_workflow = YAML.load_file(ARGV.fetch(1))

  jobs = ci_workflow.fetch("jobs")
  ci_steps = jobs.fetch("check").fetch("steps")
  {
    "CI root format" => ["Format", "cargo fmt --all -- --check"],
    "CI root clippy" => ["Clippy (deny warnings)", "cargo clippy --workspace --all-targets -- -D warnings"],
    "CI root test" => ["Test", "cargo test --workspace --all-targets -- --test-threads=1"],
    "CI root build" => ["Build", "cargo build --workspace --all-targets"],
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
    "CI release model gate self-test" => ["Release model gate self-test", "tools/release/run-model-gates.sh --self-test"],
    "CI cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
    "CI cask finalizer" => ["Release cask finalizer self-test", "tools/release/finalize-cask.sh --self-test"],
    "CI notarization helper" => ["Notarization helper self-test", "tools/release/notarize-app.sh --self-test"],
    "CI update manifest" => ["Update manifest self-test", "tools/release/write-update-manifest.sh --self-test"],
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
    "release root format" => ["Root format", "cargo fmt --all -- --check"],
    "release root clippy" => ["Root clippy", "cargo clippy --workspace --all-targets -- -D warnings"],
    "release root test" => ["Root tests", "cargo test --workspace --all-targets -- --test-threads=1"],
    "release root build" => ["Root build", "cargo build --workspace --all-targets"],
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
    "release model gate self-test" => ["Release model gate self-test", "tools/release/run-model-gates.sh --self-test"],
    "release cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
    "release cask finalizer" => ["Release cask finalizer self-test", "tools/release/finalize-cask.sh --self-test"],
    "release notarization helper" => ["Notarization helper self-test", "tools/release/notarize-app.sh --self-test"],
    "release update manifest" => ["Update manifest self-test", "tools/release/write-update-manifest.sh --self-test"],
  }.each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(validate_steps, name, run)
  end

  release_needs = Array(release.fetch("needs"))
  abort("missing release gate: release job depends on validate") unless release_needs.include?("validate")
  release_steps = release.fetch("steps")
  import_index = release_steps.index { |step| step["name"] == "Import Developer ID certificate" }
  build_index = release_steps.index { |step| step["name"] == "Build the .app bundle" }
  abort("missing release gate: imports Developer ID certificate") unless import_index
  abort("missing release gate: builds app bundle") unless build_index
  abort("missing release gate: imports Developer ID certificate before build") unless import_index < build_index
  import_step = release_steps.fetch(import_index)
  import_env = import_step.fetch("env")
  {
    "P12_BASE64" => "secrets.COMPME_DEVELOPER_ID_P12_BASE64",
    "P12_PASSWORD" => "secrets.COMPME_DEVELOPER_ID_P12_PASSWORD",
    "SIGNING_IDENTITY" => "secrets.COMPME_CODESIGN_IDENTITY",
  }.each do |key, needle|
    abort("missing release gate: Developer ID secret #{key}") unless import_env.fetch(key).include?(needle)
  end
  import_run = import_step.fetch("run")
  ["for name in P12_BASE64 P12_PASSWORD SIGNING_IDENTITY", "missing required release secret", "exit 1"].each do |needle|
    abort("missing release gate: Developer ID missing-secret failure loop") unless import_run.include?(needle)
  end
  abort("missing release gate: Developer ID identity exported to bundle build") unless import_run.include?("COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY")
  publish_index = release_steps.index { |step| step["name"] == "Publish GitHub release" }
  cask_index = release_steps.index { |step| step["name"] == "Finalize Homebrew cask" }
  abort("missing release gate: publishes GitHub release") unless publish_index
  abort("missing release gate: finalizes Homebrew cask") unless cask_index
  abort("missing release gate: finalizes Homebrew cask after publishing release") unless cask_index > publish_index
  cask_step = release_steps.fetch(cask_index)
  abort("missing release gate: finalizes Homebrew cask") unless cask_step
  cask_run = cask_step.fetch("run")
  require_active_finalizer_command!(cask_run, %q(tools/release/finalize-cask.sh "$TAG" "$artifact_path" "$VERSION" "$DEFAULT_BRANCH"))
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
bash -n "$finalize_cask_script"
bash -n "$notarize_script"
bash -n "$update_manifest_script"
"$bundle_metadata_script" >/dev/null
"$make_app_script" --self-test >/dev/null
"$gate_script" --self-test >/dev/null
"$finalize_cask_script" --self-test >/dev/null
"$notarize_script" --self-test >/dev/null
"$update_manifest_script" --self-test >/dev/null

require_line "$readme_doc" "workspace of ${workspace_members_count}$" "README workspace member count"
require_line "$development_doc" "workspace with ${workspace_members_count}[[:space:]]+members" "DEVELOPMENT workspace member count"
require_line "$readme_doc" "roughly ${workspace_test_count_commas}[[:space:]]+tests" "README workspace test count"
require_line "$development_doc" "~${workspace_test_count}[[:space:]]+tests" "DEVELOPMENT workspace test count"
require_line "$roadmap_doc" "≈${workspace_test_count}[[:space:]]+workspace tests" "ROADMAP workspace test count"
require_line "$roadmap_doc" "${workspace_test_count}[[:space:]]+tests, clippy clean" "ROADMAP readiness test count"
require_line "$grammar_spec" "≈${workspace_test_count}[[:space:]]+tests green" "grammar spec prerequisite test count"
if grep -Eq 'sha256 "0{64}"' "$cask_file"; then
  require_line "$roadmap_doc" 'first real release' "ROADMAP first release pending status"
  require_readme_homebrew_line 'Homebrew cask install is not available until the first signed `v\*` release' "README Homebrew pre-release blocked status"
  require_readme_homebrew_line 'Until then, build from' "README Homebrew source fallback"
else
  reject_readme_homebrew_line 'Homebrew cask install is not available until the first signed `v\*` release' "README Homebrew pre-release blocked status after first tag"
  reject_readme_homebrew_line 'Until then, build from' "README Homebrew source fallback after first tag"
fi
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
require_line "$feature_script" 'llama-cpp-2 feature "metal"' "model_client macOS Metal feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "dynamic-backends"' "model_client non-macOS dynamic backend assertion"
require_line "$feature_script" 'llama-cpp-2 feature "vulkan"' "model_client non-macOS Vulkan feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "default"' "model_client default feature denial"
require_line "$feature_script" 'spike macOS' "spike feature policy assertion"
require_line "$gate_script" '^model="\$\{COMPME_MODEL_GATE_PATH:-tools/spike/models/qwen2\.5-0\.5b-q4_k_m\.gguf\}"[[:space:]]*$' "pinned base GGUF model path"
require_line "$gate_script" '^url="\$\{COMPME_MODEL_GATE_URL:-https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2\.5-0\.5b-q4_k_m\.gguf\}"[[:space:]]*$' "pinned base GGUF download URL"
require_line "$gate_script" '^expected="\$\{COMPME_MODEL_GATE_SHA256:-ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484\}"[[:space:]]*$' "pinned base GGUF sha256"
require_line "$gate_script" 'COMPME_MODEL_GATE_CURL_BODY="wrong-model"' "model gate checksum failure self-test"
require_line "$finalize_cask_script" 'git merge-base --is-ancestor "\$GITHUB_SHA" "origin/\$default_branch"' "cask finalizer ancestry check"
require_line "$finalize_cask_script" 'refusing to publish a stale or out-of-order cask update' "cask finalizer stale version refusal"
require_line "$finalize_cask_script" 'COMPME_CASK_ARTIFACT="\$artifact_path" tools/release/update-cask\.sh "\$tag"' "cask finalizer artifact handoff"
require_line "$finalize_cask_script" 'git push origin "HEAD:\$default_branch"' "cask finalizer push"
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
require_line "$acceptance_doc" 'overlay-correction-presenter' "acceptance docs correction overlay gate"
require_line "$acceptance_doc" 'Apps policy grid' "acceptance docs Apps policy LOOK gate"
require_line "$acceptance_doc" 'Personalization pane' "acceptance docs Personalization LOOK gate"
require_line "$acceptance_doc" '^--allow-manual[[:space:]]*$' "acceptance docs A1b allow-manual option"
require_line "$acceptance_doc" '^Use `--allow-manual` only after executing and recording the MANUAL checklist$' "acceptance docs A1b allow-manual policy"
require_line "$acceptance_doc" '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "acceptance docs A2 self-test"
require_line "$acceptance_doc" '^tools/release/check-model-client-features\.sh[[:space:]]*$' "acceptance docs model client feature policy"
require_line "$acceptance_doc" '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "acceptance docs model gate self-test"
require_line "$acceptance_doc" '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "acceptance docs cask updater self-test"
require_line "$acceptance_doc" '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "acceptance docs cask finalizer self-test"
require_line "$acceptance_doc" '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "acceptance docs notarization helper self-test"
require_line "$acceptance_doc" '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "acceptance docs update manifest self-test"
require_line "$releasing_doc" '^[[:space:]]*COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "release docs serialized root ignored model tests"
require_line "$releasing_doc" '^[[:space:]]*COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "release docs serialized spike ignored model tests"
require_line "$releasing_doc" 'tools/bundle/check-bundle-metadata\.sh' "release docs bundle metadata check"
require_line "$releasing_doc" 'tools/release/run-model-gates\.sh --self-test' "release docs model gate self-test"
require_line "$releasing_doc" 'tools/release/finalize-cask\.sh --self-test' "release docs cask finalizer self-test"
require_line "$releasing_doc" 'tools/bundle/make-app\.sh --self-test' "release docs bundle assembler self-test"
require_line "$releasing_doc" 'tools/acceptance/missing-model-startup\.sh --self-test' "release docs missing-model startup self-test"
require_line "$releasing_doc" 'tools/acceptance/missing-model-startup\.sh`' "release docs missing-model startup product smoke"
require_line "$releasing_doc" 'tools/release/check-model-client-features\.sh' "release docs model client feature policy"
require_line "$releasing_doc" 'tools/release/update-cask\.sh --self-test' "release docs cask updater self-test"
require_line "$releasing_doc" 'tools/release/notarize-app\.sh --self-test' "release docs notarization helper self-test"
require_line "$releasing_doc" 'tools/release/write-update-manifest\.sh --self-test' "release docs update manifest self-test"
require_line "$releasing_doc" 'git pull --ff-only origin main' "release docs require up-to-date default branch before tag"
require_line "$releasing_doc" 'cask finalizer refuses to update `main`' "release docs cask finalizer ancestry guard"
require_line "$repo_root/tools/acceptance/run-a1b-live-gates.sh" 'overlay-correction-presenter' "A1b runner correction overlay gate"
require_line "$repo_root/tools/acceptance/run-a1b-live-gates.sh" 'apps-policy-toggle-look' "A1b runner Apps policy LOOK gate"
require_line "$repo_root/tools/acceptance/run-a1b-live-gates.sh" 'personalization-pane-look' "A1b runner Personalization LOOK gate"
require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "README bundle metadata check"
require_readme_gate_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "README bundle assembler self-test"
require_readme_gate_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "README E2E self-test"
require_readme_gate_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "README missing-model startup self-test"
require_readme_gate_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "README missing-model startup product smoke"
require_readme_gate_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "README A1b self-test"
require_readme_gate_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "README A2 self-test"
require_readme_gate_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "README model client feature policy"
require_readme_gate_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "README release gate policy check"
require_readme_gate_line '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "README model gate self-test"
require_readme_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "README cask updater self-test"
require_readme_gate_line '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "README cask finalizer self-test"
require_readme_gate_line '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "README notarization helper self-test"
require_readme_gate_line '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "README update manifest self-test"
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
require_development_gate_line '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "DEVELOPMENT model gate self-test"
require_development_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "DEVELOPMENT cask updater self-test"
require_development_gate_line '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "DEVELOPMENT cask finalizer self-test"
require_development_gate_line '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "DEVELOPMENT notarization helper self-test"
require_development_gate_line '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "DEVELOPMENT update manifest self-test"
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
require_grammar_spec_validation_line '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "grammar spec model gate self-test"
require_grammar_spec_validation_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "grammar spec cask updater self-test"
require_grammar_spec_validation_line '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "grammar spec cask finalizer self-test"
require_grammar_spec_validation_line '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "grammar spec notarization helper self-test"
require_grammar_spec_validation_line '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "grammar spec update manifest self-test"
require_grammar_spec_validation_line '^COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "grammar spec root ignored model tests"
require_grammar_spec_validation_line '^cd tools/spike && cargo fmt -- --check[[:space:]]*$' "grammar spec spike fmt gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo clippy --all-targets -- -D warnings[[:space:]]*$' "grammar spec spike clippy gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo test[[:space:]]*$' "grammar spec spike test gate"
require_grammar_spec_validation_line '^cd tools/spike && cargo build --bins[[:space:]]*$' "grammar spec spike build gate"
require_grammar_spec_validation_line '^cd tools/spike && COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "grammar spec spike ignored model tests"
