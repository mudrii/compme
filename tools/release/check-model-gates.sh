#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
release_workflow="${1:-$repo_root/.github/workflows/release.yml}"
ci_workflow="$repo_root/.github/workflows/ci.yml"
gate_script="$repo_root/tools/release/run-model-gates.sh"
a2_matrix_ledger_script="$repo_root/tools/release/check-a2-matrix-ledger.sh"
feature_script="$repo_root/tools/release/check-model-client-features.sh"
privacy_script="$repo_root/tools/release/check-privacy-policy.sh"
bundle_metadata_script="$repo_root/tools/bundle/check-bundle-metadata.sh"
make_app_script="$repo_root/tools/bundle/make-app.sh"
bundle_smoke_script="$repo_root/tools/bundle/bundle-smoke.sh"
finalize_cask_script="$repo_root/tools/release/finalize-cask.sh"
notarize_script="$repo_root/tools/release/notarize-app.sh"
update_manifest_script="$repo_root/tools/release/write-update-manifest.sh"
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
  "chmod 600 \"$p12\"",
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
def full_sha_action_ref?(uses)
  uses.is_a?(String) && uses.match?(/\A[^@\s]+@[0-9a-f]{40}\z/)
end
jobs.each do |job_name, job|
  Array(job["steps"]).each do |step|
    next unless step.key?("uses")
    abort("missing release gate: #{job_name} action is pinned to a full commit SHA") unless full_sha_action_ref?(step["uses"])
  end
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

  check_live_a2_ledger_fixture() {
    ruby -ryaml - "$1" <<'RUBY'
def active_shell_lines(run)
  run.lines.map do |line|
    stripped = line.strip
    next if stripped.empty? || stripped.start_with?("#")
    stripped.sub(/[[:space:]]+#.*$/, "")
  end.compact
end

workflow = YAML.load_file(ARGV.fetch(0))
steps = workflow.fetch("jobs").fetch("validate").fetch("steps")
step = steps.find { |candidate| candidate["name"] == "A2 matrix ledger live proof" }
abort("missing release gate: release validates live A2 matrix ledger") unless step
env = step.fetch("env")
abort("missing release gate: release A2 ledger reads COMPME_A2_MATRIX_LEDGER") unless env.fetch("COMPME_A2_MATRIX_LEDGER").to_s.include?("COMPME_A2_MATRIX_LEDGER")
run = step.fetch("run")
abort("missing release gate: release A2 live ledger variable guard") unless run.include?("missing required release variable: COMPME_A2_MATRIX_LEDGER")
abort("missing release gate: release A2 live ledger path guard") unless run.include?("COMPME_A2_MATRIX_LEDGER must be a committed repo-relative TSV under tools/acceptance/evidence/a2/")
abort("missing release gate: release A2 live ledger runs checker") unless active_shell_lines(run).include?("tools/release/check-a2-matrix-ledger.sh \"$COMPME_A2_MATRIX_LEDGER\"")
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

  good_a2_live_release="$tmp_dir/good-a2-live-release.yml"
  cat >"$good_a2_live_release" <<'YAML'
jobs:
  validate:
    steps:
      - name: A2 matrix ledger live proof
        env:
          COMPME_A2_MATRIX_LEDGER: ${{ vars.COMPME_A2_MATRIX_LEDGER }}
        run: |
          if [ -z "${COMPME_A2_MATRIX_LEDGER:-}" ]; then
            echo "missing required release variable: COMPME_A2_MATRIX_LEDGER" >&2
            exit 1
          fi
          echo "COMPME_A2_MATRIX_LEDGER must be a committed repo-relative TSV under tools/acceptance/evidence/a2/"
          tools/release/check-a2-matrix-ledger.sh "$COMPME_A2_MATRIX_LEDGER"
YAML
  check_live_a2_ledger_fixture "$good_a2_live_release"

  echoed_a2_live_release="$tmp_dir/echoed-a2-live-release.yml"
  cat >"$echoed_a2_live_release" <<'YAML'
jobs:
  validate:
    steps:
      - name: A2 matrix ledger live proof
        env:
          COMPME_A2_MATRIX_LEDGER: ${{ vars.COMPME_A2_MATRIX_LEDGER }}
        run: |
          if [ -z "${COMPME_A2_MATRIX_LEDGER:-}" ]; then
            echo "missing required release variable: COMPME_A2_MATRIX_LEDGER" >&2
            exit 1
          fi
          echo "COMPME_A2_MATRIX_LEDGER must be a committed repo-relative TSV under tools/acceptance/evidence/a2/"
          # tools/release/check-a2-matrix-ledger.sh "$COMPME_A2_MATRIX_LEDGER"
          echo 'tools/release/check-a2-matrix-ledger.sh "$COMPME_A2_MATRIX_LEDGER"'
YAML
  if check_live_a2_ledger_fixture "$echoed_a2_live_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: commented/echoed A2 ledger command was accepted" >&2
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
          printf '%s' "$P12_BASE64" | base64 --decode > "$p12"
          chmod 600 "$p12"
          rm -f "$p12"
          trap - EXIT
          echo "COMPME_CODESIGN_IDENTITY=$SIGNING_IDENTITY" >> "$GITHUB_ENV"
      - name: Build the .app bundle
        run: COMPME_BUNDLE_SKIP_BUILD=1 tools/bundle/make-app.sh "$RUNNER_TEMP/bundle"
YAML
  check_developer_id_fixture "$good_developer_id_release"

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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
  windows:
    runs-on: windows-latest
    steps:
      - uses: dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30
  linux:
    runs-on: ubuntu-latest
    steps:
      - uses: Swatinem/rust-cache@42dc69e1aa15d09112580998cf2ef0119e2e91ae
  release:
    needs: [validate, windows, linux]
    environment: release
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
        with:
          fetch-depth: 0
YAML
  if check_release_hardening_fixture "$mutable_prereq_action_release" >/dev/null 2>&1; then
    echo "release gate self-test failed: mutable prerequisite action ref was accepted" >&2
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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
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
publish_index = publish_steps.index { |step| step["name"] == "Publish GitHub release" }
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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
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
      - name: Publish GitHub release
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
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
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
      - name: Publish GitHub release
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

  def require_live_a2_ledger_step!(steps)
    step = steps.find { |candidate| candidate.is_a?(Hash) && candidate["name"] == "A2 matrix ledger live proof" }
    abort("missing release gate: release validates live A2 matrix ledger") unless step
    env = step.fetch("env")
    abort("missing release gate: release A2 ledger reads COMPME_A2_MATRIX_LEDGER") unless env.fetch("COMPME_A2_MATRIX_LEDGER").to_s.include?("COMPME_A2_MATRIX_LEDGER")
    run = step.fetch("run")
    lines = active_shell_lines(run)
    [
      "missing required release variable: COMPME_A2_MATRIX_LEDGER",
      "COMPME_A2_MATRIX_LEDGER must be a committed repo-relative TSV under tools/acceptance/evidence/a2/",
    ].each do |needle|
      abort("missing release gate: release A2 live ledger #{needle}") unless run.include?(needle)
    end
    abort("missing release gate: release A2 live ledger runs checker") unless lines.include?("tools/release/check-a2-matrix-ledger.sh \"$COMPME_A2_MATRIX_LEDGER\"")
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

  release_workflow = YAML.load_file(ARGV.fetch(0))
  ci_workflow = YAML.load_file(ARGV.fetch(1))

  def rust_toolchain_step_valid?(step)
    step["uses"].to_s.start_with?("dtolnay/rust-toolchain@") &&
      step.fetch("with").fetch("toolchain") == "1.96.1"
  end

  jobs = ci_workflow.fetch("jobs")
  abort("missing release gate: CI workflow defaults to read-only contents permission") unless ci_workflow.fetch("permissions").fetch("contents") == "read"
  def full_sha_action_ref?(uses)
    uses.is_a?(String) && uses.match?(/\A[^@\s]+@[0-9a-f]{40}\z/)
  end
  jobs.each do |job_name, job|
    Array(job["steps"]).each do |step|
      next unless step.key?("uses")
      abort("missing release gate: CI #{job_name} action is pinned to a full commit SHA") unless full_sha_action_ref?(step["uses"])
    end
  end
  jobs.each do |job_name, job|
    next unless %w[check spike windows linux].include?(job_name)
    abort("missing release gate: CI #{job_name} pins Rust toolchain") unless Array(job["steps"]).any? { |step| step.is_a?(Hash) && rust_toolchain_step_valid?(step) }
  end
  ci_steps = jobs.fetch("check").fetch("steps")
  # Gate steps required verbatim in BOTH the CI check job and the release
  # validate job; per-workflow extras are merged in at each call site.
  shared_gate_steps = {
    "script syntax" => ["Script syntax", "bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh"],
    "bundle metadata" => ["Bundle metadata", "tools/bundle/check-bundle-metadata.sh"],
    "bundle metadata self-test" => ["Bundle metadata self-test", "tools/bundle/check-bundle-metadata.sh --self-test"],
    "bundle assembler self-test" => ["Bundle assembler self-test", "tools/bundle/make-app.sh --self-test"],
    "bundle smoke" => ["Bundle smoke", "tools/bundle/bundle-smoke.sh"],
    "bundle smoke self-test" => ["Bundle smoke self-test", "tools/bundle/bundle-smoke.sh --self-test"],
    "E2E self-test" => ["E2E runner self-test", "tools/acceptance/e2e-complete-me.sh --self-test"],
    "missing-model startup self-test" => ["Missing-model startup self-test", "tools/acceptance/missing-model-startup.sh --self-test"],
    "missing-model startup product smoke" => ["Missing-model startup product smoke", "tools/acceptance/missing-model-startup.sh"],
    "UI-assisted session self-test" => ["UI-assisted session self-test", "tools/acceptance/run-ui-assisted-session.sh --self-test"],
    "A1b self-test" => ["A1b runner self-test", "tools/acceptance/run-a1b-live-gates.sh --self-test"],
    "A2 self-test" => ["A2 compatibility runner self-test", "tools/acceptance/run-a2-compat-gates.sh --self-test"],
    "A2 matrix ledger self-test" => ["A2 matrix ledger policy self-test", "tools/release/check-a2-matrix-ledger.sh --self-test"],
    "model client feature policy" => ["Model client feature policy", "tools/release/check-model-client-features.sh"],
    "model client feature policy self-test" => ["Model client feature policy self-test", "tools/release/check-model-client-features.sh --self-test"],
    "agent brief alignment" => ["Agent brief alignment", "tools/release/check-agent-briefs.sh"],
    "agent brief alignment self-test" => ["Agent brief alignment self-test", "tools/release/check-agent-briefs.sh --self-test"],
    "privacy policy" => ["Privacy policy", "tools/release/check-privacy-policy.sh"],
    "privacy policy self-test" => ["Privacy policy self-test", "tools/release/check-privacy-policy.sh --self-test"],
    "model gate policy" => ["Release model gate policy", "bash tools/release/check-model-gates.sh"],
    "model gate self-test" => ["Release model gate self-test", "tools/release/run-model-gates.sh --self-test"],
    "cask updater" => ["Release cask updater self-test", "tools/release/update-cask.sh --self-test"],
    "cask finalizer" => ["Release cask finalizer self-test", "tools/release/finalize-cask.sh --self-test"],
    "notarization helper" => ["Notarization helper self-test", "tools/release/notarize-app.sh --self-test"],
    "update manifest" => ["Update manifest self-test", "tools/release/write-update-manifest.sh --self-test"],
  }
  {
    "CI root format" => ["Format", "cargo fmt --all -- --check"],
    "CI root clippy" => ["Clippy (deny warnings)", "cargo clippy --locked --workspace --all-targets -- -D warnings"],
    "CI root test" => ["Test", "cargo test --locked --workspace --all-targets -- --test-threads=1"],
    "CI root build" => ["Build", "cargo build --locked --workspace --all-targets"],
    "CI platform_macos examples build" => ["Build macOS acceptance examples", "cargo build --locked -p platform_macos --examples"],
  }.merge(shared_gate_steps.transform_keys { |key| "CI #{key}" }).each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(ci_steps, name, run)
  end

  # CI windows/linux jobs gate the whole portable workspace (everything but
  # the Apple-only platform_macos crate) plus the app binary through its
  # fail-closed shell facade. The release workflow keeps the narrower
  # adapter-crate pins below until non-macOS artifacts exist.
  windows = jobs.fetch("windows")
  abort("missing release gate: platform_windows runs on Windows") unless windows.fetch("runs-on") == "windows-latest"
  require_step!(jobs, "windows", "Format (workspace)", "cargo fmt --all -- --check", "platform_windows fmt job")
  require_step!(jobs, "windows", "Clippy portable workspace (deny warnings)", "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings", "platform_windows clippy job")
  require_step!(jobs, "windows", "Test portable workspace", "cargo test --locked --workspace --exclude platform_macos", "platform_windows test job")
  require_step!(jobs, "windows", "Build app binary", "cargo build --locked -p app", "platform_windows build job")

  linux = jobs.fetch("linux")
  abort("missing release gate: platform_linux runs on Linux") unless linux.fetch("runs-on") == "ubuntu-latest"
  require_step!(jobs, "linux", "Format (workspace)", "cargo fmt --all -- --check", "platform_linux fmt job")
  require_step!(jobs, "linux", "Clippy portable workspace (deny warnings)", "cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings", "platform_linux clippy job")
  require_step!(jobs, "linux", "Test portable workspace", "cargo test --locked --workspace --exclude platform_macos", "platform_linux test job")
  require_step!(jobs, "linux", "Build app binary", "cargo build --locked -p app", "platform_linux build job")

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
  preflight_tag = preflight_steps.find { |step| step["name"] == "Verify release tag is semver and on default branch" }
  abort("missing release gate: preflight verifies tag ancestry") unless preflight_tag
  preflight_run = preflight_tag.fetch("run")
  ["release tag must look like vX.Y.Z", "git fetch origin \"$DEFAULT_BRANCH\"", "git merge-base --is-ancestor \"$GITHUB_SHA\" \"origin/$DEFAULT_BRANCH\""].each do |needle|
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
  require_step!(release_jobs, "windows", "Format", "cargo fmt -p platform_windows -- --check", "release platform_windows fmt job")
  require_step!(release_jobs, "windows", "Clippy", "cargo clippy --locked -p platform_windows --all-targets -- -D warnings", "release platform_windows clippy job")
  require_step!(release_jobs, "windows", "Test", "cargo test --locked -p platform_windows", "release platform_windows test job")
  require_step!(release_jobs, "windows", "Build", "cargo build --locked -p platform_windows", "release platform_windows build job")
  linux = release_jobs.fetch("linux")
  abort("missing release gate: release platform_linux runs on Linux") unless linux.fetch("runs-on") == "ubuntu-latest"
  require_step!(release_jobs, "linux", "Format", "cargo fmt -p platform_linux -- --check", "release platform_linux fmt job")
  require_step!(release_jobs, "linux", "Clippy", "cargo clippy --locked -p platform_linux --all-targets -- -D warnings", "release platform_linux clippy job")
  require_step!(release_jobs, "linux", "Test", "cargo test --locked -p platform_linux", "release platform_linux test job")
  require_step!(release_jobs, "linux", "Build", "cargo build --locked -p platform_linux", "release platform_linux build job")
  prebuild = release_jobs.fetch("prebuild")
  build_release = release_jobs.fetch("build_release")
  publish_release = release_jobs.fetch("publish_release")
  quote = 39.chr
  tag_job_guard = "${{ github.ref_type == #{quote}tag#{quote} && startsWith(github.ref_name, #{quote}v#{quote}) }}"
  abort("missing release gate: prebuild is limited to v* tag refs") unless prebuild.fetch("if") == tag_job_guard
  abort("missing release gate: build_release is limited to v* tag refs") unless build_release.fetch("if") == tag_job_guard
  abort("missing release gate: publish_release is limited to v* tag refs") unless publish_release.fetch("if") == tag_job_guard
  release_jobs.each do |job_name, job|
    Array(job["steps"]).each do |step|
      next unless step.key?("uses")
      abort("missing release gate: #{job_name} action is pinned to a full commit SHA") unless full_sha_action_ref?(step["uses"])
    end
  end

  {
    "release root format" => ["Root format", "cargo fmt --all -- --check"],
    "release root clippy" => ["Root clippy", "cargo clippy --locked --workspace --all-targets -- -D warnings"],
    "release root test" => ["Root tests", "cargo test --locked --workspace --all-targets -- --test-threads=1"],
    "release root build" => ["Root build", "cargo build --locked --workspace --all-targets"],
    "release workflow invokes model gate script" => ["Model-backed release gates", "bash tools/release/run-model-gates.sh"],
  }.merge(shared_gate_steps.transform_keys { |key| "release #{key}" }).each do |label, (name, run)|
    abort("missing release gate: #{label}") unless step?(validate_steps, name, run)
  end
  require_live_a2_ledger_step!(validate_steps)

  prebuild_needs = Array(prebuild.fetch("needs"))
  %w[validate windows linux].each do |job|
    abort("missing release gate: prebuild job depends on #{job}") unless prebuild_needs.include?(job)
  end
  build_release_needs = Array(build_release.fetch("needs"))
  abort("missing release gate: build_release job depends on prebuild") unless build_release_needs.include?("prebuild")
  publish_release_needs = Array(publish_release.fetch("needs"))
  abort("missing release gate: publish_release job depends on build_release") unless publish_release_needs.include?("build_release")
  # The prebuild job compiles third-party code (build.rs, proc-macros) and must
  # therefore stay completely secretless: no protected environment, no secret
  # references anywhere in the job.
  abort("missing release gate: prebuild job must not use a protected environment") unless prebuild["environment"].nil?
  abort("missing release gate: prebuild job carries no secret references") if contains_secret_reference?(prebuild)
  abort("missing release gate: prebuild job has read-only contents permission") unless prebuild.fetch("permissions").fetch("contents") == "read"
  abort("missing release gate: build_release uses protected release environment") unless build_release.fetch("environment") == "release"
  abort("missing release gate: publish_release uses protected release environment") unless publish_release.fetch("environment") == "release"
  abort("missing release gate: build_release job has read-only contents permission") unless build_release.fetch("permissions").fetch("contents") == "read"
  abort("missing release gate: publish_release job has contents write permission") unless publish_release.fetch("permissions").fetch("contents") == "write"
  preflight_steps = release_jobs.fetch("preflight").fetch("steps")
  protected_tag_step = preflight_steps.find { |step| step["name"] == "Verify release tag is semver and on default branch" }
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
  ancestry_index = prebuild_steps.index { |step| step["name"] == "Verify release tag is on default branch" }
  scrub_index = prebuild_steps.index { |step| step["name"] == "Scrub persisted git credentials" }
  rust_index = prebuild_steps.index { |step| step["name"] == "Install Rust (stable)" }
  prebuild_metadata_index = prebuild_steps.index { |step| step["name"] == "Check release tag matches bundle metadata" }
  prebuild_index = prebuild_steps.index { |step| step["name"] == "Prebuild release binary (no signing secrets in this job)" }
  prebuild_upload_index = prebuild_steps.index { |step| step["name"] == "Upload prebuilt release binary" }
  metadata_index = build_steps.index { |step| step["name"] == "Check release tag matches bundle metadata" }
  download_binary_index = build_steps.index { |step| step["name"] == "Download prebuilt release binary" }
  chmod_index = build_steps.index { |step| step["name"] == "Restore prebuilt binary executable bit" }
  import_index = build_steps.index { |step| step["name"] == "Import Developer ID certificate" }
  build_index = build_steps.index { |step| step["name"] == "Build the .app bundle" }
  notarize_index = build_steps.index { |step| step["name"] == "Notarize and staple the .app" }
  cleanup_index = build_steps.index { |step| step["name"] == "Delete signing keychain" }
  package_index = build_steps.index { |step| step["name"] == "Package + checksum" }
  manifest_index = build_steps.index { |step| step["name"] == "Write update manifest" }
  upload_index = build_steps.index { |step| step["name"] == "Upload release artifacts" }
  abort("missing release gate: verifies tag ancestry in prebuild job") unless ancestry_index
  abort("missing release gate: scrubs persisted git credentials") unless scrub_index
  abort("missing release gate: installs Rust in prebuild") unless rust_index
  abort("missing release gate: checks release tag metadata in prebuild") unless prebuild_metadata_index
  abort("missing release gate: prebuilds release binary in secretless job") unless prebuild_index
  abort("missing release gate: uploads prebuilt release binary") unless prebuild_upload_index
  abort("missing release gate: checks release tag metadata") unless metadata_index
  abort("missing release gate: downloads prebuilt release binary") unless download_binary_index
  abort("missing release gate: restores prebuilt binary executable bit") unless chmod_index
  abort("missing release gate: imports Developer ID certificate") unless import_index
  abort("missing release gate: builds app bundle") unless build_index
  abort("missing release gate: notarizes and staples app") unless notarize_index
  abort("missing release gate: deletes signing keychain") unless cleanup_index
  abort("missing release gate: packages release artifact") unless package_index
  abort("missing release gate: writes update manifest") unless manifest_index
  abort("missing release gate: uploads release artifacts from read-only build job") unless upload_index
  abort("missing release gate: verifies tag ancestry before third-party build code") unless ancestry_index < prebuild_index
  abort("missing release gate: scrubs persisted git credentials after ancestry check") unless ancestry_index < scrub_index
  abort("missing release gate: scrubs persisted git credentials before Rust/build code") unless scrub_index < rust_index
  abort("missing release gate: checks release tag metadata before prebuild") unless prebuild_metadata_index < prebuild_index
  abort("missing release gate: prebuilds release binary before artifact upload") unless prebuild_index < prebuild_upload_index
  abort("missing release gate: checks release tag metadata before Developer ID secrets") unless metadata_index < import_index
  abort("missing release gate: downloads prebuilt binary before Developer ID import") unless download_binary_index < import_index
  abort("missing release gate: restores executable bit after download and before bundle build") unless download_binary_index < chmod_index && chmod_index < build_index
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
  abort("missing release gate: release artifact chain is build -> notarize -> cleanup -> package -> manifest -> upload") unless build_index < notarize_index && notarize_index < cleanup_index && cleanup_index < package_index && package_index < manifest_index && manifest_index < upload_index
  build_steps.each_with_index do |step, idx|
    next unless contains_secret_reference?(step)
    abort("missing release gate: checks release tag metadata before secret-bearing build step #{step["name"] || idx}") unless metadata_index < idx
    abort("missing release gate: downloads prebuilt binary before secret-bearing build step #{step["name"] || idx}") unless download_binary_index < idx
  end
  ancestry_run = prebuild_steps.fetch(ancestry_index).fetch("run")
  ["git fetch origin \"$DEFAULT_BRANCH\"", "git merge-base --is-ancestor \"$GITHUB_SHA\" \"origin/$DEFAULT_BRANCH\""].each do |needle|
    abort("missing release gate: early default-branch ancestry check") unless ancestry_run.include?(needle)
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
    "chmod 600 \"$p12\"",
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
  ["security delete-keychain \"$COMPME_SIGNING_KEYCHAIN\"", "unset COMPME_SIGNING_KEYCHAIN COMPME_CODESIGN_IDENTITY", "COMPME_SIGNING_KEYCHAIN=", "COMPME_CODESIGN_IDENTITY=", ">> \"$GITHUB_ENV\""].each do |needle|
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
  manifest_step = build_steps.fetch(manifest_index)
  abort("missing release gate: manifest step exposes manifest output") unless manifest_step.fetch("id") == "manifest"
  manifest_env = manifest_step.fetch("env")
  {
    "VERSION" => "${{ steps.pkg.outputs.version }}",
    "ZIP" => "${{ steps.pkg.outputs.zip }}",
    "SHA256" => "${{ steps.pkg.outputs.sha256 }}",
  }.each do |key, expected|
    abort("missing release gate: manifest consumes package output #{key}") unless manifest_env.fetch(key) == expected
  end
  manifest_run = manifest_step.fetch("run")
  [
    "manifest=\"compme-${VERSION}-update.json\"",
    "tools/release/write-update-manifest.sh \"$VERSION\" \"$ZIP\" \"$SHA256\" > \"$manifest\"",
    "echo \"manifest=$manifest\" >> \"$GITHUB_OUTPUT\"",
  ].each do |needle|
    abort("missing release gate: manifest step #{needle}") unless manifest_run.include?(needle)
  end
  download_index = publish_steps.index { |step| step["name"] == "Download release artifacts" }
  abort("missing release gate: downloads release artifacts in publish job") unless download_index
  checksum_index = publish_steps.index { |step| step["name"] == "Verify downloaded artifact checksum" }
  publish_index = publish_steps.index { |step| step["name"] == "Publish GitHub release" }
  cask_index = publish_steps.index { |step| step["name"] == "Finalize Homebrew cask" }
  abort("missing release gate: publishes GitHub release") unless publish_index
  abort("missing release gate: finalizes Homebrew cask") unless cask_index
  abort("missing release gate: verifies downloaded artifact checksum before publishing release") unless checksum_index && download_index < checksum_index && checksum_index < publish_index
  upload_step = build_steps.fetch(upload_index)
  upload_with = upload_step.fetch("with")
  abort("missing release gate: uploads artifacts with pinned upload-artifact action") unless upload_step.fetch("uses").match?(/\Aactions\/upload-artifact@[0-9a-f]{40}\z/)
  abort("missing release gate: uploads named release artifact bundle") unless upload_with.fetch("name") == "compme-release-artifacts"
  abort("missing release gate: upload fails if release artifact is missing") unless upload_with.fetch("if-no-files-found") == "error"
  upload_path = upload_with.fetch("path").to_s
  [
    "${{ steps.pkg.outputs.zip }}",
    "${{ steps.pkg.outputs.zip }}.sha256",
    "${{ steps.manifest.outputs.manifest }}",
  ].each do |needle|
    abort("missing release gate: upload includes #{needle}") unless upload_path.include?(needle)
  end
  download_step = publish_steps.fetch(download_index)
  download_with = download_step.fetch("with")
  abort("missing release gate: downloads artifacts with pinned download-artifact action") unless download_step.fetch("uses").match?(/\Aactions\/download-artifact@[0-9a-f]{40}\z/)
  abort("missing release gate: downloads named release artifact bundle") unless download_with.fetch("name") == "compme-release-artifacts"
  abort("missing release gate: downloads release artifacts into release-artifacts") unless download_with.fetch("path") == "release-artifacts"
  checksum_lines = active_shell_lines(publish_steps.fetch(checksum_index).fetch("run"))
  ["cd release-artifacts", "test -f \"$ZIP\"", "test -f \"$ZIP.sha256\"", "shasum -a 256 -c \"$ZIP.sha256\""].each do |needle|
    abort("missing release gate: verifies downloaded artifact checksum #{needle}") unless checksum_lines.include?(needle)
  end
  publish_step = publish_steps.fetch(publish_index)
  abort("missing release gate: publishes GitHub release as draft until artifacts attach") unless publish_step.fetch("with").fetch("draft") == true
  publish_files = publish_step.fetch("with").fetch("files").to_s
  [
    "release-artifacts/compme-*-macos.zip",
    "release-artifacts/compme-*-macos.zip.sha256",
    "release-artifacts/compme-*-update.json",
  ].each do |needle|
    abort("missing release gate: publishes downloaded artifact #{needle}") unless publish_files.include?(needle)
  end
  abort("missing release gate: finalizes Homebrew cask after publishing release") unless cask_index > publish_index
  undraft_index = publish_steps.index { |step| step["name"] == "Undraft GitHub release" }
  abort("missing release gate: undrafts GitHub release after publish and before cask finalization") unless undraft_index && undraft_index > publish_index && undraft_index < cask_index
  undraft_step = publish_steps.fetch(undraft_index)
  abort("missing release gate: undraft uses GitHub token") unless undraft_step.fetch("env").fetch("GH_TOKEN") == "${{ github.token }}"
  abort("missing release gate: undrafts current tag") unless undraft_step.fetch("run") == %q(gh release edit "$GITHUB_REF_NAME" --draft=false)
  cask_step = publish_steps.fetch(cask_index)
  abort("missing release gate: finalizes Homebrew cask") unless cask_step
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
bash -n "$a2_matrix_ledger_script"
bash -n "$feature_script"
bash -n "$privacy_script"
bash -n "$bundle_metadata_script"
bash -n "$make_app_script"
bash -n "$finalize_cask_script"
bash -n "$notarize_script"
bash -n "$update_manifest_script"
"$bundle_metadata_script" >/dev/null
"$make_app_script" --self-test >/dev/null
"$a2_matrix_ledger_script" --self-test >/dev/null
GITHUB_ACTIONS=true GITHUB_REF_TYPE=tag COMPME_MODEL_GATE_PATH=/tmp/compme-poisoned-model.gguf "$gate_script" --self-test >/dev/null
"$gate_script" --self-test >/dev/null
"$privacy_script" >/dev/null
"$privacy_script" --self-test >/dev/null
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
require_line "$cask_file" '^  url "https://github\.com/mudrii/compme/releases/download/v#\{version\}/compme-#\{version\}-macos\.zip"$' "Homebrew cask GitHub release URL"
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
require_line "$bundle_smoke_script" 'COMPME_RUN_MS="\${COMPME_RUN_MS:-1500}" COMPME_STUB_COMPLETION="\${COMPME_STUB_COMPLETION:- smoke}" "\$app_bin"' "bundle smoke runs packaged app hermetically"
require_line "$bundle_smoke_script" 'COMPME_BUNDLE_SMOKE_MAKE_APP' "bundle smoke make-app override"
require_line "$bundle_smoke_script" 'COMPME_BUNDLE_SMOKE_APP_EXIT=42' "bundle smoke self-test rejects app failure"
require_line "$feature_script" 'llama-cpp-2 feature "metal"' "model_client macOS Metal feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "dynamic-backends"' "model_client non-macOS dynamic backend assertion"
require_line "$feature_script" 'llama-cpp-2 feature "vulkan"' "model_client non-macOS Vulkan feature assertion"
require_line "$feature_script" 'llama-cpp-2 feature "default"' "model_client default feature denial"
require_line "$feature_script" 'spike macOS' "spike feature policy assertion"
require_line "$privacy_script" 'sentry' "privacy policy denied package assertion"
require_line "$privacy_script" 'segment\.io' "privacy policy denied host self-test"
require_readme_gate_line '^tools/release/check-privacy-policy\.sh[[:space:]]*$' "README privacy policy gate"
require_readme_gate_line '^tools/release/check-privacy-policy\.sh --self-test[[:space:]]*$' "README privacy policy self-test gate"
require_development_gate_line '^tools/release/check-privacy-policy\.sh[[:space:]]*$' "DEVELOPMENT privacy policy gate"
require_development_gate_line '^tools/release/check-privacy-policy\.sh --self-test[[:space:]]*$' "DEVELOPMENT privacy policy self-test gate"
require_line "$acceptance_doc" '^tools/release/check-privacy-policy\.sh[[:space:]]*$' "acceptance docs privacy policy gate"
require_line "$acceptance_doc" '^tools/release/check-privacy-policy\.sh --self-test[[:space:]]*$' "acceptance docs privacy policy self-test gate"
require_line "$bundle_metadata_script" 'release tag version is empty' "bundle metadata empty release-tag version rejection"
require_line "$gate_script" '^default_model="tools/spike/models/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "pinned base GGUF model path"
require_line "$gate_script" '^default_url="https://huggingface\.co/Brianpuz/Qwen2\.5-0\.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2\.5-0\.5b-q4_k_m\.gguf"[[:space:]]*$' "pinned base GGUF download URL"
require_line "$gate_script" '^default_expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"[[:space:]]*$' "pinned base GGUF sha256"
require_line "$gate_script" 'COMPME_ALLOW_MODEL_GATE_OVERRIDE' "release-context model gate override escape hatch"
require_line "$gate_script" 'refusing \$name override in GitHub release context' "release-context model gate override rejection"
require_line "$gate_script" 'COMPME_MODEL_GATE_CURL_BODY="wrong-model"' "model gate checksum failure self-test"
require_line "$gate_script" 'latency=1 gpu=0 ctx_tokens=256 spike_model= args=test --locked -p model_client --test latency' "model gate root env self-test"
require_line "$gate_script" 'tools/spike env=1 ctx= latency=1 gpu= ctx_tokens= spike_model=\$model_path args=test --locked --test model_integration' "model gate spike env self-test"
reject_line "$repo_root/crates/model_client/tests/latency.rs" 'Metal GPU' "root model-client ignored tests stale GPU wording"
require_line "$finalize_cask_script" 'git merge-base --is-ancestor "\$GITHUB_SHA" "origin/\$default_branch"' "cask finalizer ancestry check"
require_line "$finalize_cask_script" 'tag/version mismatch' "cask finalizer tag/version guard"
require_line "$finalize_cask_script" 'refusing to publish a stale or out-of-order cask update' "cask finalizer stale version refusal"
require_line "$finalize_cask_script" 'COMPME_CASK_ARTIFACT="\$artifact_path" tools/release/update-cask\.sh "\$tag"' "cask finalizer artifact handoff"
require_line "$finalize_cask_script" 'git push origin "HEAD:\$default_branch"' "cask finalizer push"
require_line "$gate_script" '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "serialized root ignored model tests"
require_line "$gate_script" '^  COMPME_SPIKE_MODEL_PATH="\$spike_model" COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "serialized spike ignored model tests"
require_line "$acceptance_doc" '^COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked -p model_client --test latency -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized root ignored model tests"
require_line "$acceptance_doc" '^COMPME_SPIKE_MODEL_PATH="\$PWD/models/qwen2\.5-0\.5b-q4_k_m\.gguf" COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_LATENCY_BUDGET=1 cargo test --locked --test model_integration -- --ignored --test-threads=1[[:space:]]*$' "acceptance docs serialized spike ignored model tests"
require_line "$acceptance_doc" '^cargo build --locked -p platform_macos --examples[[:space:]]*$' "acceptance docs platform_macos examples build"
require_line "$acceptance_doc" '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "acceptance docs bundle metadata check"
require_line "$acceptance_doc" '^tools/bundle/check-bundle-metadata\.sh --self-test[[:space:]]*$' "acceptance docs bundle metadata self-test"
require_line "$acceptance_doc" '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "acceptance docs bundle assembler self-test"
require_line "$acceptance_doc" '^tools/bundle/bundle-smoke\.sh[[:space:]]*$' "acceptance docs bundle smoke"
require_line "$acceptance_doc" '^tools/bundle/bundle-smoke\.sh --self-test[[:space:]]*$' "acceptance docs bundle smoke self-test"
require_line "$acceptance_doc" '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "acceptance docs E2E self-test"
require_line "$acceptance_doc" '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "acceptance docs missing-model startup self-test"
require_line "$acceptance_doc" '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "acceptance docs missing-model startup product smoke"
require_line "$acceptance_doc" '^tools/acceptance/run-ui-assisted-session\.sh --self-test[[:space:]]*$' "acceptance docs UI-assisted session self-test"
require_line "$acceptance_doc" '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "acceptance docs A1b self-test"
require_line "$acceptance_doc" 'overlay-correction-presenter' "acceptance docs correction overlay gate"
require_line "$acceptance_doc" 'Apps policy grid' "acceptance docs Apps policy LOOK gate"
require_line "$acceptance_doc" 'Personalization pane' "acceptance docs Personalization LOOK gate"
require_line "$acceptance_doc" '^--allow-manual[[:space:]]*$' "acceptance docs A1b allow-manual option"
require_line "$acceptance_doc" '^Use `--allow-manual` only after executing and recording the MANUAL checklist$' "acceptance docs A1b allow-manual policy"
require_line "$acceptance_doc" '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "acceptance docs A2 self-test"
require_line "$acceptance_doc" '^tools/release/check-model-client-features\.sh[[:space:]]*$' "acceptance docs model client feature policy"
require_line "$acceptance_doc" '^tools/release/check-model-client-features\.sh --self-test[[:space:]]*$' "acceptance docs model client feature policy self-test"
require_line "$acceptance_doc" '^tools/release/check-agent-briefs\.sh[[:space:]]*$' "acceptance docs agent brief alignment"
require_line "$acceptance_doc" '^tools/release/check-agent-briefs\.sh --self-test[[:space:]]*$' "acceptance docs agent brief alignment self-test"
require_line "$acceptance_doc" '^bash tools/release/run-model-gates\.sh[[:space:]]*$' "acceptance docs model-backed release gate"
require_line "$acceptance_doc" '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "acceptance docs model gate self-test"
require_line "$acceptance_doc" '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "acceptance docs cask updater self-test"
require_line "$acceptance_doc" '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "acceptance docs cask finalizer self-test"
require_line "$acceptance_doc" '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "acceptance docs notarization helper self-test"
require_line "$acceptance_doc" '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "acceptance docs update manifest self-test"
require_line "$releasing_doc" 'push to `main` / `spike/\*\*`, PR, or `workflow_dispatch`' "release docs CI trigger truth"
require_line "$releasing_doc" '^[[:space:]]*bash tools/release/run-model-gates\.sh[[:space:]]*$' "release docs model gate wrapper"
require_line "$releasing_doc" 'COMPME_MODEL_GATE_PATH' "release docs model gate path override"
require_line "$releasing_doc" 'COMPME_MODEL_GATE_URL' "release docs model gate URL override"
require_line "$releasing_doc" 'COMPME_MODEL_GATE_SHA256' "release docs model gate SHA override"
require_line "$releasing_doc" 'COMPME_ALLOW_MODEL_GATE_OVERRIDE' "release docs model gate override escape hatch"
require_line "$releasing_doc" 'COMPME_SPIKE_MODEL_PATH' "release docs spike model path override"
require_line "$releasing_doc" 'verifies the zip against its `\.sha256`' "release docs publish checksum verification"
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
require_line "$releasing_doc" 'cask finalizer refuses to update `main`' "release docs cask finalizer ancestry guard"
require_line "$repo_root/tools/acceptance/run-a1b-live-gates.sh" 'overlay-correction-presenter' "A1b runner correction overlay gate"
require_line "$a2_matrix_ledger_script" 'status != "PASS"' "A2 matrix ledger rejects non-pass rows"
require_line "$a2_matrix_ledger_script" 'missing A2 matrix row' "A2 matrix ledger requires complete row coverage"
require_line "$acceptance_doc" '^tools/release/check-a2-matrix-ledger\.sh "\$ledger"[[:space:]]*$' "acceptance docs A2 matrix ledger validation"
require_line "$releasing_doc" 'tools/release/check-a2-matrix-ledger\.sh "\$ledger"' "release docs A2 matrix ledger validation"
require_line "$releasing_doc" 'COMPME_A2_MATRIX_LEDGER' "release docs A2 live ledger workflow variable"
require_line "$releasing_doc" 'tools/acceptance/evidence/a2/' "release docs committed A2 evidence directory"
require_line "$acceptance_doc" 'COMPME_A2_LOG_DIR' "acceptance docs A2 evidence log dir"
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
require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "README bundle metadata check"
require_readme_gate_line '^tools/bundle/check-bundle-metadata\.sh --self-test[[:space:]]*$' "README bundle metadata self-test"
require_readme_gate_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "README bundle assembler self-test"
require_readme_gate_line '^tools/bundle/bundle-smoke\.sh[[:space:]]*$' "README bundle smoke"
require_readme_gate_line '^tools/bundle/bundle-smoke\.sh --self-test[[:space:]]*$' "README bundle smoke self-test"
require_readme_gate_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "README E2E self-test"
require_readme_gate_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "README missing-model startup self-test"
require_readme_gate_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "README missing-model startup product smoke"
require_readme_gate_line '^tools/acceptance/run-ui-assisted-session\.sh --self-test[[:space:]]*$' "README UI-assisted session self-test"
require_readme_gate_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "README A1b self-test"
require_readme_gate_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "README A2 self-test"
require_readme_gate_line '^tools/release/check-a2-matrix-ledger\.sh --self-test[[:space:]]*$' "README A2 matrix ledger self-test"
require_readme_gate_line '^tools/release/check-model-client-features\.sh[[:space:]]*$' "README model client feature policy"
require_readme_gate_line '^tools/release/check-model-client-features\.sh --self-test[[:space:]]*$' "README model client feature policy self-test"
require_readme_gate_line '^bash tools/release/check-model-gates\.sh[[:space:]]*$' "README release gate policy check"
require_readme_gate_line '^tools/release/run-model-gates\.sh --self-test[[:space:]]*$' "README model gate self-test"
require_readme_gate_line '^tools/release/update-cask\.sh --self-test[[:space:]]*$' "README cask updater self-test"
require_readme_gate_line '^tools/release/finalize-cask\.sh --self-test[[:space:]]*$' "README cask finalizer self-test"
require_readme_gate_line '^tools/release/notarize-app\.sh --self-test[[:space:]]*$' "README notarization helper self-test"
require_readme_gate_line '^tools/release/write-update-manifest\.sh --self-test[[:space:]]*$' "README update manifest self-test"
require_readme_gate_line '^cargo build --locked -p platform_macos --examples[[:space:]]*$' "README platform_macos examples build"
require_readme_gate_line '^bash tools/release/run-model-gates\.sh[[:space:]]*$' "README model-backed release gate"
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
require_development_gate_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "DEVELOPMENT A2 self-test"
require_development_gate_line '^tools/release/check-a2-matrix-ledger\.sh --self-test[[:space:]]*$' "DEVELOPMENT A2 matrix ledger self-test"
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
require_grammar_spec_validation_line '^cargo fmt --all -- --check[[:space:]]*$' "grammar spec fmt gate"
require_grammar_spec_validation_line '^cargo clippy --locked --workspace --all-targets -- -D warnings[[:space:]]*$' "grammar spec clippy gate"
require_grammar_spec_validation_line '^cargo test --locked --workspace --all-targets -- --test-threads=1[[:space:]]*$' "grammar spec workspace test gate"
require_grammar_spec_validation_line '^cargo build --locked --workspace --all-targets[[:space:]]*$' "grammar spec workspace build gate"
require_grammar_spec_validation_line '^cargo build --locked -p platform_macos --examples[[:space:]]*$' "grammar spec platform_macos examples build gate"
require_grammar_spec_validation_line '^bash -n tools/acceptance/\*\.sh tools/bundle/\*\.sh tools/release/\*\.sh[[:space:]]*$' "grammar spec script syntax gate"
require_grammar_spec_validation_line '^tools/bundle/check-bundle-metadata\.sh[[:space:]]*$' "grammar spec bundle metadata gate"
require_grammar_spec_validation_line '^tools/bundle/check-bundle-metadata\.sh --self-test[[:space:]]*$' "grammar spec bundle metadata self-test"
require_grammar_spec_validation_line '^tools/bundle/make-app\.sh --self-test[[:space:]]*$' "grammar spec bundle assembler self-test"
require_grammar_spec_validation_line '^tools/acceptance/e2e-complete-me\.sh --self-test[[:space:]]*$' "grammar spec E2E self-test"
require_grammar_spec_validation_line '^tools/acceptance/missing-model-startup\.sh --self-test[[:space:]]*$' "grammar spec missing-model self-test"
require_grammar_spec_validation_line '^tools/acceptance/missing-model-startup\.sh[[:space:]]*$' "grammar spec missing-model product smoke"
require_grammar_spec_validation_line '^tools/acceptance/run-ui-assisted-session\.sh --self-test[[:space:]]*$' "grammar spec UI-assisted session self-test"
require_grammar_spec_validation_line '^tools/acceptance/run-a1b-live-gates\.sh --self-test[[:space:]]*$' "grammar spec A1b self-test"
require_grammar_spec_validation_line '^tools/acceptance/run-a2-compat-gates\.sh --self-test[[:space:]]*$' "grammar spec A2 self-test"
require_grammar_spec_validation_line '^tools/release/check-a2-matrix-ledger\.sh --self-test[[:space:]]*$' "grammar spec A2 matrix ledger self-test"
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
