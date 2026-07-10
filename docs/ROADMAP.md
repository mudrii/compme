# compme — Roadmap & Pending Work

> **Last updated:** 2026-07-10 (v0.1.4 Developer-ID signed, hardened-runtime, notarized, stapled, and published; post-tag runtime/release hardening and Settings cleanup are on `main`; cross-platform Phase 0 remains shipped) · **Branch:** `main` · **Tests:** ≈1829 workspace tests listed on current `main` (spike separate)
>
> This document cross-references the plan specs in
> [`docs/superpowers/specs/`](superpowers/specs/) against the implemented code and
> records, in detail, what remains. It is the single source of truth for "what's
> pending" — kept in sync as items ship. Status claims here are evidence-backed
> with symbol/function/gate anchors re-reviewed 2026-07-10 through `5fa5b6b`
> (workspace review/tdd/ponytail: zero-alloc slice-based trigger gates, an
> `InsertStrategy::supports_atomic_range_replace` capability predicate replacing
> `== AxSet` gates, +8 mutation-pinning tests → 1787 with the dead UTF-16 guard
> and the `trim_trailing` wrapper removed; release: stable-only version
> validation and fail-closed publication hardening, privacy scan widened to all
> text files; cross-platform Phase 0: `InsertStrategy::NativeRangeSet`,
> `platform_windows::win_host` DACL hardening + console ctrl handler, Windows
> CI job runs the new windows-only tests; `18fbc4f` corrected the pinned
> Qwen2.5-1.5B catalog byte size so the download cap admits the real artifact),
> starting from baseline `ba4e805` since `b1c9264`.

> **Release boundary:** the published v0.1.4 artifact is tag `18b8dc0`.
> `1f4c041` (cask finalization), `216fa0a` (runtime/release hardening),
> `618013d` (seam hardening and A2 local/manual-only automation policy),
> `a5781fc` (single model-location control), `18fbc4f` (catalog metadata fix),
> the documentation reconciliations through `88b22cd`, and `5fa5b6b` (release
> publication hardening) are post-tag `main`
> changes. They require a later
> release tag before they are available in the distributed binary. Unless a row
> explicitly says otherwise, current implementation/test claims below describe
> `main`; the v0.1.4 bullets describe the published artifact.

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

**Foundation ✅ DONE (2026-06-16 through 2026-07-07, gate-green on macOS):**
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
- **Shell foundation + app cfg boundary** (`2c80e74`) — `platform::shell`
  defines the portable `ShellHost`/`TrayHandle` contract and shared settings/keymap
  data; `platform_macos::MacosShellHost` wraps existing macOS behavior; Windows/Linux
  provide fail-closed `ShellHost` + `OverlayPresenter` scaffolds; `app` routes
  platform construction and macOS-only shell surfaces through `crate::shell`.
- **App target-gated platform deps** (`2c80e74`) — `app` no longer depends
  unconditionally on `platform_macos`; macOS, Windows, and Linux adapter crates are
  selected behind Cargo target gates. Non-macOS runtime remains fail-closed until
  the real adapters land.
- **CI matrix** (`a7427c6`, widened by `2c80e74`) — `windows-latest` +
  `ubuntu-latest` jobs run fmt/clippy/test over the workspace excluding only
  Apple-only `platform_macos`, then build the `app` binary through its non-mac
  facade.

**Pending (🔒 needs Windows + Linux build+test environments — not doable on macOS):**
- The actual **Windows** adapter behind `#[cfg(windows)]` (extend the existing
  host-service `windows` feature set in its `Cargo.toml`): UIA focus/caret/text +
  `WH_KEYBOARD_LL` accept tap +
  `SendInput`/ValuePattern insert + layered overlay, plus real ShellHost services
  (DPAPI/CredWrite key store, tray, confirm UI, launch-at-login, native event pump).
- The actual **Linux** adapter behind `#[cfg(target_os = "linux")]`: AT-SPI2
  read/insert/events + XTEST/`wtype` synthetic keys (IBus IME fallback on Wayland)
  + override-redirect/layer-shell overlay. (AT-SPI device key-listeners are
  deprecated → prefer XTEST/XGrabKey or libei for the accept tap.) Real ShellHost
  services still need libsecret, tray/portal integration, confirm UI, autostart,
  and a native event pump.

**Phase 0 pre-work ✅ DONE (2026-07-08, same-day as planned):**
- **`InsertStrategy::NativeRangeSet`** shipped — variant + doc contract on the
  shared enum, opted into `supports_atomic_range_replace()`; pinned by the
  enumerated-variant predicate test, an engine_core arming test
  (`offer_replacement_arms_on_native_range_set_fields`), and the
  Windows/Linux stub exhaustive-match tests (both mutation-verified).
- **Windows owner-only DACL hardening** shipped —
  `platform_windows::win_host::harden_owner_only` (protected `OICI` OWNER_RIGHTS
  ACE, propagates to existing children); wired fail-closed ahead of
  `MemoryStore::open` (db + wal/shm inherit) and into config-dir creation;
  DACL-readback test (`AceCount == 1`, inheritance) runs in the Windows CI job.
- **Windows console-control handler** shipped —
  `platform_windows::win_host::install_console_ctrl_handler` gives Ctrl-C/close
  parity with SIGINT/SIGTERM (headless toggle deferred to the real adapter).
- Cosmetics done: About credits are per-OS; shared-code comments reworded
  host-neutral (CFRunLoop/AppKit mentions scoped to macOS behavior notes).

**Detailed execution guide:**
[`2026-07-08-cross-platform-implementation-plan.md`](superpowers/specs/2026-07-08-cross-platform-implementation-plan.md)
— phased plan (Phase 0 shared-code pre-work → Windows UIA → Linux X11 →
Wayland spike → GPU runtime → per-OS packaging → acceptance), with per-method
API mapping, CI upgrades, and a risk register. Evidence re-verified against
`b367f0f`.

**Effort:** Very large, multi-phase (each platform is its own A-sized milestone).
Each method's required Win32/Linux API is mapped in its crate's `src/lib.rs` doc
comments — the scaffold doubles as the implementation guide.

### 1.2 ◑ Distribution hardening — signed/notarized releases shipped; native updater optional

**Plan:** `2026-06-03-engine-macos-mvp-design.md §9` (A3 ship) — Developer-ID
signing + hardened runtime + notarization + a native updater.

**Status:**
- Signing now defaults to ad-hoc for local source builds, but
  `tools/bundle/make-app.sh` accepts `COMPME_CODESIGN_IDENTITY` to produce a
  Developer-ID hardened-runtime, timestamped release signature.
- `tools/release/notarize-app.sh` submits the signed app archive with
  `xcrun notarytool submit --wait`, staples the ticket with `xcrun stapler`, and
  validates the staple. The tag workflow imports the Developer-ID `.p12`, fails
  closed when signing/notarization secrets are missing, requires a protected
  stable `vX.Y.Z` tag plus the protected `release` environment, validates the
  stable release version through one shared helper, requires the tag to equal
  the current default-branch HEAD at preflight and again before prebuild, and
  verifies an exact-arm64 binary both before artifact upload and before signing
  secrets are exposed. It notarizes before zipping, fails closed unless the
  deterministic signing keychain is deleted and absent, rejects pre-existing
  same-name release assets, uploads the notarized zip plus `.sha256`, and
  verifies the downloaded artifact checksum before publishing the GitHub
  release. Cask metadata is Ruby-syntax checked and explicitly arm64-only.
- The updater path is GitHub-release-driven: the tray's **Check for Updates…**
  opens the releases page, and the release workflow uploads an informational
  `compme-<version>-update.json` next to the zip and checksum (nothing consumes
  it in-app yet; a future auto-updater must add signature verification before
  trusting it). A full Sparkle/appcast client remains an optional later upgrade.
- **v0.1.0 SHIPPED 2026-07-08** (interim unsigned mode): protected `v*` tag
  ruleset + gated `release` environment created; all 13 A2 matrix rows passed
  live (`tools/acceptance/evidence/a2/v0.1.0-20260708-154651/`, variable set);
  the tag build published `compme-0.1.0-macos.zip` + `.sha256` + update
  manifest and finalized the cask sha on main. Release teething fixed en
  route: secrets-in-step-`if` startup failure, hosted-runner latency-budget
  opt-out, browser-row harness `COMPME_DEBUG`.
- **v0.1.4 SHIPPED 2026-07-10 signed and notarized:** the protected release run
  imported the Developer-ID identity, produced a hardened-runtime signature,
  notarized and stapled the app, verified the packaged checksum, published all
  three artifacts, and finalized the Homebrew cask.
- **Post-v0.1.4 release policy on current `main`:** `216fa0a` removed the
  conditional unsigned stable-release fallback so future tags fail closed
  without signing/notarization credentials; `618013d` removed A2 validation from
  CI/tag-release automation. The current release-integrity review additionally
  enforces stable-only `X.Y.Z` / `vX.Y.Z` versions, repeats the exact-default-tip
  check immediately before publication, creates the draft with
  `gh release create --verify-tag --draft --generate-notes`, refuses an existing
  release for the tag without overwriting assets, and separates cask finalization
  into a rerunnable protected-environment job. That job re-downloads and verifies
  the artifact, freezes the tag-reviewed cask updater and validator before
  switching to the default branch, validates the resulting cask, and makes a
  failed cask push recoverable without republishing. The workflow also constrains
  the artifact/cask to arm64 and allowlists the exact identities and commit SHAs
  of every workflow action. None of these post-tag policy changes is part of the
  v0.1.4 tag.

**Pending:**
- Optional later upgrade: replace the GitHub-release menu handoff with a full
  Sparkle/appcast client (must add manifest signature verification first).
- 🔒 **Repository-governance decision:** live GitHub settings still leave
  `main` unprotected, allow release-tag creation, permit release-environment
  self-review and unrestricted deployment branches, and allow actions outside
  the repository's exact checker allowlist. Closing those external controls
  needs owner authorization because branch protection conflicts with the
  current direct-to-`main` development policy. Until then, tag-controlled
  release helpers remain inside the trust boundary of whoever can advance
  `main` and create a release tag.

**Effort:** Small/optional for the remaining updater upgrade. The signed release,
CI/release/cask glue, and supply-chain hardening are implemented and machine-pinned
by `tools/release/check-model-gates.sh`; see [`RELEASING.md`](RELEASING.md).
A2 compatibility validation is now local/manual-only: CI, tag releases, and
`tools/release/check-model-gates.sh` do not execute or syntax-check its runner or
ledger checker. Teams may still collect committed pre-release evidence under
`tools/acceptance/evidence/a2/` and validate it locally.

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
  (`crates/app/src/inference.rs`), so resolved browser domains can
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

## Tier 3 — A3 settings UI (controls shipped; tray links + live LOOK remain)

Per `2026-06-10-a3-settings-ui-design.md`. The settings window now ships as 9
tabs (Setup, General, Personalization, Apps, Context, Emoji, Shortcuts,
Statistics, About). The nine-tab controls have landed in code and deterministic
tests. The remaining Tier 3 work is the tray website/support action pair tracked
below plus the live visual/physical LOOK pass —
**Live finding (2026-07-07 assisted-UI session) — FIXED same day:** Chrome
delivers a fresh AX element ref per focus notification for identifier-less web
fields, so pointer-based identity churned `StaleField` on every read (661
stale lines; ghost never rendered). Fixed by adding `CFHash`-based element
identity (`hash=` segment in `stable_field_key`/`field_element_id`) — the hash
tracks the underlying AX node across refs while the anonymous wrong-field
guard stays intact. Live after fix: 0 churn StaleFields; Chrome textarea
bind→request→completion→ghost→accept→insert end-to-end (" world" inserted with
seam). Caret precision in Chrome still degrades to the window-rect anchor —
that remains the `caret-marker-chrome-marker` calibration gate. Firefox/Zen
mirror-mode pipeline is log-proven end-to-end under ORGANIC hardware typing
(per-key reads, gen=39-41 requests, rendered mirror ghost frames — `f6fa98b`,
2026-07-07); scripted focus still misses the advisory wake. Residual: on-screen
LOOK of the mirror window plus hardware accept/cycle presses.

the authoritative pass/fail ledger is [`ACCEPTANCE.md`](ACCEPTANCE.md)'s
Manual/Live Gate Ledger (17 runner-pinned gate IDs); detailed walkthroughs live
in [`MANUAL-VALIDATION.md`](MANUAL-VALIDATION.md), and the assisted-session
driver (`tools/acceptance/run-ui-assisted-session.sh`) supports those manual
runs. The retired screenshot matrix is not current release evidence.

### 3.1 🔬 Per-app override editing rows (Apps pane) — code complete, LOOK pending
- **Status:** the Apps pane ships a compact one-line policy grid. Each recorded
  app row exposes enable, Tab-disable, mid-line, autocorrect, and grammar-fix
  policy checkboxes plus a delete action. The run loop resolves row/field edits
  into `prefs::AppPolicyField` updates and retracts visible suggestions when a
  policy edge makes the focused field ineligible.
- **Remaining:** visual LOOK only: column readability closed by Batch 1
  (assisted session); residual is name truncation and toggle-changes-behavior
  in a live settings window. A manual "add app" control is a
  future convenience, not a blocking residual for the current Apps-grid scope;
  rows are created from observed/recorded apps.
- Spec: `a3-settings-ui-design.md` Phase S2 "App Settings pane — largest".

### 3.2 🔬 Dedicated Personalization / Context / Emoji panes — code complete, LOOK pending
- **Context:** the dedicated Context tab controls clipboard and screen-OCR
  prompt context. The run loop initializes the switches from config, persists
  edits, clears disabled context cells, and gates submissions by the current
  values.
- **Emoji:** the Emoji tab controls enable, skin tone, and gender preferences.
  The gender picker is implemented and unit-tested, and its live LOOK closed
  2026-06-17 — the Emoji pane is complete; remaining 3.2 LOOK is only the
  Personalization and Context panes.
- **Personalization:** the Personalization tab now edits global instructions,
  sender name/email, and the 6-stop steering strength. Edits update the live
  inference worker profile through `set_profile` and persist through the same
  settings path. Memory storage mode remains governed by memory config; dedicated
  memory-mode and global delete-all Settings controls are deferred UI work, not
  part of the personalization profile or the current Personalization-pane scope.
- **Remaining:** visual LOOK only: pane layout, instructions field,
  sender/strength controls, and persistence closed (assisted Batches 1-2);
  Context opt-in verified live (Batch 6). Residual is a visible steering effect
  in a live app. A Context appearance sub-toggle remains a future visual option,
  not a current blocking item.

### 3.3 ✅ Statistics range / group chart controls — current scope complete
- **Range picker ✅:** Last 7/14/30 days drives the bucket span.
- **Grouping picker ✅:** Daily/Weekly re-buckets rows through the shared stats
  grouping path.
- **Metric selector closed by design:** the pane already renders separate
  sparkline rows for shown, accepted, and words. A single metric selector would
  be a redesign, not a missing control; the current code intentionally has no
  metric picker enum/control.
- **Coverage:** `stat_picker_enums_expose_menu_order_labels_and_index_decode`
  pins the range/group picker menu order, item labels, selected-index decode,
  and out-of-range clamp behavior.

### 3.4 🔬 Shortcuts pane and always-on hotkeys — code complete, physical LOOK pending
- **Status:** recorder rows, live rebind, and modifier-combo capture ship for
  Word, Full, and Grammar accept. Force-activate, per-app toggle, global toggle,
  and grammar-check shortcuts are config-backed: parsed, collision-checked,
  registered through process-lifetime Carbon hotkeys, and dispatched through the
  run loop. Toggle-app/global mirror the tray policy paths. Force-activate
  re-shows the currently held suggestion; it deliberately does not start fresh
  inference.
- **Remaining:** recorder capture/persistence synthetic-validated (Batch 2:
  ⇧F5 with modifier persisted). Residual is the physical-key edge, pinned by
  the A1b `always-on-hotkeys-physical-look` manual gate: verify configured
  force/toggle/grammar-check shortcuts fire in a granted macOS session, update
  the focused app/global policy as expected, and confirm force-activate behaves
  as the held-suggestion re-show command.

### 3.5 ☐ Emoji `includeVanillaVariants` (deferred by design)
- Deferred: an alternate vanilla glyph has no display path in the single-ghost
  replacement pipeline. Revisit when a multi-candidate replacement *display*
  exists. Spec: `a3-settings-ui-design.md:64`.

> **Corrected 2026-06-15:** the global disable submenu (For 1 Hour / Until
> Relaunch / Always) is **✅ DONE** (global submenu in
> `crates/platform_macos/src/tray.rs`, `DisableArm`; mapped through the
> `apply_global_disable` fn in `crates/app/src/run_loop.rs`, dispatched from the
> tray global-disable submenu handler). The older "NOT built — only flat Snooze-1h" note is
> superseded by the current corrected A3 status.

---

## Remaining committed macOS/A2 code gaps

The six-item June 30 macOS UI backlog is complete, but the broader committed
parity scope is not wholly code-complete. These implementation gaps must be
resolved or explicitly re-scoped before their corresponding live rows can close:

- **SidebarOnly editor-vs-sidebar detector:** VS Code/Cursor/Windsurf currently
  fail closed for every field; the host has no detector that enables only their
  AI-chat/sidebar fields.
- **Full statistical autocorrect:** the shipped `autocorrect` path is the curated
  high-confidence typo table, not the distinct full-autocorrect behavior promised
  by the parity spec; the code-field gate remains part of that work.
- **Cross-app previous-input context:** same-app redacted bounded history ships;
  the separate opt-in cross-app mode is not implemented.
- **Thesaurus selection-trigger UX:** trailing-word lookup/offers ship, but
  selection-triggered synonym behavior does not.
- **Tray website/support actions:** Settings, updates, and Quit ship; the A3
  **Visit Website** / **Contact Support** menu actions remain unimplemented.

---

## Tier 4 — 🔬 Live validation (implemented rows need human/scripted evidence)

Except for the explicit code prerequisites above, these rows are implemented to
a deterministic/build-verified standard. Selected A2
scenarios have locally invoked script evidence via
`tools/acceptance/run-a2-compat-gates.sh`, but that runner and its ledger checker
are deliberately excluded from CI, tag releases, the release-policy checker,
and generic shell-syntax validation. The listed residuals need a person at a
granted macOS desktop after any linked code prerequisite closes. Sources:
`2026-06-09-a2-parity-design.md §16`, `integration-phase-design.md`.
Gate coverage note: only the AllMonitored row has a dedicated runner gate ID
(`encrypted-memory-all-monitored-live`); the other residuals are covered by
optional local `run-a2-compat-gates.sh` smoke kinds, the table-driven `matrix`
ledger, and the
folded settings LOOK gates (`personalization-pane-look`,
`nine-tab-settings-walkthrough`).

| Item | Status | Live residual |
|---|---|---|
| Browser-domain extraction | code ✅ (`c131`); `run-a2-compat-gates.sh browser-domain-allow|browser-domain-exclude` validates host-only domain metadata and exclusion blocking; Safari allow+exclude legs live-proven 2026-07-07 (Batch 6) | Chrome/Brave live rows with the A2 matrix; exclusion gate requires `COMPME_A2_BROWSER_EXCLUDED_DOMAIN` |
| Multi-candidate Down-cycle | engine ✅; synthetic Down-cycle live-proven 2026-07-07 (`COMPME_CANDIDATES=3`, real model); `multi-candidate-cycle-physical-look` manual gate pins the physical cycle/accept UX | run the physical Down-arrow gate before the next release |
| Compatibility matrix | classifier ✅; Unsupported tiers fail closed; SidebarOnly remains blocked pending the editor-vs-sidebar detector above; `run-a2-compat-gates.sh matrix` provides exact 13-row execution and TSV ledger as a local/manual tool | after the detector lands, supply live row PID map via `COMPME_A2_MATRIX_TARGETS`; dry runs may explicitly allow skips, while recorded evidence should pass every row and satisfy `check-a2-matrix-ledger.sh` locally |
| Browser mirror-window | `set_mirror_mode` ✅; `mirror-window-firefox-zen-look` manual gate pins Firefox/Zen ghost-in-mirror confirmation | run the manual gate in a granted desktop session |
| Terminal/iTerm AI-prompt | `terminal_prompt_activates` ✅; live gating proven 2026-07-07 (Batch 6: command-line blocked, natural-language allowed) | tuning vs real agent prompts |
| Screen-context OCR | `screen_context_text` ✅; screen context can be enabled live after launch; live submit-path pass 2026-07-07 after CGImageRef encoding panic fix (`e5c055b`) | OCR quality/perf on a granted desktop + multi-display caret confirm |
| Encrypted memory — AllMonitored | core ✅; TextEdit product-loop privacy + runtime-disable proofs + Chrome domain-exclude proof ✅; records only established inserted-text deltas after a baseline, never pre-existing field text; redaction is best-effort and deliberately preserves all-one-case all-letter prose unless a credential key/prefix or entropy signal is present | remaining live residual: snoozed transition, volatile `pid:N` (secure-field fail-closed live-proven 2026-07-07, `f6fa98b`) |
| Per-app memory inspect/delete UI | count/delete_app ✅ | completed live in Apps pane; global `delete_all` and memory-mode controls are deferred UI work, not part of the current Personalization pane |
| Trailing-space toggle | accept-path ✅; `e2e-compme-trailing-space` gate | TextEdit product gate now asserts exact single-word trailing-space readback in deterministic `word-only` mode; real-model E2E must use `full`/`word` because real-model `word-only` fails closed; optional manual UX confirmation remains part of the broad settings walkthrough |
| Strength slider (6 stops) | pure ✅ | live before/after steering at multiple stops |
| Google Docs / Arc onboarding | `needs_accessibility_setup` ✅; `setup-needed-docs-arc-onboarding` manual gate pins setup-needed UX in Arc/Docs | run the manual gate in Arc with Google Docs focused |

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

**Status (2026-07-07):** G1-G5 are implemented and deterministic validation is
green. The portable correction pipeline, macOS trigger/accept routing,
fail-closed range seams, `overlay-correction-presenter`, Apps-pane `GrammarFix`
policy column, grammar-accept recorder/persistence, and correction-accept tap
isolation are in code with focused tests plus `accept_tap_acceptance` correction
requirements. Live-found+fixed: the shipped base (non-instruct) model never
produced corrections until the few-shot prompt fix (`5126509`) plus the worker
`max_tokens` fix (`4c2f8d3`). The current safety boundary uses one-token grammar
generation followed by strict whole-output vetting; the release GGUF probe
passes 7/8 typo fixes with 0/4 false fixes. The historical Batch 5 assisted
session, summarized in [`ACCEPTANCE.md`](ACCEPTANCE.md), live-proved
underline/banner render, in-place accept, and stale-correction refusal with the
real model. Residual: the formal `grammar-fix-textedit-look` A1b gate emitted by
`tools/acceptance/run-a1b-live-gates.sh` (physical trigger/accept keypresses in
a granted macOS GUI session).

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
  **The same `InsertStrategy::supports_atomic_range_replace()` gate** applies
  (see the correction branch in `engine_core/src/lib.rs`): `AxSet` and
  `NativeRangeSet` can offer an atomic correction; non-atomic
  SyntheticKeys/Clipboard/ImeCommit/None fields offer nothing (degrade), exactly
  as replacements do today.
- **Snapshot/staleness safety:** model the correction as a `Showing` with
  `presentation = Correction` and `correction_range = Some(..)`; every
  TextChanged/CaretMoved bumps `generation`/`snapshot` so a correction can't
  apply to stale text (`advance_snapshot` in `engine_core/src/lib.rs`).
- **Word geometry for the underline:** add `PlatformAdapter::text_range_rect` over
  the same scalar `CorrectionRange`. macOS converts scalar offsets to UTF-16 and
  uses `read_ax_bounds_for_range(element, loc, len)` in
  `platform_macos/src/lib.rs`.
  (Do **not** reuse the thin-caret `usable_caret_rect` guard — a word is wider
  than its threshold.)
- **Inference plumbing:** `engine::CompletionRequest` plus app-owned
  `CompletionOutcome` over channels, `LocalModel::complete(prompt, max_tokens)`
  (`model_client/src/lib.rs`), `terse_continuation_prompt` as the
  template for a new `grammar_fix_prompt`.
- **Gates/policy:** `replacement_decision`/`suggestion_gates_pass` in
  `crates/app/src/run_loop.rs`; `AppPolicy` tri-state fields and
  `AppPolicyField` in `crates/prefs/src/lib.rs`.
- **Keystroke infra:** always-on shortcuts `ShortcutBindings`/`registration_plan`
  in `platform_macos/src/lib.rs`, `ShortcutAction` in `platform/src/lib.rs`;
  ghost/correction-scoped accept keymap `AcceptKeymap`/`binding_for_hotkey_id`
  in `platform_macos/src/lib.rs`; recorder UI `KeyRecorderField` in
  `settings_window.rs`.
- **Overlay recipe:** the borderless transparent `NSPanel` in `ensure_panel`
  plus Y-flip in `overlay_frame_for_text` in `platform_macos/src/lib.rs`.

### Build — genuinely new
1. **Correction engine (LLM):** `model_client::grammar_fix_prompt(word, left_ctx)`
   (pure, next to `terse_continuation_prompt`) + a **grammar request kind** on
   `engine::CompletionRequest` and a corrected-word/range field on
   `CompletionOutcome`, routed through the existing worker/`recv_latest` loop.
   `left_ctx` is tail-bounded to `GRAMMAR_LEFT_CTX_CHARS` (400 scalars,
   `run_loop.rs` — the AX field value is unbounded input); the correction
   range stays in full-field coordinates. Like the completion prompt, it is raw
   field text sent only to the local model — never logged or persisted raw.
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
   or a sibling presenter; update `FakeOverlay` in `engine/src/lib.rs` and the
   `ux_mode`/placement plumbing to match. Degrade to a caret-anchored popup when
   `read_ax_bounds_for_range` returns `Ok(None)`.
3. **Two keystrokes:** **grammar-trigger** = new `ShortcutAction::GrammarCheck`
   (always-on Carbon hotkey, new id 8, config `COMPME_GRAMMAR_CHECK_KEY`,
   startup-string first like the other global shortcuts) — routed at the
   `HostEvent::Shortcut` match in `crates/app/src/run_loop.rs` to run detection.
   **grammar-accept** = `AcceptBinding::GrammarAccept` with
   `AcceptAction::Correction`; correction mode consumes only GrammarAccept while
   Word/Full pass through. It gets its own Carbon id, config
   `COMPME_GRAMMAR_ACCEPT_KEY`, and is live-rebindable via
   `RecorderRole::GrammarAccept`. Collision detection stays in the existing field
   arrays (`has_internal_collision` / `record_decision`).
4. **Toggle + policy wiring:** `Config.grammar_fix` (`COMPME_GRAMMAR_FIX`) in
   `crates/app/src/run_loop.rs`, `AppPolicy.grammar_fix: Option<bool>` +
   `grammar_fix_enabled(app, default)` and `AppPolicyField::GrammarFix` in
   `crates/prefs/src/lib.rs`, consulted in the new flow.

### Ordered build sequence (pure/testable first, novel FFI last)
| # | Phase | Effort | Notes |
|---|---|---|---|
| G1 | `grammar_fix_prompt` + output post-filter (model_client, pure) + word-under-caret helper (context) | S | ✅ Implemented with deterministic prompt, vetting, and caret-word tests. |
| G2 | Grammar inference request/outcome kind + worker routing; `CorrectionRange`/`Showing`/`ReplaceRange` wiring; `Config`/`AppPolicy`/`AppPolicyField` toggle wiring | M | ✅ Implemented with fake model/adapter coverage and fail-closed platform stubs. |
| G3 | Two keystrokes: `ShortcutAction::GrammarCheck` + `AcceptBinding::GrammarAccept` registration + routing | M | ✅ Implemented with config parsing, shortcut routing, accept-action isolation, and Carbon plan tests; physical keypress remains part of live LOOK. |
| G4 | Underline + correction-banner overlay (novel FFI) | L | ✅ Implemented with macOS range geometry and correction presenter tests; live visual LOOK remains pending on a granted Mac. |
| G5 | Settings: grammar-accept recorder row + Apps-pane `GrammarFix` column; live validation | M | ✅ Implemented: recorder role/collision handling, live grammar-accept rebind persistence, Apps-pane `GrammarFix` mapping, and env-shadow/config tests are covered. |

### Resolved implementation decisions
- **Underline rendering:** thin filled non-activating sub-panel under the word,
  paired with the correction banner.
- **LLM safety:** completion-native few-shot prompt, one generated token, strict
  single-word post-filter, and edit distance at most two. Grammar does not hide an
  autocorrect-table pre-pass.
- **Trigger with no error found:** silent no-op; no banner.

### Cross-platform architecture (Linux · Windows · macOS)
The portable core (G1-G2, plus policy/settings logic) is **written once** and
shared by all three OSes. Only these four trait surfaces get a per-OS impl; the
new range-bounds/range-replacement methods must land with fail-closed stubs in
every adapter when the shared trait changes:

| Surface | macOS (reference) | Windows | Linux |
|---|---|---|---|
| Global grammar-trigger hotkey | Carbon `RegisterEventHotKey` (`ShortcutBindings`, already built) | `RegisterHotKey` (Win32) | X11 `XGrabKey` / Wayland global-shortcuts portal |
| Correction-scoped grammar-accept key | Carbon accept keymap with correction-scoped `AcceptBinding::GrammarAccept` dispatch (no separate `AcceptArm` enum was needed) | keyboard hook / `RegisterHotKey` | X11/Wayland key grab |
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
remaining macOS risk narrowed 2026-07-07: underline/banner render, in-place
accept, and stale-correction refusal live-proved with the real model (Batch 5
assisted session); residual is the formal `grammar-fix-textedit-look` A1b gate
with physical trigger/accept keypresses.

---

## Out of scope (deliberate — not pending)

- **Payment / licensing tiers / subscriptions / multi-device seats** — compme is
  Apache-2.0, all features open (`a3-settings-ui-design.md:15`). No Subscription
  pane and no telemetry toggle because no analytics/telemetry is sent. Explicit
  user-initiated model downloads and URL navigation are separate network actions.
- **RTL / multilingual** — model/locale-bound, not pure-table features
  (`a2-parity-design.md:89`).
- **Candidate cycling** is an intentional superset beyond Cotypist and is not a
  parity gap. Thesaurus evidence is mixed: public help omits it, while the
  installed-binary audit exposed auto/selection feature flags. The current scope
  therefore tracks shipped trailing-word lookup separately from the pending
  selection-trigger UX above.

---

## macOS completion plan (2026-06-30)

> **Historical status (2026-07-01): this six-item macOS UI backlog is
> CODE-COMPLETE.** All six residuals below are done in code (the last gap — the Personalization multi-line
> instructions field, item 5 — shipped in `256eb14`), verified by a full-codebase
> review + tdd + ponytail pass (1829 tests, clippy clean). This historical claim
> covers the six rows only; the broader parity audit now tracks the five committed
> code gaps above in addition to a human visual-LOOK pass over the 9 settings
> panes and the Tier-4 live checklist. Developer-ID
> signing, notarization, and the first stable tags are complete through v0.1.4;
> a full native auto-updater remains optional. The authoritative live-gate ledger is `docs/ACCEPTANCE.md`
> (Manual/Live Gate Ledger); `docs/MANUAL-VALIDATION.md` carries the detailed
> walkthroughs.

**Setup-pane cleanup (2026-07-10, current `main` only; not in v0.1.4):** the
redundant conditional **Reveal Model in Finder** control was removed; the
always-visible **Show Models Folder** is the single model-location action
alongside **Choose Model…** and **Download Model**.
The `setup-model-picker-look` manual gate must verify exactly one **Show Models
Folder** control is visible and that **Reveal Model in Finder** is absent.

**Directive: finish macOS first.** The cross-platform adapters (1.1) remain
environment-gated on Windows/Linux build+test systems. Signed macOS distribution
is shipped; the optional native updater does not block the remaining LOOK work.

Verified complete-list facts (2026-06-30 plan-review pass): there is **no Tier
1.3**, and **Tier 2 is a single ✅ DONE item (2.1)**. The six rows below were
the remaining **macOS-buildable code backlog** at that point; the current
readiness surface is broader because `docs/ACCEPTANCE.md` now pins 17
manual/live gate IDs for visual LOOK checks, caret-marker calibration,
Input-Monitoring-revoked Carbon-accept proof, and other live-only evidence.
Correction to an earlier note: the **F2 insertion-order decision is already
shipped** — a fixed `AxSet → SyntheticKeys → Clipboard → None` chain
(`platform_macos/src/lib.rs` `insertion_strategy()`), not paste-first and not
per-app configurable.

### Ordered build sequence (lowest-risk / decision-free first)

| # | Item (tier) | Effort | Why this slot |
|---|---|---|---|
| 1 | ✅ **DONE (2026-06-30)** — Emoji gendered + skin-tone ZWJ assembly | S–M | Shipped: `with_skin_tone_zwj` splices the Fitzpatrick modifier into the base of the gendered ZWJ sequence (`emoji/src/lib.rs`). 27 tests pass, clippy clean. |
| 2 | ✅ **DONE (2026-06-30, closed without picker)** — Statistics metric selector (3.3) | S / 0 | Decision taken: keep the existing layout, no `NSPopUpButton`. A single-select picker trades away at-a-glance comparison for an unrequested control. The `StatMetric`/`metric_series` scaffold has since been **removed** (a later ponytail pass cut it — zero references remain in `crates/`). |
| 3 | 🟢 **CODE-COMPLETE — VISUAL LOOK pending (2026-07-01)** — Apps-pane editing rows (3.1) | M | Core + AppKit shell landed. `editAppPolicy:` checkboxes → `apps_edit` signal → run-loop resolves row→app → `set_app_policy_field` → persist. **LAYOUT BUG found + fixed (2026-07-01, `f5a81c5`):** the geometry-check pass caught a real overlap — each app was laid across 2 lines but rows advanced only 26px, so every row's policy checkboxes rendered *on top of the next app's name* (28 collisions, only visible with 2+ apps; headless "0 panics" validation couldn't see it). Redesigned to a **compact one-line grid** (name + 5 title-less checkbox columns under an `App | On Tab Mid AC GF` header + tooltips + Delete), all 8 apps fit, zero overlap, pinned by `apps_pane_grid_has_no_overlaps_within_budget` (mutation-verified). **Pre-check also resolved** — `compose_apps_policy_bits` publishes live per-app bits on show, seeded via `refresh_apps_policy_checkbox_states`. **Still needs eyes/fingers (pure visual LOOK):** bare-checkbox column look, name truncation, toggling changes behavior. |
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
   success (`insert_impl` SyntheticKeys/Clipboard branches,
   `crates/platform_macos/src/lib.rs`); replacements already fail closed.
4. **Non-atomic replacement support** — decide after the expanded compatibility
   and manual Tier-4 pass
   whether SyntheticKeys/Clipboard fields need explicit backspace-synthesis for
   emoji/autocorrect/grammar replacements. Default: keep the current fail-closed
   atomic-only behavior unless the compatibility pass proves it blocks an
   in-scope macOS app.

### Current execution order

1. **Close the five committed macOS/A2 code gaps above** (or record an explicit
   product-scope decision for any deliberate deferral), with deterministic tests.
2. **Run all 17 runner-pinned macOS manual/live gates plus the additional
   manually recorded Tier-4 rows** using the current local binary;
   record each result in `docs/ACCEPTANCE.md`. Prioritize the nine-tab Settings
   walkthrough, Setup single-location-control invariant, Apps/Personalization,
   physical hotkeys + grammar fix, Chromium-family caret calibration, and memory
   privacy residuals.
3. ✅ **Synchronize plan/support/acceptance docs (2026-07-10)** — this update
   refreshed the implementation anchor, atomic-replacement wording, Apps/memory
   status, Setup control invariant, A2 local-only policy, and platform-support
   matrix. Repeat the sync whenever manual gates close or adapter phases ship.
4. **Settle non-atomic replacement scope** from that compatibility evidence; do
   not add backspace synthesis speculatively.
5. **Windows Phase 1 (1.1–1.7)** — UIA read/caret, keyboard hook, insertion,
   layered overlay, ShellHost services, and native acceptance on Windows hardware.
6. **Linux Phase 2 (2.1–2.7), X11-first** — start with the two-day accept-key
   strategy spike, then AT-SPI2, insertion, overlay, ShellHost, and Xvfb fixtures.
7. **Wayland Phase 3 decision spike** — compare IME and portal/global-shortcut
   paths on GNOME, KDE, and sway before committing to an implementation.
8. **Off-mac runtime and distribution** — per-OS GPU baselines, Windows/Linux
   packaging/signing/publication, and feature-by-platform acceptance/docs after
   the corresponding adapter is functional.
9. **Settle repository governance with the owner** — decide whether to protect
   `main`, restrict release-tag creation and deployment branches, prevent
   release-environment self-review, and align GitHub's Actions allowlist/SHA-pin
   policy with the repository checker. Record any deliberately accepted trust
   boundary if direct-to-`main` remains the chosen workflow.
10. **Tier 1.2 optional updater** — replace the release-page handoff only with a
   signature-verifying native updater design; this remains non-blocking.
