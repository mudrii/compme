# Plan Review - Online Validation and Shortcomings

**Date:** 2026-06-04  
**Scope:** `docs/superpowers/**` and `tools/spike/**`  
**Verdict:** GO for contract-first A1a implementation and A1b planning after the 2026-06-05 P6 redo and P2 model comparison. The remaining guardrail is to keep A1a bound to the A1b macOS adapter contract; `tools/spike/FINDINGS.md` is the source of truth for the model decision.

**2026-06-05 P6 overlay update:** P6 is now PASS. Screenshot `/tmp/complete-me-p6-redo-before.png` shows visible ghost text over a Chrome click target; CoreGraphics listed the onscreen layer-101 overlay before and after the click; non-activation and click-through were verified. The click-through proof clicked `{590,1185}`, changing Chrome title from `clicked-0` to `clicked-1`.

**2026-06-05 A1b status update:** This file is a historical review snapshot. Finding 14 and the corrected-order text that says to draft A1b before A1a are superseded by `2026-06-04-a1b-macos-adapter-contract.md` and the current implementation handoff. A1b now has an active contract plan and the macOS adapter has live acceptance for TextEdit read/caret/AxSet/SyntheticKeys/Clipboard/full-accept/word-accept, accept-tap inactive/full/word/delayed-hide, Safari marker-path caret geometry, repo-local popup fallback, and overlay show/update/hide diagnostics.

This review validates the current project plan against the local spike and current online primary documentation. It is intentionally stricter than a summary: findings are ordered by execution risk.

## Online Validation Sources

**Status note (2026-06-08, historical):** This section records the *original* validation pass, when the app shell was planned as a Tauri v2 tray app. The project has since pivoted to a **native Rust/AppKit shell with no Tauri dependency** (see the pivot note in `2026-06-03-engine-macos-mvp-design.md`). The Tauri v2 sources below are **historical** — they no longer describe the active design. The CGEventTap / AX / `llama-cpp-2` sources remain current.

Context7 was used for Tauri v2 docs, per repo instructions:

- `npx ctx7@latest library tauri "..."`
- `npx ctx7@latest docs /websites/v2_tauri_app "..."`

Primary sources checked:

- Tauri v2 system tray docs: `TrayIconBuilder` is the right tray API, and Linux tray events are limited/unsupported in parts of the tray event surface: <https://v2.tauri.app/learn/system-tray/>
- Tauri v2 updater docs: updater artifacts require `bundle.createUpdaterArtifacts`, a public key, and endpoints: <https://v2.tauri.app/plugin/updater/>
- Tauri v2 global shortcut docs: plugin registers global shortcuts; it does not provide "swallow arbitrary Tab while another app owns focus" semantics: <https://v2.tauri.app/plugin/global-shortcut/>
- Apple Core Graphics `CGEventTapCreate`: event taps can observe key events with accessibility/root permission and are installed on a run loop: <https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate%28tap%3Aplace%3Aoptions%3Aeventsofinterest%3Acallback%3Auserinfo%3A%29?language=objc>
- Apple `CGEventTapOptions`: `Default` is an active filter; `ListenOnly` is passive and cannot modify/divert events: <https://developer.apple.com/documentation/coregraphics/cgeventtapoptions>
- Apple AX header docs: AX messaging can fail with `kAXErrorCannotComplete`; `AXUIElementSetMessagingTimeout` exists and sets the AX timeout: <https://developer.apple.com/documentation/applicationservices/axuielement_h>
- Apple `NSApplication.ActivationPolicy`: `.accessory` means no Dock and no menu bar, but windows can still be activated: <https://developer.apple.com/documentation/appkit/nsapplication/activationpolicy-swift.enum>
- docs.rs `llama-cpp-2`: `token_to_str` is deprecated; `token_to_piece` is the replacement and uses a stateful decoder: <https://docs.rs/llama-cpp-2/latest/llama_cpp_2/model/struct.LlamaModel.html>
- docs.rs `accessibility-sys`: `AXUIElementSetMessagingTimeout(element, timeoutInSeconds)` is available in the pinned crate family: <https://docs.rs/accessibility-sys/latest/accessibility_sys/fn.AXUIElementSetMessagingTimeout.html>
- docs.rs `core-graphics`: 0.25 exposes `CGEventTapOptions`, `CallbackResult`, and the safe `CGEventTap::new` callback shape used by the spike: <https://docs.rs/crate/core-graphics/latest/source/src/event.rs>
- docs.rs `objc2-app-kit`: `NSPanel::initWithContentRect_styleMask_backing_defer` exists in 0.3.2 and `NSPanel` is a default feature: <https://docs.rs/objc2-app-kit/latest/objc2_app_kit/struct.NSPanel.html>, <https://docs.rs/crate/objc2-app-kit/0.3.2/features>

## Blocking Shortcomings

### 1. A0 was not actually complete before the manual pass

The main spec says A0 exits only when four real-world unknowns are proven: native plus Chromium caret ladder, two-tap CGEventTap, NSPanel overlay, and warm llama latency. This was a blocker when the review was written because `tools/spike/FINDINGS.md` still said P3-P7 were compile-only and `tools/spike/MANUAL-ACCEPTANCE.md` had behavior checks unchecked.

Status after fixes: P3, P4, P5, P5b, P6, and P7 now have manual acceptance evidence recorded. P6 specifically was redone on 2026-06-05 with screenshot-backed visible text and click-through verification.

Required guardrail:

- Keep `tools/spike/FINDINGS.md` as the source of truth for the A0 exit decision.
- Keep the A1 GO wording tied to the contract-first guardrail and model decision recorded in `tools/spike/FINDINGS.md`.
- Chromium/Electron coverage still needs to stay explicit in A1b because AX marker behavior differs by toolkit.

### 2. P5 does not prove the required two-tap design

The spec and prior-art review say the production tap must be:

- permanent `ListenOnly` observer tap
- transient active `Default` consuming tap while a suggestion is visible
- teardown/re-enable handling for disabled taps

The spike `p5_tap.rs` creates a single active consuming tap and hardcodes `suggestion_visible = true`. Apple docs confirm this distinction matters: `ListenOnly` is passive and cannot modify/divert, while `Default` is the active filter mode.

Status after fixes: P5b now proves the observer/consumer split with listen-only F8 state toggling and active Tab swallowing only while a simulated suggestion is visible.

Required guardrail:

- A1b still must implement real lifecycle semantics around a real suggestion, including create/enable/teardown, tap-disabled handling, and deferred teardown after synthetic insertion.

### 3. Chromium/Web caret path remains an A1b risk

The plan identifies the web path as `AXSelectedTextMarkerRange` to `AXBoundsForTextMarkerRange`, not `NSRange`. The implemented spike only does:

- zero-length `kAXBoundsForRangeParameterizedAttribute`
- previous-character fallback

Impact: The spec's A0 requirement says native and Chromium app. The manual P4 pass verified Chrome textarea caret geometry with the existing range fallback, but the AXTextMarker path is still not first-class.

Required fix:

- Keep A1b responsible for `AXSelectedTextMarkerRange` / `AXBoundsForTextMarkerRange`.
- Add a P4b-style marker probe or adapter test before declaring broad Chromium/Electron support.

### 4. A1a `PlatformAdapter` is not the validated contract

The validated spec/cross-platform review contract includes:

- `environment()`
- `subscribe_focus`
- `subscribe_caret`
- `FieldHandle`
- per-field capabilities
- `read_context(&FieldHandle) -> Result<TextContext>`
- `caret_rect(&FieldHandle)`
- `insert(&FieldHandle, text, strategy) -> Result<Inserted>`
- `coords_global_screen`

A1a Task 1 reduces that to focused-field direct methods returning `Option`/`bool`.

Impact: The A1a plan says it implements the validated contract, but it actually creates a smaller local convenience trait. This will force contract churn in A1b/B/C.

Required fix:

- Either rename it to `FocusedPlatformProbe` or `CorePlatformPort` and declare it is a temporary A1a subset, or implement the full validated contract now.
- Prefer implementing the real types now, even with fake callbacks and fake `FieldHandle` in tests.
- Use `Result` with explicit error enums for diagnostics instead of `Option`/`bool`.

### 5. Core trigger gating is underspecified and will produce noisy suggestions

The spec requires:

- debounce
- not mid-word unless configured
- not on backspace
- min context length
- per-app override gate
- secure-field hard block

A1a `TextChanged` schedules a request whenever `ux_mode` is Inline/Popup. The event has no edit kind, no typed key, no deletion/backspace bit, no word-boundary hint, no app policy, and no min context.

Impact: The first-run trust goal is undermined. The state machine would request too often and in the wrong places.

Required fix:

- Replace `TextChanged { value, caret, now_ms }` with an input event that includes `edit_kind` (`Insert`, `Delete`, `Paste`, `Unknown`), previous caret/value metadata, and a `TriggerPolicy`.
- Add tests for no request after backspace, no request mid-word, no request below min context, and request after whitespace/sentence boundary.

### 6. Focus invalidation clears internal state without emitting `Hide`

The spec says focus/app change invalidates the suggestion. A1a `Focus` clears `showing` internally but returns no `Hide` command.

Impact: The adapter may leave stale overlay UI visible unless it independently hides on focus changes. That violates the clean `Event -> Command` contract.

Required fix:

- If `showing` is present, `Focus` must emit `Hide`.
- Add tests for focus change while showing and secure-focus transition while showing.

### 7. Secure Input is modeled too narrowly

A1a tests only use `Capabilities.secure`. The main spec says secure block includes AX secure text fields and global `IsSecureEventInputEnabled`.

Impact: A1a can pass while A1b has no explicit event/diagnostic path for global Secure Input. This is a user-facing support burden.

Required fix:

- Add a `SecurityState`/`BlockReason` to capabilities or focus events.
- Distinguish `SecureField`, `SecureInputEnabled`, `PermissionMissing`, and `UnsupportedToolkit` for diagnostics.

### 8. Model choice was internally inconsistent before P2b

A1a said use a base model default because instruct drifts to chatter, but the earlier evidence only covered `qwen2.5-0.5b-instruct-q4_k_m.gguf`.

Status after fixes: `p2_model_compare` now benchmarks instruct and base across the same autocomplete prefixes and raw/FIM/instruction prompt modes. The selected A1a development default is `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` with the terse continuation prompt. Empty-suffix FIM is rejected for now based on observed blanks/corrupt/off-topic text.

Required guardrail:

- Keep model path and prompt strategy configurable in A1a.
- Keep candidate shaping and stop-boundary work explicit; the base+terse choice is a development default, not a final product-quality guarantee.

### 9. A1a carried a known-deprecated llama API

Current docs.rs for `llama-cpp-2` marks `token_to_str` deprecated and says to use `token_to_piece`; it also notes token decoding can require a stateful decoder to avoid losing partial strings. This review originally found A1a still using `token_to_str(tok, Special::Tokenize)`.

Status after 2026-06-04 fix pass: A1a and spike code now use `token_to_piece` with a UTF-8 decoder. Keep this as a regression guard.

Required fix:

- Add a test or smoke path that compiles warning-free under `RUSTFLAGS=-D warnings` for `model_client`, if practical.

### 10. The model client design does not match the latency architecture

The spec says warm model, prefix cache, short context, serialize llama calls, and avoid full re-prefill on each keystroke. A1a's real `LlamaModel` creates a fresh context per completion and has no actor/mutex, shutdown, warm-up method, prefix-cache policy, or hybrid-model guard.

Impact: The plan may pass the current 34 ms micro-test but will not encode the production inference invariants.

Required fix:

- A1a now uses a fallible `LocalModel` completion API so llama runtime failures return `LocalModelError` instead of panicking. Keep A1a headless, but define the next production `LocalModel` lifecycle around:
  - `load`
  - `warm_up`
  - `complete`
  - `shutdown`
  - model metadata flags: `is_hybrid`, `is_recurrent`
- Document that prefix cache is A1b/A2 if not implemented now, and add a no-cache warning to the trait comments.

### 11. Text indexing is not proven against macOS AX ranges

The tests use Rust `chars()`, including one accent case. Apple accessibility docs describe text ranges as characters, not bytes, but that does not prove equivalence with Rust scalar-value indexing for emoji, composed graphemes, or NSString/UTF-16 edge cases.

Impact: Caret offsets from AX may not map cleanly to Rust `.chars().take(caret)`. Emoji and composed characters can break context slicing or caret placement.

Required fix:

- Add explicit tests for emoji, skin-tone modifiers, combined accents, and CJK.
- Decide whether internal offsets are UTF-16 code units, Unicode scalar values, or grapheme clusters.
- If using AX ranges directly, introduce conversion helpers and do not scatter `.chars().take(caret)` through the engine.

**Status (2026-06-08): RESOLVED.** The decision and conversion are in place, with the boundary in the right layer:

- **Decision:** macOS AX offsets are **UTF-16 code units** (`TextContext.offset_encoding = OffsetEncoding::Utf16CodeUnits`); the **engine works in Unicode scalars** end-to-end.
- **Conversion lives in the adapter**, not the engine. `platform_macos::byte_index_for_utf16_units` maps an AX UTF-16 offset to a char-boundary byte index (a target that bisects a surrogate pair rounds up to the char end — never a non-boundary byte). `text_context_from_value` uses it to split `left`/`right`; `splice_text_at_utf16_range` uses it for insertion. So the adapter hands the engine byte-correct substrings, and the engine's scalar `.chars().take(caret)` (re-derived in `app::wiring::value_and_caret` from `left.chars().count()`, ignoring the raw AX caret) is internally consistent. No scalar→UTF-16 conversion is needed anywhere because AX operations (caret rect, insertion) are done in UTF-16 directly against AX, never from engine offsets.
- **Tests:** `byte_index_for_utf16_units` (0 / before / mid-surrogate / after / past-end), `text_context_from_value` astral split + selection + clamp, `splice_text_*` astral replace, plus scalar-vs-UTF-16 caret cases in `app::wiring` (emoji, skin-tone, combining accent, CJK). The remaining `.chars()` usage in `context`/`core` operates on those already-byte-correct substrings using the engine's scalar convention — by design, not a defect.

### 12. `TextContext` is too small for the spec

The spec's platform contract reads left/right/selection and supports pasteboard fallback. A1a context only returns left context helpers.

Impact: Mid-line completion, suffix-overlap guards, selected-text replacement, and later pasteboard fallback will require reshaping context APIs.

Required fix:

- Define a real `TextContext { left, right, selection, caret, source, field_id }` now.
- Keep helper functions, but do not make them the main context contract.

### 13. Ranker logic is too shallow for the claimed responsibilities

A1a names `ranker` as boundary/repetition/scoring, but the implementation only caps words, splits a first word, and checks exact word repetition.

Impact: The name overstates the behavior. Sensitive penalties, suffix-overlap, stop boundaries, quality thresholds, and multi-candidate ranking are missing.

Required fix:

- Either rename it `candidate_shape` for A1a, or add planned APIs/tests for:
  - sentence/newline stop
  - suffix overlap with right context
  - repeated phrase rejection
  - max words preserving trailing-space behavior

### 14. A1b is the actual risk center but has no plan (superseded 2026-06-05)

The highest-risk work is macOS adapter behavior:

- AX worker thread
- short AX timeout
- focus/caret observer
- owner pid attribution
- two-tap CGEventTap
- synthetic event tagging
- secure input detection
- native inline prediction suppression
- overlay coordinate correctness
- insertion strategy planner

The current A1a handoff says "write A1b after A1a", but A1a depends on A1b semantics in several places.

Impact: The project may build a clean core that still does not fit the adapter.

Required fix:

- Write the A1b plan before executing A1a, at least as an interface check.
- Use A1b to review A1a event/command types before code exists.

### 15. Plan commands previously used the wrong path

Earlier drafts referenced `cotypist_alt`; the active plan files now use `/Users/mudrii/src/compme`.

Impact if it regresses: agentic execution can commit in the wrong repository or fail.

Required guardrail:

- Keep future command blocks rooted at `/Users/mudrii/src/compme`.

## Non-Blocking But Worth Fixing

- Naming a crate `core` compiles, but it is confusing because Rust has a built-in `core` crate. Prefer `engine_core` or `complete_me_core`.
- A1a root workspace excludes `tools/spike`; that is good, but the model path in A1a still depends on the spike model location. Make that dependency explicit.
- The Tauri global-shortcut plugin is useful for configurable hotkeys, but not for plain Tab accept in another app. The plan should say "global shortcut is not a consuming accept path" rather than implying the plugin is broken.
- Historical Tauri updater research is superseded for macOS. A3 should now plan a native updater, with Sparkle as the leading candidate because Cotypist ships it; any non-Sparkle updater still needs artifact generation, public-key/signing-key handling, endpoint format, manifest strategy, and failure recovery.
- `ActivationPolicy::Accessory` is validated for no Dock/no menu bar, but windows can still activate; overlay windows must still use native non-activating panel behavior.

## Corrected Execution Order

1. **Keep A0 documentation as the acceptance source of truth.**
   - P3/P4/P5/P5b/P6/P7 now have real GUI results recorded.
   - P6 was redone on 2026-06-05 with visible text and click-through proof.
   - Base-vs-instruct is resolved in `tools/spike/FINDINGS.md`; A1a uses the base Q4_K_M model with terse continuation prompting as the development default.

2. **Draft A1b before implementing A1a.**
   - Use it to validate the `Event`, `Command`, `PlatformAdapter`, `TextContext`, and insertion contracts.

3. **Revise A1a plan.**
   - Full or honestly-subsetted platform contract.
   - Real `TextContext`.
   - Trigger policy and edit-kind events.
   - Focus/secure invalidation emits `Hide`.
   - Llama API updated to `token_to_piece`.
   - Warning-free model-client target if feasible.
   - Correct repo paths.

4. **Then implement A1a test-first.**
   - Platform/context/ranker can still be parallel.
   - Core should wait until A1b contract review is complete.
   - Model client proceeds against the selected base-model contract while keeping model path and prompt strategy configurable.

## Revised Go/No-Go

Current status after the 2026-06-05 P6 redo and P2 model comparison: **GO for contract-first A1a implementation and A1b planning.**

Reason: the critical platform probes are no longer compile-only. P3/P4/P5/P5b/P6/P7 have behavior evidence recorded in `tools/spike/FINDINGS.md`; P6 now has screenshot-backed visible text, onscreen layer-101 metadata before and after the click, non-activation, and Chrome click-through proof. P2b resolved the A1a development model default. A1a has also been gated against the broader A1b `PlatformAdapter` contract.

Remaining guardrail before and during implementation:

- Keep A1a implementation aligned to `2026-06-04-a1b-macos-adapter-contract.md`; do not implement the old narrow platform trait.
