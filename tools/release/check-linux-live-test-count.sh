#!/usr/bin/env bash
# Keep ROADMAP's Linux live-test total tied to Cargo's emitted ignored-test
# list. Source attributes are deliberately not counted: doc comments and
# target cfgs make raw-grep totals lie.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

usage() {
  echo "usage: check-linux-live-test-count.sh [--self-test]" >&2
}

read_documented_counts() {
  roadmap="$1"
  sed -n 's/.*current tree carries \*\*\([0-9][0-9]*\) live tests\*\* (\([0-9][0-9]*\) AT-SPI\/X11 adapter tests + 1 each for confirm, keyring, reveal).*/\1 \2/p' "$roadmap"
}

read_emitted_counts() {
  list_file="$1"
  awk '
    /: test$/ {
      total++
      if ($0 ~ /^confirm::live_tests::/) confirm++
      else if ($0 ~ /^keyring::live_tests::/) keyring++
      else if ($0 ~ /^reveal::live_tests::/) reveal++
      else adapter++
    }
    END { print total + 0, adapter + 0, confirm + 0, keyring + 0, reveal + 0 }
  ' "$list_file"
}

check_count() {
  list_file="$1"
  roadmap="$2"
  documented="$(read_documented_counts "$roadmap")"
  if [ "$(printf '%s\n' "$documented" | sed '/^$/d' | wc -l | tr -d ' ')" -ne 1 ]; then
    echo "linux live-test count failed: ROADMAP must contain exactly one current-tree count line" >&2
    return 1
  fi

  set -- $(read_emitted_counts "$list_file")
  actual_total="$1"
  actual_adapter="$2"
  actual_confirm="$3"
  actual_keyring="$4"
  actual_reveal="$5"
  set -- $documented
  documented_total="$1"
  documented_adapter="$2"

  if [ "$actual_confirm" -ne 1 ] || [ "$actual_keyring" -ne 1 ] || [ "$actual_reveal" -ne 1 ]; then
    echo "linux live-test count failed: expected one confirm, keyring, and reveal test; got $actual_confirm/$actual_keyring/$actual_reveal" >&2
    return 1
  fi
  if [ "$actual_total" -ne "$documented_total" ] || [ "$actual_adapter" -ne "$documented_adapter" ]; then
    echo "linux live-test count failed: emitted $actual_total live tests ($actual_adapter AT-SPI/X11 adapter tests + 1 each for confirm, keyring, reveal), ROADMAP documents $documented_total ($documented_adapter AT-SPI/X11 adapter tests + 1 each)" >&2
    return 1
  fi

  echo "Linux live-test count OK: $actual_total ($actual_adapter AT-SPI/X11 adapter tests + 1 each for confirm, keyring, reveal)"
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-linux-live-count.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  cat >"$tmp/list.txt" <<'LIST'
atspi_live_tests::live_read: test
atspi_live_tests::x11_accept_tap::live_accept: test
confirm::live_tests::confirm: test
keyring::live_tests::keyring: test
reveal::live_tests::reveal: test
not a test line
LIST
  cat >"$tmp/ROADMAP.md" <<'MD'
The suite has grown: the current tree carries **5 live tests** (2 AT-SPI/X11 adapter tests + 1 each for confirm, keyring, reveal).
MD
  check_count "$tmp/list.txt" "$tmp/ROADMAP.md" >/dev/null

  sed 's/\*\*5 live tests\*\*/**6 live tests**/' "$tmp/ROADMAP.md" >"$tmp/stale.md"
  if check_count "$tmp/list.txt" "$tmp/stale.md" >/dev/null 2>&1; then
    echo "linux live-test count self-test failed: stale total was accepted" >&2
    return 1
  fi

  printf '%s\n' 'confirm::live_tests::duplicate: test' >>"$tmp/list.txt"
  if check_count "$tmp/list.txt" "$tmp/ROADMAP.md" >/dev/null 2>&1; then
    echo "linux live-test count self-test failed: duplicate shell-service test was accepted" >&2
    return 1
  fi

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/argc.err"; then
    echo "linux live-test count self-test failed: extra argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-linux-live-test-count\.sh \[--self-test\]$' "$tmp/argc.err"
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
if [ "$#" -ne 0 ]; then
  usage
  exit 2
fi
if [ "$(uname -s)" != "Linux" ]; then
  echo "linux live-test count failed: normal mode requires Linux; use --self-test elsewhere" >&2
  exit 1
fi

list_file="$(mktemp "${TMPDIR:-/tmp}/compme-linux-live-list.XXXXXX")"
trap 'rm -f "$list_file"' EXIT
cargo test --locked -p platform_linux -- --list --ignored >"$list_file"
check_count "$list_file" "$repo_root/docs/ROADMAP.md"
