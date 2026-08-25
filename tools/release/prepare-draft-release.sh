#!/usr/bin/env bash
# Make draft-release creation retry-safe without deleting the release tag.
set -euo pipefail

usage() {
  echo "usage: prepare-draft-release.sh TAG REPOSITORY | --self-test" >&2
}

prepare_draft_release() {
  tag="$1"
  repository="$2"
  if release_result="$(command gh release view "$tag" \
    --repo "$repository" \
    --json isDraft \
    --jq '.isDraft' 2>&1)"; then
    release_is_draft="$release_result"
  else
    case "$release_result" in
      *"release not found"*) return 0 ;;
      *)
        echo "failed to inspect existing release $tag: $release_result" >&2
        return 1
        ;;
    esac
  fi
  case "$release_is_draft" in
    true)
      command gh release delete "$tag" --repo "$repository" --yes
      ;;
    false)
      echo "release $tag is already published; refusing to replace it" >&2
      return 1
      ;;
    *)
      echo "release $tag returned invalid draft state: $release_is_draft" >&2
      return 1
      ;;
  esac
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-prepare-draft-release.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  fake_bin="$tmp/bin"
  mkdir -p "$fake_bin"
  cat >"$fake_bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${COMPME_PREPARE_DRAFT_GH_LOG:?}"
if [ "${1:-}" = "release" ] && [ "${2:-}" = "view" ]; then
  case "${COMPME_PREPARE_DRAFT_STATE:-}" in
    absent) echo "release not found" >&2; exit 1 ;;
    unavailable) echo "network unavailable" >&2; exit 2 ;;
    draft) printf '%s\n' true ;;
    published) printf '%s\n' false ;;
    *) printf '%s\n' unknown ;;
  esac
  exit 0
fi
if [ "${1:-}" = "release" ] && [ "${2:-}" = "delete" ]; then
  exit 0
fi
exit 64
SH
  chmod +x "$fake_bin/gh"
  PATH="$fake_bin:$PATH"
  export PATH

  : >"$tmp/absent.log"
  COMPME_PREPARE_DRAFT_STATE=absent COMPME_PREPARE_DRAFT_GH_LOG="$tmp/absent.log" \
    prepare_draft_release v9.8.7 owner/repo
  if grep -Fq 'release delete' "$tmp/absent.log"; then
    echo "prepare-draft-release self-test failed: absent release was deleted" >&2
    return 1
  fi

  : >"$tmp/draft.log"
  COMPME_PREPARE_DRAFT_STATE=draft COMPME_PREPARE_DRAFT_GH_LOG="$tmp/draft.log" \
    prepare_draft_release v9.8.7 owner/repo
  grep -Fxq 'release delete v9.8.7 --repo owner/repo --yes' "$tmp/draft.log"
  if grep -Fq -- '--cleanup-tag' "$tmp/draft.log"; then
    echo "prepare-draft-release self-test failed: draft cleanup deleted the tag" >&2
    return 1
  fi

  : >"$tmp/published.log"
  if COMPME_PREPARE_DRAFT_STATE=published COMPME_PREPARE_DRAFT_GH_LOG="$tmp/published.log" \
    prepare_draft_release v9.8.7 owner/repo >/dev/null 2>&1; then
    echo "prepare-draft-release self-test failed: published release was accepted" >&2
    return 1
  fi
  if grep -Fq 'release delete' "$tmp/published.log"; then
    echo "prepare-draft-release self-test failed: published release was deleted" >&2
    return 1
  fi

  : >"$tmp/unavailable.log"
  if COMPME_PREPARE_DRAFT_STATE=unavailable COMPME_PREPARE_DRAFT_GH_LOG="$tmp/unavailable.log" \
    prepare_draft_release v9.8.7 owner/repo >/dev/null 2>&1; then
    echo "prepare-draft-release self-test failed: lookup failure was treated as absent" >&2
    return 1
  fi

  if "$0" v9.8.7 >/dev/null 2>"$tmp/argc.err"; then
    echo "prepare-draft-release self-test failed: wrong argument count was accepted" >&2
    return 1
  fi
  grep -q '^usage: prepare-draft-release\.sh TAG REPOSITORY | --self-test$' "$tmp/argc.err"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "prepare-draft-release self-test failed: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: prepare-draft-release\.sh TAG REPOSITORY | --self-test$' "$tmp/self-test-argc.err"

  echo "prepare-draft-release self-tests passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi

if [ "$#" -ne 2 ]; then
  usage
  exit 2
fi

prepare_draft_release "$1" "$2"
