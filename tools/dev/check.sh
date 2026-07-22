#!/usr/bin/env bash
# Run the Full Local Gate exactly as documented: extract the ```sh fence under
# "## Full Local Gate" in docs/DEVELOPMENT.md and run each non-empty,
# non-comment line in order, from the repo root, in this one shell so the
# mid-block `cd tools/spike` persists across lines. Commands whose tool is
# missing from PATH (shellcheck, cargo-audit on a fresh machine) are skipped
# with a note; the first failing command stops the gate.
# Usage: check.sh [--file DEVELOPMENT.md]
#        check.sh --self-test
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

usage() {
  echo "usage: $0 [--file DEVELOPMENT.md] | --self-test" >&2
}

extract_gate() {
  # Print the body of the ```sh fence under "## Full Local Gate": every line
  # between the opening fence after the heading and its closing fence.
  awk '
    /^## Full Local Gate[[:space:]]*$/ { in_section = 1; next }
    in_section && /^## / { exit }
    in_section && !in_fence && /^```sh[[:space:]]*$/ { in_fence = 1; next }
    in_fence && /^```[[:space:]]*$/ { exit }
    in_fence { print }
  ' "$1"
}

probe_missing_tool() {
  # $1 = one gate command line. When a tool the line needs is absent from
  # PATH, print its name and return 1; return 0 when every probed tool is
  # present. Probes the first word of each pipeline stage plus the command
  # xargs would run (the gate's xargs uses flag options only, so the first
  # non-option argument is that command), and cargo-audit for `cargo audit`,
  # which docs/DEVELOPMENT.md lists as a separate install. Repo-relative
  # paths (tools/...) are not probed: a missing repo script must fail the
  # gate, not be skipped.
  local stripped="$1"
  local env_re='^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*=("[^"]*"|[^[:space:]]+)[[:space:]]+'
  while [[ "$stripped" =~ $env_re ]]; do
    stripped="${stripped#"${BASH_REMATCH[0]}"}"
  done
  if [[ -z "$stripped" ]]; then
    return 0
  fi
  local -a tools=()
  local -a stages=()
  local -a stage_words=()
  local -a line_words=()
  local stage word j t
  IFS='|' read -ra stages <<<"$stripped"
  for stage in "${stages[@]}"; do
    read -ra stage_words <<<"$stage"
    if [[ "${#stage_words[@]}" -eq 0 ]]; then
      continue
    fi
    word="${stage_words[0]}"
    tools+=("$word")
    if [[ "$word" == "xargs" ]]; then
      for ((j = 1; j < ${#stage_words[@]}; j++)); do
        case "${stage_words[$j]}" in
          -*) ;;
          *)
            tools+=("${stage_words[$j]}")
            break
            ;;
        esac
      done
    fi
  done
  read -ra line_words <<<"$stripped"
  if [[ "${#line_words[@]}" -ge 2 && "${line_words[0]}" == "cargo" && "${line_words[1]}" == "audit" ]]; then
    tools+=("cargo-audit")
  fi
  if [[ "${#tools[@]}" -eq 0 ]]; then
    return 0
  fi
  for t in "${tools[@]}"; do
    case "$t" in
      */*) ;;
      *)
        if ! command -v "$t" >/dev/null 2>&1; then
          printf '%s\n' "$t"
          return 1
        fi
        ;;
    esac
  done
  return 0
}

run_gate() {
  local file="$1"
  if [[ ! -r "$file" ]]; then
    echo "check.sh: cannot read gate file: $file" >&2
    return 2
  fi
  local extracted
  extracted="$(extract_gate "$file")"
  if [[ -z "$extracted" ]]; then
    echo "check.sh: no Full Local Gate sh-fence found in: $file" >&2
    return 2
  fi

  local -a cmds=()
  local line
  while IFS= read -r line; do
    if [[ "$line" =~ ^[[:space:]]*$ || "$line" =~ ^[[:space:]]*# ]]; then
      continue
    fi
    cmds+=("$line")
  done <<<"$extracted"
  local total="${#cmds[@]}"
  if [[ "$total" -eq 0 ]]; then
    echo "check.sh: Full Local Gate fence has no commands in: $file" >&2
    return 2
  fi

  # An inherited CDPATH could redirect the fence's relative `cd tools/spike`.
  unset CDPATH
  cd "$repo_root"

  local n=0 run=0 skipped=0 i
  local missing
  for ((i = 0; i < total; i++)); do
    line="${cmds[$i]}"
    n=$((n + 1))
    if missing="$(probe_missing_tool "$line")"; then
      printf '==> [%d/%d] %s\n' "$n" "$total" "$line"
      if ! eval "$line"; then
        printf 'check.sh: FAILED [%d/%d]: %s\n' "$n" "$total" "$line" >&2
        return 1
      fi
      run=$((run + 1))
    else
      printf -- '--  [%d/%d] skipped (missing tool: %s): %s\n' "$n" "$total" "$missing" "$line"
      skipped=$((skipped + 1))
    fi
  done

  printf 'check.sh: gate complete: %d run, %d skipped of %d commands\n' "$run" "$skipped" "$total"
  if [[ "$skipped" -gt 0 ]]; then
    printf 'check.sh: note: %d command(s) skipped for missing tools; install them and re-run for the full gate\n' "$skipped"
  fi
  return 0
}

run_self_test() {
  local name
  for name in CDPATH COMPME_CHECK_FAKE_LOG COMPME_CHECK_FAKE_CARGO_FAIL; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "check.sh self-test failed: inherited $name" >&2
      return 1
    fi
  done
  unset CDPATH COMPME_CHECK_FAKE_LOG COMPME_CHECK_FAKE_CARGO_FAIL
  # Not local: the EXIT trap expands $tmp after run_self_test returns.
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-check.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  local script="$repo_root/tools/dev/check.sh"

  # The inherited-env guard must reject user-preset vars loudly.
  if CDPATH="$tmp" "$script" --self-test >/dev/null 2>"$tmp/poison-cdpath.err"; then
    echo "check.sh self-test failed: inherited CDPATH was accepted" >&2
    return 1
  fi
  grep -q 'check.sh self-test failed: inherited CDPATH' "$tmp/poison-cdpath.err"
  if COMPME_CHECK_FAKE_LOG="$tmp/poisoned.log" "$script" --self-test >/dev/null 2>"$tmp/poison-log.err"; then
    echo "check.sh self-test failed: inherited COMPME_CHECK_FAKE_LOG was accepted" >&2
    return 1
  fi
  grep -q 'check.sh self-test failed: inherited COMPME_CHECK_FAKE_LOG' "$tmp/poison-log.err"

  local bin_full="$tmp/bin-full"
  local bin_min="$tmp/bin-min"
  mkdir -p "$bin_full" "$bin_min" "$tmp/fixture-tools"
  cat >"$tmp/fake-cargo" <<'SH'
#!/usr/bin/env bash
printf 'cargo|%s|%s\n' "$*" "$PWD" >>"$COMPME_CHECK_FAKE_LOG"
if [ -n "${COMPME_CHECK_FAKE_CARGO_FAIL:-}" ] && [ "${1:-}" = "$COMPME_CHECK_FAKE_CARGO_FAIL" ]; then
  exit 42
fi
SH
  cp "$tmp/fake-cargo" "$bin_full/cargo"
  cp "$tmp/fake-cargo" "$bin_min/cargo"
  cat >"$bin_full/shellcheck" <<'SH'
#!/usr/bin/env bash
printf 'shellcheck|%s\n' "${1:-}" >>"$COMPME_CHECK_FAKE_LOG"
SH
  cat >"$bin_full/cargo-audit" <<'SH'
#!/usr/bin/env bash
exit 0
SH
  chmod +x "$bin_full/cargo" "$bin_full/shellcheck" "$bin_full/cargo-audit" "$bin_min/cargo"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$tmp/fixture-tools/a.sh"

  local fixture="$tmp/DEVELOPMENT.md"
  cat >"$fixture" <<'MD'
# Development

## Root Workspace Commands

```sh
echo wrong-fence-extracted
```

## Full Local Gate

Run this before committing:

```sh
# a comment line that must be ignored

cargo fmt --all -- --check
cargo audit
find @TMP@/fixture-tools -type f -name '*.sh' -print0 | xargs -0 shellcheck --severity=error
cd tools/spike
cargo test --locked
```

## Another Section

```sh
echo should-not-be-extracted
```
MD
  sed "s|@TMP@|$tmp|g" "$fixture" >"$fixture.tmp"
  mv "$fixture.tmp" "$fixture"

  # Full toolchain: every command runs in fence order, and the mid-block
  # relative `cd tools/spike` persists for the lines after it.
  PATH="$bin_full:/usr/bin:/bin" COMPME_CHECK_FAKE_LOG="$tmp/full.log" \
    "$script" --file "$fixture" >"$tmp/full.out"
  cat >"$tmp/full.expected" <<EOF
cargo|fmt --all -- --check|$repo_root
cargo|audit|$repo_root
shellcheck|--severity=error
cargo|test --locked|$repo_root/tools/spike
EOF
  diff "$tmp/full.expected" "$tmp/full.log"
  grep -q '==> \[1/5\] cargo fmt --all -- --check' "$tmp/full.out"
  grep -q '==> \[4/5\] cd tools/spike' "$tmp/full.out"
  grep -q '==> \[5/5\] cargo test --locked' "$tmp/full.out"
  grep -q 'check.sh: gate complete: 5 run, 0 skipped of 5 commands' "$tmp/full.out"
  if grep -q 'wrong-fence-extracted\|should-not-be-extracted' "$tmp/full.out"; then
    echo "check.sh self-test failed: extraction leaked outside the gate fence" >&2
    return 1
  fi

  # Fresh machine (no shellcheck, no cargo-audit): both commands skip with a
  # note and the gate still completes.
  PATH="$bin_min:/usr/bin:/bin" COMPME_CHECK_FAKE_LOG="$tmp/min.log" \
    "$script" --file "$fixture" >"$tmp/min.out"
  cat >"$tmp/min.expected" <<EOF
cargo|fmt --all -- --check|$repo_root
cargo|test --locked|$repo_root/tools/spike
EOF
  diff "$tmp/min.expected" "$tmp/min.log"
  grep -q 'skipped (missing tool: cargo-audit): cargo audit' "$tmp/min.out"
  grep -q "skipped (missing tool: shellcheck): find $tmp/fixture-tools" "$tmp/min.out"
  grep -q 'check.sh: gate complete: 3 run, 2 skipped of 5 commands' "$tmp/min.out"
  grep -q 'check.sh: note: 2 command(s) skipped for missing tools' "$tmp/min.out"

  # A failing command stops the gate: echoed before running, named on failure,
  # later commands never run, no completion summary.
  if PATH="$bin_full:/usr/bin:/bin" COMPME_CHECK_FAKE_LOG="$tmp/fail.log" \
    COMPME_CHECK_FAKE_CARGO_FAIL=audit \
    "$script" --file "$fixture" >"$tmp/fail.out" 2>"$tmp/fail.err"; then
    echo "check.sh self-test failed: a failing gate command was accepted" >&2
    return 1
  fi
  grep -q '==> \[2/5\] cargo audit' "$tmp/fail.out"
  grep -q 'check.sh: FAILED \[2/5\]: cargo audit' "$tmp/fail.err"
  cat >"$tmp/fail.expected" <<EOF
cargo|fmt --all -- --check|$repo_root
cargo|audit|$repo_root
EOF
  diff "$tmp/fail.expected" "$tmp/fail.log"
  if grep -q '==> \[3/5\]\|gate complete' "$tmp/fail.out"; then
    echo "check.sh self-test failed: gate continued past the failing command" >&2
    return 1
  fi

  # A document without the gate fence, an unreadable file, and bad argument
  # shapes are all rejected.
  local nofence="$tmp/no-fence.md"
  cat >"$nofence" <<'MD'
# Development

## Full Local Gate

No sh fence in this section.

## Next Section
MD
  if "$script" --file "$nofence" >/dev/null 2>"$tmp/nofence.err"; then
    echo "check.sh self-test failed: a file without the gate fence was accepted" >&2
    return 1
  fi
  grep -q 'check.sh: no Full Local Gate sh-fence found' "$tmp/nofence.err"
  if "$script" --file "$tmp/does-not-exist.md" >/dev/null 2>"$tmp/unreadable.err"; then
    echo "check.sh self-test failed: an unreadable gate file was accepted" >&2
    return 1
  fi
  grep -q 'check.sh: cannot read gate file' "$tmp/unreadable.err"
  if "$script" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "check.sh self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: .*check\.sh \[--file DEVELOPMENT\.md\] | --self-test$' "$tmp/self-test-argc.err"
  if "$script" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "check.sh self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: ' "$tmp/normal-argc.err"
  if "$script" --file >/dev/null 2>"$tmp/file-argc.err"; then
    echo "check.sh self-test failed: --file without a path was accepted" >&2
    return 1
  fi
  grep -q '^usage: ' "$tmp/file-argc.err"

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

gate_file="$repo_root/docs/DEVELOPMENT.md"
case "$#" in
  0) ;;
  2)
    if [[ "$1" != "--file" ]]; then
      usage
      exit 2
    fi
    gate_file="$2"
    ;;
  *)
    usage
    exit 2
    ;;
esac

run_gate "$gate_file"
