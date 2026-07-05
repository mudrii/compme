#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: check-a2-matrix-ledger.sh LEDGER.tsv | --self-test" >&2
}

expected_rows='textedit notes mail word safari chrome brave browser-exclude terminal-cmd terminal-nlp unsupported clipboard screen'

check_ledger() {
  ledger="$1"
  if [ ! -f "$ledger" ]; then
    echo "missing A2 matrix ledger: $ledger" >&2
    return 1
  fi

  awk -F '\t' -v expected_rows="$expected_rows" '
    BEGIN {
      split(expected_rows, rows, " ")
      for (i in rows) {
        if (rows[i] != "") {
          expected[rows[i]] = 1
          expected_count++
        }
      }
    }
    NR == 1 {
      if ($0 != "row_id\tkind\tapp\tpid\tstatus\texpect\tlog_path") {
        print "invalid A2 matrix ledger header" > "/dev/stderr"
        failed = 1
      }
      next
    }
    {
      row_id = $1
      status = $5
      log_path = $7
      if (!(row_id in expected)) {
        printf "unexpected A2 matrix row: %s\n", row_id > "/dev/stderr"
        failed = 1
      }
      if (seen[row_id]++) {
        printf "duplicate A2 matrix row: %s\n", row_id > "/dev/stderr"
        failed = 1
      }
      if (status != "PASS") {
        printf "A2 matrix row did not pass: %s status=%s\n", row_id, status > "/dev/stderr"
        failed = 1
      }
      if (log_path == "") {
        printf "A2 matrix row missing log_path: %s\n", row_id > "/dev/stderr"
        failed = 1
      }
    }
    END {
      for (row in expected) {
        if (!(row in seen)) {
          printf "missing A2 matrix row: %s\n", row > "/dev/stderr"
          failed = 1
        }
      }
      if ((NR - 1) != expected_count) {
        printf "A2 matrix ledger row count mismatch: got %d expected %d\n", NR - 1, expected_count > "/dev/stderr"
        failed = 1
      }
      exit failed ? 1 : 0
    }
  ' "$ledger"
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-a2-ledger.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT

  good="$tmp/good.tsv"
  {
    printf 'row_id\tkind\tapp\tpid\tstatus\texpect\tlog_path\n'
    for row in $expected_rows; do
      printf '%s\tworks\tfixture\t123\tPASS\trequest\t/tmp/%s.log\n' "$row" "$row"
    done
  } >"$good"
  check_ledger "$good"

  bad_skip="$tmp/bad-skip.tsv"
  cp "$good" "$bad_skip"
  awk -F '\t' 'BEGIN { OFS = FS } NR == 2 { $5 = "SKIP"; $7 = "" } { print }' "$good" >"$bad_skip"
  if check_ledger "$bad_skip" >/dev/null 2>"$tmp/bad-skip.err"; then
    echo "A2 matrix ledger self-test failed: SKIP row was accepted" >&2
    return 1
  fi
  grep -q 'status=SKIP' "$tmp/bad-skip.err"

  missing="$tmp/missing.tsv"
  awk -F '\t' '$1 != "screen"' "$good" >"$missing"
  if check_ledger "$missing" >/dev/null 2>"$tmp/missing.err"; then
    echo "A2 matrix ledger self-test failed: missing row was accepted" >&2
    return 1
  fi
  grep -q 'missing A2 matrix row: screen' "$tmp/missing.err"

  extra="$tmp/extra.tsv"
  cp "$good" "$extra"
  printf 'surprise\tworks\tfixture\t123\tPASS\trequest\t/tmp/surprise.log\n' >>"$extra"
  if check_ledger "$extra" >/dev/null 2>"$tmp/extra.err"; then
    echo "A2 matrix ledger self-test failed: unexpected row was accepted" >&2
    return 1
  fi
  grep -q 'unexpected A2 matrix row: surprise' "$tmp/extra.err"

  if "$0" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "A2 matrix ledger self-test failed: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-a2-matrix-ledger\.sh LEDGER\.tsv | --self-test$' "$tmp/self-test-argc.err"

  if "$0" "$good" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "A2 matrix ledger self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-a2-matrix-ledger\.sh LEDGER\.tsv | --self-test$' "$tmp/normal-argc.err"

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

if [ "$#" -ne 1 ]; then
  usage
  exit 2
fi

check_ledger "$1"
