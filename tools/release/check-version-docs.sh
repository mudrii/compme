#!/usr/bin/env bash
# Fail when a documented version surface lags the workspace version (AGENTS.md
# lesson #2). The version is single-sourced in the root Cargo.toml
# [workspace.package] table; the README status line, the SECURITY supported
# release, the ROADMAP header, and the release-boundary notes in RELEASING,
# DEVELOPMENT, ACCEPTANCE, ARCHITECTURE, and MANUAL-VALIDATION must each name
# it. Casks/compme.rb and tools/bundle/Info.plist are covered by
# tools/bundle/check-bundle-metadata.sh and are deliberately not checked here.
# Anchors are line-based: each surface must keep its anchor phrase and the
# version on the SAME line; a re-wrap or reword false-fails loudly by design,
# and the fix is to update the anchor here in the same commit.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

usage() {
  echo "usage: check-version-docs.sh [--self-test]" >&2
}

require_doc_version() {
  label="$1"
  file="$2"
  anchor="$3"
  needle="$4"
  anchored="$(grep -F "$anchor" "$docs_root/$file" || true)"
  if ! grep -Fq "$needle" <<<"$anchored"; then
    echo "version-docs check failed: $file: $label does not name $needle (workspace version is $version)" >&2
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
    mkdir -p "$root/docs"
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
MD
    cat >"$root/SECURITY.md" <<'MD'
## Supported versions

The current supported release is `v1.2.3`; earlier releases are unsupported.
MD
    cat >"$root/docs/ROADMAP.md" <<'MD'
# compme — Roadmap & Pending Work

> **Last updated:** 2026-01-01 (v1.2.3 (`deadbeef`) remains the latest published artifact)
MD
    cat >"$root/docs/RELEASING.md" <<'MD'
> **Release boundary (2026-01-01):** The latest published artifact is `v1.2.3` at `deadbeef`.
MD
    cat >"$root/docs/DEVELOPMENT.md" <<'MD'
## Repository State

The current checkout develops on `main`; the latest published release is
`v1.2.3`. Specifically, `v1.2.3` points to `deadbeef`.
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

  # The ROADMAP needle is the bare version: a header without the parenthesized
  # commit still passes because the anchor already pins the context.
  write_fixtures "$root"
  cat >"$root/docs/ROADMAP.md" <<'MD'
# compme — Roadmap & Pending Work

> **Last updated:** 2026-01-01 (v1.2.3 remains the latest published artifact)
MD
  if ! out="$(COMPME_VERSION_DOCS_ROOT="$root" "$0" 2>&1)"; then
    echo "version-docs self-test failed: ROADMAP header without commit parens should pass, got: $out" >&2
    return 1
  fi

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "version-docs self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-version-docs\.sh \[--self-test\]$' "$tmp/self-test-argc.err"
  if "$0" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "version-docs self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-version-docs\.sh \[--self-test\]$' "$tmp/normal-argc.err"

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
if [ "$#" -ne 0 ]; then
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

backticked='`v'"$version"'`'
stale=0
require_doc_version "status line" "README.md" "Latest published artifact" "$backticked" || stale=1
require_doc_version "supported-release table" "SECURITY.md" "supported release is" "$backticked" || stale=1
require_doc_version "header" "docs/ROADMAP.md" "remains the latest published artifact" "v$version" || stale=1
require_doc_version "release-boundary note" "docs/RELEASING.md" "latest published artifact is" "$backticked" || stale=1
require_doc_version "repository-state note" "docs/DEVELOPMENT.md" "points to" "$backticked" || stale=1
require_doc_version "release-boundary header" "docs/ACCEPTANCE.md" "latest published artifact" "$backticked" || stale=1
require_doc_version "release-boundary note" "docs/ARCHITECTURE.md" "Release boundary" "$backticked" || stale=1
require_doc_version "validation boundary note" "docs/MANUAL-VALIDATION.md" "Validate the latest published" "$backticked" || stale=1
if [ "$stale" -ne 0 ]; then
  exit 1
fi

echo "Version docs OK: v$version in README.md, SECURITY.md, docs/ROADMAP.md, docs/RELEASING.md, docs/DEVELOPMENT.md, docs/ACCEPTANCE.md, docs/ARCHITECTURE.md, docs/MANUAL-VALIDATION.md"
