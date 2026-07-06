#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
log_path="${COMPME_UI_LOG:-/tmp/cm-ui.log}"

build_launch_env() {
  launch_env=(
    "COMPME_DEBUG=${COMPME_DEBUG:-1}"
    "COMPME_STUB_COMPLETION=${COMPME_STUB_COMPLETION:- world}"
    "COMPME_EMOJI=${COMPME_EMOJI:-1}"
    "COMPME_AUTOCORRECT=${COMPME_AUTOCORRECT:-1}"
    "COMPME_BRITISH_ENGLISH=${COMPME_BRITISH_ENGLISH:-1}"
    "COMPME_THESAURUS=${COMPME_THESAURUS:-1}"
    "COMPME_GRAMMAR_FIX=${COMPME_GRAMMAR_FIX:-1}"
    "COMPME_GRAMMAR_CHECK_KEY=${COMPME_GRAMMAR_CHECK_KEY:-control+option+111}"
    "COMPME_GRAMMAR_ACCEPT_KEY=${COMPME_GRAMMAR_ACCEPT_KEY:-shift+97}"
  )
}

run_self_test() {
  build_launch_env
  expected=(
    COMPME_DEBUG
    COMPME_STUB_COMPLETION
    COMPME_EMOJI
    COMPME_AUTOCORRECT
    COMPME_BRITISH_ENGLISH
    COMPME_THESAURUS
    COMPME_GRAMMAR_FIX
    COMPME_GRAMMAR_CHECK_KEY
    COMPME_GRAMMAR_ACCEPT_KEY
  )

  if [[ "${#launch_env[@]}" -ne "${#expected[@]}" ]]; then
    echo "self-test failed: launch env count drifted" >&2
    return 1
  fi

  launch_env_lines="$(printf '%s\n' "${launch_env[@]}")"
  for key in "${expected[@]}"; do
    if ! grep -Eq "^${key}=" <<<"$launch_env_lines"; then
      echo "self-test failed: missing $key from launch environment" >&2
      return 1
    fi
  done

  COMPME_DEBUG=0 COMPME_GRAMMAR_FIX=0 build_launch_env
  launch_env_lines="$(printf '%s\n' "${launch_env[@]}")"
  if ! grep -Eq '^COMPME_DEBUG=0$' <<<"$launch_env_lines"; then
    echo "self-test failed: COMPME_DEBUG override was not preserved" >&2
    return 1
  fi
  if ! grep -Eq '^COMPME_GRAMMAR_FIX=0$' <<<"$launch_env_lines"; then
    echo "self-test failed: COMPME_GRAMMAR_FIX override was not preserved" >&2
    return 1
  fi

  echo "Self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 [--self-test]" >&2
    exit 2
  fi
  run_self_test
  exit
fi

if [[ "$#" -ne 0 ]]; then
  echo "usage: $0 [--self-test]" >&2
  exit 2
fi

cat <<'EOF'
Compme UI assisted test session

After the app starts:
1. Click the Compme menu-bar icon and screenshot the open menu.
2. Open Settings.
3. Screenshot each tab:
   Setup, General, Personalization, Apps, Context, Emoji, Shortcuts,
   Statistics, About.
4. Paste the screenshots plus:
   tail -80 /tmp/cm-ui.log

The complete matrix is docs/UI-ASSISTED-TEST-MATRIX.md.
EOF

cd "$repo_root"
# Keep the global grammar-check shortcut away from Shift+F5: Batch 2 records
# Shift+F5 in the Shortcuts pane, and a registered Carbon shortcut consumes the
# chord before the recorder's NSView receives keyDown.
build_launch_env
env "${launch_env[@]}" cargo run -p app 2>&1 | tee "$log_path"
