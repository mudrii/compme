# compme — Roadmap & Pending Work

> **Last updated:** 2026-07-04 (plan/docs review after release-gate hardening) · **Branch:** `main` · **Tests:** full deterministic gates green on macOS (≈1655 workspace tests; spike separate)
>
> This document cross-references the plan specs in
> [`docs/superpowers/specs/`](superpowers/specs/) against the implemented code and
> records, in detail, what remains. It is the single source of truth for "what's
> pending" — kept in sync as items ship. Status claims here are evidence-backed
> with symbol/function anchors re-reviewed 2026-07-04.

## Status legend

| Symbol | Meaning |
|---|---|
| ✅ DONE | Implemented, tested, and (where applicable) live-validated |
| ◑ PARTIAL | Core/backing exists; a concrete piece is missing (detailed below) |
| ☐ PENDING | Not started |
| 🔬 LOOK | Code complete to a deterministic/build-verified standard; only live human/scripted GUI evidence remains |
| 🔒 BLOCKED | Needs an external resource (Apple Developer ID, new upstream release, user decision) |

The deterministic MVP (roadmap phases A0/A1a/A1b/A2/A3 *cores*) is **DONE and
tested**. Everything below is what the plan still calls for.

---

## Tier 1 — Largest committed deliverables

### 1.1 ◑🔒 Cross-platform adapters (Windows + Linux) — foundation shipped, real impls env-gated

**Plan:** `README.md:10` — *"macOS ships first; Windows and Linux are committed
deliverables built behind a shared cross-platform `PlatformAdapter` contract."*
The `platform` crate was deliberately shaped as a trait/contract to accept them.

**Foundation ✅ DONE (2026-06-16, gate-green on macOS):**
- **`crates/platform_windows`** (`1f8cace`) — implements every IO/subscribe
  method of the `platform::PlatformAdapter` contract as a **fail-closed stub**
  (the two optional anchor/URL methods take the trait's safe `Ok(None)`
  defaults, pinned by test): `environment()`
  reports Windows; every subscribe/IO method returns `PlatformError::UnsupportedField`
  (never panics, no partial state); each method is doc-commented with the Win32 API
  its real impl will use (UIA / `WH_KEYBOARD_LL` / `SendInput` / layered overlay).
  Unit-tested (environment, fail-closed `subscribe_focus` + `insert_replacing`).
- **`crates/platform_linux`** (`5236a56`) — the same, for Linux (AT-SPI2 / XTEST /
  `wtype` / IBus / X11-or-layer-shell overlay).
- **CI matrix** (`a7427c6`) — `windows-latest` + `ubuntu-latest` jobs run
  fmt/clippy/test/build scoped to each new crate (`-p platform_windows` /
  `-p platform_linux`), so the real per-OS code gets gated the moment it lands.
- Both crates are **inert** — nothing wires them into the app (still `platform_macos`),
  so the workspace builds + gates green on the macOS-only dev host.

**Pending (🔒 needs Windows + Linux build+test environments — not doable on macOS):**
- The actual **Windows** adapter behind `#[cfg(windows)]` (uncomment the `windows`
  dep in its `Cargo.toml`): UIA focus/caret/text + `WH_KEYBOARD_LL` accept tap +
  `SendInput`/ValuePattern insert + layered overlay.
- The actual **Linux** adapter behind `#[cfg(target_os = "linux")]`: AT-SPI2
  read/insert/events + XTEST/`wtype` synthetic keys (IBus IME fallback on Wayland)
  + override-redirect/layer-shell overlay. (AT-SPI device key-listeners are
  deprecated → prefer XTEST/XGrabKey or libei for the accept tap.)
- The **app's adapter selection** — a `#[cfg]` target switch to pick the right
  adapter (currently hardcoded `platform_macos`) — lands with the impls.

**Effort:** Very large, multi-phase (each platform is its own A-sized milestone).
Each method's required Win32/Linux API is mapped in its crate's `src/lib.rs` doc
comments — the scaffold doubles as the implementation guide.

### 1.2 ◑🔒 Distribution hardening (signing, notarization, updater)

**Plan:** `2026-06-03-engine-macos-mvp-design.md §9` (A3 ship) — Developer-ID
signing + hardened runtime + notarization + a native updater.

**Status:**
- Signing now defaults to ad-hoc for local source builds, but
  `tools/bundle/make-app.sh` accepts `COMPME_CODESIGN_IDENTITY` to produce a
  Developer-ID hardened-runtime, timestamped release signature.
- `tools/release/notarize-app.sh` submits the signed app archive with
  `xcrun notarytool submit --wait`, staples the ticket with `xcrun stapler`, and
  validates the staple. The tag workflow imports the Developer-ID `.p12`, fails
  closed when signing/notarization secrets are missing, notarizes before zipping,
  and uploads the notarized zip.
- The updater path is GitHub-release-driven: the tray has **Check for Updates…**
  and the release workflow uploads `compme-<version>-update.json` next to the
  zip and checksum. A full Sparkle/appcast client remains an optional later
  upgrade.
- **No `v*` git tags** yet (`git tag -l 'v*'` empty), so the first real release
  still needs the external Developer-ID secrets plus a maintainer-created tag.

**Pending:**
- Configure GitHub Secrets for Developer-ID signing and notarization.
- Run the first tag build and verify the notarized zip, `.sha256`, and update
  manifest publish correctly, and that the workflow commits the finalized
  `Casks/compme.rb` checksum back to the default branch.
- Optional later upgrade: replace the GitHub-release menu handoff with a full
  Sparkle/appcast client.

**Effort:** Medium. **Blocked on an Apple Developer ID account ($99/yr) — human-gated.**
The CI/release/cask glue is already written and validated; only the secrets +
identity + first tag are missing.

---

## Tier 2 — Personalization correctness

### 2.1 ✅ Per-app / per-domain instruction steering — config and runtime wired

**Plan:** `2026-06-09-a2-parity-design.md:13,27` called for per-app/per-domain
instruction maps, with the settings design deferring the editing UI.

**Status (re-validated 2026-06-15):**
- `build_personalization` parses `COMPME_INSTRUCTIONS_APPS` /
  `COMPME_INSTRUCTIONS_APP_<TARGET>` into `PersonalizationProfile.per_app`
  and `COMPME_INSTRUCTIONS_DOMAINS` /
  `COMPME_INSTRUCTIONS_DOMAIN_<TARGET>` into
  `PersonalizationProfile.per_domain` (`crates/app/src/run_loop.rs`,
  `build_personalization`).
- Ambiguous target suffixes are ignored instead of applying one value to
  multiple apps/domains (`instruction_map_from_config` in `run_loop.rs`).
- Inference now calls
  `profile.build_preamble(Some(&request.field.app), request.domain.as_deref())`
  (`crates/app/src/inference.rs:304-307`), so resolved browser domains can
  activate per-domain steering.
- The submit path reads the cached browser domain into `RequestLogContext`, and
  `submit_request_and_track` copies it onto the request before dispatch
  (`run_loop.rs`). Existing per-app keying
  remains by canonical bundle id.

**Coverage:**
- `personalization_built_from_per_app_and_domain_config_keys` covers config
  population, missing values, normalized domains, and combined global/app/domain
  preambles.
- `personalization_skips_ambiguous_per_target_instruction_keys`
  covers collision handling.
- `per_domain_personalization_uses_request_domain`
  (`crates/app/src/inference.rs`) covers runtime domain steering.
- Focused revalidation passed on 2026-06-15:
  `cargo test -p app personalization_built_from_per_app_and_domain_config_keys`,
  `cargo test -p app personalization_skips_ambiguous_per_target_instruction_keys`,
  and `cargo test -p app per_domain_personalization_uses_request_domain`.

**Remaining:** no code/test gap for instruction steering. The global
Personalization pane editor has shipped under Tier 3.2; a per-app/per-domain
instruction editor remains a future enhancement, not a runtime steering gap.

---

## Tier 3 — A3 settings UI (code-complete; live LOOK remaining)

Per `2026-06-10-a3-settings-ui-design.md`. The settings window now ships as 9
tabs (Setup, General, Personalization, Apps, Context, Emoji, Shortcuts,
Statistics, About). The macOS-buildable Tier 3 controls have landed in code and
deterministic tests; the remaining work is the live visual/physical LOOK pass
tracked in [`MANUAL-VALIDATION.md`](MANUAL-VALIDATION.md), plus optional UX
enhancements explicitly called out below.

### 3.1 🔬 Per-app override editing rows (Apps pane) — code complete, LOOK pending
- **Status:** the Apps pane ships a compact one-line policy grid. Each recorded
  app row exposes enable, Tab-disable, mid-line, autocorrect, and grammar-fix
  policy checkboxes plus a delete action. The run loop resolves row/field edits
  into `prefs::AppPolicyField` updates and retracts visible suggestions when a
  policy edge makes the focused field ineligible.
- **Remaining:** visual LOOK only: column readability, name truncation, and
  toggling behavior in a real settings window. A manual "add app" control is a
  future convenience, not a blocking residual for the current Apps-grid scope;
  rows are created from observed/recorded apps.
- Spec: `a3-settings-ui-design.md` Phase S2 "App Settings pane — largest".

### 3.2 🔬 Dedicated Personalization / Context / Emoji panes — code complete, LOOK pending
- **Context:** the dedicated Context tab controls clipboard and screen-OCR
  prompt context. The run loop initializes the switches from config, persists
  edits, clears disabled context cells, and gates submissions by the current
  values.
- **Emoji:** the Emoji tab controls enable, skin tone, and gender preferences.
  The gender picker is implemented and unit-tested, so the Emoji pane is complete
  for the current scope.
- **Personalization:** the Personalization tab now edits global instructions,
  sender name/email, and the 6-stop steering strength. Edits update the live
  inference worker profile through `set_profile` and persist through the same
  settings path. Memory storage mode remains governed by memory config and UI
  controls elsewhere; it is not part of the personalization profile.
- **Remaining:** visual LOOK only: the pane layout, multiline instructions field
  behavior, sender/strength controls, and visible steering effect in a live app.
  A Context appearance sub-toggle remains a future visual option, not a current
  blocking item.

### 3.3 ✅ Statistics range / group / metric controls — current scope complete
- **Range picker ✅:** Last 7/14/30 days drives the bucket span.
- **Grouping picker ✅:** Daily/Weekly re-buckets rows through the shared stats
  grouping path.
- **Metric selector closed by design:** the pane already renders separate
  sparkline rows for shown, accepted, and words. A single metric selector would
  be a redesign, not a missing control. The pure metric selection model remains
  available if that redesign is chosen later.

### 3.4 🔬 Shortcuts pane and always-on hotkeys — code complete, physical LOOK pending
- **Status:** recorder rows, live rebind, modifier-combo capture, config parsing,
  internal collision checks, process-lifetime Carbon registration, and run-loop
  dispatch are implemented for force-activate, per-app toggle, and global toggle.
  Toggle-app/global mirror the tray policy paths. Force-activate re-shows the
  currently held suggestion; it deliberately does not start fresh inference.
- **Remaining:** physical keypress LOOK only: verify configured force/toggle
  shortcuts fire in a granted macOS session, update the focused app/global policy
  as expected, and that force-activate behaves as the held-suggestion re-show
  command.

### 3.5 ☐ Emoji `includeVanillaVariants` (deferred by design)
- Deferred: an alternate vanilla glyph has no display path in the single-ghost
  replacement pipeline. Revisit when a multi-candidate replacement *display*
  exists. Spec: `a3-settings-ui-design.md:64`.

> **Corrected 2026-06-15:** the global disable submenu (For 1 Hour / Until
> Relaunch / Always) is **✅ DONE** (`crates/platform_macos/src/tray.rs:238-246`,
> `DisableArm` `:53-59`; mapped through the `apply_global_disable` fn in
> `run_loop.rs`, dispatched from the tray global-disable submenu handler
> (symbol anchors — line numbers here drifted three times)). The older "NOT built — only flat Snooze-1h" note is
> superseded by the current corrected A3 status.

---

## Tier 4 — 🔬 Live validation (code complete; needs human/scripted evidence)

These are implemented to a deterministic/build-verified standard and (mostly)
scripted-smoke-gated via `tools/acceptance/run-a2-compat-gates.sh`. They need a
person at a granted macOS desktop, not new code. Sources:
`2026-06-09-a2-parity-design.md §16`, `integration-phase-design.md`.

| Item | Status | Live residual |
|---|---|---|
| Browser-domain extraction | code ✅ (`c131`) | 9-item LOOK checklist (Safari/Chrome/Brave detect + exclusion suppress) |
| Multi-candidate Down-cycle | engine ✅ | physical Down-arrow cycle UX confirmation |
| Compatibility matrix | classifier ✅ | per-app behavior matches its tier, across the matrix |
| Browser mirror-window | `set_mirror_mode` ✅ | live Firefox/Zen ghost-in-mirror confirmation |
| Terminal/iTerm AI-prompt | `terminal_prompt_activates` ✅ | tuning vs real agent prompts |
| Screen-context OCR | `screen_context_text` ✅ | OCR quality/perf on a granted desktop + multi-display caret confirm |
| Encrypted memory — AllMonitored | core ✅; TextEdit product-loop privacy + runtime-disable proofs + Chrome domain-exclude proof ✅ | remaining live residual: secure input, snoozed transition, volatile `pid:N` |
| Per-app memory inspect/delete UI | count/delete_app ✅ | completed live in Apps pane; remaining global delete_all/mode controls tie to Personalization |
| Trailing-space toggle | accept-path ✅ | live evidence for exact inserted text |
| Strength slider (6 stops) | pure ✅ | live before/after steering at multiple stops |
| Google Docs / Arc onboarding | `needs_accessibility_setup` ✅ | live Docs round-trip |

---

## Tier 5 — 🟢 Standalone grammar/spell-fix mode (CODE-COMPLETE, live LOOK pending)

**Intent (2026-07-01 user request):** a *separate* feature from inline
completion — press a **grammar-trigger** key, the nearest misspelled/ungrammatical
word at the caret is **underlined in place**, the suggested correction is shown in
a **banner above it**, and a **separate grammar-accept** key replaces the word.
This is a detect→underline→confirm flow, distinct from the type-ahead ghost.

**Implementation spec:** [`superpowers/specs/2026-07-01-grammar-fix-design.md`](superpowers/specs/2026-07-01-grammar-fix-design.md)
— phase-by-phase build plan (G1-G5) with exact files, signatures, tests, and
acceptance criteria. Start there for implementation.

**Status (2026-07-02):** G1-G5 are implemented and deterministic validation is
green. The portable correction pipeline, macOS trigger/accept routing,
fail-closed range seams, underline/banner presenter, Apps-pane `GrammarFix`
policy column, and grammar-accept recorder/persistence are in code with focused
tests. The remaining acceptance item is the interactive TextEdit grammar LOOK
gate emitted by `tools/acceptance/run-a1b-live-gates.sh --self-test`, which
requires a granted macOS GUI session.

**Decisions settled (with the requester, 2026-07-01):**
0. **Cross-platform by construction — Linux, Windows, and macOS.** No part of the
   feature may be macOS-only. All detection, correction, orchestration, prompt,
   policy, and state logic lives in the **portable crates** (`model_client`,
   `engine_core`, `engine`, `run_loop`, `context`, `prefs`, a `grammar` crate);
   only thin surfaces sit behind the `platform` trait boundary, each OS providing
   its own impl: (a) global hotkey registration, (b) the correction overlay
   (underline + banner), (c) text-range bounds, and (d) text-range replacement.
   Some of these are new trait methods, so they land with compile-safe,
   fail-closed `platform_linux`/`platform_windows` stubs. macOS is the
   **reference implementation**. This matches the repo's existing seam:
   `platform_linux`/`platform_windows` already fail closed for unsupported field
   operations, and `OverlayPlacement` already enumerates `LayeredWindow` (Win),
   `LayerShell`/`OverrideRedirect` (Linux), and `NativePanel` (mac).
1. **Detection/correction engine = the installed local LLM**, not a platform
   spell API (NSSpellChecker/UITextChecker) and not a bundled dictionary. compme
   already runs a local llama.cpp model; grammar-fix becomes a new *inference
   request kind*, which keeps detection **inherently cross-platform** (one code
   path, no per-OS spell binding) and stronger than a word list.
   `autocorrect`/`thesaurus` stay closed tables (they can only fire on their
   31/handful of entries), so they cannot be the engine — at most a zero-cost,
   portable pre-pass.
2. **Scope = the nearest word at the caret**, not a whole-field scan-and-cycle.
   Use a word-under-caret helper over `left_context + right_context` that returns
   a scalar `CorrectionRange`; `trailing_word` is insufficient for mid-word cases
   such as `te|h`. Multi-error cycling is a later extension, not v1.
3. **Two dedicated keystrokes** (the user asked for a separate fix key), not a
   reuse of accept-word/full.
4. **Its own enable toggle + Apps-pane column** ("a separate feature for grammar
   only"), gated off in code fields like `autocorrect`.

### Reuse — already built (do NOT rebuild)
- **In-place replace mechanics:** grammar-fix needs a new range replacement path,
  not the existing `Command::Replace { replace_left }` model. Add a leaf-owned
  scalar `CorrectionRange` at the `platform` boundary, carry that same range
  through the request/outcome/showing state, and emit `Command::ReplaceRange` →
  `insert_replacing_range`. `replace_left` remains for emoji/autocorrect only.
  **Same `InsertStrategy::AxSet` gate** applies (`engine_core/src/lib.rs:791`):
  on non-AxSet fields offer nothing (degrade), exactly as replacements do today.
- **Snapshot/staleness safety:** model the correction as a `Showing` with
  `presentation = Correction` and `correction_range = Some(..)`; every
  TextChanged/CaretMoved bumps `generation`/`snapshot` so a correction can't
  apply to stale text (`engine_core/src/lib.rs:193-201`).
- **Word geometry for the underline:** add `PlatformAdapter::text_range_rect` over
  the same scalar `CorrectionRange`. macOS converts scalar offsets to UTF-16 and
  uses `read_ax_bounds_for_range(element, loc, len)` (`platform_macos/src/lib.rs:4559`).
  (Do **not** reuse the thin-caret `usable_caret_rect` guard — a word is wider
  than its threshold.)
- **Inference plumbing:** `engine::CompletionRequest` plus app-owned
  `CompletionOutcome` over channels, `LocalModel::complete(prompt, max_tokens)`
  (`model_client/src/lib.rs:78`), `terse_continuation_prompt` (`:578`) as the
  template for a new `grammar_fix_prompt`.
- **Gates/policy:** `replacement_decision`/`suggestion_gates_pass`
  (`run_loop.rs:533`); `AppPolicy` tri-state fields (`prefs/src/lib.rs:13`);
  Apps-pane checkbox enum `AppPolicyField` (`prefs/src/lib.rs:46`).
- **Keystroke infra:** always-on shortcuts `ShortcutBindings`/`registration_plan`
  (`platform_macos/src/lib.rs:2521/2568`), `ShortcutAction` (`platform/src/lib.rs:202`);
  ghost-scoped accept keymap `AcceptKeymap`/`binding_for_hotkey_id`
  (`platform_macos/src/lib.rs:3142/3425`); recorder UI `KeyRecorderField`
  (`settings_window.rs:692`).
- **Overlay recipe:** the borderless transparent `NSPanel` in `ensure_panel`
  (`platform_macos/src/lib.rs:776`) + Y-flip in `overlay_frame_for_text` (`:969`).

### Build — genuinely new
1. **Correction engine (LLM):** `model_client::grammar_fix_prompt(word, left_ctx)`
   (pure, next to `terse_continuation_prompt`) + a **grammar request kind** on
   `engine::CompletionRequest` and a corrected-word/range field on
   `CompletionOutcome`, routed through the existing worker/`recv_latest` loop.
   Tight prompt: "return the corrected word only, or the word unchanged"; low
   `max_tokens`; **post-filter** the model output (reject multi-word / large-edit
   / meaning-changing responses; require small edit distance) so it can't rewrite
   the user's word into something else.
2. **Correction UI (novel FFI):** underline the misspelled word in place + a
   correction **banner** above it. Neither primitive exists (the overlay only
   appends uniform ghost text at the caret; no attributed strings anywhere).
   Build as **two thin borderless panels** cloning the `ensure_panel` recipe: a
   1-2px filled underline panel positioned under the word rect, and a small
   background-filled banner panel above it showing the suggestion. New
   `OverlayPresenter` method(s) (e.g. `show_correction(word_rect, suggestion)`)
   or a sibling presenter; update `FakeOverlay` (`engine/src/lib.rs:554`) and the
   `ux_mode`/placement plumbing to match. Degrade to a caret-anchored popup when
   `read_ax_bounds_for_range` returns `Ok(None)`.
3. **Two keystrokes:** **grammar-trigger** = new `ShortcutAction::GrammarCheck`
   (always-on Carbon hotkey, new id 8, config `COMPME_GRAMMAR_CHECK_KEY`,
   startup-string first like the other global shortcuts) — routed at the
   `HostEvent::Shortcut` match (`run_loop.rs:3715`) to run detection.
   **grammar-accept** = new `AcceptBinding::GrammarAccept` role with explicit
   accept-arm modes: `AcceptArm::Correction` swallows only GrammarAccept while
   Word/Full pass through, and `AcceptArm::Ghost` keeps the existing Word/Full
   behavior while GrammarAccept passes through. It gets a new Carbon id, config
   `COMPME_GRAMMAR_ACCEPT_KEY`, and is live-rebindable via a third
   `RecorderRole` later. Collision detection stays in the existing field arrays
   (`has_internal_collision` / `record_decision`).
4. **Toggle + policy wiring:** `Config.grammar_fix` (`COMPME_GRAMMAR_FIX`,
   `run_loop.rs:169/277`), `AppPolicy.grammar_fix: Option<bool>` + a
   `grammar_fix_enabled(app, default)` getter (`prefs/src/lib.rs:133` mirror), a
   `AppPolicyField::GrammarFix` Apps-pane column, consulted in the new flow.

### Ordered build sequence (pure/testable first, novel FFI last)
| # | Phase | Effort | Notes |
|---|---|---|---|
| G1 | `grammar_fix_prompt` + output post-filter (model_client, pure) + word-under-caret helper (context) | S | ✅ Implemented with deterministic prompt, vetting, and caret-word tests. |
| G2 | Grammar inference request/outcome kind + worker routing; `CorrectionRange`/`Showing`/`ReplaceRange` wiring; `Config`/`AppPolicy`/`AppPolicyField` toggle wiring | M | ✅ Implemented with fake model/adapter coverage and fail-closed platform stubs. |
| G3 | Two keystrokes: `ShortcutAction::GrammarCheck` + `AcceptBinding::GrammarAccept` registration + routing | M | ✅ Implemented with config parsing, shortcut routing, accept-action isolation, and Carbon plan tests; physical keypress remains part of live LOOK. |
| G4 | Underline + correction-banner overlay (novel FFI) | L | ✅ Implemented with macOS range geometry and correction presenter tests; live visual LOOK remains pending on a granted Mac. |
| G5 | Settings: grammar-accept recorder row + Apps-pane `GrammarFix` column; live validation | M | ✅ Implemented: recorder role/collision handling, live grammar-accept rebind persistence, Apps-pane `GrammarFix` mapping, and env-shadow/config tests are covered. |

### Open decisions (recommended defaults)
- **Underline rendering:** *recommend a thin filled sub-panel* under the word rect
  (matches the existing "position a transparent panel" pattern) over an
  attributed-string `NSUnderlineStyle` (the repo uses no attributed strings yet).
- **LLM safety:** *recommend* a strict single-word post-filter + small-edit-distance
  guard; the local model must not turn a typo-fix into a paraphrase. If the model
  is unreliable at word-level, fall back to the pure `autocorrect` table for the
  known-typo subset.
- **Trigger with no error found:** *recommend* a silent no-op (or a subtle flash),
  not a "nothing to fix" banner.

### Cross-platform architecture (Linux · Windows · macOS)
The portable core (G1-G2, plus policy/settings logic) is **written once** and
shared by all three OSes. Only these four trait surfaces get a per-OS impl; the
new range-bounds/range-replacement methods must land with fail-closed stubs in
every adapter when the shared trait changes:

| Surface | macOS (reference) | Windows | Linux |
|---|---|---|---|
| Global grammar-trigger hotkey | Carbon `RegisterEventHotKey` (`ShortcutBindings`, already built) | `RegisterHotKey` (Win32) | X11 `XGrabKey` / Wayland global-shortcuts portal |
| Correction-scoped grammar-accept key | Carbon accept keymap with explicit `AcceptArm::Correction` / `AcceptArm::Ghost` modes | keyboard hook / `RegisterHotKey` | X11/Wayland key grab |
| Word rect + in-place replace | AX `kAXBoundsForRange` via `text_range_rect` + `insert_replacing_range` | UI Automation `TextPattern` `BoundingRectangles` + range `SetValue`/`SetText` strategy | AT-SPI2 `Text`/`EditableText`, or IME/synthetic fallback |
| Underline + banner overlay | borderless `NSPanel` (`NativePanel`) | layered top-most window (`LayeredWindow`) | `wlr-layer-shell` (`LayerShell`) / override-redirect X11 (`OverrideRedirect`) |

Detection (LLM inference) has **no per-OS surface at all** — it runs through the
same portable `model_client`/`inference` path on every OS. Sequencing: macOS
lands G1-G5 first as the reference; Windows and Linux first get fail-closed
stubs for the new trait rows, then real implementations as follow-on platform
work. Grammar-fix stays inert there until each row is built — never misbehaves.
This is the same parity model as Tier 1.1 foundation work, and it depends on the
platform text-range read/replace impls that Windows/Linux owe regardless of this
feature.

**Effort/status:** Large milestone now code-complete for the macOS reference:
portable core (G1-G2) and macOS reference surfaces (G3-G5) are implemented and
headless-tested. Windows and Linux retain fail-closed stubs for the new range
and correction surfaces until their real four-row trait impls are built. The
remaining macOS risk is live LOOK validation of the underline/banner and
physical trigger/accept interaction.

---

## Out of scope (deliberate — not pending)

- **Payment / licensing tiers / subscriptions / multi-device seats** — compme is
  Apache-2.0, all features open (`a3-settings-ui-design.md:15`). No Subscription
  pane, no telemetry toggle (nothing is ever sent; About pane states this).
- **RTL / multilingual** — model/locale-bound, not pure-table features
  (`a2-parity-design.md:89`).
- **Candidate cycling & thesaurus** are intentional **supersets** beyond Cotypist,
  already shipped — *not* parity gaps (`a2-parity-design.md:69-76`).

---

## macOS completion plan (2026-06-30)

> **Status (2026-07-01): the macOS-buildable backlog is CODE-COMPLETE.** All six
> residuals below are done in code (the last gap — the Personalization multi-line
> instructions field, item 5 — shipped in `256eb14`), verified by a full-codebase
> review + tdd + ponytail pass (1655 tests, clippy clean). What remains for
> "ready to use" is **not development**: (a) a human **visual-LOOK pass** on a
> granted Mac over the 9 settings panes + the Tier-4 live checklist, and (b)
> **distribution** (Developer-ID signing + notarization + first `v*` tag), which is
> Apple-ID-gated. See `docs/MANUAL-VALIDATION.md` for the live checklist.

**Directive: finish macOS first.** Cross-platform adapters (1.1) and distribution
(1.2) stay parked until the macOS feature set is complete — both are externally
blocked anyway (1.1 needs Windows/Linux build+test environments; 1.2 needs an
Apple Developer ID). Everything below is buildable on the macOS dev host today.

Verified complete-list facts (2026-06-30 plan-review pass): there is **no Tier
1.3**, and **Tier 2 is a single ✅ DONE item (2.1)** — so the macOS-buildable
backlog is exactly the six residuals below, nothing hidden. Correction to an
earlier note: the **F2 insertion-order decision is already shipped** — a fixed
`AxSet → SyntheticKeys → Clipboard → None` chain (`platform_macos/src/lib.rs`
`insertion_strategy()`), not paste-first and not per-app configurable.

### Ordered build sequence (lowest-risk / decision-free first)

| # | Item (tier) | Effort | Why this slot |
|---|---|---|---|
| 1 | ✅ **DONE (2026-06-30)** — Emoji gendered + skin-tone ZWJ assembly | S–M | Shipped: `with_skin_tone_zwj` splices the Fitzpatrick modifier into the base of the gendered ZWJ sequence (`emoji/src/lib.rs`). 27 tests pass, clippy clean. |
| 2 | ✅ **DONE (2026-06-30, closed without picker)** — Statistics metric selector (3.3) | S / 0 | Decision taken: keep the existing layout, no `NSPopUpButton`. A single-select picker trades away at-a-glance comparison for an unrequested control. The `StatMetric`/`metric_series` scaffold has since been **removed** (a later ponytail pass cut it — zero references remain in `crates/`). |
| 3 | 🟢 **CODE-COMPLETE — VISUAL LOOK pending (2026-07-01)** — Apps-pane editing rows (3.1) | M | Core + AppKit shell landed. `editAppPolicy:` checkboxes → `apps_edit` signal → run-loop resolves row→app → `set_app_policy_field` → persist. **LAYOUT BUG found + fixed (2026-07-01, `f5a81c5`):** the geometry-check pass caught a real overlap — each app was laid across 2 lines but rows advanced only 26px, so every row's policy checkboxes rendered *on top of the next app's name* (28 collisions, only visible with 2+ apps; headless "0 panics" validation couldn't see it). Redesigned to a **compact one-line grid** (name + 4 title-less checkbox columns under an `App | On Tab Mid AC` header + tooltips + Delete), all 8 apps fit, zero overlap, pinned by `apps_pane_grid_has_no_overlaps_within_budget` (mutation-verified). **Pre-check also resolved** — `compose_apps_policy_bits` publishes live per-app bits on show, seeded via `refresh_apps_policy_checkbox_states`. **Still needs eyes/fingers (pure visual LOOK):** bare-checkbox column look, name truncation, toggling changes behavior. |
| 4 | 🟢 **REGISTRATION runtime-validated — FORCE/TOGGLE DISPATCH needs physical keypress (2026-06-30)** — Always-on hotkeys (3.4) | M | Core + FFI shell landed. **Headless LOOK confirmed for the pre-grammar hotkey set (with COMPME_DEBUG, env keys, TextEdit focus):** `global shortcuts configured` parses env correctly; on text-field focus Carbon hotkeys through ids 5/6/7 (keycodes 96/97/98, shift mask) register via `registration_plan`→`register_hotkey`; collision check passes. Hotkeys re-register per arm-cycle. **Accept hotkeys 1–4 are script-validated** by the rebuilt A1b Carbon accept gates; this row now tracks the remaining always-on force/toggle hotkeys only. Grammar hotkeys ids 8/9 are tracked by the grammar LOOK gate and A1b docs/scripts. **Cannot headless-validate force/toggle dispatch yet:** needs a PHYSICAL press of shift+F5/F6/F7 to confirm ForceActivate/ToggleApp/ToggleGlobal reactions. ForceActivate → `Engine::on_force_show` (re-presents held candidate, 3 tests); ToggleApp/Global call real mechanisms. **Deferred:** re-show only works while a suggestion is held (TODO(LOOK) in `engine_core`). |
| 5 | 🟢 **CODE-COMPLETE — VISUAL LOOK pending (2026-07-01)** — Personalization pane (3.2) | L | Core (live `set_profile` reload) + pane shell landed. New "Personalization" pane (3 knobs) → `personalization_edit` signal → run loop applies + `set_profile` (live) + `persist_setting`. **Headless LOOK confirmed:** Settings window opens with the new pane present (AXTabButton focus events seen), **0 panics**. **Roadmap correction:** MemoryStore is governed by `config.memory.mode`, NOT the profile. **Last code gap closed (2026-07-01, `256eb14`):** the global-instructions input is now a **multi-line wrapping `NSTextField`** (`setUsesSingleLineMode(false)` + word-wrapping cell; Return commits, Option-Return inserts a newline — tested target/action path preserved), field grown to ~5–6 lines with sender/strength rows shifted down. **Still needs eyes/fingers (pure visual LOOK, no code):** pane + multi-line field render/commit correctly; edits visibly re-steer output (the re-steer *path* is already unit-tested via live `set_profile`). |
| — | Emoji `includeVanillaVariants` (3.5) | — | **Do not schedule.** Hard-blocked on a multi-candidate replacement *display* that does not exist yet. |

### Open decisions to settle (recommended defaults)

1. **Stats metric picker** — ✅ **SETTLED (2026-06-30): closed as DONE without a
   picker.** Keep the existing layout. A picker trades the at-a-glance comparison
   for an unrequested control.
2. **force-activate semantics** (gates item 4) — ✅ **SETTLED (2026-06-30):
   "force-show the current pending suggestion now"** (cheap, predictable) over
   "kick a fresh inference request" (latency + races).
3. **Non-AxSet plain-insert posture** — *recommended: keep best-effort*; add a
   post-insert readback only if a live per-app pass (Terminal/iTerm/Safari)
   shows wrong text. Plain inserts via SyntheticKeys/Clipboard currently assume
   success (`platform_macos/src/lib.rs:1082`); replacements already fail closed.

### After macOS is complete — longer-term order (unchanged)

1. **Tier 1.2** distribution — wire notarization the moment a Developer ID is
   available; cut the first `v*` tag (CI/cask glue already written).
2. **Tier 1.1** cross-platform adapters — a dedicated milestone of their own
   (Windows/UIA, Linux/AT-SPI2, GNOME-Wayland IME path).
3. **Tier 4** — opportunistic live LOOK gates, whenever a macOS GUI session is
   available.
