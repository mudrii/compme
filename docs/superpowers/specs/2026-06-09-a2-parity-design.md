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

### Steering preamble injection
`app/inference.rs` computes `PersonalizationProfile::build_preamble(app, None)` per request and prepends it to the shaped prompt; `app/run_loop.rs` builds the profile + prefs from config keys (`COMPLETE_ME_INSTRUCTIONS`, `COMPLETE_ME_STRENGTH`, `COMPLETE_ME_SENDER_NAME/EMAIL`, `COMPLETE_ME_EXCLUDED_APPS`, `COMPLETE_ME_DEFAULT_ENABLED`).

## Documented limitations (from code review — deliberate, not bugs)

- **Domain gating is `None`** until browser-domain extraction lands (a later A2/A3 browser feature). Per-domain exclude/instructions are crate-tested but the run loop passes `domain = None`.
- **Per-app *personalization* maps** are not yet config-wired (only global instructions are). When A3 settings add them, the inference worker must key on the resolved **bundle id** (via `bundle_id_for_pid`), not `field.app` (`pid:N`) — the same fix already applied to the prefs gate.
- **Already-visible ghost on a mid-session pref change / snooze is not retracted** (review finding #2). Gating runs at request-submission, so it blocks the *next* completion but does not dismiss a ghost already on screen. This is latent: snooze and runtime per-app toggling have no control surface yet (A3). When they do, the snooze/exclude edge must call `engine.on_dismiss()` like the disable/secure edges already do.
- **A gate-dropped request leaves the engine's `requested` set** with no inbound completion (review finding #3). Benign: the next edit advances the snapshot and stales it; no ghost can show without a completion. Self-healing, documented for any future pending-generation throttle.

## Remaining A2 — pure/implementable (deterministic, unit-testable)

| Feature | Plan | §16 gate |
|---|---|---|
| **Multi-candidate + cycle** | `model_client` N-sample (temp/seed variation, shared-prefix decode); `engine_core` holds candidates + a `Cycle` event; accept inserts the shown one | N candidates generated; cycle switches; accept inserts shown |
| **Encrypted local memory** | `memory` crate: rusqlite store of accepted completions, **redacted on insert** (`redaction`), AEAD-encrypted values behind a `KeyProvider` trait (Keychain impl live, in-memory for tests); inspect/count/delete-all/delete-per-app | DB encrypted at rest; key in OS keystore; inspect + delete; accepted-only vs all-monitored modes (default off) |
| **Pasteboard / previous-input context** | `context` augmentation + adapter pasteboard read (already present as fallback); opt-in; bounded; redacted | clipboard/previous-input augments prompt when enabled; off by default |

## Remaining A2 — GUI / permission / live-bound (specified; validation environment-bound)

These cannot be fully validated headlessly (need a console GUI session, TCC permissions, or specific apps). Each carries its §16 acceptance gate; mark "parity" only when the gate passes live, mirroring §15 G7 / Task 5c live residuals.

| Feature | §16 gate | Why environment-bound |
|---|---|---|
| Screen Recording / OCR context | opt-in behind Screen Recording permission; local OCR only; works without it | needs the Screen Recording TCC grant + ScreenCaptureKit/Vision live |
| Google Docs Accessibility setup | onboarding detects missing AX/Text-Metrics, guides; verified Docs round-trip | needs Chrome + a Google Doc, live |
| Browser mirror-window fallback (Firefox/Zen) | mirror renders + accepts; documented UX | needs Firefox/Zen live |
| Terminal/iTerm AI-agent prompt activation | activates only in NL prompt contexts, not arbitrary shell | needs Terminal/iTerm + heuristic tuning live |
| Compatibility matrix (Works/Setup/Mirror/Partial/Sidebar/Unsupported) | inline+accept verified per app family; tiers explicit | needs each representative app live |

## Testing strategy
Every pure feature is unit-tested (RED→GREEN), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` stay green, and each lands with a code review whose findings are fixed before commit. GUI/permission-bound features are specified with executable/manual gates recorded in acceptance logs when run on a console session.
