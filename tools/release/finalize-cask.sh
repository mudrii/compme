#!/usr/bin/env bash
# Finalize the Homebrew cask on the default branch after a GitHub release
# artifact has been published.
#
# Usage: finalize-cask.sh TAG ARTIFACT_PATH VERSION DEFAULT_BRANCH
#        finalize-cask.sh --self-test
set -euo pipefail

repo_root="${COMPME_FINALIZE_CASK_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"

usage() {
  echo "usage: finalize-cask.sh TAG ARTIFACT_PATH VERSION DEFAULT_BRANCH | --self-test" >&2
}

finalize_cask() {
  tag="$1"
  artifact_path="$2"
  version="$3"
  default_branch="$4"

  if [ "$tag" != "v$version" ]; then
    echo "tag/version mismatch: $tag != v$version" >&2
    return 1
  fi
  if [ -z "${GITHUB_SHA:-}" ]; then
    echo "GITHUB_SHA is required" >&2
    return 1
  fi

  cd "$repo_root"
  git fetch origin "$default_branch"
  if ! git merge-base --is-ancestor "$GITHUB_SHA" "origin/$default_branch"; then
    echo "release tag commit $GITHUB_SHA is not on origin/$default_branch" >&2
    return 1
  fi
  git checkout "$default_branch"
  git pull --ff-only origin "$default_branch"
  current_cask_version="$(ruby -ne 'puts $1 if /^  version "([^"]+)"/' Casks/compme.rb)"
  if [ "$current_cask_version" != "$version" ]; then
    echo "default-branch cask version is $current_cask_version, expected $version" >&2
    echo "refusing to publish a stale or out-of-order cask update" >&2
    return 1
  fi
  COMPME_CASK_ARTIFACT="$artifact_path" tools/release/update-cask.sh "$tag"
  if git diff --quiet -- Casks/compme.rb; then
    echo "Casks/compme.rb already matches $tag"
  else
    git config user.name "github-actions[bot]"
    git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
    git add Casks/compme.rb
    git commit -m "chore(release): cask $tag"
    git push origin "HEAD:$default_branch"
  fi
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
      echo "## Homebrew cask"
      echo "Finalized \`Casks/compme.rb\` for \`$tag\` on \`$default_branch\`."
      echo "version: \`$version\`"
    } >>"$GITHUB_STEP_SUMMARY"
  fi
}

make_fixture_repo() {
  root="$1"
  behavior="$2"
  mkdir -p "$root/remote.git"
  git init --bare "$root/remote.git" >/dev/null
  git clone "$root/remote.git" "$root/work" >/dev/null 2>&1
  (
    cd "$root/work"
    git checkout -b main >/dev/null 2>&1
    mkdir -p Casks tools/release
    cat >Casks/compme.rb <<'CASK'
cask "compme" do
  version "9.8.7"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
end
CASK
    cat >tools/release/update-cask.sh <<SH
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "\${COMPME_CASK_ARTIFACT:-}" >>"\${COMPME_FINALIZE_CASK_ARTIFACT_LOG:-/dev/null}"
case "$behavior" in
  noop) exit 0 ;;
  modify) ruby -0pi -e 'sub(/sha256 "[0-9a-f]+"/, "sha256 \\"1111111111111111111111111111111111111111111111111111111111111111\\"")' Casks/compme.rb ;;
  fail) exit 42 ;;
esac
SH
    chmod +x tools/release/update-cask.sh
    git add .
    git -c user.name=t -c user.email=t@example.test commit -m initial >/dev/null
    git push origin main >/dev/null 2>&1
  )
}

detach_release_checkout() {
  work="$1"
  sha="$2"
  git -C "$work" checkout --detach "$sha" >/dev/null 2>&1
  git -C "$work" branch -D main >/dev/null 2>&1
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-finalize-cask.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  make_fixture_repo "$tmp/noop" noop
  noop_sha="$(git -C "$tmp/noop/work" rev-parse HEAD)"
  detach_release_checkout "$tmp/noop/work" "$noop_sha"
  before_count="$(git -C "$tmp/noop/work" rev-list --count origin/main)"
  : >"$tmp/artifacts.log"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/noop/work" \
    COMPME_FINALIZE_CASK_ARTIFACT_LOG="$tmp/artifacts.log" \
    GITHUB_SHA="$noop_sha" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >"$tmp/noop.out"
  after_count="$(git -C "$tmp/noop/work" rev-list --count origin/main)"
  test "$before_count" = "$after_count"
  test "$(git -C "$tmp/noop/work" branch --show-current)" = "main"
  grep -q "already matches v9.8.7" "$tmp/noop.out"
  grep -Fxq "$tmp/artifact.zip" "$tmp/artifacts.log"

  make_fixture_repo "$tmp/summary" noop
  summary_sha="$(git -C "$tmp/summary/work" rev-parse HEAD)"
  detach_release_checkout "$tmp/summary/work" "$summary_sha"
  summary_file="$tmp/github-step-summary.md"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/summary/work" \
    GITHUB_SHA="$summary_sha" \
    GITHUB_STEP_SUMMARY="$summary_file" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >/dev/null
  grep -Fxq "## Homebrew cask" "$summary_file"
  grep -Fxq 'Finalized `Casks/compme.rb` for `v9.8.7` on `main`.' "$summary_file"
  grep -Fxq 'version: `9.8.7`' "$summary_file"

  make_fixture_repo "$tmp/modify" modify
  modify_sha="$(git -C "$tmp/modify/work" rev-parse HEAD)"
  detach_release_checkout "$tmp/modify/work" "$modify_sha"
  : >"$tmp/artifacts.log"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/modify/work" \
    COMPME_FINALIZE_CASK_ARTIFACT_LOG="$tmp/artifacts.log" \
    GITHUB_SHA="$modify_sha" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >/dev/null
  git -C "$tmp/modify/work" fetch origin main >/dev/null 2>&1
  git -C "$tmp/modify/work" log --oneline origin/main -1 | grep -q "chore(release): cask v9.8.7"
  grep -Fxq "$tmp/artifact.zip" "$tmp/artifacts.log"

  make_fixture_repo "$tmp/update-fail" fail
  update_fail_sha="$(git -C "$tmp/update-fail/work" rev-parse HEAD)"
  before_count="$(git -C "$tmp/update-fail/work" rev-list --count origin/main)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/update-fail/work" \
    GITHUB_SHA="$update_fail_sha" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >/dev/null 2>"$tmp/update-fail.err"; then
    echo "finalize-cask self-test failed: update-cask failure was accepted" >&2
    return 1
  fi
  after_count="$(git -C "$tmp/update-fail/work" rev-list --count origin/main)"
  test "$before_count" = "$after_count"

  make_fixture_repo "$tmp/push-fail" modify
  push_fail_sha="$(git -C "$tmp/push-fail/work" rev-parse HEAD)"
  before_remote_sha="$(git -C "$tmp/push-fail/work" rev-parse origin/main)"
  cat >"$tmp/push-fail/remote.git/hooks/pre-receive" <<'SH'
#!/usr/bin/env bash
exit 1
SH
  chmod +x "$tmp/push-fail/remote.git/hooks/pre-receive"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/push-fail/work" \
    GITHUB_SHA="$push_fail_sha" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >/dev/null 2>"$tmp/push-fail.err"; then
    echo "finalize-cask self-test failed: push failure was accepted" >&2
    return 1
  fi
  git -C "$tmp/push-fail/work" fetch origin main >/dev/null 2>&1
  test "$(git -C "$tmp/push-fail/work" rev-parse origin/main)" = "$before_remote_sha"

  make_fixture_repo "$tmp/version-mismatch" noop
  mismatch_sha="$(git -C "$tmp/version-mismatch/work" rev-parse HEAD)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/version-mismatch/work" \
    GITHUB_SHA="$mismatch_sha" \
    "$0" v9.9.9 "$tmp/artifact.zip" 9.9.9 main >/dev/null 2>"$tmp/mismatch.err"; then
    echo "finalize-cask self-test failed: version mismatch was accepted" >&2
    return 1
  fi
  grep -q "refusing to publish a stale or out-of-order cask update" "$tmp/mismatch.err"

  make_fixture_repo "$tmp/tag-version-mismatch" noop
  tag_version_mismatch_sha="$(git -C "$tmp/tag-version-mismatch/work" rev-parse HEAD)"
  before_count="$(git -C "$tmp/tag-version-mismatch/work" rev-list --count origin/main)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/tag-version-mismatch/work" \
    GITHUB_SHA="$tag_version_mismatch_sha" \
    "$0" v9.8.8 "$tmp/artifact.zip" 9.8.7 main >/dev/null 2>"$tmp/tag-version-mismatch.err"; then
    echo "finalize-cask self-test failed: tag/version mismatch was accepted" >&2
    return 1
  fi
  after_count="$(git -C "$tmp/tag-version-mismatch/work" rev-list --count origin/main)"
  test "$before_count" = "$after_count"
  grep -q "tag/version mismatch" "$tmp/tag-version-mismatch.err"

  make_fixture_repo "$tmp/ancestor" noop
  bad_sha="$(git -C "$tmp/ancestor/work" -c user.name=t -c user.email=t@example.test commit-tree "$(git -C "$tmp/ancestor/work" rev-parse HEAD^{tree})" -m detached)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/ancestor/work" \
    GITHUB_SHA="$bad_sha" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >/dev/null 2>"$tmp/ancestor.err"; then
    echo "finalize-cask self-test failed: non-ancestor tag was accepted" >&2
    return 1
  fi
  grep -q "is not on origin/main" "$tmp/ancestor.err"

  make_fixture_repo "$tmp/missing-sha" noop
  # env -u: on GitHub runners GITHUB_SHA is always set, which made this
  # missing-sha case fail for the wrong reason (and the bare grep below exit
  # silently) — the step went red on CI while passing locally.
  if env -u GITHUB_SHA COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/missing-sha/work" \
    "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 main >/dev/null 2>"$tmp/missing-sha.err"; then
    echo "finalize-cask self-test failed: missing GITHUB_SHA was accepted" >&2
    return 1
  fi
  grep -q "GITHUB_SHA is required" "$tmp/missing-sha.err"

  if "$0" v9.8.7 "$tmp/artifact.zip" 9.8.7 >/dev/null 2>"$tmp/usage.err"; then
    echo "finalize-cask self-test failed: wrong argument count was accepted" >&2
    return 1
  fi
  grep -q "usage: finalize-cask.sh TAG ARTIFACT_PATH VERSION DEFAULT_BRANCH" "$tmp/usage.err"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "finalize-cask self-test failed: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -q "usage: finalize-cask.sh TAG ARTIFACT_PATH VERSION DEFAULT_BRANCH" "$tmp/self-test-argc.err"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi

if [ "$#" -ne 4 ]; then
  usage
  exit 2
fi

finalize_cask "$@"
