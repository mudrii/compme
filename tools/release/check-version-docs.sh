#!/usr/bin/env bash
# Published claims follow Casks/compme.rb, which keeps serving the previous
# artifact until verified cask finalization. The workspace candidate is
# single-sourced in Cargo.toml and checked separately in DEVELOPMENT.md.
# The README status line and release boundary, the
# SECURITY supported release, the ROADMAP header and release boundary, and the
# release-boundary notes in RELEASING,
# DEVELOPMENT, ACCEPTANCE, ARCHITECTURE, and MANUAL-VALIDATION must each name
# the published version. Bundle/cask release-window validity is also covered by
# tools/bundle/check-bundle-metadata.sh. --sync-published is used only after
# publication by the cask finalizer to reconcile these claims atomically.
# Anchors are line-based: each surface must keep its anchor phrase and the
# version on the SAME line; a re-wrap or reword false-fails loudly by design,
# and the fix is to update the anchor here in the same commit.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

usage() {
  echo "usage: check-version-docs.sh [--self-test | --sync-published TAG_SHA]" >&2
}

require_doc_version() {
  label="$1"
  file="$2"
  anchor="$3"
  needle="$4"
  anchored="$(grep -F "$anchor" "$docs_root/$file" || true)"
  # The first version belongs to the claim; a historical version later on the
  # same line must not conceal a stale or premature publication claim.
  claimed="$(grep -Eo 'v[0-9]+\.[0-9]+\.[0-9]+' <<<"$anchored" | head -n 1 || true)"
  expected="$(grep -Eo 'v[0-9]+\.[0-9]+\.[0-9]+' <<<"$needle" | head -n 1 || true)"
  if [ "$claimed" != "$expected" ] || ! grep -Fq "$needle" <<<"$anchored"; then
    echo "version-docs check failed: $file: $label does not name $needle (workspace $version, published $published_version)" >&2
    return 1
  fi
}

run_self_test() {
  for name in COMPME_VERSION_DOCS_ROOT; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "version-docs self-test failed: inherited $name" >&2
      return 1
    fi
  done
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-version-docs.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  unset COMPME_VERSION_DOCS_ROOT

  # The inherited-env guard above must reject a preset
  # COMPME_VERSION_DOCS_ROOT: the entrypoint scrubs only the CI-provided
  # GITHUB_* vars, so a preset root reaches the guard and fails loudly.
  if COMPME_VERSION_DOCS_ROOT="$tmp/poisoned" "$0" --self-test >/dev/null 2>"$tmp/poison-root.err"; then
    echo "version-docs self-test failed: inherited COMPME_VERSION_DOCS_ROOT was accepted" >&2
    return 1
  fi
  grep -q 'version-docs self-test failed: inherited COMPME_VERSION_DOCS_ROOT' "$tmp/poison-root.err"

  write_fixtures() {
    root="$1"
    mkdir -p "$root/docs" "$root/Casks"
    printf 'cask "compme" do\n  version "1.2.3"\nend\n' >"$root/Casks/compme.rb"
    cat >"$root/Cargo.toml" <<'TOML'
[workspace]
members = []

[workspace.package]
version = "1.2.3"
TOML
    cat >"$root/README.md" <<'MD'
### Current platform support

| Platform | Product status |
|---|---|
| macOS | **Latest published artifact:** signed, notarized, and stapled `v1.2.3` |

**Release boundary:** `v1.2.3` points to `deadbeef`.
MD
    cat >"$root/SECURITY.md" <<'MD'
## Supported versions

The current supported release is `v1.2.3`; earlier releases are unsupported.
MD
    cat >"$root/docs/ROADMAP.md" <<'MD'
# compme — Roadmap & Pending Work

> **Last updated:** 2026-01-01 (v1.2.3 (`deadbeef`) remains the latest published artifact)

> **Release boundary:** the published `v1.2.3` artifact is tag `v1.2.3` (commit `deadbeef`).
MD
    cat >"$root/docs/RELEASING.md" <<'MD'
> **Release boundary (2026-01-01):** The latest published artifact is `v1.2.3` at `deadbeef`.
MD
    cat >"$root/docs/DEVELOPMENT.md" <<'MD'
## Repository State

**Workspace version:** `v1.2.3`.
The current checkout develops on `main`; the latest published release is `v1.2.3`.
Specifically, `v1.2.3` points to `deadbeef`.
MD
    cat >"$root/docs/ACCEPTANCE.md" <<'MD'
# Acceptance

> **Release boundary (2026-01-01):** this document tracks current `main`. The
> latest published artifact, `v1.2.3` (`deadbeef`), includes the fixes.
MD
    cat >"$root/docs/ARCHITECTURE.md" <<'MD'
# Architecture

**Release boundary:** the published `v1.2.3` artifact points to `deadbeef`; this
page documents current `main`.
MD
    cat >"$root/docs/MANUAL-VALIDATION.md" <<'MD'
# compme — Manual UX Validation Checklist

> **Release boundary (2026-01-01):** this checklist tracks current `main`.
> Validate the latest published `v1.2.3` binary from tag `deadbeef` and its
> release assets.
MD
  }

  root="$tmp/root"
  write_fixtures "$root"

  if ! out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
    echo "version-docs self-test failed: current fixtures should pass, got: $out" >&2
    return 1
  fi
  case "$out" in
    *"Version docs OK: v1.2.3"*) ;;
    *) echo "version-docs self-test failed: expected OK message, got: $out" >&2; return 1 ;;
  esac

  # Preparing a candidate must retain accurate published claims. Finalization
  # updates only the named boundaries; dated history must survive untouched.
  sed 's/1\.2\.3/1.2.4/' "$root/Cargo.toml" >"$tmp/candidate.toml"
  mv "$tmp/candidate.toml" "$root/Cargo.toml"
  sed '/Workspace version/s/1\.2\.3/1.2.4/' "$root/docs/DEVELOPMENT.md" >"$tmp/candidate.md"
  mv "$tmp/candidate.md" "$root/docs/DEVELOPMENT.md"
  COMPME_VERSION_DOCS_ROOT="$root" "$0" >/dev/null
  sed '/Latest published artifact/s/$/; historical v1.2.3 kept./' "$root/README.md" >"$tmp/history.md"
  mv "$tmp/history.md" "$root/README.md"
  printf '\nHistorical v1.2.3 release.\n' >>"$root/README.md"
  sed 's/1\.2\.3/1.2.4/' "$root/Casks/compme.rb" >"$tmp/finalized.rb"
  mv "$tmp/finalized.rb" "$root/Casks/compme.rb"
  if COMPME_VERSION_DOCS_ROOT="$root" "$0" >/dev/null 2>&1; then
    echo "version-docs self-test failed: finalized cask with stale docs passed" >&2
    return 1
  fi
  release_sha=0123456789abcdef0123456789abcdef01234567
  COMPME_VERSION_DOCS_ROOT="$root" "$0" --sync-published "$release_sha" >/dev/null
  grep -Fq "points to \`$release_sha\`" "$root/README.md"
  grep -Fxq 'Historical v1.2.3 release.' "$root/README.md"
  grep -Fq 'historical v1.2.3 kept.' "$root/README.md"
  cp "$root/README.md" "$tmp/readme-before"
  printf '# Missing boundary\n' >"$root/docs/MANUAL-VALIDATION.md"
  if COMPME_VERSION_DOCS_ROOT="$root" "$0" --sync-published "$release_sha" >/dev/null 2>&1; then
    echo "version-docs self-test failed: missing finalization anchor passed" >&2
    return 1
  fi
  cmp "$root/README.md" "$tmp/readme-before"
  if COMPME_VERSION_DOCS_ROOT="$root" "$0" --sync-published invalid >/dev/null 2>&1; then
    echo "version-docs self-test failed: invalid release SHA was accepted" >&2
    return 1
  fi
  write_fixtures "$root"
  sed '/Latest published artifact/s/v1\.2\.3/v1.2.4/; /Latest published artifact/s/$/; historical v1.2.3/' "$root/README.md" >"$tmp/premature.md"
  mv "$tmp/premature.md" "$root/README.md"
  if COMPME_VERSION_DOCS_ROOT="$root" "$0" >/dev/null 2>&1; then
    echo "version-docs self-test failed: historical version concealed premature publication"
    return 1
  fi
  write_fixtures "$root"
  printf '  version "1.2.3"\n  version "1.2.4"\n' >"$root/Casks/compme.rb"
  if COMPME_VERSION_DOCS_ROOT="$root" "$0" >/dev/null 2>&1; then
    echo "version-docs self-test failed: ambiguous published version passed" >&2
    return 1
  fi
  write_fixtures "$root"
  sed '/Workspace version/s/1\.2\.3/1.2.2/' "$root/docs/DEVELOPMENT.md" >"$tmp/stale.md"
  mv "$tmp/stale.md" "$root/docs/DEVELOPMENT.md"
  if COMPME_VERSION_DOCS_ROOT="$root" "$0" >/dev/null 2>&1; then
    echo "version-docs self-test failed: stale workspace candidate passed" >&2
    return 1
  fi

  for stale_file in README.md SECURITY.md docs/ROADMAP.md docs/RELEASING.md docs/DEVELOPMENT.md docs/ACCEPTANCE.md docs/ARCHITECTURE.md docs/MANUAL-VALIDATION.md; do
    write_fixtures "$root"
    sed 's/1\.2\.3/9.9.9/g' "$root/$stale_file" >"$tmp/stale.md"
    mv "$tmp/stale.md" "$root/$stale_file"
    if out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
      echo "version-docs self-test failed: stale $stale_file passed" >&2
      return 1
    fi
    case "$out" in
      *"$stale_file"*) ;;
      *) echo "version-docs self-test failed: failure did not name $stale_file, got: $out" >&2; return 1 ;;
    esac
  done

  # A current tag reference must not conceal a stale published-release claim.
  write_fixtures "$root"
  sed '/latest published release is/s/1\.2\.3/9.9.9/g' "$root/docs/DEVELOPMENT.md" >"$tmp/stale.md"
  mv "$tmp/stale.md" "$root/docs/DEVELOPMENT.md"
  if out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
    echo "version-docs self-test failed: stale published-release claim passed" >&2
    return 1
  fi
  case "$out" in
    *"docs/DEVELOPMENT.md: published-release claim"*) ;;
    *) echo "version-docs self-test failed: wrong published-release diagnostic: $out" >&2; return 1 ;;
  esac

  # Even a current version on the SAME line cannot validate a stale claim.
  write_fixtures "$root"
  sed 's/latest published release is `v1.2.3`/latest published release is `v9.9.9`; checkout version is `v1.2.3`/' "$root/docs/DEVELOPMENT.md" >"$tmp/stale.md"
  mv "$tmp/stale.md" "$root/docs/DEVELOPMENT.md"
  if out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
    echo "version-docs self-test failed: mixed-version published-release claim passed" >&2
    return 1
  fi
  case "$out" in
    *"docs/DEVELOPMENT.md: published-release claim"*) ;;
    *) echo "version-docs self-test failed: wrong mixed-version diagnostic: $out" >&2; return 1 ;;
  esac

  # The ROADMAP needle is the bare version: a header without the parenthesized
  # commit still passes because the anchor already pins the context.
  write_fixtures "$root"
  # A release-boundary note lagging behind a current status line must fail on
  # its own: the boundary anchors exist precisely because v0.1.6 left them
  # naming v0.1.5 while every status line was current.
  for boundary_file in README.md docs/ROADMAP.md; do
    write_fixtures "$root"
    sed '/Release boundary/s/1\.2\.3/9.9.9/g' "$root/$boundary_file" >"$tmp/stale.md"
    mv "$tmp/stale.md" "$root/$boundary_file"
    if out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
      echo "version-docs self-test failed: stale release boundary in $boundary_file passed" >&2
      return 1
    fi
    case "$out" in
      *"$boundary_file: release-boundary note"*) ;;
      *) echo "version-docs self-test failed: stale boundary did not name $boundary_file's note, got: $out" >&2; return 1 ;;
    esac
  done
  write_fixtures "$root"
  cat >"$root/docs/ROADMAP.md" <<'MD'
# compme — Roadmap & Pending Work

> **Last updated:** 2026-01-01 (v1.2.3 remains the latest published artifact)

> **Release boundary:** the published `v1.2.3` artifact is tag `v1.2.3` (commit `deadbeef`).
MD
  if ! out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
    echo "version-docs self-test failed: ROADMAP header without commit parens should pass, got: $out" >&2
    return 1
  fi

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "version-docs self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -Fq 'usage: check-version-docs.sh [' "$tmp/self-test-argc.err"
  if "$0" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "version-docs self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -Fq 'usage: check-version-docs.sh [' "$tmp/normal-argc.err"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  # Scrub only the CI-provided vars (CI always exports GITHUB_ACTIONS); a
  # user-preset COMPME_VERSION_DOCS_ROOT must reach run_self_test's
  # inherited-env guard and be rejected there, so the self-test environment
  # stays hermetic.
  unset GITHUB_ACTIONS GITHUB_REF_TYPE
  run_self_test
  exit 0
fi
if [ "$#" -ne 0 ] && { [ "$#" -ne 2 ] || [ "$1" != "--sync-published" ]; }; then
  usage
  exit 2
fi

docs_root="${COMPME_VERSION_DOCS_ROOT:-$repo_root}"

version="$(awk '
  /^\[workspace\.package\]/ { in_pkg = 1; next }
  /^\[/ { in_pkg = 0 }
  in_pkg && /^version[[:space:]]*=[[:space:]]*"/ {
    sub(/^[^"]*"/, "")
    sub(/".*$/, "")
    print
    exit
  }
' "$docs_root/Cargo.toml")"
if [ -z "$version" ]; then
  echo "version-docs check failed: no version in [workspace.package] of $docs_root/Cargo.toml" >&2
  exit 1
fi

published_version="$(sed -n 's/^  version "\([^"]*\)"$/\1/p' "$docs_root/Casks/compme.rb")"
"$repo_root/tools/release/validate-version.sh" "$published_version" >/dev/null

if [ "${1:-}" = "--sync-published" ]; then
  # Validate every anchor before writing any file, and leave historical prose
  # alone. The caller already verified publication, checksum, and tag ancestry.
  ruby -e '
    root, version, sha = ARGV
    abort("invalid release SHA") unless /\A[0-9a-f]{40}\z/.match?(sha)
    surfaces = {
      "README.md" => ["Latest published artifact", "**Release boundary:**"],
      "SECURITY.md" => ["supported release is"],
      "docs/ROADMAP.md" => ["remains the latest published artifact", "**Release boundary:** the published"],
      "docs/RELEASING.md" => ["latest published artifact is"],
      "docs/DEVELOPMENT.md" => ["latest published release is", "points to"],
      "docs/ACCEPTANCE.md" => ["latest published artifact"],
      "docs/ARCHITECTURE.md" => ["Release boundary"],
      "docs/MANUAL-VALIDATION.md" => ["Validate the latest published"]
    }
    updates = surfaces.map do |file, anchors|
      path = File.join(root, file)
      lines = File.readlines(path)
      anchors.each do |anchor|
        matches = lines.each_index.select { |i| lines[i].include?(anchor) }
        abort("#{file}: expected exactly one #{anchor} anchor") unless matches.length == 1
        i = matches.first
        abort("#{file}: missing published version at #{anchor}") unless /v\d+\.\d+\.\d+/.match?(lines[i])
        # README status rows also contain dated release history on this line.
        lines[i] = if file == "README.md"
          lines[i].sub(/v\d+\.\d+\.\d+/, "v#{version}")
        else
          lines[i].gsub(/v\d+\.\d+\.\d+/, "v#{version}")
        end
        lines[i] = lines[i].gsub(/`[0-9a-f]{7,40}`/, "`#{sha}`")
      end
      [path, lines.join]
    end
    updates.each { |path, content| File.write(path, content) }
  ' "$docs_root" "$published_version" "$2"
fi

backticked='`v'"$published_version"'`'
stale=0
require_doc_version "workspace candidate" "docs/DEVELOPMENT.md" "**Workspace version:**" "**Workspace version:** \`v$version\`" || stale=1
require_doc_version "status line" "README.md" "Latest published artifact" "$backticked" || stale=1
require_doc_version "release-boundary note" "README.md" "**Release boundary:**" "$backticked" || stale=1
require_doc_version "supported-release table" "SECURITY.md" "supported release is" "$backticked" || stale=1
require_doc_version "header" "docs/ROADMAP.md" "remains the latest published artifact" "v$published_version" || stale=1
require_doc_version "release-boundary note" "docs/ROADMAP.md" "**Release boundary:** the published" "$backticked" || stale=1
require_doc_version "release-boundary note" "docs/RELEASING.md" "latest published artifact is" "$backticked" || stale=1
require_doc_version "repository-state note" "docs/DEVELOPMENT.md" "points to" "$backticked" || stale=1
# Bind the version to the claim itself, not another version on the same line.
require_doc_version "published-release claim" "docs/DEVELOPMENT.md" "latest published release is" "latest published release is $backticked" || stale=1
require_doc_version "release-boundary header" "docs/ACCEPTANCE.md" "latest published artifact" "$backticked" || stale=1
require_doc_version "release-boundary note" "docs/ARCHITECTURE.md" "Release boundary" "$backticked" || stale=1
require_doc_version "validation boundary note" "docs/MANUAL-VALIDATION.md" "Validate the latest published" "$backticked" || stale=1
if [ "$stale" -ne 0 ]; then
  exit 1
fi

echo "Version docs OK: v$published_version published, v$version workspace in README.md, SECURITY.md, docs/ROADMAP.md, docs/RELEASING.md, docs/DEVELOPMENT.md, docs/ACCEPTANCE.md, docs/ARCHITECTURE.md, docs/MANUAL-VALIDATION.md"
