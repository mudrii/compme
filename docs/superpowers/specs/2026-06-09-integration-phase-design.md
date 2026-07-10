# Integration Phase — Wiring the Pure Cores into the Live Loop

**Date:** 2026-06-09
**Status:** **COMPLETE — steps 1–6 done. Step 6 (live macOS §16 accept gate)
PASSED 2026-06-10** (physical Tab accept in TextEdit: `:smile`→😄 with deletion,
`teh`→`the`; `color`→`colour` offered + placed; Esc-dismiss verified — see
ACCEPTANCE.md, A2 Local-Replacement Live Gate). The live validation also
surfaced and fixed four real integration bugs: Carbon hotkey events were never
dispatched (no NSApp event drain — `pump_app_events`), `SharedAdapter` silently
dropped `replace_left` (trait default removed, method now required), and the
overlay placement needed live calibration (AX caret rect bottom edge = line
top; box/font hug the line). The backspace poster seam exists, and a plain
insert fallback for silently ignored AxSet writes was LIVE-VALIDATED 2026-06-10
in iTerm2. Non-AxSet replacements remain fail-closed residual work because the
global input channels cannot atomically delete `replace_left` and insert the
replacement.

## Why this exists

Pure §16 parity features were exhausted as pure crates in cycles 11–18:
`emoji`, `thesaurus`, `autocorrect`, `localize`, `webconfig`. The later
integration pass wired the local-replacement consumers into the live run loop
behind default-off flags; the remaining thesaurus work is live LOOK/selection
UX validation, not core host wiring. The repeated loop finding at the time was
the #1 blocker: a
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

### 1. Replacement shape = `Command::Replace` with `replace_left: usize` (caret-anchored, left-only)
All four features replace a token that **ends at the caret** (the user just typed
`:smile`, `teh`, `color`). Deletion is therefore purely to the left; no right-side
or arbitrary-range replacement is needed. The count is in **characters**
(`chars().count()` of the matched/typed token), converted to UTF-16 units at the
AX boundary. `emoji::suggest` already returns this as `replace_chars`.

- `engine_core::Command::Replace { field, text, replace_left: usize }` — carries
  caret-anchored replacement text separately from plain inserts. `replace_left`
  is always non-zero for production replacement offers; plain completions continue
  through `Command::Insert`.

### 2. A replacement is "a `Showing` with `replace_left > 0`"
Add `replace_left: usize` (default 0) to `engine_core::Showing`. Model completions
create showings with `replace_left = 0` (unchanged). Both accept paths emit
`Command::Replace { replace_left: showing.replace_left, .. }` when the showing is
a replacement, otherwise they keep emitting `Command::Insert`:
- `AcceptFull` → emit with the showing's `replace_left`.
- `AcceptWord` on a replacement: replacements are atomic single tokens, so
  `next_word` returns the whole glyph/word with empty rest → behaves as full →
   emit `Command::Replace`. (Word-accept never *partially* replaces.)
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
`platform::PlatformAdapter` gained a **required** method; the first live
validation found that a default silently hid `SharedAdapter` support, so the
default was removed:
```rust
fn insert_replacing(&self, field, text, replace_left: usize, strategy)
    -> Result<Inserted, PlatformError>;
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
  byte-identical (the then-current platform_macos regression suite passed
  unchanged; the exact count is historical). **Live AX
  deletion CONFIRMED (step 6, 2026-06-10):** the typed token is physically deleted
  and replaced in TextEdit.
- **`platform_macos` honoring — SyntheticKeys / Clipboard (superseded prototype):**
  the first implementation synthesized N backspaces before posting replacement
  text. That is not atomic, so the committed adapter now refuses every non-zero
  `replace_left` on `SyntheticKeys`/`Clipboard` before posting either deletion or
  text. Replacement offers are gated on
  `InsertStrategy::supports_atomic_range_replace()` (`AxSet` or future
  `NativeRangeSet`). The 2026-06-10 iTerm2 run validated only the
  silently-ignored-`AxSet` fallback for a **plain append** (`replace_left == 0`);
  silently ignored AxSet replacements fail closed rather than falling back.

### 5. Flags / config (default off; host-read)
`COMPME_EMOJI` (+ `_SKIN_TONE`, `_GENDER`), `COMPME_AUTOCORRECT`,
`COMPME_BRITISH_ENGLISH`, `COMPME_THESAURUS` — already reserved in §8 of
the engine-macos design as the wiring contract. Each gates whether the host calls
the corresponding crate in the `TextChanged` replacement-detection step.

## Build order (each step tested before the next)
1. **engine_core:** `Command::Replace` + `Showing.replace_left` (default 0) +
   accept paths emit replacement commands + `preview` parity. Tests: completions
   still emit plain inserts; a showing with `replace_left=N` accepts to
   `Replace{replace_left:N}`. *(pure)*
2. **engine_core:** `offer_replacement` reusing completion gating. Tests: offers only
   when gates pass; secure/suppressed/disabled block it; emits ShowGhost+Shown.
   *(pure)*
3. **engine + platform:** required `insert_replacing`; dispatch threads
   `replace_left`; `FakeAdapter` wiring test. *(pure)*
4. **platform_macos:** `replacement_range` pure helper + test; AxSet honoring wired.
   *(pure helper + FFI call)*
5. **[DONE, cycle 26] app/run_loop:** the observe `Observation::Typed` branch now,
   after `on_text_changed`, calls `emoji_offer(&ctx.left, &config.emoji)`; on a hit
   it `latest.clear()`s (preempts the just-queued model request — Cotypist behavior)
   and `engine.offer_replacement(field, glyph, replace_chars)`. Gated by
   `COMPME_EMOJI` (+ `_SKIN_TONE`/`_GENDER`), default off → `config.emoji ==
   None`. Pure `emoji_offer`/`build_emoji_config`/`parse_skin_tone`/`parse_gender`
   helpers are unit-tested. Preempt is safe: `on_text_changed` advances the snapshot
   (stale prior requests discarded) and the current model request is cleared, so no
   completion can supersede the emoji ghost.
6. **[DONE, 2026-06-10] Live validation (manual, §16):** physical-key accept of an
   emoji/typo replacement in TextEdit (AxSet) deletes the typed token and inserts
   the replacement — PASSED (`:smile`→😄, `teh`→`the`; `colour` offered + placed;
   Esc-dismiss verified; ACCEPTANCE.md). The iTerm2 readback-fallback validation
   covered a silently ignored AxSet plain insert, not a non-AxSet replacement;
   SyntheticKeys/Clipboard replacements remain fail-closed residual work.

Steps 1–6 are done; step 6 passed live (mirrors the existing Carbon-accept manual
gates). Emoji was the first consumer wired; autocorrect/localize reuse the same
path, and thesaurus now uses that path behind `COMPME_THESAURUS`. Remaining
thesaurus work is live LOOK/UX validation and any future selection-trigger design.

## Pre-wiring checklist (from the step 1–3 code review)

These were the safety blockers that had to close before step 5 wired
`offer_replacement` into the production run loop. They are now complete, which is
why the local-replacement path can safely emit production replacement offers.

1. **[DONE, cycle 24] Echo absorption for replacements.** `preview_accept_insert`
   now returns `(field, text, replace_left)` — mirroring the accept paths byte-for-
   byte (a replacement is atomic + unfinalized). `FieldTracker::apply_self_replace`
   (delete-left then insert, clamped) was added and the run-loop accept path routes
   to it when `replace_left > 0`. Fully unit-tested; no behavior change in
   production for plain completions (`replace_left == 0`), while local
   replacements use the widened self-echo path after step 5.
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

## Resolved UX decision
When both a local replacement and a model completion are plausible, the local
replacement preempts the model request for that turn. Emoji, autocorrect,
British-English normalization, and the current thesaurus host path are
type-triggered trailing-token offers. Selection-triggered thesaurus UX remains
future parity work and should reuse the same lookup once a selection surface
exists.
