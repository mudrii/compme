#!/usr/bin/env bash
# Enforce the single-brief contract for multi-harness use: AGENTS.md is the
# only agent brief; CLAUDE.md/GEMINI.md/QWEN.md must stay symlinks to it, and
# no harness-specific rule file may fork the objective or roadmap pointer.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

usage() {
  echo "usage: check-agent-briefs.sh [--self-test] [repo-root]" >&2
}

# Symlink names fanned out from AGENTS.md (harnesses that require a fixed name).
symlink_briefs=(CLAUDE.md GEMINI.md QWEN.md)

# Rogue per-harness brief/rule files that would silently fork the objective.
# Codex/OpenCode/pi/kimi-code/recent Cursor read AGENTS.md natively.
denied_briefs=(
  .aider.conf.yml
  .clinerules
  .cursorrules
  .windsurfrules
  .github/copilot-instructions.md
  CODEX.md
  CURSOR.md
  KIMI.md
  OPENCODE.md
  PI.md
)

check_repo() {
  root="$1"

  if [ ! -f "$root/AGENTS.md" ] || [ -L "$root/AGENTS.md" ]; then
    echo "agent brief check failed: AGENTS.md must be a regular file at repo root" >&2
    return 1
  fi
  if ! grep -Fq "docs/ROADMAP.md is the single source of truth" "$root/AGENTS.md"; then
    echo "agent brief check failed: AGENTS.md lost its roadmap single-source-of-truth pointer" >&2
    return 1
  fi

  for brief in "${symlink_briefs[@]}"; do
    if [ ! -L "$root/$brief" ]; then
      echo "agent brief check failed: $brief must be a symlink to AGENTS.md (found $([ -e "$root/$brief" ] && echo "a regular file — harness drift" || echo "nothing"))" >&2
      return 1
    fi
    target="$(readlink "$root/$brief")"
    if [ "$target" != "AGENTS.md" ]; then
      echo "agent brief check failed: $brief points at $target, expected AGENTS.md" >&2
      return 1
    fi
  done

  for denied in "${denied_briefs[@]}"; do
    if [ -e "$root/$denied" ]; then
      echo "agent brief check failed: rogue harness brief $denied (put shared guidance in AGENTS.md)" >&2
      return 1
    fi
  done
  # .cursor/rules content also forks guidance per-harness; a pointer-only file
  # would still drift, so deny the directory outright.
  if [ -d "$root/.cursor/rules" ] && [ -n "$(ls -A "$root/.cursor/rules" 2>/dev/null)" ]; then
    echo "agent brief check failed: .cursor/rules contains harness-specific rules (use AGENTS.md)" >&2
    return 1
  fi

  echo "agent briefs OK: AGENTS.md canonical, ${#symlink_briefs[@]} symlinks intact, no rogue briefs"
}

make_good_fixture() {
  root="$1"
  mkdir -p "$root"
  cat >"$root/AGENTS.md" <<'MD'
# fixture — Agent Brief
- docs/ROADMAP.md is the single source of truth for pending work and status.
MD
  for brief in "${symlink_briefs[@]}"; do
    ln -s AGENTS.md "$root/$brief"
  done
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-agent-briefs.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  make_good_fixture "$tmp/good"
  check_repo "$tmp/good" >/dev/null

  make_good_fixture "$tmp/drifted-copy"
  rm "$tmp/drifted-copy/CLAUDE.md"
  echo "divergent brief" >"$tmp/drifted-copy/CLAUDE.md"
  if check_repo "$tmp/drifted-copy" >/dev/null 2>"$tmp/drifted-copy.err"; then
    echo "agent brief self-test failed: regular-file CLAUDE.md was accepted" >&2
    return 1
  fi
  grep -q "harness drift" "$tmp/drifted-copy.err"

  make_good_fixture "$tmp/missing-link"
  rm "$tmp/missing-link/QWEN.md"
  if check_repo "$tmp/missing-link" >/dev/null 2>&1; then
    echo "agent brief self-test failed: missing QWEN.md symlink was accepted" >&2
    return 1
  fi

  make_good_fixture "$tmp/wrong-target"
  rm "$tmp/wrong-target/GEMINI.md"
  ln -s README.md "$tmp/wrong-target/GEMINI.md"
  if check_repo "$tmp/wrong-target" >/dev/null 2>"$tmp/wrong-target.err"; then
    echo "agent brief self-test failed: wrong symlink target was accepted" >&2
    return 1
  fi
  grep -q "points at README.md" "$tmp/wrong-target.err"

  make_good_fixture "$tmp/lost-pointer"
  echo "# brief without roadmap pointer" >"$tmp/lost-pointer/AGENTS.md"
  if check_repo "$tmp/lost-pointer" >/dev/null 2>"$tmp/lost-pointer.err"; then
    echo "agent brief self-test failed: missing roadmap pointer was accepted" >&2
    return 1
  fi
  grep -q "single-source-of-truth pointer" "$tmp/lost-pointer.err"

  make_good_fixture "$tmp/rogue-brief"
  echo "custom cursor rules" >"$tmp/rogue-brief/.cursorrules"
  if check_repo "$tmp/rogue-brief" >/dev/null 2>"$tmp/rogue-brief.err"; then
    echo "agent brief self-test failed: rogue .cursorrules was accepted" >&2
    return 1
  fi
  grep -q "rogue harness brief" "$tmp/rogue-brief.err"

  make_good_fixture "$tmp/rogue-cursor-rules"
  mkdir -p "$tmp/rogue-cursor-rules/.cursor/rules"
  echo "rule" >"$tmp/rogue-cursor-rules/.cursor/rules/custom.mdc"
  if check_repo "$tmp/rogue-cursor-rules" >/dev/null 2>&1; then
    echo "agent brief self-test failed: .cursor/rules content was accepted" >&2
    return 1
  fi

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "agent brief self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-agent-briefs\.sh \[--self-test\] \[repo-root\]$' "$tmp/self-test-argc.err"

  if "$0" "$tmp/good" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "agent brief self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-agent-briefs\.sh \[--self-test\] \[repo-root\]$' "$tmp/normal-argc.err"

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  if [[ "$#" -ne 1 ]]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi
if [[ "$#" -gt 1 ]]; then
  usage
  exit 2
fi

check_repo "${1:-$repo_root}"
