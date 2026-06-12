# A2 Parity Features — Design & Status

> A3 settings/tray UI plan (Cotypist-reference, pane-by-pane): `2026-06-10-a3-settings-ui-design.md`.

**Date:** 2026-06-09
**Status:** In progress. Pure/deterministic features implemented + unit-tested + reviewed; GUI/permission-bound features specified with their §16 acceptance gates (environment-bound, like §15 G7 / Task 5c live residuals).
**Scope:** A2 from the roadmap (`2026-06-03-engine-macos-mvp-design.md` §9) — prompt-based personalization, per-app/per-domain gating, encrypted local memory, context augmentation, multi-candidate, compatibility surfaces. Acceptance gates are in design spec §16.

## Implemented this cycle (tested + reviewed)

| Feature | Crate(s) | Tests | §16 gate status |
|---|---|---|---|
| **Custom instructions (global)** prompt steering | `personalization` + `app` wiring | 17 + integration | Partial: global steers completions live (preamble prepended per request). Per-app/per-domain *maps* are crate-tested but not yet config-wired (A3 settings). |
| **6-stop strength slider, full reach (no caps)** | `personalization` | ✓ (pairwise-distinct stops) | Partial/deterministic: six distinct stops and no tier caps in the pure profile. §16 still needs settings persistence/UI evidence plus live before/after completion steering at multiple stops. |
| **Sender identity** in prompt | `personalization` | ✓ | Partial/deterministic: name/email feed the preamble; editable settings/live prompt evidence remains with the A3 settings surface. |
| **Per-app enable/exclude + pause/snooze** | `prefs` + `app` gate | 8 + 2 builder + resolve | Partial: per-app exclude gates live (keyed on resolved bundle id); **snooze now triggerable + visible** (tray "Snooze for 1 hour" item, monotonic-clock session-only, CMð¤/status-line overlay while Ready; snooze edge dismisses a visible ghost). **Snooze validated live 2026-06-10** (trigger log line, snoozed=true render, typed `:smile` while snoozed → decision=None throughout, zero suggestions). Residual: duration submenu, runtime per-app exclude editing surface, ghost-dismiss-on-snooze live repro (unit-covered). |
| **Per-app Tab disable** primitive | `prefs::tab_disabled` | ✓ | Logic present; tap-layer consumption of the per-app flag is A3 (needs the accept tap to read prefs). |
| **PII redaction** (pre-persistence) | `redaction` | 14 | Foundation for encrypted memory + diagnostics; emails/Luhn-cards/secrets scrubbed. |
| **Encrypted local memory** (inspect/delete) | `memory` + `app` wiring | 11 + 4 app | Partial/core: AES-256-GCM (app bound as AAD), ciphertext-only-on-disk assertions, redact-on-insert, opt-in `StorageMode` semantics, `count`/`recent`/`delete_all`/`delete_app`, and `secure_delete` are covered. **Run-loop recording now wired**: `app` opens the store from `COMPME_MEMORY` (off/accepted/all, default off) + `COMPME_MEMORY_PATH`/`_KEY`, and records Full-accepts under the resolved bundle id (skips volatile `pid:N`); fail-closed when key/path missing. **Keychain-backed key SHIPPED** (`platform_macos::keychain::KeychainKeyStore`, generate-on-first-use via Security framework + `getentropy`, fail-closed load-or-create; `COMPME_MEMORY_KEY` env stays the operator override). **Live keychain + on-disk validation COMPLETE 2026-06-10**: with `COMPME_MEMORY=accepted` + `_PATH` and NO `_KEY`, the `com.compme.memory` login-keychain entry was created on first use and REUSED across three runs (single `genp` entry; store reopened over existing encrypted records each time); two Full-accept records landed (com.apple.TextEdit, 69/72-byte blobs) and `strings` over the db shows NO typed plaintext — the ciphertext-only-on-disk gate holds live, non-vacuously. §16 residual: inspect/delete settings UI (and a decrypt-readback spot-check if ever doubted). |

### Steering preamble injection
`app/inference.rs` computes `PersonalizationProfile::build_preamble(app, None)` per request and prepends it to the shaped prompt; `app/run_loop.rs` builds the profile + prefs from config keys (`COMPME_INSTRUCTIONS`, `COMPME_STRENGTH`, `COMPME_SENDER_NAME/EMAIL`, `COMPME_EXCLUDED_APPS`, `COMPME_DEFAULT_ENABLED`).

## Documented limitations (from code review — deliberate, not bugs)

- **Domain gating is still inert at runtime**: the run loop threads `cached_domain` through both gate call sites (c129 slice 1 **[2026-06-12]**), but the cache stays `None` until the AX browser-domain source lands (slices 2-3). Per-domain exclude/instructions are crate-tested and now consumable end-to-end.
- **Per-app *personalization* maps** are not yet config-wired (only global instructions are). When A3 settings add them, the inference worker must key on the resolved **bundle id** (via `bundle_id_for_pid`), not `field.app` (`pid:N`) — the same fix already applied to the prefs gate.
- **Already-visible ghost on a mid-session pref change** (review finding #2). Gating runs at request-submission, so it blocks the *next* completion but does not dismiss a ghost already on screen. **The snooze edge is now handled** (the tray snooze calls `engine.on_dismiss()` + clears the pending request, like the disable/secure edges). Still latent for runtime per-app exclude changes, which have no control surface yet (A3) — when one lands, its edge must do the same.
- **A gate-dropped request leaves the engine's `requested` set** with no inbound completion (review finding #3). Benign: the next edit advances the snapshot and stales it; no ghost can show without a completion. Self-healing, documented for any future pending-generation throttle.

## Implemented since (deterministic, unit-tested + reviewed)

| Feature | Plan | §16 gate |
|---|---|---|
| **Multi-candidate + cycle** | ✅ `model_client::complete_n` N-sample (greedy + temp/top_k/top_p/seed); `engine_core` `CompletionReadyMulti`/`Cycle` + candidate list; Down-arrow cycle key; accept inserts shown; AcceptWord collapses to active; public `Engine` behavior now has cycle/wrap/accept tests | Deterministic engine/model coverage done. §16 live product evidence still needs the physical Down-cycle + accept path after the Carbon re-close. |
| **Previous-input context** | ✅ `context::build_context_block` (bounded, newline-collapsed, opt-in); `app` `PreviousInputs` per-app ring (redacted, deduped) recorded on Full-accept under the resolved bundle id (not volatile `pid:N`); worker prepends the block; app tests pin same-app bundle scoping | Deterministic prompt augmentation done; off by default. §16 live evidence still needs accepted-completion recording through the product loop. **Clipboard context is implemented separately** via `read_pasteboard_text` + run-loop refresh. |
| **Compatibility tiers** | ✅ `compat::compatibility_tier(bundle_id)` → Works/SetupNeeded/MirrorOnly/Partial/SidebarOnly/Unsupported/Unknown; run loop gates out `Unsupported` and fail-closes `SidebarOnly` until an AI-chat/sidebar field detector exists | ◑ deterministic classifier + unsupported/sidebar gating done; **per-app live behavior verification** (each app behaves as its tier claims) is environment-bound. |
| **British English normalization** (Cotypist 0.22 Labs) | ✅ new pure crate `localize`: curated US→UK spelling map keyed only on US-only forms (shared spellings untouched — no false positives), query-case reapplied via shared `crates/textcase::CasePattern` (lower/Title/UPPER), mirrors the `autocorrect`/`thesaurus` shape; default **off** via `COMPME_BRITISH_ENGLISH` | ◑ pure crate + unit tests done; §16 live product evidence (toggle steers inserted completions toward UK spelling end-to-end) is the FFI residual — host integration like `autocorrect`/`thesaurus`. **[2026-06-11] STALE — host integration shipped (`COMPME_BRITISH_ENGLISH` read + replacement offer) and the §16 live gate passed 2026-06-10 (color→colour accept, docs/ACCEPTANCE.md).** |
| **Trailing space after single-word completions** (Cotypist Shortcuts toggle) | ✅ **wired** end-to-end: `engine_core` self-gating `append_single_word_space` applied at AcceptFull/AcceptWord/preview behind `SuggestionMachine::with_trailing_space`; `engine` passthrough; `app` reads `COMPME_TRAILING_SPACE` (default **off** → byte-identical accept) and chains it onto the engine. Preview mirrors the inserted bytes so echo-absorption stays consistent. Unit + integration + config tests. | Deterministic accept-path coverage done; off by default. §16 live evidence is the physical-key accept after the Carbon re-close (the toggle's effect on the inserted text). |

## Remaining A2 — GUI / permission / live-bound (specified; validation environment-bound)

These are implemented to a deterministic/build-verified standard: real compiling
code, pure cores unit-tested, and FFI surfaces build-verified. The scripted live
gate (`tools/acceptance/run-a2-compat-gates.sh`) is request-path smoke evidence
for selected scenarios, not full §16 acceptance. What remains is live validation
on a GUI session (mirroring §15 G7 / Task 5c live residuals) and, for settings
features, a persisted UI/control surface.

| Feature | What's implemented | Live-validation residual |
|---|---|---|
| Screen Recording / OCR context | ✅ `platform_macos::screen_recording_permission`/`request_screen_recording_permission` (CGPreflight/Request) + **`screen_context_text`: capture the display containing the caret (fallback main display) → local Vision OCR (`VNRecognizeTextRequest`)**, redacted + bounded, published into a `WorkerContext.screen` cell; `COMPME_SCREEN_CONTEXT` opt-in, off by default, degrades to field-only when ungranted. **OCR runs on a dedicated `screen_ocr::ScreenOcr` worker thread** (coalescing, one-submit staleness) so the ~200–800 ms Vision pass never stalls the AppKit run loop / overlay / Carbon accept callbacks (§11 latency floor). | live OCR quality/perf tuning on a granted desktop, plus multi-display caret-display confirmation. |
| Google Docs / Arc setup onboarding | ✅ `compat::needs_accessibility_setup` (browser/Arc/Dia + unreadable field; tested) wired on the read-context error path — surfaces setup guidance once per app (the Google-Docs-in-Chrome case). | live Docs round-trip; domain-precise trigger when browser-domain extraction lands. |
| Browser mirror-window fallback | ✅ `Engine::set_mirror_mode` — MirrorOnly apps (Firefox/Zen) render the ghost in the floating non-activating mirror window (front-app popup anchor) instead of inline; run loop sets it per focused app's tier; engine test pins it. | live Firefox/Zen confirmation. |
| Terminal/iTerm AI-agent activation | ✅ `compat::terminal_prompt_activates` (sigil-aware; tested) gates terminals to natural-language prompts before submit. | live tuning vs real agent prompts. |
| Clipboard context | ✅ `read_pasteboard_text` + run-loop refresh (redacted) into `WorkerContext.clipboard`; `COMPME_CLIPBOARD_CONTEXT` opt-in; `COMPME_DIAG_CONTEXT=1` gate proves a marker reaches the submit path. | — |
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

## Parity notes — compme supersets beyond Cotypist
Cotypist deliberately omits two things compme implements. (a) Candidate/suggestion
**cycling** — Cotypist's docs state this "removes the temptation to look for a
next-suggestion shortcut." (b) A **thesaurus/synonym** tool. compme implements
both: the Down-arrow multi-candidate cycle (`engine_core` `Cycle`) and
`crates/thesaurus`. These are **intentional supersets, not parity requirements**,
and should **not** be treated as parity gaps in future audits.
Source: cotypist.app/help/tips.

## Parity re-check vs Cotypist 0.22 "Cotypist Labs" (2026-06-09)
A fresh re-check against Cotypist's 0.22 "Cotypist Labs" release **supersedes the
prior "pure §16 features exhausted" conclusion**: the 0.22 Labs headlines are
British English, RTL, multilingual, and mid-line completion
(source: cotypist.app + its Labs/changelog). Of these:

- **British English normalization** was a freshly-surfaced *pure* gap — compme did
  not have it and it is fully pure-buildable. It is being closed **this cycle** by
  the new `localize` crate (US→UK spelling map + `textcase::CasePattern` case
  reapplication + `COMPME_BRITISH_ENGLISH` host toggle, default off), mirroring
  the existing `autocorrect`/`thesaurus` crates (see the row above). RTL/multilingual
  remain model/locale-bound, not pure-table features.
- **Mid-line completion** is **NOT a separate gap** — it is already a pure capability
  in compme. `engine_core::passes_trigger_gates` only suppresses mid-*word*, not
  mid-*line*: a caret at a word boundary with right-context already triggers, and
  `ranker::strip_suffix_overlap` dedupes the right side so the completion does not
  duplicate following text. No new work is required for it.

## Next phase — integration (design committed)

Pure parity is exhausted. **Emoji + autocorrect + British-English (`localize`) are
WIRED and LIVE-VALIDATED** through the `replace_left` replacement pipeline
(run_loop detection → `offer_replacement` → `Command::Replace` → AxSet honoring),
default-off, gated, race-free; **the live §16 accept gate (step 6) PASSED
2026-06-10** (physical Tab accept with deletion in TextEdit — ACCEPTANCE.md, A2
Local-Replacement Live Gate). The remaining
*unwired* cores are **`thesaurus`** (deliberate compme superset, selection-triggered
— a different trigger design) and **`webconfig`** (A3 URL-scheme reception +
signing). Full resolved design — `replace_left`
shape, `Showing.replace_left` model, `offer_replacement` entry point, offer-vs-model
priority, `insert_replacing` adapter contract, AxSet honoring, SyntheticKeys
residual, build order, default-off flags — is in
[`2026-06-09-integration-phase-design.md`](2026-06-09-integration-phase-design.md).
