#!/usr/bin/env bash
# Remove checkout's persisted GitHub auth header without masking unset errors.
set -euo pipefail

credential_key='http.https://github.com/.extraheader'
git_bin="${COMPME_SCRUB_GIT_BIN:-git}"
script_path="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

usage() {
  echo "usage: scrub-git-credentials.sh | --self-test" >&2
}

scrub_credentials() {
  get_status=0
  "$git_bin" config --local --get-all "$credential_key" >/dev/null || get_status=$?
  case "$get_status" in
    0) "$git_bin" config --local --unset-all "$credential_key" ;;
    1) return 0 ;;
    *) return "$get_status" ;;
  esac
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-scrub-git-credentials.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  git init -q "$tmp/repo"

  # Missing is the normal case when checkout used persist-credentials:false.
  (cd "$tmp/repo" && "$script_path")

  git -C "$tmp/repo" config --local --add "$credential_key" first
  git -C "$tmp/repo" config --local --add "$credential_key" second
  (cd "$tmp/repo" && "$script_path")
  if git -C "$tmp/repo" config --local --get-all "$credential_key" >/dev/null; then
    echo "scrub-git-credentials self-test failed: persisted header remains" >&2
    return 1
  fi

  cat >"$tmp/failing-git" <<'SH'
#!/usr/bin/env bash
case " $* " in
  *" --get-all "*) exit 0 ;;
  *" --unset-all "*) exit 42 ;;
esac
exit 64
SH
  chmod +x "$tmp/failing-git"
  if COMPME_SCRUB_GIT_BIN="$tmp/failing-git" "$script_path" >/dev/null 2>&1; then
    echo "scrub-git-credentials self-test failed: unset failure was masked" >&2
    return 1
  fi

  cat >"$tmp/failing-get-git" <<'SH'
#!/usr/bin/env bash
case " $* " in
  *" --get-all "*) exit 41 ;;
esac
exit 64
SH
  chmod +x "$tmp/failing-get-git"
  if COMPME_SCRUB_GIT_BIN="$tmp/failing-get-git" "$script_path" >/dev/null 2>&1; then
    echo "scrub-git-credentials self-test failed: lookup failure was treated as absent" >&2
    return 1
  fi

  echo "scrub-git-credentials self-tests passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi

if [ "$#" -ne 0 ]; then
  usage
  exit 2
fi

scrub_credentials
