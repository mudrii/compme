#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
log_path="${COMPME_UI_LOG:-/tmp/cm-ui.log}"

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
COMPME_DEBUG="${COMPME_DEBUG:-1}" \
COMPME_STUB_COMPLETION="${COMPME_STUB_COMPLETION:- world}" \
COMPME_EMOJI="${COMPME_EMOJI:-1}" \
COMPME_AUTOCORRECT="${COMPME_AUTOCORRECT:-1}" \
COMPME_BRITISH_ENGLISH="${COMPME_BRITISH_ENGLISH:-1}" \
COMPME_THESAURUS="${COMPME_THESAURUS:-1}" \
COMPME_GRAMMAR_FIX="${COMPME_GRAMMAR_FIX:-1}" \
COMPME_GRAMMAR_CHECK_KEY="${COMPME_GRAMMAR_CHECK_KEY:-shift+96}" \
COMPME_GRAMMAR_ACCEPT_KEY="${COMPME_GRAMMAR_ACCEPT_KEY:-shift+97}" \
cargo run -p app 2>&1 | tee "$log_path"
