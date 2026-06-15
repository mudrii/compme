# compme — Roadmap & Pending Work

> **Last re-analyzed:** 2026-06-15 (re-validated, gate run) · **Branch:** `spike/a0` · **Tests:** full deterministic gates passed; root listed `1111` tests and spike listed `30` tests
>
> This document cross-references the plan specs in
> [`docs/superpowers/specs/`](superpowers/specs/) against the implemented code and
> records, in detail, what remains. It is the single source of truth for "what's
> pending" — kept in sync as items ship. Status claims here are evidence-backed
> with `file:line` anchors verified 2026-06-15.

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

### 1.1 ☐🔒 Cross-platform adapters (Windows + Linux)

**Plan:** `README.md:10` — *"macOS ships first; Windows and Linux are committed
deliverables built behind a shared cross-platform `PlatformAdapter` contract."*
The `platform` crate was deliberately shaped as a trait/contract to accept them.

**Status:** Only `platform` (contract) and `platform_macos` (impl) exist. No
`platform_windows`, no `platform_linux` crate. This is the single largest unbuilt
block and the biggest gap between the README's public promise and the code.

**Pending:**
- `crates/platform_windows/` — UI Automation (UIA) for text-field read/insert,
  `WH_KEYBOARD_LL` low-level keyboard hook for the accept key, a layered overlay
  window for the ghost, foreground-window/process identity for per-app gating.
- `crates/platform_linux/` — AT-SPI2 for accessibility read/insert, XTEST / `wtype`
  for synthetic keys, an IBus IME path for Wayland (where synthetic injection is
  restricted), X11 + Wayland overlay surfaces.
- Both must satisfy the existing `platform` trait so `app`/`engine` need no changes.

**Effort:** Very large, multi-phase (each platform is its own A-sized milestone).
**Recommendation:** Scope as a dedicated milestone, not a loop tick. Until then,
the README line should be read as "planned," and CI cannot enforce it.

### 1.2 ☐🔒 Distribution hardening (signing, notarization, updater)

**Plan:** `2026-06-03-engine-macos-mvp-design.md §9` (A3 ship) — Developer-ID
signing + hardened runtime + notarization + a native updater.

**Status:**
- Signing is **ad-hoc only**: `tools/bundle/make-app.sh:29` runs
  `codesign --force --sign -` (ad-hoc), `:30` verifies. No `notarytool`,
  `stapler`, or Developer-ID identity anywhere.
- **No Sparkle / auto-updater** in code (only mentioned as a future candidate in
  design docs; `2026-06-10-a3-settings-ui-design.md:19` defers it explicitly).
- **No `v*` git tags** yet (`git tag -l 'v*'` empty), so the Homebrew cask
  scaffolding (`Casks/compme.rb`, `.github/workflows/release.yml`,
  `tools/release/update-cask.sh`) is in place but not yet resolvable.

**Pending:**
- Developer-ID Application signing + `--options runtime` (hardened runtime) in
  `make-app.sh`, then `xcrun notarytool submit … --wait` + `xcrun stapler staple`.
- Sparkle integration (appcast feed, `SUFeedURL`, EdDSA-signed updates) **or** a
  GitHub-release-driven "Check for Updates" — its own ship item.
- Cut the first `v*` tag once signing lands → release workflow produces the
  notarized zip + sha256 → cask becomes installable.

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
  (`crates/app/src/run_loop.rs:832-840`) and `COMPME_INSTRUCTIONS_DOMAINS` /
  `COMPME_INSTRUCTIONS_DOMAIN_<TARGET>` into
  `PersonalizationProfile.per_domain` (`run_loop.rs:841-846`).
- Ambiguous target suffixes are ignored instead of applying one value to
  multiple apps/domains (`instruction_map_from_config`, `run_loop.rs:859-887`).
- Inference now calls
  `profile.build_preamble(Some(&request.field.app), request.domain.as_deref())`
  (`crates/app/src/inference.rs:297-302`), so resolved browser domains can
  activate per-domain steering.
- The submit path reads the cached browser domain into `RequestLogContext`
  (`run_loop.rs:3735-3740`), and `submit_request_and_track` copies it onto the
  request before dispatch (`run_loop.rs:1178-1180`). Existing per-app keying
  remains by canonical bundle id.

**Coverage:**
- `personalization_built_from_per_app_and_domain_config_keys`
  (`run_loop.rs:3888-3940`) covers config population, missing values, normalized
  domains, and combined global/app/domain preambles.
- `personalization_skips_ambiguous_per_target_instruction_keys`
  (`run_loop.rs:3942-3970`) covers collision handling.
- `per_domain_personalization_uses_request_domain`
  (`crates/app/src/inference.rs`) covers runtime domain steering.
- Focused revalidation passed on 2026-06-15:
  `cargo test -p app personalization_built_from_per_app_and_domain_config_keys`,
  `cargo test -p app personalization_skips_ambiguous_per_target_instruction_keys`,
  and `cargo test -p app per_domain_personalization_uses_request_domain`.

**Remaining:** no code/test gap for instruction steering. The user-facing
settings editor for these values remains part of Tier 3.2.

---

## Tier 3 — A3 settings-UI residuals (medium, build-then-LOOK)

Per `2026-06-10-a3-settings-ui-design.md`. The window ships as 6 tabs
(Setup/General/Apps/Shortcuts/Statistics/About via NSTabView). Backing config +
crates exist for all of these; what's missing is UI surface.

### 3.1 ☐ Per-app override *editing* rows (Apps pane) — the largest residual pane
- **Status:** Apps pane is **display + delete only** — per-app recorded-input
  count rows (`settings_window.rs:293`) with per-row Delete buttons (`:581`,
  gated by `apps_row_is_deletable` `:563`). No add-app control, no per-row
  enable / mid-line / autocorrect / Tab-disable toggles.
- **Backing exists:** `prefs` per-app override fields + `tab_disabled` tap
  suppression are live; only the editing UI is missing.
- Spec: `a3-settings-ui-design.md:50,78` (Phase S2 "App Settings pane — largest").

### 3.2 ☐ Dedicated Personalization / Context / Emoji panes
- **Status:** Do not exist as panes. General carries 4 switches —
  `general_enabled`, `labs_midline` (mid-line, moved here from Labs),
  `general_autocorrect`, `general_trailing_space` (`settings_window.rs:904-1016`).
- **Pending:** Personalization pane (mode AcceptedOnly/AllMonitored, instructions
  editor, 6-stop strength slider); Context pane (screenshot-context + clipboard
  toggles — `COMPME_SCREEN_CONTEXT`/`COMPME_CLIPBOARD_CONTEXT` backing exists);
  Emoji pane (skin tone / gender — `COMPME_EMOJI`/`_SKIN_TONE`/`_GENDER` backing
  exists). Spec: `a3-settings-ui-design.md:46,47,48,73`.

### 3.3 ☐ Statistics range / group / metric controls
- **Status:** Sparklines only — fixed shown/accepted/words rows + lifetime row
  (`run_loop.rs:1841-1850`, `stats::sparkline` at `stats/src/lib.rs:87`). No
  range/group/metric pickers. Spec: `a3-settings-ui-design.md:52` ("DONE-MVP …
  range/group/metric controls deferred").

### 3.4 ◑ Shortcuts pane — recorder done; new hotkeys pending
- **Status:** ✅ `KeyRecorderField` rows + live rebind + modifier-combo capture
  (⌃⌥⇧⌘) are DONE and live-validated. **Pending:** force-activate hotkey, per-app
  temp-toggle shortcut, global-toggle shortcut (three *new* hotkeys, not yet
  surfaced). Spec: `a3-settings-ui-design.md:49,75`.

### 3.5 ☐ Emoji `includeVanillaVariants` (deferred by design)
- Deferred: an alternate vanilla glyph has no display path in the single-ghost
  replacement pipeline. Revisit when a multi-candidate replacement *display*
  exists. Spec: `a3-settings-ui-design.md:64`.

> **Corrected 2026-06-15:** the global disable submenu (For 1 Hour / Until
> Relaunch / Always) is **✅ DONE** (`crates/platform_macos/src/tray.rs:238-246`,
> `DisableArm` `:53-59`; mapped in `run_loop.rs:1668-1680` and consumed at
> `run_loop.rs:3122-3143`). The older "NOT built — only flat Snooze-1h" note is
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
| Encrypted memory — AllMonitored | core ✅ (AcceptedOnly live-validated) | AllMonitored live privacy gate (redacted typed runs, secure/disabled/snoozed/excluded blocks) |
| Memory inspect/delete UI | count/delete_all/delete_app ✅ | settings-pane inspect/delete surface (ties to Tier 3.2) |
| Trailing-space toggle | accept-path ✅ | live evidence for exact inserted text |
| Strength slider (6 stops) | pure ✅ | live before/after steering at multiple stops |
| Google Docs / Arc onboarding | `needs_accessibility_setup` ✅ | live Docs round-trip |

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

## Recommended sequencing

1. **Tier 3** settings panes (3.2 Personalization/Context/Emoji, then 3.1 Apps
   editing rows) — FFI, build-then-LOOK like the model picker / recorder.
2. **Tier 1.2** distribution — wire notarization the moment a Developer ID is
   available; cut the first `v*` tag.
3. **Tier 1.1** cross-platform adapters — a dedicated milestone of their own.
4. **Tier 4** — opportunistic, whenever a macOS GUI session is available.
