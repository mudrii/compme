# compme — Roadmap & Pending Work

> **Last updated:** 2026-06-17 (third TDD audit coverage pass) · **Branch:** `main` · **Tests:** full deterministic gates green on macOS (≈1169 workspace tests; spike separate)
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
  and `COMPME_INSTRUCTIONS_DOMAINS` /
  `COMPME_INSTRUCTIONS_DOMAIN_<TARGET>` into
  `PersonalizationProfile.per_domain` (`crates/app/src/run_loop.rs`,
  `build_personalization`).
- Ambiguous target suffixes are ignored instead of applying one value to
  multiple apps/domains (`instruction_map_from_config` in `run_loop.rs`).
- Inference now calls
  `profile.build_preamble(Some(&request.field.app), request.domain.as_deref())`
  (`crates/app/src/inference.rs:297-302`), so resolved browser domains can
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

**Remaining:** no code/test gap for instruction steering. The user-facing
settings editor for these values remains part of Tier 3.2.

---

## Tier 3 — A3 settings-UI residuals (medium, build-then-LOOK)

Per `2026-06-10-a3-settings-ui-design.md`. The window ships as 8 tabs
(Setup/General/Apps/Context/Emoji/Shortcuts/Statistics/About via NSTabView). Backing
config + crates exist for the remaining panes; what's missing is narrower UI
surface: Apps editing rows, a Personalization pane (mode/strength/instructions),
the Statistics metric picker, the Context appearance sub-toggle, and the new
Shortcuts hotkeys.

> **Autonomous-loop status (2026-06-15):** the cleanly loop-doable Tier-3
> controls have shipped — Statistics **range** + **grouping** pickers (3.3) and
> the Emoji **gender** picker (3.2), plus the pure foundations for the Shortcuts
> hotkeys (3.4) and Statistics chart model. The **remaining** items are
> design-gated or need a runtime-application refactor, not clean FFI-over-pure-layer
> (see each below) — they are handed off rather than blind-built. Live UX gates
> for what shipped are in [`MANUAL-VALIDATION.md`](MANUAL-VALIDATION.md).

### 3.1 ☐ Per-app override *editing* rows (Apps pane) — the largest residual pane
- **Status:** Apps pane is **display + delete only** — per-app recorded-input
  count rows (`settings_window.rs:1171-1193`) with per-row Delete buttons gated
  by `apps_row_is_deletable`. No add-app control, no per-row
  enable / mid-line / autocorrect / Tab-disable toggles.
- **Backing exists:** `prefs` per-app override fields + `tab_disabled` tap
  suppression are live; only the editing UI is missing.
- Spec: `a3-settings-ui-design.md:50,78` (Phase S2 "App Settings pane — largest").

### 3.2 ◑ Dedicated Personalization / Context / Emoji panes
- **Status:** Context now exists as a dedicated settings tab with clipboard and
  screen-OCR context switches (`pane_titles` includes `Context`;
  `settings_window.rs` renders the two switch rows and writes
  `context_clipboard` / `context_screen` atomics; `run_loop.rs` initializes them
  from config, persists switch edges, clears disabled context cells, and gates
  screen submissions by the current config). General
  carries 4 switches —
  `general_enabled`, `labs_midline` (mid-line, moved here from Labs),
  `general_autocorrect`, `general_trailing_space` (`settings_window.rs:926-1045`).
  Emoji now exists as a dedicated tab with a live `COMPME_EMOJI` enable switch
  `COMPME_EMOJI_SKIN_TONE` popup:
  `pane_titles` includes `Emoji`; `settings_window.rs` renders the rows and
  writes `emoji_enabled` / `emoji_skin_tone_index`; `run_loop.rs` initializes
  them from config and persists switch and skin-tone edges.
- **Emoji gender ✅ DONE (`6366f64`):** a `COMPME_EMOJI_GENDER` popup
  (Neutral/Female/Male) below the skin-tone popup, mirroring the skin-tone
  feature (`emoji_gender_index` + `handle_emoji_gender_change`, unit-tested). The
  **Emoji pane is now complete** (enable + skin-tone + gender).
- **Pending — Personalization pane (🔒 design/refactor-gated, NOT clean FFI):**
  mode (AcceptedOnly/AllMonitored), 6-stop strength, instructions editor. Backing
  is parsed at startup (`build_personalization`, `parse_storage_mode`), but the
  `PersonalizationProfile` is **moved into the inference worker** at startup
  (`inference.rs`), so a *runtime-applying* control needs shared-mutable-profile
  threading (a refactor + design choice); **mode** changes also need encrypted-store
  open/close lifecycle; the **instructions** editor is a novel text-input + persist-timing
  UX decision. Persist-only "applies next launch" is possible but is itself a UX
  call. Context appearance sub-toggle remains deferred. Spec:
  `a3-settings-ui-design.md:46,47,48,73`.

### 3.3 ◑ Statistics range / group / metric controls — range + group DONE
- **Range picker ✅ DONE (`48f7fc5`):** an NSPopUpButton (Last 7/14/30 days)
  drives the `daily_buckets` span via `StatRange::from_index().days()`.
- **Grouping picker ✅ DONE (`3722a1d`):** a second popup (Daily/Weekly)
  re-buckets the rows via `stats::group_buckets`; `metric_series` was refactored
  onto it so the weekly chunk-of-7 rule lives once. Both pickers are bare
  self-describing popups on the header row.
- **Metric picker — deferred (design):** the pane renders one sparkline row per
  metric (shown/accepted/words) already, so a metric *selector* implies a
  single-metric-chart redesign — arguably already satisfied by the 3-row layout.
  The pure selection model (`StatMetric::{ALL,label,from_index}` + `metric_series`)
  is shipped and unit-tested, ready if a redesign is chosen.
- Spec: `a3-settings-ui-design.md:52`.

### 3.4 ◑ Shortcuts pane — recorder + parse foundation done; new hotkeys gated
- **Status:** ✅ `KeyRecorderField` rows + live rebind + modifier-combo capture
  (⌃⌥⇧⌘) are DONE and live-validated. **Parse foundation ✅ DONE (`52f1bc6`):**
  `ShortcutBindings::from_config` parses `COMPME_FORCE_ACTIVATE_KEY` /
  `_TOGGLE_APP_KEY` / `_TOGGLE_GLOBAL_KEY` (+ internal-collision check), unit-tested.
- **Pending — registration + actions (🔒 design/novel-FFI-gated):** the three
  hotkeys need **always-on** Carbon registration (a new lifecycle — accept keys
  are *transient*, armed only while a suggestion shows) and on-fire behavior.
  toggle-app / toggle-global mirror the existing tray disable submenus, but
  **force-activate's semantics ("force a completion now") are an unresolved design
  decision**, and persistent global-hotkey registration + fire-handling is novel
  FFI requiring live validation. Spec: `a3-settings-ui-design.md:49,75`.

### 3.5 ☐ Emoji `includeVanillaVariants` (deferred by design)
- Deferred: an alternate vanilla glyph has no display path in the single-ghost
  replacement pipeline. Revisit when a multi-candidate replacement *display*
  exists. Spec: `a3-settings-ui-design.md:64`.

> **Corrected 2026-06-15:** the global disable submenu (For 1 Hour / Until
> Relaunch / Always) is **✅ DONE** (`crates/platform_macos/src/tray.rs:238-246`,
> `DisableArm` `:53-59`; mapped in `run_loop.rs:3357-3370` through
> `apply_global_disable`). The older "NOT built — only flat Snooze-1h" note is
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
| Encrypted memory — AllMonitored | core ✅; positive TextEdit product-loop privacy proof ✅ | remaining live residual: secure input, snoozed/runtime transition, browser-domain exclusion, volatile `pid:N` |
| Per-app memory inspect/delete UI | count/delete_app ✅ | completed live in Apps pane; remaining global delete_all/mode controls tie to Personalization |
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
