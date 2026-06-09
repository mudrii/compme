# A2 Parity Features — Design & Status

**Date:** 2026-06-09
**Status:** In progress. Pure/deterministic features implemented + unit-tested + reviewed; GUI/permission-bound features specified with their §16 acceptance gates (environment-bound, like §15 G7 / Task 5c live residuals).
**Scope:** A2 from the roadmap (`2026-06-03-engine-macos-mvp-design.md` §9) — prompt-based personalization, per-app/per-domain gating, encrypted local memory, context augmentation, multi-candidate, compatibility surfaces. Acceptance gates are in design spec §16.

## Implemented this cycle (tested + reviewed)

| Feature | Crate(s) | Tests | §16 gate status |
|---|---|---|---|
| **Custom instructions (global)** prompt steering | `personalization` + `app` wiring | 14 + integration | Partial: global steers completions live (preamble prepended per request). Per-app/per-domain *maps* are crate-tested but not yet config-wired (A3 settings). |
| **6-stop strength slider, full reach (no caps)** | `personalization` | ✓ (pairwise-distinct stops) | Partial/deterministic: six distinct stops and no tier caps in the pure profile. §16 still needs settings persistence/UI evidence plus live before/after completion steering at multiple stops. |
| **Sender identity** in prompt | `personalization` | ✓ | Partial/deterministic: name/email feed the preamble; editable settings/live prompt evidence remains with the A3 settings surface. |
| **Per-app enable/exclude + pause/snooze** | `prefs` + `app` gate | 8 + 2 builder + resolve | Partial: per-app exclude gates live (keyed on resolved bundle id); snooze logic done but not yet UI/signal-triggerable (A3 control surface). |
| **Per-app Tab disable** primitive | `prefs::tab_disabled` | ✓ | Logic present; tap-layer consumption of the per-app flag is A3 (needs the accept tap to read prefs). |
| **PII redaction** (pre-persistence) | `redaction` | 14 | Foundation for encrypted memory + diagnostics; emails/Luhn-cards/secrets scrubbed. |
| **Encrypted local memory** (inspect/delete) | `memory` + `app` wiring | 11 + 4 app | Partial/core: AES-256-GCM (app bound as AAD), ciphertext-only-on-disk assertions, redact-on-insert, opt-in `StorageMode` semantics, `count`/`recent`/`delete_all`/`delete_app`, and `secure_delete` are covered. **Run-loop recording now wired**: `app` opens the store from `COMPLETE_ME_MEMORY` (off/accepted/all, default off) + `COMPLETE_ME_MEMORY_PATH`/`_KEY`, and records Full-accepts under the resolved bundle id (skips volatile `pid:N`); fail-closed when key/path missing. §16 remains open for the **Keychain-backed `KeyProvider`** (key is env-supplied `StaticKey` until A3), inspect/delete settings UI, and live file inspection. |

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
| **Multi-candidate + cycle** | ✅ `model_client::complete_n` N-sample (greedy + temp/top_k/top_p/seed); `engine_core` `CompletionReadyMulti`/`Cycle` + candidate list; Down-arrow cycle key; accept inserts shown; AcceptWord collapses to active; public `Engine` behavior now has cycle/wrap/accept tests | Deterministic engine/model coverage done. §16 live product evidence still needs the physical Down-cycle + accept path after the Carbon re-close. |
| **Previous-input context** | ✅ `context::build_context_block` (bounded, newline-collapsed, opt-in); `app` `PreviousInputs` per-app ring (redacted, deduped) recorded on Full-accept under the resolved bundle id (not volatile `pid:N`); worker prepends the block; app tests pin same-app bundle scoping | Deterministic prompt augmentation done; off by default. §16 live evidence still needs accepted-completion recording through the product loop. **Clipboard context is implemented separately** via `read_pasteboard_text` + run-loop refresh. |
| **Compatibility tiers** | ✅ `compat::compatibility_tier(bundle_id)` → Works/SetupNeeded/MirrorOnly/Partial/SidebarOnly/Unsupported/Unknown; run loop gates out `Unsupported` and fail-closes `SidebarOnly` until an AI-chat/sidebar field detector exists | ◑ deterministic classifier + unsupported/sidebar gating done; **per-app live behavior verification** (each app behaves as its tier claims) is environment-bound. |

## Remaining A2 — GUI / permission / live-bound (specified; validation environment-bound)

These are implemented to a deterministic/build-verified standard: real compiling
code, pure cores unit-tested, and FFI surfaces build-verified. The scripted live
gate (`tools/acceptance/run-a2-compat-gates.sh`) is request-path smoke evidence
for selected scenarios, not full §16 acceptance. What remains is live validation
on a GUI session (mirroring §15 G7 / Task 5c live residuals) and, for settings
features, a persisted UI/control surface.

| Feature | What's implemented | Live-validation residual |
|---|---|---|
| Screen Recording / OCR context | ✅ `platform_macos::screen_recording_permission`/`request_screen_recording_permission` (CGPreflight/Request) + **`screen_context_text`: capture the display containing the caret (fallback main display) → local Vision OCR (`VNRecognizeTextRequest`)**, redacted + bounded, published into a `WorkerContext.screen` cell; `COMPLETE_ME_SCREEN_CONTEXT` opt-in, off by default, degrades to field-only when ungranted. **OCR runs on a dedicated `screen_ocr::ScreenOcr` worker thread** (coalescing, one-submit staleness) so the ~200–800 ms Vision pass never stalls the AppKit run loop / overlay / Carbon accept callbacks (§11 latency floor). | live OCR quality/perf tuning on a granted desktop, plus multi-display caret-display confirmation. |
| Google Docs / Arc setup onboarding | ✅ `compat::needs_accessibility_setup` (browser/Arc/Dia + unreadable field; tested) wired on the read-context error path — surfaces setup guidance once per app (the Google-Docs-in-Chrome case). | live Docs round-trip; domain-precise trigger when browser-domain extraction lands. |
| Browser mirror-window fallback | ✅ `Engine::set_mirror_mode` — MirrorOnly apps (Firefox/Zen) render the ghost in the floating non-activating mirror window (front-app popup anchor) instead of inline; run loop sets it per focused app's tier; engine test pins it. | live Firefox/Zen confirmation. |
| Terminal/iTerm AI-agent activation | ✅ `compat::terminal_prompt_activates` (sigil-aware; tested) gates terminals to natural-language prompts before submit. | live tuning vs real agent prompts. |
| Clipboard context | ✅ `read_pasteboard_text` + run-loop refresh (redacted) into `WorkerContext.clipboard`; `COMPLETE_ME_CLIPBOARD_CONTEXT` opt-in; `COMPLETE_ME_DIAG_CONTEXT=1` gate proves a marker reaches the submit path. | — |
| Compatibility matrix gating | ✅ `compat::compatibility_tier` + unsupported/sidebar gating + onboarding; `run-a2-compat-gates.sh` exercises works/unsupported/terminal/clipboard/screen. | per-app live confirmation across the matrix (script-driven). |

## Testing strategy
Every pure feature is unit-tested (RED→GREEN). FFI is build-verified, and
acceptance scripts provide GUI smoke evidence where synthetic automation is
valid. Current Carbon accept consumption, app-family compatibility, onboarding,
mirror rendering, insertion behavior, and settings persistence require explicit
live/manual evidence before marking the matching §16 gates closed. `cargo
clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` stay
green, and each feature lands with a code review whose findings are fixed before
commit.
