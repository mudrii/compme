# Integration Phase — Wiring the Pure Cores into the Live Loop

**Date:** 2026-06-09
**Status:** **Steps 1–5 built (uncommitted working tree), unit/build-verified.**
Emoji + autocorrect + British-English are wired through the replacement pipeline,
default-off, gated, race-free, AxSet-only. The remaining hop is **step 6: the live
macOS §16 accept gate** (and the SyntheticKeys/Clipboard backspace-synthesis
residual) — GUI/FFI-bound, needs live validation, not loop-cyclable. Per-step
status markers are inline below.

## Why this exists

Pure §16 parity features are exhausted (cycles 11–18): `emoji`, `thesaurus`,
`autocorrect`, `localize`, `webconfig` are complete, unit-tested pure crates that
are **not wired into the live completion flow**. The repeated loop finding is that
the only substantive work left is integration, and its #1 blocker is a
**delete-before-insert (replacement) primitive**: these features all *replace*
already-typed characters (`:smile`→😀, `teh`→`the`, `color`→`colour`,
word→synonym) rather than append a continuation.

A cycle-18/19 investigation found the primitive does **not** cleanly separate from
the offer/gating path (a replacement must live on the engine's `Showing` state,
accept-honoring needs an offer entry point, and the host must both *produce* the
suggestion and *honor* the deletion). Building it as isolated plumbing yields
unused code + an untested branch — anti-patterns this project rejects. Hence: build
it top-to-bottom, with its first consumer (emoji), behind a default-off flag.

## Resolved design decisions

### 1. Replacement shape = `replace_left: usize` (caret-anchored, left-only)
All four features replace a token that **ends at the caret** (the user just typed
`:smile`, `teh`, `color`). Deletion is therefore purely to the left; no right-side
or arbitrary-range replacement is needed. The count is in **characters**
(`chars().count()` of the matched/typed token), converted to UTF-16 units at the
AX boundary. `emoji::suggest` already returns this as `replace_chars`.

- `engine_core::Command::Insert { field, text, replace_left: usize }` — add the
  field; `replace_left == 0` is a plain insert (every current emit site sets 0 →
  byte-identical behavior; existing tests change only the struct literal).

### 2. A replacement is "a `Showing` with `replace_left > 0`"
Add `replace_left: usize` (default 0) to `engine_core::Showing`. Model completions
create showings with `replace_left = 0` (unchanged). Both accept paths emit
`Command::Insert { replace_left: showing.replace_left, .. }`:
- `AcceptFull` → emit with the showing's `replace_left`.
- `AcceptWord` on a replacement: replacements are atomic single tokens, so
  `next_word` returns the whole glyph/word with empty rest → behaves as full →
  emit `replace_left`. (Word-accept never *partially* replaces.)
- `preview_accept_insert` must apply the **same** `replace_left`/text as `on_event`
  (the host absorbs the self-insert echo via `preview`; divergence re-arms a
  spurious request — the trailing-space feature already established this invariant).
  Preview returns `(field, text)`; the host needs the count too → either widen the
  preview return to include `replace_left`, or have the host read it off the offer.

### 3. Offer entry point (pure, host-driven)
Add `engine_core` method `offer_replacement(field, text, replace_left) -> Vec<Command>`:
sets `self.showing = Some(Showing { candidates: vec![text], replace_left, .. })` and
emits `ShowGhost` + a `Shown` stat event — **reusing the exact gating** that model
completions pass (enabled, not secure, field writable, not suppressed). It is
crate-agnostic: the **host** computes `text`+`replace_left` by calling
`emoji::suggest`/`autocorrect::correct`/`localize::to_british`/`thesaurus::synonyms`
and feeds them in. engine_core gains **no** dependency on those crates.

- **Offer priority vs model completion:** a detected local replacement is instant
  and high-confidence; it should **preempt** an in-flight model request for that
  keystroke (show the replacement ghost; don't also fire the model, or let the
  model supersede it). Decision: when the host detects a replacement opportunity on
  `TextChanged`, it calls `offer_replacement` and **skips** the model submit for
  that turn. (Cotypist behaves this way — emoji/typo offers are local + immediate.)

### 4. Host adapter contract (the FFI hop)
`platform::PlatformAdapter` gains a **defaulted** method:
```rust
fn insert_replacing(&self, field, text, replace_left: usize, strategy)
    -> Result<Inserted, PlatformError> { self.insert(field, text, strategy) } // default ignores
```
Engine dispatch calls `insert_replacing` when `replace_left > 0`, else `insert`
(common path unchanged → zero risk to existing tests). `FakeAdapter` overrides it
to record `replace_left` for the wiring test.

- **[DONE, cycle 25] `platform_macos` honoring — AxSet:** `insert_replacing` is
  overridden (via a shared `insert_impl(replace_left)`); for AxSet it threads
  `replace_left` into `insert_for_field`, which calls the pure `extend_range_left`
  helper to widen the splice range left by the typed token's UTF-16 width before
  the existing `splice_text_at_utf16_range`. `extend_range_left` is unit-tested
  (ASCII end-to-end, astral/UTF-16, zero=unchanged, clamp). `replace_left == 0` is
  byte-identical (the 164 existing platform_macos tests pass unchanged). Only the
  live AX deletion behavior in real apps remains to confirm (step 6).
- **`platform_macos` honoring — SyntheticKeys / Clipboard:** cannot read-modify-write
  a range. `insert_impl` falls these back to a plain append-only insert
  (`replace_left` ignored), so they would leave the typed token in place. Honoring
  means synthesizing N backspaces before the insert, verified live in target apps.
  **This is the one true live-validated residual** (built in a later cycle). Safe because production emits `replace_left > 0` only
  once the offer path + AxSet honoring ship together, and replacement features are
  gated to AxSet-capable fields first.

### 5. Flags / config (default off; host-read)
`COMPLETE_ME_EMOJI` (+ `_SKIN_TONE`, `_GENDER`), `COMPLETE_ME_AUTOCORRECT`,
`COMPLETE_ME_BRITISH_ENGLISH`, `COMPLETE_ME_THESAURUS` — already reserved in §8 of
the engine-macos design as the wiring contract. Each gates whether the host calls
the corresponding crate in the `TextChanged` replacement-detection step.

## Build order (each step tested before the next)
1. **engine_core:** `Command::Insert.replace_left` + `Showing.replace_left` (default
   0) + accept paths emit it + `preview` parity. Tests: completions always emit 0;
   a showing with `replace_left=N` accepts to `Insert{replace_left:N}`. *(pure)*
2. **engine_core:** `offer_replacement` reusing completion gating. Tests: offers only
   when gates pass; secure/suppressed/disabled block it; emits ShowGhost+Shown.
   *(pure)*
3. **engine + platform:** defaulted `insert_replacing`; dispatch threads
   `replace_left`; `FakeAdapter` wiring test. *(pure)*
4. **platform_macos:** `replacement_range` pure helper + test; AxSet honoring wired.
   *(pure helper + FFI call)*
5. **[DONE, cycle 26] app/run_loop:** the observe `Observation::Typed` branch now,
   after `on_text_changed`, calls `emoji_offer(&ctx.left, &config.emoji)`; on a hit
   it `latest.clear()`s (preempts the just-queued model request — Cotypist behavior)
   and `engine.offer_replacement(field, glyph, replace_chars)`. Gated by
   `COMPLETE_ME_EMOJI` (+ `_SKIN_TONE`/`_GENDER`), default off → `config.emoji ==
   None`. Pure `emoji_offer`/`build_emoji_config`/`parse_skin_tone`/`parse_gender`
   helpers are unit-tested. Preempt is safe: `on_text_changed` advances the snapshot
   (stale prior requests discarded) and the current model request is cleared, so no
   completion can supersede the emoji ghost.
6. **Live validation (manual, §16):** physical-key accept of an emoji/typo/UK-spelling
   replacement in TextEdit (AxSet) deletes the typed token and inserts the
   replacement; then SyntheticKeys/Clipboard backspace-synthesis + live re-run.

Steps 1–5 are unit/build-verified; step 6 is the live §16 gate (mirrors the existing
Carbon-accept manual gates). First consumer to wire: **emoji** (simplest, already
returns `replace_chars`), then autocorrect/localize/thesaurus reuse the same path.

## Pre-wiring checklist (from the step 1–3 code review)

Steps 1–3 are built (engine_core `Command::Replace` + `Showing.replace_left` +
`offer_replacement`; defaulted `insert_replacing`; engine dispatch arm) and are
correct + safe **only because nothing emits `Command::Replace` in production yet**.
Before step 5 wires `offer_replacement` into the run loop, these MUST be done or a
replacement accept will misbehave:

1. **[DONE, cycle 24] Echo absorption for replacements.** `preview_accept_insert`
   now returns `(field, text, replace_left)` — mirroring the accept paths byte-for-
   byte (a replacement is atomic + unfinalized). `FieldTracker::apply_self_replace`
   (delete-left then insert, clamped) was added and the run-loop accept path routes
   to it when `replace_left > 0`. Fully unit-tested; no behavior change in
   production (replace_left stays 0 until the detection point in step 5 lands).
2. **[DONE, cycle 21] Field-identity guard.** `offer_replacement` now returns early
   unless `self.field.as_ref() == Some(field)` (focus-race guard; tested).
3. **[DONE, cycle 21] FakeAdapter wiring test.** `FakeAdapter::insert_replacing`
   override + `Engine::offer_replacement` passthrough + a test
   (`replacement_accept_forwards_replace_left_through_dispatch`) proving dispatch
   routes a replacement accept through `insert_replacing` with the right
   `replace_left`, not the plain insert path.

Already handled in steps 1–3: replacement accepts are **atomic** (Word == Full, so
multi-word synonyms are never split and never drop the deletion); replacement text
is **not** trailing-spaced; `enabled()` blocks secure fields; the snapshot guard
discards stale model completions so they cannot supersede an offered ghost.

## Open question for the human
Offer UX when *both* a local replacement and a model completion are plausible, and
whether thesaurus (selection-triggered, not type-triggered) needs a different
trigger than emoji/autocorrect/localize. Defaulting to "local replacement preempts;
thesaurus is a separate selection-mode trigger" per §4 above unless overridden.
