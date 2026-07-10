# A2 Parity Features — Design & Status

> A3 settings/tray UI plan (Cotypist-reference, pane-by-pane): `2026-06-10-a3-settings-ui-design.md`.

**Date:** 2026-06-09
**Status:** The implemented A2 slice is deterministic/build-verified, but the
broader committed parity scope is not wholly code-complete. Pure shipped features
are unit-tested and reviewed; GUI/permission-bound features retain live LOOK
residuals, while the SidebarOnly field detector, full statistical autocorrect,
cross-app previous-input mode, and thesaurus selection trigger remain code gaps
tracked in `docs/ROADMAP.md`.
**Release boundary:** the published v0.1.4 workflow still ran the A2 runner and
matrix-ledger checks. The local/manual-only policy below landed afterward in
`618013d` on `main` and applies to the next release, not the v0.1.4 artifact.
**Live pending status (re-verified 2026-07-10):** see
[`docs/ROADMAP.md`](../../ROADMAP.md). The personalization/settings surface is
code-complete for global instructions, sender identity, strength, Apps policy
rows, and config-backed per-app/per-domain steering: `field.app` is canonicalized
to a real bundle id before inference, `build_personalization` populates
per-app/per-domain instruction maps from config keys, and inference passes
`request.domain` into `build_preamble`. Remaining LOOK work is visual/physical
validation; A2 compatibility evidence is local/manual-only, and a GUI editor for
per-app/per-domain instruction text remains a future A3 enhancement, not runtime
instruction steering.
**Scope:** A2 from the roadmap (`2026-06-03-engine-macos-mvp-design.md` §9) — prompt-based personalization, per-app/per-domain gating, encrypted local memory, context augmentation, multi-candidate, compatibility surfaces. Acceptance gates are in design spec §16.

## Implemented this cycle (tested + reviewed)

| Feature | Crate(s) | Tests | §16 gate status |
|---|---|---|---|
| **Custom instructions (global/app/domain)** prompt steering | `personalization` + `app` wiring | 17 + integration | Deterministic runtime steering is wired: global instructions, per-app maps, and per-domain maps can all be populated from config and are prepended per request. The global instructions editor is shipped; only per-app/per-domain instruction editors remain a future UI enhancement. |
| **6-stop strength slider, full reach (no caps)** | `personalization` | ✓ (pairwise-distinct stops) | Six distinct stops, no tier caps, Settings persistence, and UI interaction are implemented. The remaining LOOK is a visible before/after completion-steering effect. |
| **Sender identity** in prompt | `personalization` | ✓ | Name/email feed the preamble and the Settings controls persist. The remaining Personalization LOOK is visible completion steering, not missing sender UI. |
| **Per-app enable/exclude + pause/snooze** | `prefs` + `app` gate | 8 + 2 builder + resolve | Partial: per-app exclude gates live (keyed on resolved bundle id); **snooze now triggerable + visible** (tray "Snooze for 1 hour" item, monotonic-clock session-only, CM💤/status-line overlay while Ready; snooze edge dismisses a visible ghost). **Snooze validated live 2026-06-10** (trigger log line, snoozed=true render, typed `:smile` while snoozed → decision=None throughout, zero suggestions). The Apps-pane On control now provides the runtime editing surface; residuals are physical toggle/behavior LOOK and ghost-dismiss-on-snooze live repro (unit-covered). |
| **Per-app Tab disable** primitive | `prefs::tab_disabled` + `app` focus wiring + macOS tap suppression | ✓ | Logic, Apps-pane Tab control, persistence, and tap-layer consumption are wired: focus resolves prefs and suppresses the literal Tab hotkey before the next arm cycle. Residual: physical toggle/behavior LOOK and per-app live matrix validation, not core tap consumption. |
| **PII redaction** (pre-persistence) | `redaction` | 14 | Foundation for encrypted memory + diagnostics; emails/Luhn-cards/secrets scrubbed. |
| **Encrypted local memory** (inspect/delete) | `memory` + `app` wiring | 11 + 24 app | Partial/core: AES-256-GCM (app bound as AAD), ciphertext-only-on-disk assertions, redact-on-insert, opt-in `StorageMode` semantics, `count`/`recent`/`delete_all`/`delete_app`, and `secure_delete` are covered. **Run-loop recording now wired**: `app` opens the store from `COMPME_MEMORY` (off/accepted/all, default off) + `COMPME_MEMORY_PATH`/`_KEY`; AcceptedOnly records Full-accepts under the resolved bundle id, and AllMonitored additionally records redactable typed runs assembled from established inserted-text deltas after a prior baseline while honoring collection-off, volatile `pid:N`, secure-input, disabled, snooze, app/domain exclude, browser-domain freshness, and compatibility gates; fail-closed when key/path missing. **Keychain-backed key SHIPPED** (`platform_macos::keychain::KeychainKeyStore`, generate-on-first-use via Security framework + `getentropy`, fail-closed load-or-create; `COMPME_MEMORY_KEY` env stays the operator override). **AcceptedOnly live keychain + on-disk validation COMPLETE 2026-06-10** and AllMonitored privacy paths now have TextEdit product-loop, runtime-disable, Chrome domain-exclude, and secure-field fail-closed evidence; AllMonitored records only established inserted-text deltas after a baseline, never pre-existing field text. §16 residual: snoozed transition, volatile `pid:N`, deferred global `delete_all` / memory-mode UI, and a decrypt-readback spot-check if ever doubted. |

### Steering preamble injection
`app/inference.rs` computes `PersonalizationProfile::build_preamble(app, domain)` per request and prepends it to the shaped prompt; `app/run_loop.rs` builds the profile + prefs from config keys (`COMPME_INSTRUCTIONS`, `COMPME_INSTRUCTIONS_APPS` / `_APP_*`, `COMPME_INSTRUCTIONS_DOMAINS` / `_DOMAIN_*`, `COMPME_STRENGTH`, `COMPME_SENDER_NAME/EMAIL`, `COMPME_EXCLUDED_APPS`, `COMPME_DEFAULT_ENABLED`).

## Documented limitations (from code review — deliberate, not bugs)

- **Domain gating is WIRED end-to-end**: the run loop threads `cached_domain` through both gate call sites (c129 slice 1), and the AX browser-domain source landed (c131 slices 2-3 **[2026-06-12]**: Focus-arm read, `AXDocument` → bounded `AXWebArea`/`AXURL` BFS, is_browser pre-gated, host-only privacy boundary, fail-open). Residual: live LOOK validation per the c128 design's 9-item checklist.
- **Per-app/per-domain personalization maps** are now config-wired and key against the resolved bundle id/domain at inference time. A3 settings still need editor controls for these values; until then they are configured through `COMPME_INSTRUCTIONS_APPS` / `_APP_*` and `COMPME_INSTRUCTIONS_DOMAINS` / `_DOMAIN_*`.
- **Already-visible ghost on a mid-session pref change** (review finding #2). Gating runs at request-submission, so preference edges that change the active policy must also dismiss any visible ghost. **Handled edges:** tray snooze, confirmed deep-link app/domain overrides (signed or unsigned reversible), and tray per-app disable/input-collection toggles all clear pending monitored state/latest request and call `engine.on_dismiss()`. Future preference surfaces must keep that dismiss edge when they mutate active app/domain policy.
- **A gate-dropped request leaves the engine's `requested` set** with no inbound completion (review finding #3). Benign: the next edit advances the snapshot and stales it; no ghost can show without a completion. Self-healing, documented for any future pending-generation throttle.

## Implemented since (deterministic, unit-tested + reviewed)

| Feature | Plan | §16 gate |
|---|---|---|
| **Multi-candidate + cycle** | ✅ `model_client::complete_n` N-sample (greedy + temp/top_k/top_p/seed); `engine_core` `CompletionReadyMulti`/`Cycle` + candidate list; Down-arrow cycle key; accept inserts shown; AcceptWord collapses to active; public `Engine` behavior now has cycle/wrap/accept tests | Deterministic engine/model coverage done. The rebuilt scripted Carbon gate covers Down-cycle dispatch; physical Down-cycle remains UX confirmation. |
| **Previous-input context** | ✅ `context::build_context_block` (bounded, newline-collapsed, opt-in); `app` `PreviousInputs` per-app ring (redacted, deduped) recorded on Full-accept under the resolved bundle id (not volatile `pid:N`); worker prepends the block; app tests pin same-app bundle scoping | Same-app prompt augmentation is deterministic and off by default; §16 live evidence still needs accepted-completion recording through the product loop. The separately promised opt-in **cross-app** mode is not implemented. **Clipboard context is implemented separately** via `read_pasteboard_text` + run-loop refresh. |
| **Compatibility tiers** | ✅ classifier for Works/SetupNeeded/MirrorOnly/Partial/SidebarOnly/Unsupported/Unknown; Unsupported fails closed. ⚠️ SidebarOnly currently fails closed for every field until an AI-chat/editor detector exists. | Classifier/gating tests are done, but the SidebarOnly detector is a code prerequisite, not a live-only residual. Per-app live verification follows after it lands. |
| **British English normalization** (Cotypist 0.22 Labs) | ✅ pure crate `localize` plus host integration: curated US→UK spelling map keyed only on US-only forms (shared spellings untouched), query-case reapplied via shared `crates/textcase::CasePattern`, default **off** via `COMPME_BRITISH_ENGLISH`; replacement offer reaches the shared AxSet accept path. | ✅ §16 live gate passed 2026-06-10 (`color`→`colour`, docs/ACCEPTANCE.md). Non-atomic SyntheticKeys/Clipboard replacements remain deliberately fail-closed pending compatibility evidence; no backspace-synthesis implementation is promised. |
| **Trailing space after single-word completions** (Cotypist Shortcuts toggle) | ✅ **wired** end-to-end: `engine_core` self-gating `append_single_word_space` applied at AcceptFull/AcceptWord/preview behind `SuggestionMachine::with_trailing_space`; `engine` passthrough; `app` reads `COMPME_TRAILING_SPACE` (default **off** → byte-identical accept) and chains it onto the engine. Preview mirrors the inserted bytes so echo-absorption stays consistent. Unit + integration + config tests. | Deterministic accept-path coverage done; off by default. The A1b TextEdit product gate now covers exact inserted text through `e2e-compme-trailing-space` with deterministic `COMPME_E2E_ACCEPT=word-only`; real-model E2E intentionally rejects `word-only` and must use `full` or `word`. |

## Remaining A2 code

- SidebarOnly editor-vs-sidebar detection.
- Full statistical autocorrect distinct from the curated typo-fix table, including
  its code-field gate and separate control semantics.
- Opt-in, privacy-bounded cross-app previous-input context.
- Selection-triggered thesaurus UX (trailing-word lookup already ships).

## Remaining A2 — GUI / permission / live-bound (specified; validation environment-bound)

These rows are implemented to a deterministic/build-verified standard except for
the explicitly linked SidebarOnly detector prerequisite: real compiling code,
pure cores unit-tested, and FFI surfaces build-verified. The scripted live
gate (`tools/acceptance/run-a2-compat-gates.sh`) is request-path smoke evidence
for selected scenarios, not full §16 acceptance. What remains is live validation
on a GUI session (mirroring §15 G7 / Task 5c live residuals) and, for settings
features, a persisted UI/control surface.

| Feature | What's implemented | Live-validation residual |
|---|---|---|
| Screen Recording / OCR context | ✅ `platform_macos::screen_recording_permission`/`request_screen_recording_permission` (CGPreflight/Request) + **`screen_context_text`: capture the display containing the caret (fallback main display) → local Vision OCR (`VNRecognizeTextRequest`)**, redacted + bounded, published into a field-tagged `WorkerContext.screen` cell; `COMPME_SCREEN_CONTEXT` opt-in, off by default, degrades to field-only when ungranted. **OCR runs on a dedicated `screen_ocr::ScreenOcr` worker thread** with a bounded latest-slot queue (new requests overwrite stale pending work) so the ~200–800 ms Vision pass never stalls the AppKit run loop / overlay / Carbon accept callbacks (§11 latency floor), and inference rejects stale OCR output when its field no longer matches the completion request. | live OCR quality/perf tuning on a granted desktop, plus multi-display caret-display confirmation. |
| Google Docs / Arc setup onboarding | ✅ `compat::needs_accessibility_setup` (browser/Arc/Dia + unreadable field; tested) wired on the read-context error path — surfaces setup guidance once per app (the Google-Docs-in-Chrome case). Browser-domain extraction has landed separately. | live `setup-needed-docs-arc-onboarding` manual gate in Arc/Docs. |
| Browser mirror-window fallback | ✅ `Engine::set_mirror_mode` — MirrorOnly apps (Firefox/Zen) render the ghost in the floating non-activating mirror window (front-app popup anchor) instead of inline; run loop sets it per focused app's tier; engine test pins it. | live Firefox/Zen confirmation. |
| Terminal/iTerm AI-agent activation | ✅ `compat::terminal_prompt_activates` (sigil-aware; tested) gates terminals to natural-language prompts before submit. | live tuning vs real agent prompts. |
| Clipboard context | ✅ `read_pasteboard_text` + run-loop refresh (redacted) into `WorkerContext.clipboard`; `COMPME_CLIPBOARD_CONTEXT` opt-in; `COMPME_DIAG_CONTEXT=1` gate proves a marker reaches the submit path. | — |
| Compatibility matrix gating | ✅ classifier, Unsupported fail-closed behavior, onboarding, and browser-domain allow/exclude smoke paths. ⚠️ SidebarOnly is blocked wholesale until the editor-vs-sidebar detector lands. A local/manual pass can write a 13-row ledger under `tools/acceptance/evidence/a2/`; CI/tag workflows do not consume it. | implement the SidebarOnly detector first, then perform per-app live confirmation and pass every manually recorded ledger row through `tools/release/check-a2-matrix-ledger.sh` locally. |

## Testing strategy
Every pure feature is unit-tested (RED→GREEN). FFI is build-verified, and
acceptance scripts provide GUI smoke evidence where synthetic automation is
valid. Current Carbon accept consumption, app-family compatibility, onboarding,
mirror rendering, insertion behavior, and settings persistence require explicit
live/manual evidence before marking the matching §16 gates closed. Keep
`cargo fmt --all -- --check`,
`cargo clippy --locked --workspace --all-targets -- -D warnings`,
`cargo test --locked --workspace --all-targets -- --test-threads=1`, and
`cargo build --locked --workspace --all-targets` green. The canonical automated
release gate list lives in `docs/ACCEPTANCE.md` and `docs/RELEASING.md`; it
includes model-backed release gates, bundle/release helper self-tests,
agent-brief alignment, and privacy-policy checks. A2 validation is a separate
local/manual pre-release activity: CI, tag releases, and the release-policy
checker deliberately exclude both `run-a2-compat-gates.sh` and
`check-a2-matrix-ledger.sh` from execution and generic syntax validation.

## Parity notes — evidence boundaries
Candidate/suggestion **cycling** is an intentional compme superset: Cotypist's
public tips omit a next-suggestion shortcut. Thesaurus provenance is different:
the public help surface does not advertise it, but the installed-binary audit
exposed `featureThesaurus{AutoMode,SelectionMode}` and a Labs thesaurus surface.
Compme's trailing-word lookup ships; selection-trigger UX remains committed work.
Do not classify the entire thesaurus surface as either a completed superset or a
closed parity requirement without preserving that public-vs-binary distinction.

## Parity re-check vs Cotypist 0.22 "Cotypist Labs" (2026-06-09)
A fresh re-check against Cotypist's 0.22 "Cotypist Labs" release **supersedes the
prior "pure §16 features exhausted" conclusion**: the 0.22 Labs headlines are
British English, RTL, multilingual, and mid-line completion
(source: cotypist.app + its Labs/changelog). Of these:

- **British English normalization** was a freshly-surfaced *pure* gap and was
  closed by
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

The local trailing-token replacement slice is implemented. **Emoji + curated
typo autocorrect + British-English (`localize`) are
WIRED and LIVE-VALIDATED** through the `replace_left` replacement pipeline
(run_loop detection → `offer_replacement` → `Command::Replace` → AxSet honoring),
default-off, gated, race-free; **the live §16 accept gate (step 6) PASSED
2026-06-10** (physical Tab accept with deletion in TextEdit — ACCEPTANCE.md, A2
Local-Replacement Live Gate). **Thesaurus is also wired** through the same
default-off local replacement path (`COMPME_THESAURUS`), with deterministic
offer coverage in the app run-loop tests; remaining thesaurus work is live LOOK
validation plus the committed selection-triggered UX, not trailing-word core wiring.
**Webconfig is wired** for signed and unsigned reversible `compme://` URL
reception and draining in the run loop. Signed links verify only when a trusted
key is configured; every link requires user confirmation before applying.
Remaining work is live browser/setup validation, not URL-event plumbing. Full
resolved design — `replace_left`
shape, `Showing.replace_left` model, `offer_replacement` entry point,
offer-vs-model priority, `insert_replacing` adapter contract, AxSet honoring,
SyntheticKeys residual, build order, default-off flags — is in
[`2026-06-09-integration-phase-design.md`](2026-06-09-integration-phase-design.md).
