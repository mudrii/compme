# Standalone grammar/spell-fix mode — implementation spec

**Status:** ◕ Implemented · deterministic validation green 2026-07-02 · pending live LOOK validation
**Roadmap entry:** `docs/ROADMAP.md` → "Tier 5 — Standalone grammar/spell-fix mode".
**Prereqs:** clean `main` (builds, clippy clean, ≈1585 tests green).

This spec turns the roadmap Tier 5 bullet into an executable, phase-by-phase plan.
Every phase is sized to land independently, pure/testable layers first, novel FFI
last. File:line anchors are from a 2026-07-01 4-agent code analysis; verify them
before editing (they drift).

## Implementation progress

- G1 correction prompt/vetting and caret-word extraction are implemented with focused tests.
- G2 headless inference request kind, correction presentation state, run-loop gating,
  prefs/config, and correction accept side effects are implemented with focused tests.
- G3 macOS grammar trigger/accept key routing is implemented behind configurable
  `COMPME_GRAMMAR_CHECK_KEY` and `COMPME_GRAMMAR_ACCEPT_KEY`.
- G4 macOS scalar-range conversion, range bounds, fail-closed AxSet range
  replacement, and a two-panel underline/banner correction presenter are
  implemented with focused geometry tests. Live LOOK validation remains pending
  because it requires granted Accessibility permissions and an interactive macOS app.

---

## Intent

A **separate** feature from inline completion: press a **grammar-trigger** key →
the nearest misspelled/ungrammatical word at the caret is **underlined in place**
and its correction is shown in a **banner above it** → press a **separate
grammar-accept** key → the word is replaced. Detect → underline → confirm, not
type-ahead.

## Settled decisions
- **D0 — Cross-platform by construction (Linux/Windows/macOS).** All detection,
  correction, orchestration, prompt, policy, and state logic is portable (`model_client`,
  `engine_core`, `engine`, `run_loop`, `context`, `prefs`, `grammar`). Only the
  platform trait boundary is per-OS: global hotkey, correction overlay, text-range
  bounds, and text-range replacement. Some of those are new trait methods; they
  must land with fail-closed Windows/Linux stubs and fake-adapter coverage in the
  same phase that changes the trait. macOS is the reference impl.
- **D1 — Engine = the installed local LLM.** New inference request kind through the
  existing llama.cpp path. No platform spell API, no bundled dictionary. One code
  path on every OS.
- **D2 — Scope = nearest word at the caret** (v1). Multi-error cycling deferred.
- **D3 — Two dedicated keystrokes** (trigger + accept), not reused accept keys.
- **D4 — Own toggle + Apps-pane column**, gated off in code fields like autocorrect.

## Deliberate divergences (project scope)
- Not a spell-checker clone: correction is LLM-driven, so it catches grammar and
  arbitrary typos, but is **post-filtered** to a safe single-word swap (§Phase G1)
  so the model can never paraphrase the user's word.
- v1 corrects **one** word (the one at the caret). Whole-field lint is out of scope.

---

## End-to-end data flow (target)

```
grammar-trigger key (per-OS hotkey)
  → HostEvent::Shortcut(ShortcutAction::GrammarCheck)     [run_loop match ~3715]
  → run_loop: read_context(field) → word-under-caret + left_ctx + CorrectionRange
  → gate: grammar_fix_enabled(app) && suggestion_gates_pass && caps.AxSet
  → dispatch CompletionRequest{ kind: GrammarFix, word, left_ctx, correction_range }
      worker: prompt = model_client::grammar_fix_prompt(word, left_ctx)
              raw = model.complete(prompt, small_max_tokens)
              // bypass shape_prompt/context/personalization/complete_n
              vetted = grammar::vet_correction(word, raw)  → Option<String>
      → CompletionOutcome{ correction: Some(word'), correction_range }
  → run_loop: engine.on_correction(field, word', correction_range)
      → Command::ShowCorrection{ field, correction_range, suggestion }
      → engine dispatch resolves PlatformAdapter::text_range_rect(field, correction_range)
      → OverlayPresenter::show_correction(word_rect, suggestion)   [underline+banner]
      → arm correction-only accept mode
grammar-accept key
  → AcceptBinding::GrammarAccept → TapControl::Accept(AcceptAction::Correction)
  → engine AcceptCorrection path
  → Command::ReplaceRange{ field, text: word', correction_range }
  → insert_replacing_range(...)  → tracker.apply_self_replace_range(...)
Any TextChanged/CaretMoved before accept → advance_snapshot() invalidates it.
```

---

## Phase G1 — Correction engine core (pure, no FFI) · effort S

**New/changed:**
- `crates/model_client/src/lib.rs`: add
  `pub fn grammar_fix_prompt(word: &str, left_ctx: &str) -> String` next to
  `terse_continuation_prompt` (:578). Instruction-style prompt that asks for the
  single corrected word only (or the word unchanged), with the left context for
  disambiguation. Keep it terse; the caller uses a small `max_tokens`.
- `crates/grammar/src/lib.rs`: add
  `pub fn vet_correction(original: &str, model_output: &str) -> Option<String>`.
  Pure post-filter, the safety gate for D1. Returns `Some(correction)` only if the
  model output, after trimming surrounding whitespace, has exactly one
  whitespace-delimited token and that token is: (a) non-empty, (b) a single word,
  (c) different from `original` (no no-op), and (d) within a small bounded edit
  distance of `original` (reject paraphrases / hallucinations). Do not validate
  only the first token: `"the cat"` must be rejected, not truncated to `"the"`.
  Preserve the original word's case via `textcase::CasePattern` (as
  `autocorrect`/`grammar` already do). Add a tiny bounded Levenshtein helper
  (`// ponytail: capped at MAX_EDIT, good enough for word-level typo distance`).
- `crates/grammar/Cargo.toml`: add `textcase = { path = "../textcase" }` when
  `vet_correction` starts using `textcase::CasePattern`; the current grammar
  crate has no dependencies.
- Keep `capitalize_pronoun` as-is; `vet_correction` is independent.

**Tests (grammar + model_client `#[cfg(test)]`):**
- RED-first before implementation: add
  `grammar_fix_prompt_is_single_line_and_includes_word_and_left_context`. It must
  prove the prompt contains the word and left context, is deterministic, and does
  not leak newlines that would break single-line parsing.
- RED-first before implementation: add
  `vet_correction_accepts_one_edit_and_preserves_case` and
  `vet_correction_rejects_empty_identical_multi_word_large_edit_and_non_ascii`.
  These must prove a plausible one-edit fix (`"teh"→"the"`) is accepted, case is
  preserved (`"Teh"→"The"`), and identical output, empty output, multi-word output
  (`"the cat"`), large-edit paraphrases (`"teh"→"kitten"`), and non-ASCII/CJK
  input are clean misses, never panics.
- If G1 adds the optional autocorrect pre-pass, add
  `grammar_autocorrect_prepass_rejects_multi_word_table_entries` and
  `vet_correction_rejects_alot_to_a_lot_for_single_word_mode` before wiring it.
  The current autocorrect table intentionally maps `alot` to `a lot`; grammar-fix
  is a single-word replacement mode, so that table entry must not bypass
  `vet_correction`.

**Acceptance:** `cargo test -p grammar -p model_client` green; clippy clean.

---

## Phase G2 — Inference request kind + engine wiring + policy (headless) · effort M

**New/changed:**
- `crates/platform/src/lib.rs`: add a leaf-owned scalar range type, for example
  `pub struct CorrectionRange { pub start: usize, pub end: usize }`, documented as
  Unicode-scalar offsets in `left + right` context text. Do not put this type in
  `engine_core`: `engine_core` already depends on `platform`, and `context` /
  `platform` cannot depend back on `engine_core` without a cycle.
- `crates/engine/src/lib.rs`: extend the public `CompletionRequest` type (:44)
  with a request-kind discriminator:
  `enum RequestKind { Completion, GrammarFix { word: String, left_ctx: String,
  correction_range: CorrectionRange } }`. `engine::dispatch` constructs
  `RequestKind::Completion` for normal requests; grammar detection constructs
  `RequestKind::GrammarFix`.
- `crates/app/src/inference.rs`: keep owning `CompletionOutcome`, and make the
  worker receive path request-kind aware before any screen-OCR wait. Today the
  completion worker runs `request_with_screen_context` before prompt shaping;
  `GrammarFix` must bypass that wait because it uses only `word + left_ctx`.
  In the worker serve loop branch before the existing completion prompt path,
  build
  `grammar_fix_prompt`, call `model.complete(prompt, GRAMMAR_MAX_TOKENS)`, run
  `grammar::vet_correction`, and emit a `CompletionOutcome` carrying
  `correction: Option<String>` + the original `correction_range`. This branch
  must not wait for screen OCR, call `shape_prompt`, prepend
  personalization/context blocks, or use `complete_n`; those are
  completion-specific and would turn a grammar prompt into an inline-continuation
  request. `recv_latest` (:266) coalescing still applies (a newer trigger
  supersedes an older one).
- `crates/engine_core/src/lib.rs`: add a `presentation: Presentation` field
  (`enum Presentation { Ghost, Correction }`, default `Ghost`) to `Showing` (:172
  area) plus `correction_range: Option<platform::CorrectionRange>`. Thread it
  from a new `offer_correction(field, suggestion, correction_range)` that
  mirrors `offer_replacement_multi` (:780) but sets `presentation = Correction`
  and emits `Command::ShowCorrection { field, correction_range, suggestion }`
  instead of `ShowGhost`. Add an explicit `Event::AcceptCorrection` arm that only
  commits a `Showing { presentation: Correction, .. }` and emits
  `Command::ReplaceRange { field, text, correction_range }`. Do not reuse
  `AcceptFull`/`AcceptWord`: those commit the existing `replace_left` model and
  can only delete characters immediately left of the caret. Same
  `InsertStrategy::AxSet` gate (:791).
- `crates/engine/src/lib.rs`: add `pub fn on_correction(...)` wrapping
  `offer_correction` (mirror `on_replacement` :283). Extend dispatch for
  `ShowCorrection`: resolve `adapter.text_range_rect(field, correction_range)`,
  fall back to `caret_rect`/`popup_anchor` only when range bounds return `Ok(None)`,
  then call `overlay.show_correction`. Extend `FakeAdapter`/`FakeOverlay` (:554)
  so tests can observe the correction presentation and range.
- `crates/app/src/run_loop.rs`: add `grammar_fix: bool` to `Config` (:169) parsed as
  `COMPME_GRAMMAR_FIX` in `from_lookup` (:277). Add the detection helper: on a
  grammar trigger, use `read_context` -> `left/right`, extract the word under the
  caret (see G1a below), gate via `replacement_decision`-style checks (:533) plus
  the new per-app policy, and dispatch the `GrammarFix` request with its
  `CorrectionRange`; on the outcome, call `engine.on_correction`. If browser
  domain exclusion rules are configured, wrap the gate with
  `browser_domain_fresh_enough_for_rules` exactly like the submit and local
  replacement paths do, so an unknown or stale URL fails closed instead of
  offering grammar fixes on an excluded domain.
- `crates/context/src/lib.rs` (G1a): add
  `pub fn word_at_caret(value, caret) -> Option<(&str, CorrectionRange)>` — the
  word the caret is in/just after (combine trailing word of `left_context` with
  any leading fragment of `right_context`). Returns the word text + scalar range.
  The range, not a `replace_left` count, is authoritative for both underline
  geometry and acceptance so mid-word corrections replace the whole word rather
  than only the left fragment.
- `crates/app/src/run_loop.rs`: reconstruct `value = left_context + right_context`
  and derive the scalar caret as `left_context.chars().count()` before calling
  `word_at_caret`. Do not feed `TextContext.caret` directly into the scalar helper:
  macOS can report `OffsetEncoding::Utf16CodeUnits`, so astral characters before
  the caret would otherwise shift the range. Reject or explicitly handle non-empty
  selections before dispatching a correction request.
- `crates/platform/src/lib.rs`: add
  `fn text_range_rect(&self, field: &FieldHandle, range: CorrectionRange) ->
  Result<Option<ScreenRect>, PlatformError>` and
  `fn insert_replacing_range(&self, field: &FieldHandle, text: &str,
  range: CorrectionRange, strategy: InsertStrategy) -> Result<Inserted,
  PlatformError>`. `insert_replacing` remains the left-of-caret replacement path
  for emoji/autocorrect; grammar uses range replacement. Add compile-safe,
  fail-closed impls in `platform_macos`, `platform_linux`, `platform_windows`, and
  every fake adapter in the same phase that extends the trait.
- `crates/prefs/src/lib.rs`: add `grammar_fix: Option<bool>` to `AppPolicy` (:13),
  a `grammar_fix_enabled(app, default)` getter (mirror `autocorrect_enabled` :133),
  and a `AppPolicyField::GrammarFix` variant (:46) for the Apps-pane checkbox.

**Tests (all headless, fake model + fake adapter):**
- Worker RED-first tests:
  `grammar_fix_request_bypasses_screen_wait_context_personalization_and_complete_n`,
  `grammar_fix_request_preserves_range_and_vets_model_output`, and
  `grammar_fix_rejected_output_returns_no_correction`. Together they must prove a
  `GrammarFix` request with a misspelled word yields a vetted `correction` and
  preserved `correction_range`, rejected model output yields `correction: None`,
  the request does not wait for screen OCR/context, does not prepend
  screen/clipboard/personalization context, does not call `shape_prompt` or
  `complete_n`, and still coalesces/supersedes older grammar requests.
- Engine RED-first tests:
  `offer_correction_shows_correction_with_exact_range`,
  `on_correction_shows_correction_with_range_and_invalidates_on_text_changed`,
  `accept_correction_emits_replace_range_with_exact_range`,
  `accept_correction_emits_replace_range`, and
  `accept_full_and_word_do_not_commit_correction_presentation`. Together they
  must prove `Showing{ presentation: Correction, correction_range: Some(..) }`,
  `ShowCorrection`, invalidation on `TextChanged`, `Command::ReplaceRange` with
  the exact range, and that `AcceptFull` / `AcceptWord` never commit correction
  presentations.
- `context::word_at_caret` RED-first tests:
  `word_at_caret_returns_whole_word_and_scalar_range_at_end`,
  `word_at_caret_returns_whole_word_and_scalar_range_mid_word`,
  `word_at_caret_handles_astral_prefix_without_utf16_offset_drift`, and
  `word_at_caret_returns_none_at_boundary_or_empty_field`. Include multibyte and
  astral-prefix text; ranges are Unicode-scalar ranges and the helper must not
  panic.
- Platform seam RED-first tests:
  `platform_seam_replaces_midword_range_without_left_fragment_leak` and
  `platform_seam_text_range_rect_converts_scalar_range_and_fails_closed`.
  `insert_replacing_range` replaces `te|h` with `the` as `the`, never `theh`;
  `text_range_rect` converts scalar ranges to platform-native range units and
  returns `Ok(None)` for unsupported bounds.
- Prefs RED-first tests:
  `grammar_fix_enabled_inherits_global_default_without_app`,
  `grammar_fix_enabled_respects_per_app_override`, and
  `set_app_policy_field_writes_grammar_fix`. They must prove per-app override
  resolution and `AppPolicyField::GrammarFix` policy-bits round-trip.
- Run-loop RED-first tests:
  `grammar_trigger_dispatches_word_at_caret_scalar_range`,
  `grammar_detection_blocks_without_fresh_browser_domain_when_domain_rules_exist`,
  `grammar_detection_respects_enable_per_app_snooze_and_axset`,
  `grammar_detection_rejects_non_empty_selection`, and
  `grammar_detection_rejects_non_axset_before_model_request`. Together they must
  prove the enable gate, per-app exclude, snooze, browser-domain freshness when
  domain rules exist, non-empty selection rejection, and AxSet fail-closed gate.

**Acceptance:** `cargo test --workspace` green; clippy clean. No FFI touched yet —
the whole flow is exercised with the fake model + fake overlay.

---

## Phase G3 — Two keystrokes (registration + routing) · effort M

**New/changed (`platform` + `platform_macos` reference impl):**
- `crates/platform/src/lib.rs`: add `ShortcutAction::GrammarCheck` (:202) and a
  portable `AcceptAction::Correction`. The shared tap signal remains
  `TapControl::Accept(AcceptAction)`, so Windows/Linux can map their own
  grammar-accept key to the same action without depending on macOS-local binding
  names.
- `crates/platform_macos/src/lib.rs`:
  - **trigger:** `CARBON_HOTKEY_GRAMMAR_CHECK: u32 = 8` (:118 area); add
    `grammar_check: Option<(i64,u32)>` to `ShortcutBindings` (:2521), wire into
    `from_config`, `has_internal_collision` (:2548), `registration_plan` (:2568),
    and `shortcut_action_for_hotkey_id` (:2586). Always-on, like ForceActivate.
  - **accept:** add a macOS-local `AcceptBinding::GrammarAccept`, new Carbon id,
    and a `grammar` slot in `AcceptKeymap` (:3142). Extend `binding_for_hotkey_id`
    (:3425) so the fired id maps to `GrammarAccept`, then map that binding to
    `AcceptAction::Correction`.
  - Extend the accept-arm state instead of relying on today's
    `action.is_some()` visibility gate. Use an explicit arm mode such as
    `AcceptArm::Ghost { full: true, word: true }` vs
    `AcceptArm::Correction { grammar: true }`. In correction mode, Word/Full
    accept keys pass through and only the grammar-accept key is swallowed. In
    ghost mode, grammar-accept passes through.
- `crates/app/src/run_loop.rs`: `Config` gets `grammar_check_key: Option<String>`
  and `grammar_accept_key: Option<(i64,u32)>`, parsed in `from_lookup` (:289-295)
  as `COMPME_GRAMMAR_CHECK_KEY` / `COMPME_GRAMMAR_ACCEPT_KEY`. Add the
  `HostEvent::Shortcut(GrammarCheck)` arm (:3715) → run G2 detection. Route
  `HostEvent::Accept(AcceptAction::Correction)` to `engine.on_accept_correction`;
  do not fold it through `Full`.

**Tests:** RED-first tests include
`config_parses_grammar_check_and_grammar_accept_keys`,
`grammar_check_shortcut_routes_to_detection`, and
`grammar_accept_action_routes_to_accept_correction_not_full`.
`ShortcutBindings::from_config` parses the grammar chord;
`has_internal_collision` catches shortcut-shortcut collisions;
`shortcut_plan_minus_accept_collisions` drops grammar-trigger chords that collide
with accept bindings; `registration_plan` lists the grammar hotkey only when
bound; `shortcut_action_for_hotkey_id` /
`binding_for_hotkey_id` inverse-map the new ids; correction arm mode swallows
only `GrammarAccept` and passes Word/Full through; ghost arm mode preserves the
existing Word/Full/Esc/Down behavior and passes GrammarAccept through. Config
parses both new env keys. (Dispatch on a physical keypress is a live-LOOK item,
like 3.4.)

**Acceptance:** pure parse/plan layers unit-tested + green; live keypress deferred
to G4 validation.

---

## Phase G4 — Underline + correction-banner overlay (novel FFI) · effort L

The genuinely new UI. No underline/banner/attributed-string primitive exists today.

**New/changed:**
- `crates/platform/src/lib.rs`: add to `OverlayPresenter` (:526)
  `fn show_correction(&mut self, word_rect: ScreenRect, suggestion: &str) ->
  Result<(), PlatformError>` (or a sibling `CorrectionPresenter` trait). `hide()`
  reused. The word rect comes from the new `PlatformAdapter::text_range_rect`
  seam, not from the overlay presenter.
- `crates/platform_macos/src/lib.rs`: implement `show_correction` by cloning the
  `ensure_panel` recipe (:776) into **two** borderless, mouse-transparent panels:
  (1) a 1–2px filled underline panel under the word rect (from
  `text_range_rect`, backed by `read_ax_bounds_for_range` :4559 with scalar to
  UTF-16 conversion; do **not** apply the thin-caret `usable_caret_rect` guard),
  (2) a small background-filled banner panel anchored just above the word rect
  showing `suggestion`. Use a correction-specific frame helper that shares the
  AX-to-Cocoa Y-flip math from `overlay_frame_for_text` (:969) but does not apply
  caret-width heuristics or ghost text width clamps. Degrade to a caret-anchored
  popup when the word rect is `Ok(None)`.
- Wire `Command::ShowCorrection` (G2) → `show_correction`.

**Tests / validation:** geometry unit test (underline sits under the word rect,
banner above, both within the containing display's global Cocoa coordinate space,
including a secondary-display / negative-y case matching existing overlay tests);
the rest is **live LOOK** on a granted Mac (add a row to `docs/ACCEPTANCE.md` and
the checked-in live gate list): type a typo, press trigger, confirm underline +
banner render at the word, press accept, confirm in-place replacement.

**Acceptance:** deterministic geometry test green; `docs/ACCEPTANCE.md` row and
checked-in live gate entry added and checked on-device.

---

## Phase G5 — Settings surface · effort M

- `crates/platform_macos/src/settings_window.rs`: add `RecorderRole::GrammarAccept`
  (:37) + a recorder row (reuse `KeyRecorderField` :692); widen `record_decision`
  (:66), `rebind_request_for`, and the persisted accept-key config writer from
  2-role to N-role collision/keymap handling. Add the `GrammarFix` checkbox
  column to the Apps-pane grid (`apps_layout`, geometry test updated for the
  extra column).
- `crates/app/src/run_loop.rs`: handle the new `RebindRequest` role (:4002) →
  live-rebind the grammar-accept key. Surface `grammar_fix` in the General/Apps
  panes. Keep the field-index plumbing synchronized across `APP_POLICY_FIELDS`,
  Apps-pane titles/headers, `apps_policy_field_from_index`,
  `compose_apps_policy_bits`, and the settings boundary tests. Add
  `COMPME_GRAMMAR_FIX` and any per-app persistence keys to `SWITCH_KEYS` so env
  shadows are warned about at relaunch.
- grammar-trigger stays a startup config string for v1 (like the other global
  shortcuts); a recorder row for it rides the future `ShortcutBindings` UI tick.

**Tests:** recorder N-role collision; Apps-pane geometry with the new column has no
overlaps; live rebind persists.

**Acceptance:** `cargo test --workspace` green; Apps-pane + recorder validated.

---

## Cross-platform follow-on (after macOS reference lands)

Each OS implements the same four trait rows at the trait-extension phase;
grammar-fix stays inert (fail-closed `UnsupportedField`) until a full platform
row is built, never misbehaves.

| Surface | Windows | Linux |
|---|---|---|
| Global trigger hotkey | `RegisterHotKey` | X11 `XGrabKey` / Wayland global-shortcuts portal |
| Grammar-accept key | keyboard hook / `RegisterHotKey` | X11/Wayland key grab |
| Word rect + in-place replace | UI Automation `TextPattern` BoundingRectangles + range `SetValue`/`SetText` strategy | AT-SPI2 `Text`/`EditableText`, or IME/synthetic fallback |
| Underline + banner overlay | layered top-most window (`LayeredWindow`) | `wlr-layer-shell` (`LayerShell`) / override-redirect X11 |

Detection (LLM) has **no** per-OS surface — same `model_client`/`inference` path
everywhere. Geometry and mutation do have per-OS seams: every platform must add
range bounds and range replacement, not just the existing left-of-caret
`insert_replacing` path.

---

## Open decisions (recommended defaults)
- **Underline rendering:** thin filled sub-panel under the word rect (matches the
  existing panel pattern) over attributed-string `NSUnderlineStyle` (repo uses no
  attributed strings yet). *Recommended: sub-panel.*
- **LLM reliability fallback:** if the local model is weak at word-level, keep the
  pure `autocorrect` table as a zero-cost pre-pass for the known-typo subset and
  only fall through to the LLM for misses.
- **Trigger with no correction:** silent no-op (or a subtle flash), no banner.
- **`GRAMMAR_MAX_TOKENS` / `MAX_EDIT`:** pick small constants in G1/G2; tune on-device.

## Risk register
- **R1 (high):** underline+banner overlay is net-new FFI on each OS — the primary
  risk and the live-LOOK gate. Mitigation: geometry unit tests + reuse the proven
  `ensure_panel` recipe.
- **R2 (med):** LLM produces a wrong/over-eager correction. Mitigation: strict
  `vet_correction` (single word + bounded edit distance) — pure and fully tested in G1.
- **R3 (med):** non-AxSet fields can't atomically replace. Mitigation: inherit the
  existing AxSet gate — offer nothing there (same as replacements today).
- **R4 (low):** keystroke collisions. Mitigation: existing `has_internal_collision`
  / `record_decision` auto-cover once the new ids join the field arrays.
- **R5 (high):** range drift between detection, underline, and accept could replace
  the wrong text. Mitigation: store one `CorrectionRange` on the request/outcome/
  showing state, invalidate it on every `TextChanged`/`CaretMoved`, and use that
  same range for both `text_range_rect` and `ReplaceRange`.

## Validation commands
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --all-targets -- --test-threads=1`
- `cargo build --workspace --all-targets`
- `cargo build -p platform_macos --examples`
- `bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh`
- `tools/bundle/check-bundle-metadata.sh`
- `tools/bundle/make-app.sh --self-test`
- `tools/acceptance/e2e-complete-me.sh --self-test`
- `tools/acceptance/missing-model-startup.sh --self-test`
- `tools/acceptance/missing-model-startup.sh`
- `tools/acceptance/run-a1b-live-gates.sh --self-test`
- `tools/acceptance/run-a2-compat-gates.sh --self-test`
- `tools/release/check-model-client-features.sh`
- `bash tools/release/check-model-gates.sh`
- `tools/release/update-cask.sh --self-test`
- `COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1`
- `cd tools/spike && cargo fmt -- --check`
- `cd tools/spike && cargo clippy --all-targets -- -D warnings`
- `cd tools/spike && cargo test`
- `cd tools/spike && cargo build --bins`
- `cd tools/spike && COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1`

## Definition of done (macOS reference)
G1–G5 landed, the validation command set above is green, the grammar LOOK gate is
listed in `docs/ACCEPTANCE.md` / `tools/acceptance/run-a1b-live-gates.sh --self-test`
and checked on-device, and the ROADMAP Tier 5 status flipped from ☐ to 🟢 with
verified anchors.
