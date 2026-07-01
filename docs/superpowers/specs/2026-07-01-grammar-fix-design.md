# Standalone grammar/spell-fix mode — implementation spec

**Status:** ☐ Not started · planned 2026-07-01 · owner: next implementation session
**Roadmap entry:** `docs/ROADMAP.md` → "Tier 5 — Standalone grammar/spell-fix mode".
**Prereqs:** clean `main` (builds, clippy clean, ≈1509 tests green).

This spec turns the roadmap Tier 5 bullet into an executable, phase-by-phase plan.
Every phase is sized to land independently, pure/testable layers first, novel FFI
last. File:line anchors are from a 2026-07-01 4-agent code analysis; verify them
before editing (they drift).

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
  `engine_core`, `engine`, `run_loop`, `context`, `prefs`, `grammar`). Only three
  surfaces are per-OS, each an existing `platform`-trait method: global hotkey,
  correction overlay, text-range read+replace. macOS is the reference impl.
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
  → run_loop: read_context(field) → word-under-caret + left_ctx
  → gate: grammar_fix_enabled(app) && suggestion_gates_pass && caps.AxSet
  → dispatch CompletionRequest{ kind: GrammarFix, word, left_ctx }   [inference.rs]
      worker: prompt = model_client::grammar_fix_prompt(word, left_ctx)
              raw = model.complete(prompt, small_max_tokens)
              vetted = grammar::vet_correction(word, raw)  → Option<String>
      → CompletionOutcome{ correction: Some(word'), replace_left }
  → run_loop: engine.on_correction(field, word', replace_left, word_range)
      → Command::ShowCorrection{ field, misspelled_range, suggestion }
      → OverlayPresenter::show_correction(word_rect, suggestion)   [underline+banner]
      → arm grammar-accept key (ghost-scoped)
grammar-accept key
  → AcceptBinding::GrammarAccept → engine AcceptFull path
  → Command::Replace{ field, text: word', replace_left }  [existing]
  → insert_replacing(...)  → tracker.apply_self_replace(...)
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
  model output, after trimming to its first whitespace-delimited token, is: (a)
  non-empty, (b) a single word, (c) different from `original` (no no-op), (d)
  within a small bounded edit distance of `original` (reject paraphrases /
  hallucinations). Preserve the original word's case via `textcase::CasePattern`
  (as `autocorrect`/`grammar` already do). Add a tiny bounded Levenshtein helper
  (`// ponytail: capped at MAX_EDIT, good enough for word-level typo distance`).
- Keep `capitalize_pronoun` as-is; `vet_correction` is independent.

**Tests (grammar + model_client `#[cfg(test)]`):**
- `grammar_fix_prompt` contains the word and the left context, is deterministic,
  and does not leak newlines that would break single-line parsing.
- `vet_correction` accepts a plausible one-edit fix (`"teh"→"the"`), preserves
  case (`"Teh"→"The"`), rejects: identical output, empty, multi-word
  (`"the cat"`), and a large-edit paraphrase (`"teh"→"kitten"`). Non-ASCII/CJK
  input is a clean miss, never a panic.

**Acceptance:** `cargo test -p grammar -p model_client` green; clippy clean.

---

## Phase G2 — Inference request kind + engine wiring + policy (headless) · effort M

**New/changed:**
- `crates/app/src/inference.rs`: extend `CompletionRequest` (~:286 area) with a
  request-kind discriminator — `enum RequestKind { Completion, GrammarFix }` (or a
  bool if cleaner against the actual struct) plus the `word`/`left_ctx` payload for
  the grammar case. In the worker serve loop, branch on the kind: for `GrammarFix`
  build `grammar_fix_prompt`, call `model.complete(prompt, GRAMMAR_MAX_TOKENS)`,
  run `grammar::vet_correction`, and emit a `CompletionOutcome` carrying
  `correction: Option<String>` + `replace_left` (word char count). `recv_latest`
  (:266) coalescing still applies (a newer trigger supersedes an older one).
- `crates/engine_core/src/lib.rs`: add a `presentation: Presentation` field
  (`enum Presentation { Ghost, Correction }`, default `Ghost`) to `Showing` (:172
  area) and thread it from a new
  `offer_correction(field, suggestion, replace_left, misspelled_range)` that mirrors
  `offer_replacement_multi` (:780) but sets `presentation = Correction` and emits a
  new `Command::ShowCorrection { field, misspelled_range, suggestion }` instead of
  `ShowGhost`. Accept reuses the existing `AcceptFull`/`AcceptWord` arms (:526-572)
  → `Command::Replace` unchanged. Same `InsertStrategy::AxSet` gate (:791).
- `crates/engine/src/lib.rs`: add `pub fn on_correction(...)` wrapping
  `offer_correction` (mirror `on_replacement` :283). Extend `FakeOverlay` (:554) so
  tests can observe the correction presentation.
- `crates/app/src/run_loop.rs`: add `grammar_fix: bool` to `Config` (:169) parsed as
  `COMPME_GRAMMAR_FIX` in `from_lookup` (:277). Add the detection helper: on a
  grammar trigger, use `read_context`→`left`, extract the word under the caret
  (see G1a below), gate via `replacement_decision`-style checks (:533) plus the new
  per-app policy, and dispatch the `GrammarFix` request; on the outcome, call
  `engine.on_correction`.
- `crates/context/src/lib.rs` (G1a): add `pub fn word_at_caret(value, caret) ->
  Option<(&str, Range)>` — the word the caret is in/just after (combine trailing
  word of `left_context` with any leading fragment of `right_context`). Returns the
  word text + its scalar range (for `replace_left` and the underline range).
- `crates/prefs/src/lib.rs`: add `grammar_fix: Option<bool>` to `AppPolicy` (:13),
  a `grammar_fix_enabled(app, default)` getter (mirror `autocorrect_enabled` :133),
  and a `AppPolicyField::GrammarFix` variant (:46) for the Apps-pane checkbox.

**Tests (all headless, fake model + fake adapter):**
- Worker: a `GrammarFix` request with a misspelled word yields an outcome whose
  `correction` is the vetted word and `replace_left` == word char count; a
  vet-rejected model output yields `correction: None` (no offer).
- Engine: `on_correction` produces a `Showing{ presentation: Correction,
  replace_left>0 }` and a `ShowCorrection` command; a subsequent `TextChanged`
  invalidates it (advance_snapshot); accept emits `Command::Replace` with the
  right `replace_left`.
- `context::word_at_caret`: caret at word end, mid-word, at a boundary, empty
  field, multibyte — correct range, no panic.
- prefs: `grammar_fix_enabled` resolves per-app override over the global default;
  `AppPolicyField::GrammarFix` round-trips through the policy-bits pack/unpack.
- run_loop: grammar detection respects the enable gate, per-app exclude, snooze,
  and the AxSet gate (no offer on non-AxSet fields).

**Acceptance:** `cargo test --workspace` green; clippy clean. No FFI touched yet —
the whole flow is exercised with the fake model + fake overlay.

---

## Phase G3 — Two keystrokes (registration + routing) · effort M

**New/changed (`platform` + `platform_macos` reference impl):**
- `crates/platform/src/lib.rs`: add `ShortcutAction::GrammarCheck` (:202) and
  `AcceptBinding::GrammarAccept` (accept-binding enum). Portable — Windows/Linux
  map their own hotkeys to the same actions.
- `crates/platform_macos/src/lib.rs`:
  - **trigger:** `CARBON_HOTKEY_GRAMMAR_CHECK: u32 = 8` (:118 area); add
    `grammar_check: Option<(i64,u32)>` to `ShortcutBindings` (:2521), wire into
    `from_config`, `has_internal_collision` (:2548), `registration_plan` (:2568),
    and `shortcut_action_for_hotkey_id` (:2586). Always-on, like ForceActivate.
  - **accept:** new Carbon id + a `grammar` slot in `AcceptKeymap` (:3142); extend
    `binding_for_hotkey_id` (:3425) and the per-arm arm/disarm path so the accept
    key is armed with the correction and disarmed on hide (mirror Word/Full).
- `crates/app/src/run_loop.rs`: `Config` gets `grammar_check_key: Option<String>`
  and `grammar_accept_key: Option<(i64,u32)>`, parsed in `from_lookup` (:289-295)
  as `COMPME_GRAMMAR_CHECK_KEY` / `COMPME_GRAMMAR_ACCEPT_KEY`. Add the
  `HostEvent::Shortcut(GrammarCheck)` arm (:3715) → run G2 detection. Add the
  grammar-accept arm next to Word/Full accept.

**Tests:** `ShortcutBindings::from_config` parses the grammar chord and detects a
collision with an existing shortcut/accept key; `registration_plan` lists the
grammar hotkey only when bound; `shortcut_action_for_hotkey_id` / `binding_for_hotkey_id`
inverse-map the new ids; config parses both new env keys. (Dispatch on a physical
keypress is a live-LOOK item, like 3.4.)

**Acceptance:** pure parse/plan layers unit-tested + green; live keypress deferred
to G4 validation.

---

## Phase G4 — Underline + correction-banner overlay (novel FFI) · effort L

The genuinely new UI. No underline/banner/attributed-string primitive exists today.

**New/changed:**
- `crates/platform/src/lib.rs`: add to `OverlayPresenter` (:526)
  `fn show_correction(&mut self, word_rect: ScreenRect, suggestion: &str) ->
  Result<(), PlatformError>` (or a sibling `CorrectionPresenter` trait). `hide()`
  reused. Update `FakeOverlay` (`engine/src/lib.rs:554`) + any `ux_mode`/placement
  plumbing.
- `crates/platform_macos/src/lib.rs`: implement `show_correction` by cloning the
  `ensure_panel` recipe (:776) into **two** borderless, mouse-transparent panels:
  (1) a 1–2px filled underline panel under the word rect (from
  `read_ax_bounds_for_range` :4559, char→UTF-16 via `extend_range_left` :4840; do
  **not** apply the thin-caret `usable_caret_rect` guard), (2) a small
  background-filled banner panel anchored just above the word rect showing
  `suggestion`. Reuse `overlay_frame_for_text` (:969) Y-flip. Degrade to a
  caret-anchored popup when the word rect is `Ok(None)`.
- Wire `Command::ShowCorrection` (G2) → `show_correction`.

**Tests / validation:** geometry unit test (underline sits under the word rect,
banner above, both within screen bounds — mirror `apps_pane_grid_has_no_overlaps`
style); the rest is **live LOOK** on a granted Mac (add a row to
`docs/MANUAL-VALIDATION.md`): type a typo, press trigger, confirm underline +
banner render at the word, press accept, confirm in-place replacement.

**Acceptance:** deterministic geometry test green; manual-validation row added and
checked on-device.

---

## Phase G5 — Settings surface · effort M

- `crates/platform_macos/src/settings_window.rs`: add `RecorderRole::GrammarAccept`
  (:37) + a recorder row (reuse `KeyRecorderField` :692); widen `record_decision`
  (:66) from 2-role to N-role collision. Add the `GrammarFix` checkbox column to the
  Apps-pane grid (`apps_layout`, geometry test updated for the extra column).
- `crates/app/src/run_loop.rs`: handle the new `RebindRequest` role (:4002) →
  live-rebind the grammar-accept key. Surface `grammar_fix` in the General/Apps
  panes.
- grammar-trigger stays a startup config string for v1 (like the other global
  shortcuts); a recorder row for it rides the future `ShortcutBindings` UI tick.

**Tests:** recorder N-role collision; Apps-pane geometry with the new column has no
overlaps; live rebind persists.

**Acceptance:** `cargo test --workspace` green; Apps-pane + recorder validated.

---

## Cross-platform follow-on (after macOS reference lands)

Each OS implements the same four trait rows; grammar-fix stays inert (fail-closed
`UnsupportedField`) until a row is built, never misbehaves.

| Surface | Windows | Linux |
|---|---|---|
| Global trigger hotkey | `RegisterHotKey` | X11 `XGrabKey` / Wayland global-shortcuts portal |
| Grammar-accept key | keyboard hook / `RegisterHotKey` | X11/Wayland key grab |
| Word rect + in-place replace | UI Automation `TextPattern` BoundingRectangles + `SetText` | AT-SPI2 `Text`/`EditableText`, or IME/synthetic fallback |
| Underline + banner overlay | layered top-most window (`LayeredWindow`) | `wlr-layer-shell` (`LayerShell`) / override-redirect X11 |

Detection (LLM) has **no** per-OS surface — same `model_client`/`inference` path
everywhere. This is the same parity model as Tier 1.1 foundation work and depends
on the Windows/Linux `insert_replacing`/text-read impls they owe regardless.

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

## Definition of done (macOS reference)
G1–G5 landed, `cargo test --workspace` green, clippy clean, a MANUAL-VALIDATION
row for the trigger→underline→banner→accept flow checked on-device, and the
ROADMAP Tier 5 status flipped from ☐ to 🟢 with verified anchors.
