# Development brief — 2026-09-22 full-codebase review fixes

**Date:** 2026-09-22 · **Tree reviewed:** `9ebd11b` (main) · **Status:** authorized for
implementation; nothing below has landed yet.
**Evidence base:** independent review of every crate, the docs, the scripts, and CI;
the Linux-portable gate on the reviewed tree is green (1,866 passed / 0 failed /
50 ignored; model_client CPU real-model gate 6/6; quality corpus pass). Every
"confirmed" item below was reproduced by tracing the code or by a probe run
against the real model; the probe outputs are quoted where they exist.

Read `AGENTS.md` first. Rules that matter for this batch: minimal diffs, every
non-trivial change ships with a test, run the gate before each commit, and
re-stamp any checker pin you move in the same commit. Commit each numbered item
separately on `main`.

## Gate on this Linux host

The documented Full Local Gate stops at `cargo clippy --workspace` here (objc2 is
Apple-only). Run the CI Linux lane instead, and push so the mac lane proves the
`platform_macos` items:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings
cargo test --locked --workspace --exclude platform_macos --exclude app --all-targets
cargo test --locked -p app --all-targets -- --test-threads=1
cargo test --locked --doc --workspace --exclude platform_macos
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --exclude platform_macos
tools/release/check-version-docs.sh
bash tools/release/check-model-gates.sh --self-test
```

For item 1 also run the real-model step by hand (the script's later
`tools/spike` step is mac-only):

```sh
COMPME_MODEL_GPU_LAYERS=0 COMPME_MODEL_CONTEXT_TOKENS=256 COMPME_REQUIRE_MODEL_TESTS=1 \
COMPME_REQUIRE_MODEL_CONTEXT=1 COMPME_REQUIRE_LATENCY_BUDGET=0 \
cargo test --locked -p model_client --test latency -- --ignored --test-threads=1
```

Tripwire: `tools/release/check-model-gates.sh` pins named test symbols in
`crates/app/src/inference.rs`, `run_loop_tests.rs`, `model_client/tests/latency.rs`
and others (`grep require_test_symbol`). Do not rename pinned tests; adding tests
is fine. The macOS lane also pins the workspace test count in README,
DEVELOPMENT (two lines), ROADMAP header, and the grammar spec — CI on the mac
lane will tell you the new number; update all five in one commit.

---

## Batch A — one-line-class fixes with regressions (do first)

### A1. Prefix-KV reuse is dead on the production completion path (HIGH, confirmed)

`crates/model_client/src/lib.rs:506` — `complete_candidates_on_worker` runs
`prev_tokens.clear()` at the top of every candidate iteration, including index 0.
The app always calls `complete_n` (`crates/app/src/inference.rs:523`,
`DEFAULT_CANDIDATES = 1`), so every debounce re-decodes the whole prompt.

Probe on the real model (CPU, ~300-token prompt), same process, sequential:

```
complete#1   14413ms   (cold)
complete#2     731ms   (KV prefix reused)
complete_n#1 14300ms
complete_n#2 14046ms   (no reuse: production path)
complete#3     737ms
```

Fix: clear only before the second and later candidates, so candidate 0 takes the
normal `reusable_prefix_len` path and the prompt stays in `prev_tokens` after the
call:

```rust
for index in 0..n {
    check_shutdown(cancellation)?;
    if index > 0 {
        // Candidates must not share generated KV; candidate 0 keeps the
        // cross-request prompt-prefix reuse that `complete` gets.
        prev_tokens.clear();
    }
    ...
}
```

Check that after the loop `prev_tokens` holds the prompt (not candidate n-1's
generated tail) or that `complete_on_worker` already leaves it that way for the
last candidate; read `complete_on_worker` before deciding. The function doc
("prev_tokens is left holding the prompt so the next request can reuse its KV
prefix") must become true.

Regression: add an `#[ignore]` real-model test next to
`prefix_reuse_matches_fresh_context_output` in `crates/model_client/tests/latency.rs`
that calls `complete_n(prompt, 8, 1)` twice on a long prompt and asserts the
second call is at least 5x faster than the first (or, more robustly, asserts
equality with `complete` output and a measured ratio printed). Also add a
`complete_n` variant of `warm_completion_under_500ms` gated by
`COMPME_REQUIRE_LATENCY_BUDGET`, since the current budget test measures a path
the binary never uses for completions. Wire the new test into
`tools/release/run-model-gates.sh` only if it stays inside the existing
`--test latency -- --ignored` invocation (it will).

### A2. Redaction misses `KEY_PASSWORD=value` (MEDIUM, confirmed privacy miss)

`crates/redaction/src/lib.rs:140-149` (`credential_re`) and `:154-160`
(`whitespace_credential_re`) anchor the key name with `\b`. In the `regex` crate
`_` is a word character, so `DB_PASSWORD=`, `POSTGRES_PASSWORD=`, `SLACK_TOKEN=`
never match. Probe:

```
redact("DB_PASSWORD=hunter2 POSTGRES_PASSWORD=root mypassword=abc")
  -> "DB_PASSWORD=hunter2 POSTGRES_PASSWORD=root mypassword=abc"   (nothing redacted)
```

Fix: replace the leading `\b` before the key alternation with a boundary that
also accepts `_` and `-` as a left separator, e.g. `(?:^|[^A-Za-z0-9])` captured
and re-emitted, or `(?:\b|_)`. Keep
`token_secret_keys_respect_word_boundary` (`:1143`) passing: `mypassword=` and
`xtoken=` must still survive. Add assertions:

```rust
assert_eq!(redact("DB_PASSWORD=hunter2"), "DB_PASSWORD=[redacted-secret]");
assert_eq!(redact("POSTGRES_PASSWORD: root"), "POSTGRES_PASSWORD: [redacted-secret]");
assert_eq!(redact("SLACK_TOKEN=abc"), "SLACK_TOKEN=[redacted-secret]");
assert_eq!(redact("mypassword=hunter2value"), "mypassword=hunter2value");
```

Check whether the replacement branch re-emits the captured key verbatim
(`caps[1]`) so the prefix character is preserved.

### A3. Redaction over-redacts ordinary hyphen/snake words (MEDIUM, confirmed)

`crates/redaction/src/lib.rs:33-47` — the branch
`(?:sk|ghp|gho|ghu|ghs|ghr|pk|rk)[-_][A-Za-z0-9_-]{16,}` has no left boundary and
`is_keyed` (`:245`) uses `starts_with`. Probe:

```
redact("risk-assessment-framework task_management_service network-security-policies")
  -> "ri[redacted-secret] ta[redacted-secret] netwo[redacted-secret]"
```

Fix: require a left boundary for the vendor-prefix branch. The `regex` crate has
no lookbehind, so either prefix the whole alternation with `(?:^|[^A-Za-z0-9])`
and re-emit that character in the closure, or check in the closure that the byte
before the match start is not alphanumeric (`caps.get(0).unwrap().start()` and
the input string are both available if you switch `replace_all` to a closure
that receives the haystack). Regression: the three words above survive verbatim;
`sk-` and `ghp_` real keys at start-of-string, after a space, and after `(` are
still redacted.

### A4. Fullwidth-digit card numbers are never redacted (LOW-MEDIUM, confirmed)

`card_run_re` (`:70`) uses `\d` (Unicode `\p{Nd}` in `regex`), but
`redact_card_run` (`:85-100`) counts only `is_ascii_digit()`. Probe:

```
redact("４１１１１１１１１１１１１１１１") -> unchanged ; redact("4111111111111111") -> "[redacted-card]"
```

Fix: in `redact_card_run` treat any `char::is_numeric()` digit with
`to_digit(10)` (fullwidth digits are `Nd`, `to_digit` handles them via
`char::to_digit` only for ASCII, so map with `c.to_digit(10)` after NFKC or use
`unicode` value arithmetic: `(c as u32).wrapping_sub('０' as u32)` for the
fullwidth block U+FF10..U+FF19). Byte offsets must use `char_indices` positions
(multi-byte digits), which the function already does for separators. Regression:
the fullwidth PAN redacts; a mixed ASCII/fullwidth PAN redacts; a fullwidth
non-Luhn 16-digit id survives.

### A5. `COMPME_MODEL_PATH` missing from the env-shadow warning list (MEDIUM, confirmed)

`crates/app/src/settings_runtime.rs:21` — `SWITCH_KEYS: [&str; 37]` documents
itself as "every runtime-persisted config key that can be shadowed by the
process environment", but `COMPME_MODEL_PATH` is persisted by four flows
(`run_loop.rs:4557` auto-adopt, `:4905`, `:4959`, `:5096`). A user who launched
with the env var set downloads a model, relaunches, and silently keeps the old
model.

Fix: add `"COMPME_MODEL_PATH"` and bump the array length to 38. Regression: a
test that persists the model path with the env var set and asserts the shadow
warning names `COMPME_MODEL_PATH`. Check `run_loop_tests.rs` for an existing
`SWITCH_KEYS` completeness test and extend it.

### A6. `ready` flips true after a backend warm-up failure (MEDIUM, confirmed)

`crates/app/src/inference.rs:451-460` — a non-shutdown `warm_up` error is logged
and `ready.store(true)` still runs; `effective_model_available`
(`run_loop.rs:4146`) folds in only panics. The tray shows Ready while every
request errors. Test `warm_up_failure_is_non_fatal` pins the current behaviour.

Decide with the owner: either (a) keep non-fatal but surface a distinct
"model loaded, decode failing" status through the same channel panics use, or
(b) leave `ready` false and retry warm-up on the next request. (a) is the
smaller diff. Update `warm_up_failure_is_non_fatal` to assert the new status
rather than delete it (it is not a pinned symbol; verify with grep).

### A7. dotenv-style config does not unquote values (MEDIUM, plausible; confirm first)

`crates/app/src/config.rs:32` keeps quotes verbatim, so
`COMPME_MODEL_PATH="/Users/x/My Models/a.gguf"` fails the `.gguf` check with the
quotes inside the path. Confirm with:

```sh
printf 'COMPME_MODEL_PATH="/tmp/x.gguf"\n' > /tmp/c/config.env
COMPME_CONFIG=/tmp/c/config.env COMPME_RUN_MS=1 target/debug/compme
```

If confirmed: strip one matching pair of surrounding `"` or `'` in
`decode_env_value` only when the value is not `ESCAPED_VALUE_PREFIX`-encoded
(the encoder never emits quotes, so round-trips are unaffected). Regression in
the config tests: quoted path parses; a value containing an inner quote is
untouched; the escaped-prefix path is unchanged.

---

## Batch B — adapter correctness (each needs the mac or Linux live lane)

### B1. macOS AX worker dies permanently on an unpaired-surrogate attribute (HIGH, confirmed by trace)

`crates/platform_macos/src/ax_worker.rs:1001-1006` (`ObserverEvent` arm) and
`:1039-1045` (`PollFocusedElement` arm) resolve identity via
`lib.rs:5842-5862` → `read_optional_ax_string_attribute` (`lib.rs:6075`) →
`CFString::to_string()`. core-foundation 0.10.1 asserts
`chars_written == char_len` (`string.rs:91`) and panics on a lone surrogate.
Only the `Run`/`InstallResource`/`RemoveResource` arms are under `catch_unwind`
(`:903`, `:917`, `:931`). One bad `AXIdentifier`/`AXRole` from an
Electron/Chromium field unwinds the worker thread; every later `AxWorker::run`
returns `CannotComplete("AX worker is not running")` until relaunch. The crate
already documents and fixed this exact hazard for `AXValue`
(`lib.rs:4937-4940`, `cf_string_to_exact_string`) and for no other attribute.

Fix (two parts): route `read_optional_ax_string_attribute` through the same
lossy conversion as `cf_string_to_exact_string` (or `CFString::to_string`
replaced by a `CFStringGetBytes`-based lossy path), and wrap the two event arms
in `catch_unwind` the way `:903` does, logging and continuing. Regression: a unit
test feeding `CFStringCreateWithCharacters(&[0xD800])` to
`resolve_ax_element_identity` must return an identity (lossy) and not panic; a
worker test mirroring `ax_worker_contains_a_panicking_job_and_serves_the_next_job`
for the `ObserverEvent` arm. macOS-only: prove on the pushed mac lane.

### B2. macOS `insert_for_field` bypasses the injected secure-input provider (MEDIUM, confirmed)

`crates/platform_macos/src/lib.rs:4600` hard-codes `macos_secure_input_enabled()`
while `insert_range_for_field` (`:4772`), `capabilities_for_field` (`:4005`),
`read_context_for_field` (`:4190`), and `caret_rect_for_field` (`:4215`) use the
injected `Arc<SecureInputProvider>`. The test
`field_workers_fail_closed_when_secure_input_flips_before_ax`
(`lib_tests.rs:1573-1605`) probes seven entry points and omits `insert` and
`insert_replacing`. Fix: use the provider; extend that test to nine probes.

### B3. macOS global-insert stale-focus check is pid-only; `generation` never read (HIGH, confirmed; design-level)

`lib.rs:1448-1456` (`ensure_global_insert_target`) compares only the frontmost
pid; the SyntheticKeys/Clipboard branches at `:1178-1203` then post without
resolving the focused element. `grep '\.generation' crates/platform_macos/src`
is empty despite the contract at `crates/platform/src/lib.rs:25-29`.
`docs/ARCHITECTURE.md` ("Stale-focus rejection before global synthetic or paste
insertion") overstates this. Minimum fix: before a global insert, resolve the
focused element on the AX worker and compare its identity (the same identity
string the pollers use) to the handle's; return `StaleField` on mismatch. Then
either wire `generation` or delete the contract sentence and the ARCHITECTURE
claim. Ask the owner which; do not leave the doc overstating.

### B4. Linux range replacement never restores the caret (HIGH, confirmed missing call)

`crates/platform_linux/src/atspi_live.rs:1265-1327` — read → guard →
`SetTextContents(updated)` → readback, with no `SetCaretOffset` anywhere in
production (`grep -i set_caret_offset crates/platform_linux/src/*.rs` hits tests
only). GtkTextView lands the caret at 0, GtkEntry at the end. Fix: after a
successful readback, `SetCaretOffset(start + replacement_chars)` and treat a
failure to set the caret as a logged warning, not an `outcome_unknown`. Also
review `MutationCoordinator::outcome_unknown` (`:257`): a single normalising
field (max-length entry, trailing-trim) currently quarantines every later write
for the session; consider scoping the quarantine to the field identity.
Regression: extend `live_range_replace_swaps_exactly_the_range`
(`atspi_live_tests.rs:686`) to assert the caret offset on both the entry and the
text-view fixture. Live lane invocation is in `docs/DEVELOPMENT.md` and the
AT-SPI harness; `--test-threads=1` is mandatory.

### B5. Linux accept tap: `set_action` writes the action outside the `grabbed` lock (MEDIUM, confirmed)

`crates/platform_linux/src/x11_tap.rs:584-587` — the `action` write is scoped
before `grabbed` is taken. Interleaving with the watchdog's `Disarm`
(`clear_armed_state`, `:566-572`): engine writes `Some(Full)`; watchdog writes
`None`, takes `grabbed`, ungrabs, clears; engine takes `grabbed`, sees
`(Some, false)`, grabs → grab installed with `action == None`, every Tab replays
to the app while a ghost is visible. Fix: take `grabbed` first, then write
`action` inside that critical section (the resolve path only reads `action`, so
holding `grabbed` across the write is fine; the X round trip still happens after
the `action` write, not while holding it). Add an invariant check test using the
existing pure `arm_transition` helper if the connection cannot be faked.

Related in the same file: `MAX_ARMED_MS` (30 s, `x11_keys.rs:106`, applied at
`x11_tap.rs:1145`) disarms silently while the ghost stays visible. Either notify
the engine through the `TapControl` callback or pin in a test that the engine's
hide deadline is always shorter.

### B6. Linux `subscribe_accept` install failure is fatal at startup (LOW, confirmed)

`crates/platform_linux/src/lib.rs:380` propagates `X11AcceptTap::install` errors
raw, which `crates/app/src/run_loop.rs:4157-4165` classifies `Fatal` when
trusted. Degrade to `None` (no accept tap) with a logged reason, matching the
rationale at `lib.rs:253-260` and commit `b8d3626`.

---

## Batch C — tests and hygiene (no behaviour change)

- C1. `crates/platform_linux/src/lib.rs:1270` and `~1303`
  (`url_launcher_reaps_child_without_blocking_the_caller`,
  `url_launcher_reports_a_failure_detected_during_the_poll_window`) spawn
  `Command::new("sh")` with no `#[cfg(unix)]`; they pass on the Windows lane only
  because Git-for-Windows ships `sh`. Gate them `#[cfg(unix)]`.
- C2. `crates/platform_macos/src/lib_tests.rs:5206-5223` and `:5374-5393` write
  `ACCEPT_KEYMAP` and `TAB_HOTKEY_SUPPRESSED` unguarded while `:5147-5152` claims
  one test owns the keymap. Give both the same test lock `SHORTCUT_BINDINGS`
  got (`:320-352`). Then correct `.github/workflows/ci.yml:104-106`, which
  attributes `--test-threads=1` to the pasteboard (no test touches
  `generalPasteboard`).
- C3. `crates/context/src/lib.rs:99` `word_at_caret` is `#[cfg(test)]`-only yet
  has seven tests (`:440`, `:451`, `:530`, `:541`, `:561`, `:575`, `:613`,
  `:630`). Port the cases without a `word_at_split_caret` twin, delete the rest
  and the helper. `check-model-gates.sh` pins three test symbols in this file;
  check they are not among the deleted ones.
- C4. `crates/engine_core/src/lib.rs:1083` `offer_replacement` is a
  `#[cfg(test)]` wrapper; seven `offer_replacement_*` tests duplicate their
  `_multi_` twins (3918/4182, 3937/4202, 3963/4218, 3982/4358, 3999/4377,
  4022/4400, 4160/4334). Delete the duplicates and the wrapper. One pinned test
  symbol lives in this file; check it first.
- C5. `crates/app/src/run_loop.rs:363-380` `HostEventRoute`/`host_event_route`
  are `#[cfg(test)]` and used by nothing in production; tests at
  `run_loop_tests.rs:10542-10566` assert a mapping no code path uses. Delete
  both. Nine pinned symbols live in `run_loop_tests.rs`; check them first.
- C6. `crates/app/src/run_loop.rs:6486` `if let Ok(mut text) =
  settings_flags.shortcuts_text.lock()` silently skips on poison; every
  neighbouring lock uses `unwrap_or_else(PoisonError::into_inner)`. Align it.
- C7. `crates/model_fetch/src/lib.rs:72-74` `FetchError` doc names a `Cancelled`
  variant that does not exist; `download_url` (unbounded, public) has no
  production caller next to `download_url_bounded`. Fix the doc; remove or
  `pub(crate)` the dead entry point.
- C8. Doc comment drift: `crates/platform/src/lib.rs:190` says no adapter reports
  `NativeRangeSet` (Linux does, `atspi_caps.rs:107`); `crates/emoji/src/lib.rs:176`
  says gendered variants keep the default tone (they do not; see
  `gendered_match_combines_skin_tone`); `crates/ranker/src/lib.rs:161` says
  "repeated three or more times" but only exact k-fold repetition is detected.
  Reword each to match the code.

---

## Batch D — docs and CI (owner decisions, listed for completeness)

- D1. `.github/workflows/ci.yml:28-30` — `group: ci-${{ github.ref }}` keeps one
  pending run per group, so ten consecutive main commits on 2026-09-20 show
  `cancelled` with no verdict (`gh run list --branch main --workflow ci.yml`).
  Change the group to `ci-${{ github.ref }}-${{ github.sha }}` on main (or
  `${{ github.run_id }}`) and re-stamp `check-model-gates.sh:200-205`, which
  pins the current shape. The `protect-main` ruleset has no required checks.
- D2. Three live macOS test counts: ROADMAP:3 "≈2227", ROADMAP:252 "2,222",
  IMPLEMENTATION-EVIDENCE:255 "2,226". Reconcile to the mac lane's number.
- D3. `docs/DEVELOPMENT.md` has no Linux-host portable fence; add the block at
  the top of this file under "Full Local Gate" as a named subsection, and state
  that the canonical fence needs a Mac. `tools/dev/check.sh` exits 0 after
  skipping every cargo line when cargo is off PATH; make skipped `cargo` lines
  a non-zero exit.
- D4. `tools/release/check-model-gates.sh` (5,010 lines) has no EXIT trap for
  its self-test temp dir (`:1374-1376`); add `trap cleanup EXIT`.
- D5. The 2026-09-19/20 batches are recorded three times each (HANDOFF,
  IMPLEMENTATION-EVIDENCE, AUDIT-REPAIRS, ROADMAP, Qfd). Fold into ROADMAP + Qfd
  and delete the dated files; root `2FIX.md`/`FIXED.md` belong under
  `docs/superpowers/plans/` with a superseded banner.

## Reporting

For each item, report: the diff summary, the regression test name and that it
failed before the fix, the exact gate commands run and their result, and the CI
run URL for anything touching `platform_macos` or `platform_linux` live paths.
Do not mark B-items done from this host; the mac and Linux live lanes are the
authority.
