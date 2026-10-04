# Test consolidation — 2026-09-26

Current status (verified 2026-10-03): the consolidation landed with G7 in
`7894243`. Native CI on test-corrected `261a1e6` passed all five lanes,
including the consolidated macOS tests and the later 2284-test inventory
([run 36532353871](https://github.com/mudrii/compme/actions/runs/36532353871)).
The counts and uncommitted/native-pending statements below record the original
checkpoint. Mac Full Local Gate and physical release acceptance remain pending.

Base commit: `12d581555dcbcaa811654f9e4a98e998bf7383b9`.
Owner-authorized cleanup after source and provenance review. Production behavior
is unchanged; the only removed helpers were `#[cfg(test)]` forwarders.

## Removed tests and retained coverage

| Removed test | Surviving coverage |
|---|---|
| `caret_diagnostics_falls_back_when_marker_unusable` | `caret_diagnostics_falls_back_from_unusable_marker_rect`, now using the named width limit and retaining the raw marker assertion |
| `caret_rect_error_propagates_when_showing` | `anchor_error_reconciles_machine_and_retracts_shown_stat`, including the same typed error assertion |
| `no_request_when_context_below_min` | `trailing_whitespace_does_not_count_toward_min_context`, now exercising both original inputs |
| `repetition_penalty_is_low_for_exact_recent_text` | `repetition_penalty_matches_contiguous_word_run`, equivalent lowercase interior two-word match |
| `truncate_at_sentence_end_keeps_decimal` | `truncate_at_sentence_end_keeps_decimals`, including the removed test's exact input |
| `strip_suffix_overlap_removes_multi_word_overlap` | `strip_suffix_overlap_strips_multi_word_tail_when_candidate_is_longer`, equivalent two-word suffix/prefix overlap |
| `complete_n_zero_is_empty` | `complete_n_zero_returns_empty_without_dispatch`, now through `Box<dyn LocalModel>` with a shared call counter |
| `recent_pages_return_correct_newest_first_order_across_page_boundaries` | `recent_truncates_to_limit_newest_first`; genuine multi-page traversal remains in `recent_pages_past_full_corrupt_pages_to_fill_the_limit` |
| `is_browser_recognizes_web_browsers` | `browser_families_match_variants_but_not_lookalikes`, which contains all three original cases |
| `emoji_offer_gated_by_enable_and_shortcode` | `feature_policy::tests::emoji_offer_needs_prefs_and_a_trailing_shortcode`, which also pins the exact glyph |
| `trailing_word_extracts_the_word_at_the_caret` | `feature_policy::tests::trailing_word_takes_the_alphabetic_run_at_the_end`, augmented with standalone Unicode, colon, and digit boundaries |

Also removed three redundant `correct(...).is_none()` assertions and the two
unused run-loop test forwarders `emoji_offer` and `trailing_word`.

The 11-test reduction updates the four current inventory surfaces from 2252 to
2241 (two occurrences in DEVELOPMENT). This is a delta from the documented
macOS inventory, not a newly executed native count. The inventory checker
derives its expectation dynamically and requires no implementation change.

Distinct subscription/no-subscription engine tests, preference initialization
and transition tests, production policy wrappers, no-hash downloads, source-alone
context tests, empty-window stats, literal emoji expectations, redaction
assertions, deliberate hardening, platform parity, and live gates are retained.

## Validation

`tools/dev/check.sh --fence "Linux Host Gate"` passed all 44 commands, with
zero skipped commands, using Rust 1.97.0 on Linux:

- Portable workspace tests: 1233 passed, 0 failed, 51 ignored.
- Serialized app tests: 648 passed, 0 failed, 2 ignored subprocess helpers.
- Doctests: 1 passed, 0 failed.
- Formatting, Linux clippy, documentation, and macOS all-targets cross-check
  passed, along with the documented script/policy checks and self-tests.
- `git diff --check` passed; source and documentation diffs were reviewed.

The Nix shell needed `LIBCLANG_PATH`, glibc headers via
`BINDGEN_EXTRA_CLANG_ARGS`, and the compiler runtime via `LD_LIBRARY_PATH`.
Initial environment-only failures finding libclang, `stdio.h`, and
`libstdc++.so.6` were resolved before the successful gate run.

Logs: `.gate/test-consolidation-20260926-linux.log` and
`.gate/test-consolidation-20260926-full.log` (local evidence, not committed).

The Full Local Gate stopped at command 2 of 57, workspace clippy, on Linux:

```text
error: `objc2` only works on Apple platforms. Pass `--target aarch64-apple-darwin` or similar to compile for macOS.
check.sh: FAILED [2/57]: cargo clippy --locked --workspace --all-targets -- -D warnings
```

No native CI pass, live acceptance result, or mutation-equivalence result is
claimed by this cleanup.

Remaining Full Local Gate checks not covered by the successful Linux gate:
native macOS clippy/tests/build/examples/rustdoc and the native inventory/policy
check require macOS; Swift icon generation, bundle smoke and its self-test,
the A1b runner self-test, and the real missing-model startup check require the
macOS environment. The root all-targets build cannot complete on Linux because
it includes `platform_macos`. Cargo audit was not run after the full gate
stopped. Model-backed release/quality gates require model artifacts, and the
separate `tools/spike` fmt/clippy/test/build gate was not run. The 53 ignored
Linux/model/app tests retain their existing harness requirements; the app's
two subprocess helpers are invoked by their ordinary parent tests.

Changes remain uncommitted: Full Local Gate and native macOS CI verification
are still outstanding. No gates were weakened or bypassed.
