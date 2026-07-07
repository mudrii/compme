#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: check-a2-matrix-ledger.sh LEDGER.tsv | --self-test" >&2
}

expected_rows='textedit notes mail word safari chrome brave browser-exclude terminal-cmd terminal-nlp unsupported clipboard screen'
ledger_max_age_seconds="${COMPME_A2_LEDGER_MAX_AGE_SECONDS:-86400}"
ledger_max_future_skew_seconds="${COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS:-300}"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
evidence_prefix="tools/acceptance/evidence/a2/"

validate_max_age() {
  ledger_max_age_seconds="${COMPME_A2_LEDGER_MAX_AGE_SECONDS:-86400}"
  case "$ledger_max_age_seconds" in
    ''|*[!0-9]*)
      echo "invalid COMPME_A2_LEDGER_MAX_AGE_SECONDS: $ledger_max_age_seconds" >&2
      return 1
      ;;
    0)
      echo "invalid COMPME_A2_LEDGER_MAX_AGE_SECONDS: $ledger_max_age_seconds" >&2
      return 1
      ;;
  esac
  ledger_max_future_skew_seconds="${COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS:-300}"
  case "$ledger_max_future_skew_seconds" in
    ''|*[!0-9]*)
      echo "invalid COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS: $ledger_max_future_skew_seconds" >&2
      return 1
      ;;
  esac
}

expected_row_app() {
  case "$1" in
    textedit) printf '%s\n' 'com.apple.TextEdit' ;;
    notes) printf '%s\n' 'com.apple.Notes' ;;
    mail) printf '%s\n' 'com.apple.mail' ;;
    word) printf '%s\n' 'com.microsoft.Word' ;;
    safari) printf '%s\n' 'com.apple.Safari' ;;
    chrome) printf '%s\n' 'com.google.Chrome' ;;
    brave) printf '%s\n' 'com.brave.Browser' ;;
    browser-exclude) printf '%s\n' 'browser-domain' ;;
    terminal-cmd|terminal-nlp) printf '%s\n' 'terminal' ;;
    unsupported) printf '%s\n' 'unsupported-app' ;;
    clipboard|screen) printf '%s\n' 'works-app' ;;
    *) return 1 ;;
  esac
}

expected_row_kind() {
  case "$1" in
    textedit|notes|mail|word) printf '%s\n' 'works' ;;
    safari|chrome|brave) printf '%s\n' 'browser-domain-allow' ;;
    browser-exclude) printf '%s\n' 'browser-domain-exclude' ;;
    terminal-cmd) printf '%s\n' 'terminal-cmd' ;;
    terminal-nlp) printf '%s\n' 'terminal-nlp' ;;
    unsupported) printf '%s\n' 'unsupported' ;;
    clipboard) printf '%s\n' 'clipboard' ;;
    screen) printf '%s\n' 'screen' ;;
    *) return 1 ;;
  esac
}

expected_row_expect() {
  case "$1" in
    textedit|notes|mail|word|terminal-nlp) printf '%s\n' 'request' ;;
    safari|chrome|brave) printf '%s\n' 'domain-request' ;;
    browser-exclude) printf '%s\n' 'blocked-prefs' ;;
    terminal-cmd) printf '%s\n' 'blocked-terminal' ;;
    unsupported) printf '%s\n' 'blocked-app' ;;
    clipboard|screen) printf '%s\n' 'context-request' ;;
    *) return 1 ;;
  esac
}

app_pattern() {
  case "$1" in
    com.apple.TextEdit) printf '%s\n' 'com\.apple\.TextEdit' ;;
    com.apple.Notes) printf '%s\n' 'com\.apple\.Notes' ;;
    com.apple.mail) printf '%s\n' 'com\.apple\.mail' ;;
    com.microsoft.Word) printf '%s\n' 'com\.microsoft\.Word' ;;
    com.apple.Safari) printf '%s\n' 'com\.apple\.Safari' ;;
    com.google.Chrome) printf '%s\n' 'com\.google\.Chrome' ;;
    com.brave.Browser) printf '%s\n' 'com\.brave\.Browser' ;;
    terminal) printf '%s\n' '(com\.apple\.Terminal|com\.googlecode\.iterm2)' ;;
    browser-domain) printf '%s\n' '(com\.apple\.Safari|com\.google\.Chrome|com\.brave\.Browser)' ;;
    works-app) printf '%s\n' '(com\.apple\.Safari|com\.google\.Chrome|com\.apple\.mail|com\.microsoft\.Word|com\.apple\.TextEdit|com\.apple\.Notes|notion\.id|md\.obsidian|com\.apple\.MobileSMS)' ;;
    unsupported-app) printf '%s\n' '[^[:space:]]+' ;;
    *) return 1 ;;
  esac
}

log_proves_row() {
  row_id="$1"
  row_kind="$2"
  row_app="$3"
  row_expect="$4"
  log_path="$5"
  row_app_pattern="$(app_pattern "$row_app")" || return 1

  case "$row_expect" in
    request)
      grep -Eq "^compme: request gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app=${row_app_pattern} app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true$" "$log_path" \
        && ! grep -Eq '^compme: request gen=[0-9][0-9]* .* app=unknown ' "$log_path"
      ;;
    domain-request)
      grep -Eq "^compme: domain=[[:alnum:].-]+ \\(${row_app_pattern}\\)$" "$log_path" \
        && ! grep -Eq '^compme: domain=(https?://|[^ ]*[/?:#])' "$log_path" \
        && grep -Eq "^compme: request gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app=${row_app_pattern} app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true$" "$log_path"
      ;;
    context-request)
      grep -Eq "^compme: request gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app=${row_app_pattern} app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true$" "$log_path" || return 1
      case "$row_kind" in
        clipboard)
          grep -Eq 'prompt_context=Some\("sources=[^"]*clipboard[^"]*clipboard_chars=[1-9][0-9]*([^0-9]|")' "$log_path" \
            && grep -Eq 'clipboard_context=Some\(chars=[1-9][0-9]* marker=true\)' "$log_path"
          ;;
        screen)
          grep -Eq 'prompt_context=Some\("sources=[^"]*screen[^"]*screen_chars=[1-9][0-9]*([^0-9]|")' "$log_path"
          ;;
        *)
          return 1
          ;;
      esac
      ;;
    blocked-prefs)
      grep -Eq "^compme: domain=[[:alnum:].-]+ \\(${row_app_pattern}\\)$" "$log_path" \
        && grep -Eq "^compme: request blocked gen=[0-9][0-9]* prompt_chars=[1-9][0-9]* app=${row_app_pattern} app_allows=true terminal_ok=true domain_ready=true prefs_ok=false prompt_marker=true$" "$log_path" \
        && ! grep -Eq '^compme: request gen=' "$log_path"
      ;;
    blocked-terminal)
      grep -Eq "^compme: request blocked .*prompt_chars=[1-9][0-9]* app=${row_app_pattern} .*terminal_ok=false .*prompt_marker=true$" "$log_path" \
        && ! grep -Eq '^compme: request gen=' "$log_path"
      ;;
    blocked-app)
      grep -Eq "^compme: request blocked .*prompt_chars=[1-9][0-9]* app=${row_app_pattern} .*app_allows=false .*prompt_marker=true$" "$log_path" \
        && ! grep -Eq '^compme: request gen=' "$log_path"
      ;;
    *)
      echo "unknown A2 matrix row expectation: $row_id expect=$row_expect" >&2
      return 1
      ;;
  esac
}

log_path_is_safe_evidence() {
  case "$1" in
    /*|..|../*|*/../*) return 1 ;;
    "$evidence_prefix"*) return 0 ;;
    *) return 1 ;;
  esac
}

ledger_path_is_safe_evidence() {
  case "$1" in
    /*|..|../*|*/../*) return 1 ;;
    "$evidence_prefix"*.tsv) return 0 ;;
    *) return 1 ;;
  esac
}

check_ledger() {
  ledger="$1"
  if [ ! -f "$ledger" ]; then
    echo "missing A2 matrix ledger: $ledger" >&2
    return 1
  fi
  case "$ledger" in
    "$repo_root"/*) ledger_rel="${ledger#"$repo_root"/}" ;;
    *) ledger_rel="$ledger" ;;
  esac
  if ! ledger_path_is_safe_evidence "$ledger_rel"; then
    echo "A2 matrix ledger must be a committed repo-relative TSV under $evidence_prefix: $ledger" >&2
    return 1
  fi
  if ! git -C "$repo_root" ls-files --error-unmatch "$ledger_rel" >/dev/null 2>&1; then
    echo "A2 matrix ledger is not tracked: $ledger_rel" >&2
    return 1
  fi
  # Committed-content check: the working tree must match HEAD so tampered
  # uncommitted evidence fails loud. Skipped only on an unborn HEAD (the
  # --self-test fixture repos track files in the index without a commit);
  # real evidence repos always have commits, so the real path verifies
  # committed content.
  head_exists=0
  if git -C "$repo_root" rev-parse --quiet --verify HEAD >/dev/null 2>&1; then
    head_exists=1
  fi
  if [ "$head_exists" = 1 ] && ! git -C "$repo_root" diff --quiet HEAD -- "$ledger_rel"; then
    echo "A2 matrix ledger differs from committed content: $ledger_rel" >&2
    return 1
  fi
  validate_max_age || return 1
  now="$(date +%s)"

  if ! awk -F '\t' -v expected_rows="$expected_rows" -v now="$now" -v max_age="$ledger_max_age_seconds" -v max_future_skew="$ledger_max_future_skew_seconds" '
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
      if ($0 != "generated_at_epoch\trow_id\tkind\tapp\tpid\tstatus\texpect\tlog_path") {
        print "invalid A2 matrix ledger header" > "/dev/stderr"
        failed = 1
      }
      next
    }
    {
      generated_at = $1
      row_id = $2
      status = $6
      log_path = $8
      if (generated_at !~ /^[0-9]+$/ || generated_at == "0") {
        printf "invalid A2 matrix generated_at_epoch: %s row=%s\n", generated_at, row_id > "/dev/stderr"
        failed = 1
      } else {
        if (generated_at > now + max_future_skew) {
          printf "future A2 matrix ledger: row=%s generated_at=%s now=%s max_future_skew=%ds\n", row_id, generated_at, now, max_future_skew > "/dev/stderr"
          failed = 1
        }
        age = now - generated_at
        if (age > max_age) {
          printf "stale A2 matrix ledger: row=%s age=%ds max=%ds\n", row_id, age, max_age > "/dev/stderr"
          failed = 1
        }
      }
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
  ' "$ledger"; then
    return 1
  fi

  old_ifs="$IFS"
  while IFS="$(printf '\t')" read -r generated_at row_id row_kind row_app _pid _status row_expect log_path || [ -n "${generated_at:-}${row_id:-}" ]; do
    if [ "$generated_at" = "generated_at_epoch" ]; then
      continue
    fi
    expected_app="$(expected_row_app "$row_id")" || {
      IFS="$old_ifs"
      echo "unexpected A2 matrix row: $row_id" >&2
      return 1
    }
    expected_kind="$(expected_row_kind "$row_id")" || {
      IFS="$old_ifs"
      echo "unexpected A2 matrix row: $row_id" >&2
      return 1
    }
    expected_expect="$(expected_row_expect "$row_id")" || {
      IFS="$old_ifs"
      echo "unexpected A2 matrix row: $row_id" >&2
      return 1
    }
    if [ "$row_kind" != "$expected_kind" ]; then
      IFS="$old_ifs"
      echo "A2 matrix row kind mismatch: $row_id kind=$row_kind expected=$expected_kind" >&2
      return 1
    fi
    if [ "$row_app" != "$expected_app" ]; then
      IFS="$old_ifs"
      echo "A2 matrix row app mismatch: $row_id app=$row_app expected=$expected_app" >&2
      return 1
    fi
    if [ "$row_expect" != "$expected_expect" ]; then
      IFS="$old_ifs"
      echo "A2 matrix row expect mismatch: $row_id expect=$row_expect expected=$expected_expect" >&2
      return 1
    fi
    if ! log_path_is_safe_evidence "$log_path"; then
      IFS="$old_ifs"
      echo "A2 matrix row log_path must be committed repo evidence: $row_id path=$log_path" >&2
      return 1
    fi
    full_log_path="$repo_root/$log_path"
    if ! git -C "$repo_root" ls-files --error-unmatch "$log_path" >/dev/null 2>&1; then
      IFS="$old_ifs"
      echo "A2 matrix row log_path is not tracked: $row_id path=$log_path" >&2
      return 1
    fi
    if [ "$head_exists" = 1 ] && ! git -C "$repo_root" diff --quiet HEAD -- "$log_path"; then
      IFS="$old_ifs"
      echo "A2 matrix row log_path differs from committed content: $row_id path=$log_path" >&2
      return 1
    fi
    if [ ! -f "$full_log_path" ]; then
      IFS="$old_ifs"
      echo "A2 matrix row log_path missing on disk: $row_id path=$log_path" >&2
      return 1
    fi
    if ! log_proves_row "$row_id" "$row_kind" "$row_app" "$row_expect" "$full_log_path"; then
      IFS="$old_ifs"
      echo "A2 matrix row log_path lacks proof: $row_id kind=$row_kind expect=$row_expect path=$log_path" >&2
      return 1
    fi
  done <"$ledger"
  IFS="$old_ifs"
}

run_self_test() {
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-a2-ledger.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  generated_at="$(date +%s)"
  repo_root="$tmp/repo"
  log_dir_rel="$evidence_prefix/self-test"
  log_dir="$repo_root/$log_dir_rel"
  mkdir -p "$log_dir"
  git -C "$repo_root" init -q

  write_good_log() {
    row="$1"
    kind="$2"
    app="$3"
    expect="$4"
    log="$5"
    case "$expect" in
      request)
        case "$kind" in
          terminal-nlp)
            printf 'compme: request gen=7 prompt_chars=44 app=com.apple.Terminal app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\n' >"$log"
            ;;
          *)
            printf 'compme: request gen=7 prompt_chars=44 app=%s app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\n' "$app" >"$log"
            ;;
        esac
        ;;
      domain-request)
        printf 'compme: domain=docs.google.com (%s)\ncompme: request gen=7 prompt_chars=44 app=%s app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\n' "$app" "$app" >"$log"
        ;;
      context-request)
        case "$kind" in
          clipboard)
            printf 'compme: request gen=7 prompt_chars=44 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\ncompme: clipboard_context=Some(chars=24 marker=true)\ncompme: prompt_context=Some("sources=clipboard chars=24 clipboard_chars=24")\n' >"$log"
            ;;
          screen)
            printf 'compme: request gen=7 prompt_chars=44 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\ncompme: screen_context=Some(12)\ncompme: prompt_context=Some("sources=screen chars=12 screen_chars=12")\n' >"$log"
            ;;
        esac
        ;;
      blocked-prefs)
        printf 'compme: domain=docs.google.com (com.google.Chrome)\ncompme: request blocked gen=8 prompt_chars=44 app=com.google.Chrome app_allows=true terminal_ok=true domain_ready=true prefs_ok=false prompt_marker=true\n' >"$log"
        ;;
      blocked-terminal)
        printf 'compme: request blocked gen=8 prompt_chars=20 app=com.apple.Terminal app_allows=true terminal_ok=false domain_ready=true prefs_ok=true prompt_marker=true\n' >"$log"
        ;;
      blocked-app)
        printf 'compme: request blocked gen=7 prompt_chars=28 app=com.mitchellh.ghostty app_allows=false terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\n' >"$log"
        ;;
      *)
        printf 'bad fixture row=%s kind=%s expect=%s\n' "$row" "$kind" "$expect" >"$log"
        ;;
    esac
  }

  row_spec() {
    case "$1" in
      safari|chrome|brave) printf '%s\n' "browser-domain-allow|$(expected_row_app "$1")|domain-request" ;;
      browser-exclude) printf '%s\n' "browser-domain-exclude|browser-domain|blocked-prefs" ;;
      terminal-cmd) printf '%s\n' "terminal-cmd|terminal|blocked-terminal" ;;
      terminal-nlp) printf '%s\n' "terminal-nlp|terminal|request" ;;
      unsupported) printf '%s\n' "unsupported|unsupported-app|blocked-app" ;;
      clipboard) printf '%s\n' "clipboard|works-app|context-request" ;;
      screen) printf '%s\n' "screen|works-app|context-request" ;;
      *) printf '%s\n' "works|$(expected_row_app "$1")|request" ;;
    esac
  }

  good="$repo_root/$log_dir_rel/good.tsv"
  {
    printf 'generated_at_epoch\trow_id\tkind\tapp\tpid\tstatus\texpect\tlog_path\n'
    for row in $expected_rows; do
      IFS='|' read -r kind app expect <<EOF
$(row_spec "$row")
EOF
      write_good_log "$row" "$kind" "$app" "$expect" "$log_dir/$row.log"
      printf '%s\t%s\t%s\t%s\t123\tPASS\t%s\t%s/%s.log\n' "$generated_at" "$row" "$kind" "$app" "$expect" "$log_dir_rel" "$row"
    done
  } >"$good"
  (cd "$repo_root" && git add "$log_dir_rel"/*.log "$log_dir_rel/good.tsv")
  check_ledger "$good"

  track_ledger() {
    git -C "$repo_root" add "${1#"$repo_root"/}"
  }

  untracked_ledger="$repo_root/$log_dir_rel/untracked.tsv"
  cp "$good" "$untracked_ledger"
  if check_ledger "$untracked_ledger" >/dev/null 2>"$tmp/untracked-ledger.err"; then
    echo "A2 matrix ledger self-test failed: untracked ledger was accepted" >&2
    return 1
  fi
  grep -q "A2 matrix ledger is not tracked: $log_dir_rel/untracked.tsv" "$tmp/untracked-ledger.err"

  unsafe_ledger="$tmp/unsafe.tsv"
  cp "$good" "$unsafe_ledger"
  if check_ledger "$unsafe_ledger" >/dev/null 2>"$tmp/unsafe-ledger.err"; then
    echo "A2 matrix ledger self-test failed: unsafe ledger path was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix ledger must be a committed repo-relative TSV under tools/acceptance/evidence/a2/' "$tmp/unsafe-ledger.err"

  bad_skip="$repo_root/$log_dir_rel/bad-skip.tsv"
  cp "$good" "$bad_skip"
  awk -F '\t' 'BEGIN { OFS = FS } NR == 2 { $6 = "SKIP"; $8 = "" } { print }' "$good" >"$bad_skip"
  track_ledger "$bad_skip"
  if check_ledger "$bad_skip" >/dev/null 2>"$tmp/bad-skip.err"; then
    echo "A2 matrix ledger self-test failed: SKIP row was accepted" >&2
    return 1
  fi
  grep -q 'status=SKIP' "$tmp/bad-skip.err"

  missing="$repo_root/$log_dir_rel/missing.tsv"
  awk -F '\t' '$2 != "screen"' "$good" >"$missing"
  track_ledger "$missing"
  if check_ledger "$missing" >/dev/null 2>"$tmp/missing.err"; then
    echo "A2 matrix ledger self-test failed: missing row was accepted" >&2
    return 1
  fi
  grep -q 'missing A2 matrix row: screen' "$tmp/missing.err"

  extra="$repo_root/$log_dir_rel/extra.tsv"
  cp "$good" "$extra"
  printf '%s\tsurprise\tworks\tfixture\t123\tPASS\trequest\t/tmp/surprise.log\n' "$generated_at" >>"$extra"
  track_ledger "$extra"
  if check_ledger "$extra" >/dev/null 2>"$tmp/extra.err"; then
    echo "A2 matrix ledger self-test failed: unexpected row was accepted" >&2
    return 1
  fi
  grep -q 'unexpected A2 matrix row: surprise' "$tmp/extra.err"

  missing_log="$repo_root/$log_dir_rel/missing-log.tsv"
  cp "$good" "$missing_log"
  track_ledger "$missing_log"
  rm -f "$log_dir/textedit.log"
  if check_ledger "$missing_log" >/dev/null 2>"$tmp/missing-log.err"; then
    echo "A2 matrix ledger self-test failed: nonexistent log_path was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path missing on disk: textedit' "$tmp/missing-log.err"
  write_good_log textedit works com.apple.TextEdit request "$log_dir/textedit.log"

  untracked_log="$repo_root/$log_dir_rel/untracked-log.tsv"
  cp "$good" "$untracked_log"
  write_good_log textedit works com.apple.TextEdit request "$log_dir/untracked.log"
  awk -F '\t' 'BEGIN { OFS = FS } $2 == "textedit" { $8 = path } { print }' path="$log_dir_rel/untracked.log" "$good" >"$untracked_log"
  track_ledger "$untracked_log"
  if check_ledger "$untracked_log" >/dev/null 2>"$tmp/untracked-log.err"; then
    echo "A2 matrix ledger self-test failed: untracked log_path was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path is not tracked: textedit' "$tmp/untracked-log.err"

  unsafe_log_path="$repo_root/$log_dir_rel/unsafe-log-path.tsv"
  awk -F '\t' 'BEGIN { OFS = FS } $2 == "textedit" { $8 = "/tmp/textedit.log" } { print }' "$good" >"$unsafe_log_path"
  track_ledger "$unsafe_log_path"
  if check_ledger "$unsafe_log_path" >/dev/null 2>"$tmp/unsafe-log-path.err"; then
    echo "A2 matrix ledger self-test failed: unsafe log_path was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path must be committed repo evidence: textedit' "$tmp/unsafe-log-path.err"

  tampered_log="$repo_root/$log_dir_rel/tampered-log.tsv"
  cp "$good" "$tampered_log"
  track_ledger "$tampered_log"
  printf 'no request evidence\n' >"$log_dir/textedit.log"
  if check_ledger "$tampered_log" >/dev/null 2>"$tmp/tampered-log.err"; then
    echo "A2 matrix ledger self-test failed: tampered log_path was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path lacks proof: textedit' "$tmp/tampered-log.err"
  write_good_log textedit works com.apple.TextEdit request "$log_dir/textedit.log"

  wrong_app="$repo_root/$log_dir_rel/wrong-app.tsv"
  cp "$good" "$wrong_app"
  awk -F '\t' 'BEGIN { OFS = FS } $2 == "notes" { $4 = "com.apple.TextEdit"; $8 = path } { print }' path="$log_dir_rel/textedit.log" "$good" >"$wrong_app"
  track_ledger "$wrong_app"
  if check_ledger "$wrong_app" >/dev/null 2>"$tmp/wrong-app.err"; then
    echo "A2 matrix ledger self-test failed: wrong app row was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row app mismatch: notes app=com.apple.TextEdit expected=com.apple.Notes' "$tmp/wrong-app.err"

  wrong_kind="$repo_root/$log_dir_rel/wrong-kind.tsv"
  awk -F '\t' 'BEGIN { OFS = FS } $2 == "textedit" { $3 = "unsupported" } { print }' "$good" >"$wrong_kind"
  track_ledger "$wrong_kind"
  if check_ledger "$wrong_kind" >/dev/null 2>"$tmp/wrong-kind.err"; then
    echo "A2 matrix ledger self-test failed: wrong kind row was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row kind mismatch: textedit kind=unsupported expected=works' "$tmp/wrong-kind.err"

  wrong_expect="$repo_root/$log_dir_rel/wrong-expect.tsv"
  awk -F '\t' 'BEGIN { OFS = FS } $2 == "textedit" { $7 = "blocked-app" } { print }' "$good" >"$wrong_expect"
  track_ledger "$wrong_expect"
  if check_ledger "$wrong_expect" >/dev/null 2>"$tmp/wrong-expect.err"; then
    echo "A2 matrix ledger self-test failed: wrong expect row was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row expect mismatch: textedit expect=blocked-app expected=request' "$tmp/wrong-expect.err"

  wrong_app_log="$repo_root/$log_dir_rel/wrong-app-log.tsv"
  cp "$good" "$wrong_app_log"
  track_ledger "$wrong_app_log"
  printf 'compme: request gen=7 prompt_chars=44 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\n' >"$log_dir/notes.log"
  if check_ledger "$wrong_app_log" >/dev/null 2>"$tmp/wrong-app-log.err"; then
    echo "A2 matrix ledger self-test failed: wrong app log was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path lacks proof: notes' "$tmp/wrong-app-log.err"
  write_good_log notes works com.apple.Notes request "$log_dir/notes.log"

  no_newline="$repo_root/$log_dir_rel/no-newline.tsv"
  awk -F '\t' 'BEGIN { OFS = FS } { lines[NR] = $0 } END { for (i = 1; i < NR; i++) print lines[i]; printf "%s", lines[NR] }' "$good" >"$no_newline"
  track_ledger "$no_newline"
  rm -f "$log_dir/screen.log"
  if check_ledger "$no_newline" >/dev/null 2>"$tmp/no-newline.err"; then
    echo "A2 matrix ledger self-test failed: no-newline missing final log was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path missing on disk: screen' "$tmp/no-newline.err"
  write_good_log screen screen works-app context-request "$log_dir/screen.log"

  stale="$repo_root/$log_dir_rel/stale.tsv"
  awk -F '\t' 'BEGIN { OFS = FS } NR > 1 { $1 = 946684800 } { print }' "$good" >"$stale"
  track_ledger "$stale"
  if COMPME_A2_LEDGER_MAX_AGE_SECONDS=60 check_ledger "$stale" >/dev/null 2>"$tmp/stale.err"; then
    echo "A2 matrix ledger self-test failed: stale ledger was accepted" >&2
    return 1
  fi
  grep -q 'stale A2 matrix ledger:' "$tmp/stale.err"

  future="$repo_root/$log_dir_rel/future.tsv"
  awk -F '\t' -v future_epoch="$((generated_at + 3600))" 'BEGIN { OFS = FS } NR > 1 { $1 = future_epoch } { print }' "$good" >"$future"
  track_ledger "$future"
  if check_ledger "$future" >/dev/null 2>"$tmp/future.err"; then
    echo "A2 matrix ledger self-test failed: future ledger was accepted" >&2
    return 1
  fi
  grep -q 'future A2 matrix ledger:' "$tmp/future.err"

  if COMPME_A2_LEDGER_MAX_AGE_SECONDS=abc check_ledger "$good" >/dev/null 2>"$tmp/bad-age.err"; then
    echo "A2 matrix ledger self-test failed: invalid max age was accepted" >&2
    return 1
  fi
  grep -q 'invalid COMPME_A2_LEDGER_MAX_AGE_SECONDS' "$tmp/bad-age.err"

  if COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS=abc check_ledger "$good" >/dev/null 2>"$tmp/bad-future-skew.err"; then
    echo "A2 matrix ledger self-test failed: invalid future skew was accepted" >&2
    return 1
  fi
  grep -q 'invalid COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS' "$tmp/bad-future-skew.err"

  # Committed-repo fixture: with a HEAD present, uncommitted tampering must
  # fail even when the tampered content still contains valid proof lines.
  committed_repo="$tmp/repo-committed"
  repo_root="$committed_repo"
  log_dir="$committed_repo/$log_dir_rel"
  mkdir -p "$log_dir"
  git -C "$committed_repo" init -q
  committed_good="$committed_repo/$log_dir_rel/good.tsv"
  {
    printf 'generated_at_epoch\trow_id\tkind\tapp\tpid\tstatus\texpect\tlog_path\n'
    for row in $expected_rows; do
      IFS='|' read -r kind app expect <<EOF
$(row_spec "$row")
EOF
      write_good_log "$row" "$kind" "$app" "$expect" "$log_dir/$row.log"
      printf '%s\t%s\t%s\t%s\t123\tPASS\t%s\t%s/%s.log\n' "$generated_at" "$row" "$kind" "$app" "$expect" "$log_dir_rel" "$row"
    done
  } >"$committed_good"
  (cd "$committed_repo" && git add "$log_dir_rel"/*.log "$log_dir_rel/good.tsv")
  git -C "$committed_repo" -c user.name=self-test -c user.email=self-test@example.invalid \
    -c commit.gpgsign=false commit -q --no-verify -m 'a2 ledger self-test fixture'
  check_ledger "$committed_good"

  printf 'compme: request gen=9 prompt_chars=50 app=com.apple.TextEdit app_allows=true terminal_ok=true domain_ready=true prefs_ok=true prompt_marker=true\n' >>"$log_dir/textedit.log"
  if check_ledger "$committed_good" >/dev/null 2>"$tmp/uncommitted-log.err"; then
    echo "A2 matrix ledger self-test failed: uncommitted log tamper containing proof was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix row log_path differs from committed content: textedit' "$tmp/uncommitted-log.err"
  git -C "$committed_repo" checkout -q -- "$log_dir_rel/textedit.log"

  printf '# tampered\n' >>"$committed_good"
  if check_ledger "$committed_good" >/dev/null 2>"$tmp/uncommitted-ledger.err"; then
    echo "A2 matrix ledger self-test failed: uncommitted ledger tamper was accepted" >&2
    return 1
  fi
  grep -q 'A2 matrix ledger differs from committed content:' "$tmp/uncommitted-ledger.err"

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
