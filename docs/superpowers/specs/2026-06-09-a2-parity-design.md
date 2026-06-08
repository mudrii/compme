# A2 Parity Features — Design & Status

**Date:** 2026-06-09
**Status:** In progress. Pure/deterministic features implemented + unit-tested + reviewed; GUI/permission-bound features specified with their §16 acceptance gates (environment-bound, like §15 G7 / Task 5c live residuals).
**Scope:** A2 from the roadmap (`2026-06-03-engine-macos-mvp-design.md` §9) — prompt-based personalization, per-app/per-domain gating, encrypted local memory, context augmentation, multi-candidate, compatibility surfaces. Acceptance gates are in design spec §16.

## Implemented this cycle (tested + reviewed)

| Feature | Crate(s) | Tests | §16 gate status |
|---|---|---|---|
| **Custom instructions (global)** prompt steering | `personalization` + `app` wiring | 14 + integration | Partial: global steers completions live (preamble prepended per request). Per-app/per-domain *maps* are crate-tested but not yet config-wired (A3 settings). |
| **6-stop strength slider, full reach (no caps)** | `personalization` | ✓ (pairwise-distinct stops) | ✅ all 6 stops selectable, observably stronger steer; no tier gating. |
| **Sender identity** in prompt | `personalization` | ✓ | ✅ name/email feed the preamble. |
| **Per-app enable/exclude + pause/snooze** | `prefs` + `app` gate | 8 + 2 builder + resolve | Partial: per-app exclude gates live (keyed on resolved bundle id); snooze logic done but not yet UI/signal-triggerable (A3 control surface). |
| **Per-app Tab disable** primitive | `prefs::tab_disabled` | ✓ | Logic present; tap-layer consumption of the per-app flag is A3 (needs the accept tap to read prefs). |
| **PII redaction** (pre-persistence) | `redaction` | 14 | Foundation for encrypted memory + diagnostics; emails/Luhn-cards/secrets scrubbed. |
| **Encrypted local memory** (inspect/delete) | `memory` | 11 | ✅ core: AES-256-GCM (app bound as AAD), ciphertext-only on disk (asserted), redact-on-insert, opt-in `StorageMode` (Off default / AcceptedOnly / AllMonitored, behaviorally distinct), `count`/`recent`/`delete_all`/`delete_app`, `secure_delete`. **A3 live residual:** Keychain `KeyProvider` (tests use `StaticKey`); run-loop wiring to record accepted completions + a settings surface. |

### Steering preamble injection
`app/inference.rs` computes `PersonalizationProfile::build_preamble(app, None)` per request and prepends it to the shaped prompt; `app/run_loop.rs` builds the profile + prefs from config keys (`COMPLETE_ME_INSTRUCTIONS`, `COMPLETE_ME_STRENGTH`, `COMPLETE_ME_SENDER_NAME/EMAIL`, `COMPLETE_ME_EXCLUDED_APPS`, `COMPLETE_ME_DEFAULT_ENABLED`).

## Documented limitations (from code review — deliberate, not bugs)

- **Domain gating is `None`** until browser-domain extraction lands (a later A2/A3 browser feature). Per-domain exclude/instructions are crate-tested but the run loop passes `domain = None`.
- **Per-app *personalization* maps** are not yet config-wired (only global instructions are). When A3 settings add them, the inference worker must key on the resolved **bundle id** (via `bundle_id_for_pid`), not `field.app` (`pid:N`) — the same fix already applied to the prefs gate.
- **Already-visible ghost on a mid-session pref change / snooze is not retracted** (review finding #2). Gating runs at request-submission, so it blocks the *next* completion but does not dismiss a ghost already on screen. This is latent: snooze and runtime per-app toggling have no control surface yet (A3). When they do, the snooze/exclude edge must call `engine.on_dismiss()` like the disable/secure edges already do.
- **A gate-dropped request leaves the engine's `requested` set** with no inbound completion (review finding #3). Benign: the next edit advances the snapshot and stales it; no ghost can show without a completion. Self-healing, documented for any future pending-generation throttle.

## Implemented since (deterministic, unit-tested + reviewed)

| Feature | Plan | §16 gate |
|---|---|---|
| **Multi-candidate + cycle** | ✅ `model_client::complete_n` N-sample (greedy + temp/top_k/top_p/seed); `engine_core` `CompletionReadyMulti`/`Cycle` + candidate list; Down-arrow cycle key; accept inserts shown; AcceptWord collapses to active | ✅ N candidates generated; cycle switches; accept inserts shown |
| **Previous-input context** | ✅ `context::build_context_block` (bounded, newline-collapsed, opt-in); `app` `PreviousInputs` per-app ring (redacted, deduped) recorded on Full-accept; worker prepends the block | ✅ previous-input augments prompt when on; off by default; per-app scoped (cross-app is a separate opt-in we don't clone). **Clipboard half:** assembler supports a pasteboard source; the live adapter pasteboard read wiring is the residual. |
| **Compatibility tiers** | ✅ `compat::compatibility_tier(bundle_id)` → Works/SetupNeeded/MirrorOnly/Partial/SidebarOnly/Unsupported/Unknown; run loop gates out `Unsupported` apps | ◑ deterministic classifier + unsupported-gating done; **per-app live behavior verification** (each app behaves as its tier claims) is environment-bound. |

## Remaining A2 — GUI / permission / live-bound (specified; validation environment-bound)

All of these are **implemented** to the project's build-verified+live standard (real compiling code, pure cores unit-tested, FFI build-verified) with a scripted live gate (`tools/acceptance/run-a2-compat-gates.sh`). What remains is **live validation on a GUI session** (mirroring §15 G7 / Task 5c live residuals) — not unimplemented code.

| Feature | What's implemented | Live-validation residual |
|---|---|---|
| Screen Recording / OCR context | ✅ `platform_macos::screen_recording_permission`/`request_screen_recording_permission` (CGPreflight/Request) + **`screen_context_text`: main-display capture (CGDisplayCreateImage) → local Vision OCR (`VNRecognizeTextRequest`)**, redacted + bounded, wired as a `WorkerContext.screen` source; `COMPLETE_ME_SCREEN_CONTEXT` opt-in, off by default, degrades to field-only when ungranted. | live OCR quality/perf tuning on a granted desktop. |
| Google Docs / Arc setup onboarding | ✅ `compat::needs_accessibility_setup` (browser/Arc/Dia + unreadable field; tested) wired on the read-context error path — surfaces setup guidance once per app (the Google-Docs-in-Chrome case). | live Docs round-trip; domain-precise trigger when browser-domain extraction lands. |
| Browser mirror-window fallback | ✅ `Engine::set_mirror_mode` — MirrorOnly apps (Firefox/Zen) render the ghost in the floating non-activating mirror window (front-app popup anchor) instead of inline; run loop sets it per focused app's tier; engine test pins it. | live Firefox/Zen confirmation. |
| Terminal/iTerm AI-agent activation | ✅ `compat::terminal_prompt_activates` (sigil-aware; tested) gates terminals to natural-language prompts before submit. | live tuning vs real agent prompts. |
| Clipboard context | ✅ `read_pasteboard_text` + run-loop refresh (redacted) into `WorkerContext.clipboard`; `COMPLETE_ME_CLIPBOARD_CONTEXT` opt-in; worker test. | — |
| Compatibility matrix gating | ✅ `compat::compatibility_tier` + unsupported-gating + onboarding; `run-a2-compat-gates.sh` exercises works/unsupported/terminal/clipboard. | per-app live confirmation across the matrix (script-driven). |

## Testing strategy
Every pure feature is unit-tested (RED→GREEN); FFI is build-verified and exercised by acceptance scripts on a GUI session (the project's standard for AppKit/AX/CGEvent code — the overlay, tray, AX worker, accept tap, and now Vision OCR + screen capture are all built and clippy-clean, validated live). `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` stay green, and each feature lands with a code review whose findings are fixed before commit.
