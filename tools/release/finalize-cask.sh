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

freeze_release_helpers() {
  frozen_root="$1"
  tag_sha="$2"
  mkdir -p "$frozen_root/tools/release"
  for helper in validate-version.sh update-cask.sh; do
    destination="$frozen_root/tools/release/$helper"
    if ! git -C "$repo_root" show "$tag_sha:tools/release/$helper" >"$destination"; then
      rm -f "$destination"
      echo "missing release helper at verified tag commit $tag_sha: tools/release/$helper" >&2
      return 1
    fi
    chmod +x "$destination"
  done
}

verify_published_artifact() {
  tag="$1"
  artifact_path="$2"
  version="$3"
  checksum_dir="$4"
  artifact_name="compme-${version}-macos.zip"
  checksum_name="${artifact_name}.sha256"

  if [ ! -f "$artifact_path" ]; then
    echo "missing local release artifact: $artifact_path" >&2
    return 1
  fi
  if [ "$(basename "$artifact_path")" != "$artifact_name" ]; then
    echo "local release artifact filename mismatch: expected $artifact_name, got $(basename "$artifact_path")" >&2
    return 1
  fi

  if ! release_ineligible="$(command gh release view "$tag" \
    --repo mudrii/compme \
    --json isDraft,isPrerelease \
    --jq '.isDraft or .isPrerelease')"; then
    echo "failed to inspect published release state for $tag" >&2
    return 1
  fi
  if [ "$release_ineligible" != "false" ]; then
    echo "release $tag must be published and stable before cask finalization" >&2
    return 1
  fi

  mkdir -p "$checksum_dir"
  if ! command gh release download "$tag" \
    --repo mudrii/compme \
    --pattern "$checksum_name" \
    --dir "$checksum_dir"; then
    echo "failed to download published checksum for $tag" >&2
    return 1
  fi
  checksum_path="$checksum_dir/$checksum_name"
  if [ ! -f "$checksum_path" ]; then
    echo "published checksum download did not produce $checksum_name" >&2
    return 1
  fi

  if ! published_sha="$(ruby -e '
    path, expected_name = ARGV
    content = File.binread(path)
    match = /\A([0-9a-f]{64})  #{Regexp.escape(expected_name)}\n?\z/.match(content)
    abort("invalid published checksum: expected one lowercase SHA-256 line for #{expected_name}") unless match
    puts match[1]
  ' "$checksum_path" "$artifact_name")"; then
    return 1
  fi
  local_sha="$(shasum -a 256 "$artifact_path" | awk '{print $1}')"
  if [ "$local_sha" != "$published_sha" ]; then
    echo "local artifact checksum does not match published checksum for $tag" >&2
    echo "  local:     $local_sha" >&2
    echo "  published: $published_sha" >&2
    return 1
  fi
}

require_exact_cask_line() {
  cask_path="$1"
  expected_line="$2"
  label="$3"
  count="$(grep -Fxc "$expected_line" "$cask_path" || true)"
  if [ "$count" -ne 1 ]; then
    echo "$cask_path: expected exactly one $label line: $expected_line" >&2
    return 1
  fi
}

validate_finalized_cask() {
  cask_path="$1"
  version="$2"
  artifact_path="$3"
  expected_sha="$(shasum -a 256 "$artifact_path" | awk '{print $1}')"

  if ! ruby -c "$cask_path" >/dev/null; then
    echo "$cask_path: invalid Ruby syntax after cask update" >&2
    return 1
  fi
  require_exact_cask_line "$cask_path" "  version \"$version\"" "version"
  require_exact_cask_line "$cask_path" "  sha256 \"$expected_sha\"" "artifact sha256"
  require_exact_cask_line "$cask_path" \
    '  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"' \
    "release URL"
  require_exact_cask_line "$cask_path" "  depends_on macos: :sonoma" "macOS floor"
  require_exact_cask_line "$cask_path" "  depends_on arch: :arm64" "arm64 dependency"
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
  git fetch origin "$default_branch" "refs/tags/$tag:refs/tags/$tag"
  tag_sha="$(git rev-parse "refs/tags/$tag^{commit}")"
  if [ "$tag_sha" != "$GITHUB_SHA" ]; then
    echo "GITHUB_SHA $GITHUB_SHA does not match tag $tag commit $tag_sha" >&2
    return 1
  fi
  if ! git merge-base --is-ancestor "$GITHUB_SHA" "origin/$default_branch"; then
    echo "release tag commit $GITHUB_SHA is not on origin/$default_branch" >&2
    return 1
  fi

  frozen_root="$(mktemp -d "${TMPDIR:-/tmp}/compme-finalize-helpers.XXXXXX")"
  trap 'rm -rf "$frozen_root"' EXIT
  freeze_release_helpers "$frozen_root" "$tag_sha"
  frozen_validator="$frozen_root/tools/release/validate-version.sh"
  frozen_updater="$frozen_root/tools/release/update-cask.sh"
  "$frozen_validator" "$version"
  verify_published_artifact \
    "$tag" "$artifact_path" "$version" "$frozen_root/published-checksum"

  git checkout "$default_branch"
  git pull --ff-only origin "$default_branch"
  current_cask_version="$(ruby -ne 'puts $1 if /^  version "([^"]+)"/' Casks/compme.rb)"
  if [ "$current_cask_version" != "$version" ]; then
    echo "default-branch cask version is $current_cask_version, expected $version" >&2
    echo "refusing to publish a stale or out-of-order cask update" >&2
    return 1
  fi
  cask_path="$repo_root/Casks/compme.rb"
  COMPME_CASK_PATH="$cask_path" \
    COMPME_CASK_ARTIFACT="$artifact_path" \
    "$frozen_updater" "$tag"
  validate_finalized_cask "$cask_path" "$version" "$artifact_path"
  if git diff --quiet -- Casks/compme.rb; then
    echo "Casks/compme.rb already matches $tag"
  else
    git add Casks/compme.rb
    git -c user.name="github-actions[bot]" \
      -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
      commit -m "chore(release): cask $tag"
  fi
  # Always push, including the no-diff path: a previous failed push can leave
  # the finalized commit locally clean but still absent from the remote.
  git push origin "HEAD:$default_branch"
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
  artifact_sha="$(shasum -a 256 "$(dirname "$root")/compme-9.8.7-macos.zip" | awk '{print $1}')"
  initial_sha="$artifact_sha"
  if [ "$behavior" = "modify" ]; then
    initial_sha="0000000000000000000000000000000000000000000000000000000000000000"
  fi
  mkdir -p "$root/remote.git"
  git init --bare "$root/remote.git" >/dev/null
  git clone "$root/remote.git" "$root/work" >/dev/null 2>&1
  (
    cd "$root/work"
    git checkout -b main >/dev/null 2>&1
    mkdir -p Casks tools/release
    cat >Casks/compme.rb <<CASK
cask "compme" do
  version "9.8.7"
  sha256 "$initial_sha"
  url "https://github.com/mudrii/compme/releases/download/v#{version}/compme-#{version}-macos.zip"
  depends_on macos: :sonoma
  depends_on arch: :arm64
end
CASK
    cat >tools/release/validate-version.sh <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "tag-validator" >>"${COMPME_FINALIZE_CASK_HELPER_LOG:-/dev/null}"
if [ "$#" -ne 1 ] || [ "$1" != "9.8.7" ]; then
  echo "invalid version: ${1:-}" >&2
  exit 1
fi
SH
    cat >tools/release/update-cask.sh <<SH
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "tag-updater" >>"\${COMPME_FINALIZE_CASK_HELPER_LOG:-/dev/null}"
printf '%s\n' "\${COMPME_CASK_ARTIFACT:-}" >>"\${COMPME_FINALIZE_CASK_ARTIFACT_LOG:-/dev/null}"
cask="\${COMPME_CASK_PATH:?explicit cask path is required}"
case "$behavior" in
  noop) exit 0 ;;
  modify)
    sha="\$(shasum -a 256 "\${COMPME_CASK_ARTIFACT:?}" | awk '{print \$1}')"
    SHA="\$sha" ruby -0pi -e 'replacement = "sha256 \"" + ENV.fetch("SHA") + "\""; sub(/sha256 "[0-9a-f]+"/, replacement)' "\$cask"
    ;;
  bad-syntax) printf '%s\n' 'this is not (' >>"\$cask" ;;
  wrong-arch) ruby -0pi -e 'sub(/depends_on arch: :arm64/, "depends_on arch: :x86_64")' "\$cask" ;;
  wrong-version) ruby -0pi -e 'sub(/version "9\.8\.7"/, "version \\"9.9.9\\"")' "\$cask" ;;
  wrong-url) ruby -0pi -e 'sub(%r{https://github\.com/mudrii/compme}, "https://example.invalid")' "\$cask" ;;
  wrong-sha) ruby -0pi -e 'sub(/sha256 "[0-9a-f]+"/, "sha256 \\"0000000000000000000000000000000000000000000000000000000000000000\\"")' "\$cask" ;;
  fail) exit 42 ;;
esac
SH
    chmod +x tools/release/validate-version.sh tools/release/update-cask.sh
    git add .
    git -c user.name=t -c user.email=t@example.test commit -m initial >/dev/null
    git tag v9.8.7
    git push origin main refs/tags/v9.8.7 >/dev/null 2>&1
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
  artifact="$tmp/compme-9.8.7-macos.zip"
  printf 'fixture artifact\n' >"$artifact"
  fake_bin="$tmp/bin"
  mkdir -p "$fake_bin"
  cat >"$fake_bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "release" ] && [ "${2:-}" = "view" ]; then
  printf '%s\n' "${COMPME_FINALIZE_CASK_TEST_RELEASE_INELIGIBLE:-false}"
  exit 0
fi
if [ "${COMPME_FINALIZE_CASK_TEST_GH_FAIL:-0}" = "1" ]; then
  exit 41
fi
download_dir=""
pattern=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) download_dir="$2"; shift 2 ;;
    --pattern) pattern="$2"; shift 2 ;;
    *) shift ;;
  esac
done
test -n "$download_dir"
test -n "$pattern"
mkdir -p "$download_dir"
sha="$(shasum -a 256 "${COMPME_FINALIZE_CASK_TEST_PUBLISHED_ARTIFACT:?}" | awk '{print $1}')"
printf '%s  %s\n' "$sha" "${pattern%.sha256}" >"$download_dir/$pattern"
SH
  chmod +x "$fake_bin/gh"
  export PATH="$fake_bin:$PATH"
  export COMPME_FINALIZE_CASK_TEST_PUBLISHED_ARTIFACT="$artifact"

  make_fixture_repo "$tmp/noop" noop
  noop_sha="$(git -C "$tmp/noop/work" rev-parse HEAD)"
  detach_release_checkout "$tmp/noop/work" "$noop_sha"
  before_count="$(git -C "$tmp/noop/work" rev-list --count origin/main)"
  : >"$tmp/artifacts.log"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/noop/work" \
    COMPME_FINALIZE_CASK_ARTIFACT_LOG="$tmp/artifacts.log" \
    GITHUB_SHA="$noop_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >"$tmp/noop.out"
  after_count="$(git -C "$tmp/noop/work" rev-list --count origin/main)"
  test "$before_count" = "$after_count"
  test "$(git -C "$tmp/noop/work" branch --show-current)" = "main"
  grep -q "already matches v9.8.7" "$tmp/noop.out"
  grep -Fxq "$artifact" "$tmp/artifacts.log"

  tampered_dir="$tmp/tampered-artifact"
  mkdir -p "$tampered_dir"
  tampered_artifact="$tampered_dir/compme-9.8.7-macos.zip"
  printf 'tampered fixture artifact\n' >"$tampered_artifact"
  make_fixture_repo "$tampered_dir/repo" noop
  tampered_sha="$(git -C "$tampered_dir/repo/work" rev-parse HEAD)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tampered_dir/repo/work" \
    GITHUB_SHA="$tampered_sha" \
    "$0" v9.8.7 "$tampered_artifact" 9.8.7 main \
    >/dev/null 2>"$tampered_dir/rejected.err"; then
    echo "finalize-cask self-test failed: artifact differing from published checksum was accepted" >&2
    return 1
  fi
  grep -Fq "local artifact checksum does not match published checksum" \
    "$tampered_dir/rejected.err"

  make_fixture_repo "$tmp/checksum-download-fail" noop
  checksum_fail_sha="$(git -C "$tmp/checksum-download-fail/work" rev-parse HEAD)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/checksum-download-fail/work" \
    COMPME_FINALIZE_CASK_TEST_GH_FAIL=1 \
    GITHUB_SHA="$checksum_fail_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main \
    >/dev/null 2>"$tmp/checksum-download-fail.err"; then
    echo "finalize-cask self-test failed: published checksum download failure was accepted" >&2
    return 1
  fi
  grep -Fq "failed to download published checksum for v9.8.7" \
    "$tmp/checksum-download-fail.err"

  make_fixture_repo "$tmp/draft-release" noop
  draft_sha="$(git -C "$tmp/draft-release/work" rev-parse HEAD)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/draft-release/work" \
    COMPME_FINALIZE_CASK_TEST_RELEASE_INELIGIBLE=true \
    GITHUB_SHA="$draft_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main \
    >/dev/null 2>"$tmp/draft-release.err"; then
    echo "finalize-cask self-test failed: draft release was accepted" >&2
    return 1
  fi
  grep -Fq "must be published and stable before cask finalization" \
    "$tmp/draft-release.err"

  make_fixture_repo "$tmp/summary" noop
  summary_sha="$(git -C "$tmp/summary/work" rev-parse HEAD)"
  detach_release_checkout "$tmp/summary/work" "$summary_sha"
  summary_file="$tmp/github-step-summary.md"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/summary/work" \
    GITHUB_SHA="$summary_sha" \
    GITHUB_STEP_SUMMARY="$summary_file" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null
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
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null
  git -C "$tmp/modify/work" fetch origin main >/dev/null 2>&1
  git -C "$tmp/modify/work" log --oneline origin/main -1 | grep -q "chore(release): cask v9.8.7"
  grep -Fxq "$artifact" "$tmp/artifacts.log"

  make_fixture_repo "$tmp/frozen-provenance" modify
  frozen_sha="$(git -C "$tmp/frozen-provenance/work" rev-parse HEAD)"
  cat >"$tmp/frozen-provenance/work/tools/release/validate-version.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "default-validator" >>"${COMPME_FINALIZE_CASK_HELPER_LOG:?}"
exit 71
SH
  cat >"$tmp/frozen-provenance/work/tools/release/update-cask.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "default-updater" >>"${COMPME_FINALIZE_CASK_HELPER_LOG:?}"
exit 72
SH
  chmod +x \
    "$tmp/frozen-provenance/work/tools/release/validate-version.sh" \
    "$tmp/frozen-provenance/work/tools/release/update-cask.sh"
  git -C "$tmp/frozen-provenance/work" add tools/release
  git -C "$tmp/frozen-provenance/work" \
    -c user.name=t -c user.email=t@example.test \
    commit -m default-helper-drift >/dev/null
  git -C "$tmp/frozen-provenance/work" push origin main >/dev/null 2>&1
  detach_release_checkout "$tmp/frozen-provenance/work" "$frozen_sha"
  : >"$tmp/frozen-helpers.log"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/frozen-provenance/work" \
    COMPME_FINALIZE_CASK_HELPER_LOG="$tmp/frozen-helpers.log" \
    GITHUB_SHA="$frozen_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null
  grep -Fxq "tag-validator" "$tmp/frozen-helpers.log"
  grep -Fxq "tag-updater" "$tmp/frozen-helpers.log"
  if grep -Fq "default-" "$tmp/frozen-helpers.log"; then
    echo "finalize-cask self-test failed: default-branch helper executed" >&2
    return 1
  fi

  make_fixture_repo "$tmp/dirty-tag-checkout" modify
  dirty_tag_sha="$(git -C "$tmp/dirty-tag-checkout/work" rev-parse refs/tags/v9.8.7^{commit})"
  detach_release_checkout "$tmp/dirty-tag-checkout/work" "$dirty_tag_sha"
  cat >"$tmp/dirty-tag-checkout/work/tools/release/validate-version.sh" <<'SH'
set -euo pipefail
printf '%s\n' "dirty-validator" >>"${COMPME_FINALIZE_CASK_HELPER_LOG:?}"
exit 73
SH
  cat >"$tmp/dirty-tag-checkout/work/tools/release/update-cask.sh" <<'SH'
set -euo pipefail
printf '%s\n' "dirty-updater" >>"${COMPME_FINALIZE_CASK_HELPER_LOG:?}"
exit 74
SH
  chmod +x \
    "$tmp/dirty-tag-checkout/work/tools/release/validate-version.sh" \
    "$tmp/dirty-tag-checkout/work/tools/release/update-cask.sh"
  : >"$tmp/dirty-tag-helpers.log"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/dirty-tag-checkout/work" \
    COMPME_FINALIZE_CASK_HELPER_LOG="$tmp/dirty-tag-helpers.log" \
    GITHUB_SHA="$dirty_tag_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null
  grep -Fxq "tag-validator" "$tmp/dirty-tag-helpers.log"
  grep -Fxq "tag-updater" "$tmp/dirty-tag-helpers.log"
  if grep -Fq "dirty-" "$tmp/dirty-tag-helpers.log"; then
    echo "finalize-cask self-test failed: dirty working-tree helper executed" >&2
    return 1
  fi
  git -C "$tmp/dirty-tag-checkout/work" fetch origin main >/dev/null 2>&1
  git -C "$tmp/dirty-tag-checkout/work" log --oneline origin/main -1 |
    grep -q "chore(release): cask v9.8.7"

  for rejection in \
    "bad-syntax|invalid Ruby syntax after cask update" \
    "wrong-arch|expected exactly one arm64 dependency line" \
    "wrong-version|expected exactly one version line" \
    "wrong-url|expected exactly one release URL line" \
    "wrong-sha|expected exactly one artifact sha256 line"; do
    behavior="${rejection%%|*}"
    expected_error="${rejection#*|}"
    make_fixture_repo "$tmp/$behavior" "$behavior"
    rejection_sha="$(git -C "$tmp/$behavior/work" rev-parse HEAD)"
    detach_release_checkout "$tmp/$behavior/work" "$rejection_sha"
    if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/$behavior/work" \
      GITHUB_SHA="$rejection_sha" \
      "$0" v9.8.7 "$artifact" 9.8.7 main \
      >/dev/null 2>"$tmp/$behavior.err"; then
      echo "finalize-cask self-test failed: $behavior cask update was accepted" >&2
      return 1
    fi
    grep -Fq "$expected_error" "$tmp/$behavior.err"
  done

  make_fixture_repo "$tmp/update-fail" fail
  update_fail_sha="$(git -C "$tmp/update-fail/work" rev-parse HEAD)"
  before_count="$(git -C "$tmp/update-fail/work" rev-list --count origin/main)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/update-fail/work" \
    GITHUB_SHA="$update_fail_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/update-fail.err"; then
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
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/push-fail.err"; then
    echo "finalize-cask self-test failed: push failure was accepted" >&2
    return 1
  fi
  git -C "$tmp/push-fail/work" fetch origin main >/dev/null 2>&1
  test "$(git -C "$tmp/push-fail/work" rev-parse origin/main)" = "$before_remote_sha"
  failed_push_commit="$(git -C "$tmp/push-fail/work" rev-parse HEAD)"
  rm "$tmp/push-fail/remote.git/hooks/pre-receive"
  COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/push-fail/work" \
    GITHUB_SHA="$push_fail_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >"$tmp/push-retry.out"
  git -C "$tmp/push-fail/work" fetch origin main >/dev/null 2>&1
  test "$(git -C "$tmp/push-fail/work" rev-parse origin/main)" = "$failed_push_commit"
  grep -q "already matches v9.8.7" "$tmp/push-retry.out"

  make_fixture_repo "$tmp/version-mismatch" noop
  mismatch_sha="$(git -C "$tmp/version-mismatch/work" rev-parse HEAD)"
  ruby -0pi -e 'sub(/version "9\.8\.7"/, "version \"9.9.9\"")' "$tmp/version-mismatch/work/Casks/compme.rb"
  git -C "$tmp/version-mismatch/work" add Casks/compme.rb
  git -C "$tmp/version-mismatch/work" -c user.name=t -c user.email=t@example.test commit -m newer-version >/dev/null
  git -C "$tmp/version-mismatch/work" push origin main >/dev/null 2>&1
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/version-mismatch/work" \
    GITHUB_SHA="$mismatch_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/mismatch.err"; then
    echo "finalize-cask self-test failed: version mismatch was accepted" >&2
    return 1
  fi
  grep -q "refusing to publish a stale or out-of-order cask update" "$tmp/mismatch.err"

  make_fixture_repo "$tmp/tag-version-mismatch" noop
  tag_version_mismatch_sha="$(git -C "$tmp/tag-version-mismatch/work" rev-parse HEAD)"
  before_count="$(git -C "$tmp/tag-version-mismatch/work" rev-list --count origin/main)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/tag-version-mismatch/work" \
    GITHUB_SHA="$tag_version_mismatch_sha" \
    "$0" v9.8.8 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/tag-version-mismatch.err"; then
    echo "finalize-cask self-test failed: tag/version mismatch was accepted" >&2
    return 1
  fi
  after_count="$(git -C "$tmp/tag-version-mismatch/work" rev-list --count origin/main)"
  test "$before_count" = "$after_count"
  grep -q "tag/version mismatch" "$tmp/tag-version-mismatch.err"

  make_fixture_repo "$tmp/invalid-version" noop
  invalid_version_sha="$(git -C "$tmp/invalid-version/work" rev-parse HEAD)"
  git -C "$tmp/invalid-version/work" tag v01.2.3 "$invalid_version_sha"
  git -C "$tmp/invalid-version/work" push origin refs/tags/v01.2.3 >/dev/null 2>&1
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/invalid-version/work" \
    GITHUB_SHA="$invalid_version_sha" \
    "$0" v01.2.3 "$artifact" 01.2.3 main >/dev/null 2>"$tmp/invalid-version.err"; then
    echo "finalize-cask self-test failed: invalid version was accepted" >&2
    return 1
  fi
  grep -q "invalid version: 01.2.3" "$tmp/invalid-version.err"

  make_fixture_repo "$tmp/tag-sha-mismatch" noop
  tag_sha="$(git -C "$tmp/tag-sha-mismatch/work" rev-parse refs/tags/v9.8.7)"
  wrong_sha="$(git -C "$tmp/tag-sha-mismatch/work" -c user.name=t -c user.email=t@example.test commit-tree "$(git -C "$tmp/tag-sha-mismatch/work" rev-parse HEAD^{tree})" -m wrong-sha)"
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/tag-sha-mismatch/work" \
    GITHUB_SHA="$wrong_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/tag-sha-mismatch.err"; then
    echo "finalize-cask self-test failed: mismatched tag SHA was accepted" >&2
    return 1
  fi
  grep -q "does not match tag v9.8.7 commit $tag_sha" "$tmp/tag-sha-mismatch.err"

  make_fixture_repo "$tmp/ancestor" noop
  bad_sha="$(git -C "$tmp/ancestor/work" -c user.name=t -c user.email=t@example.test commit-tree "$(git -C "$tmp/ancestor/work" rev-parse HEAD^{tree})" -m detached)"
  git -C "$tmp/ancestor/work" tag -d v9.8.7 >/dev/null
  git -C "$tmp/ancestor/work" push origin :refs/tags/v9.8.7 >/dev/null 2>&1
  git -C "$tmp/ancestor/work" tag v9.8.7 "$bad_sha"
  git -C "$tmp/ancestor/work" push origin refs/tags/v9.8.7 >/dev/null 2>&1
  if COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/ancestor/work" \
    GITHUB_SHA="$bad_sha" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/ancestor.err"; then
    echo "finalize-cask self-test failed: non-ancestor tag was accepted" >&2
    return 1
  fi
  grep -q "is not on origin/main" "$tmp/ancestor.err"

  make_fixture_repo "$tmp/missing-sha" noop
  # env -u: on GitHub runners GITHUB_SHA is always set, which made this
  # missing-sha case fail for the wrong reason (and the bare grep below exit
  # silently) — the step went red on CI while passing locally.
  if env -u GITHUB_SHA COMPME_FINALIZE_CASK_REPO_ROOT="$tmp/missing-sha/work" \
    "$0" v9.8.7 "$artifact" 9.8.7 main >/dev/null 2>"$tmp/missing-sha.err"; then
    echo "finalize-cask self-test failed: missing GITHUB_SHA was accepted" >&2
    return 1
  fi
  grep -q "GITHUB_SHA is required" "$tmp/missing-sha.err"

  if "$0" v9.8.7 "$artifact" 9.8.7 >/dev/null 2>"$tmp/usage.err"; then
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
