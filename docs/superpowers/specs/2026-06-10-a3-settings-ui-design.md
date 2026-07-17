# A3 Settings UI + Tray Menu — Cotypist-Reference Plan

> **Status annotations updated 2026-07-17:** window ships as 9 tabs
> (Setup/General/Personalization/Apps/Context/Emoji/Shortcuts/Statistics/About)
> via NSTabView. The Context tab exposes clipboard + screen-OCR context
> switches, the Emoji tab exposes the live `COMPME_EMOJI` enable switch plus the
> `COMPME_EMOJI_SKIN_TONE` and `COMPME_EMOJI_GENDER` popups, and the
> Personalization tab edits global instructions, sender identity, and steering
> strength. The Statistics tab exposes the shipped range/grouping controls; their
> LOOK gate was completed 2026-06-17. A metric-picker redesign was closed by
> design in the Roadmap.
>
> **Current-main release boundary:** the latest published artifact is v0.1.5
> (`14ae81e`) and includes the single model-location-control cleanup (one
> **Show Models Folder**, no **Reveal Model in Finder**). Current `main` also
> contains the tray website/support actions described below.
>
> **Live pending status (re-verified 2026-07-17): see [`docs/ROADMAP.md`](../../ROADMAP.md)**
> and [`docs/ACCEPTANCE.md`](../../ACCEPTANCE.md) — the remaining Tier 3 work is
> LOOK validation for shipped controls: Apps policy grid, Personalization pane,
> Shortcuts recorder/physical hotkeys, Setup model picker, menu-bar icon, and the
> nine-tab walkthrough. The tray's Visit Website / Contact Support actions are
> code-complete and their browser handoff is pinned by
> `tray-external-links-look`; inline per-line annotations below are historical where a
> dated correction supersedes them.

Reference: 13 Cotypist 2026.1 screenshots captured 2026-06-10 (tray menu +
every settings pane). This maps each surface to compme's existing backing and
sequences the build. Native AppKit, no web view (engine-macos §9 A3 "no
Tauri"). Most logic already exists as crates + `COMPME_*` env config; the UI
is a front-end over `config.env` via `config::persist_setting` (c50) plus the
live tray flags.

## Deliberate divergences (Project Scope)

- **No Subscription pane** — compme is Apache-2.0, all features open.
- **No telemetry toggle** — Cotypist's "share anonymous usage statistics"
  pane has no compme equivalent because Compme sends no analytics/telemetry.
  Explicit user-initiated model downloads and URL navigation are separate
  network actions. The About pane states the no-telemetry policy instead.
- **No Sparkle auto-updater initially** — GitHub-release **Check for Updates...**
  handoff is shipped; Sparkle/appcast remains optional later.

## Phase S1 — Tray menu extensions (loop-doable now)

Cotypist tray: per-app disable submenu, global disable submenu, per-app
input-collection submenu, Settings (⌘,), support links, updates, Quit.

| Item | Backing | Status |
|---|---|---|
| Disable Completions in <frontmost app> ▸ (1h / until relaunch / always) | `prefs` per-app exclude + snooze-style timer keyed by app | shipped for current-frontmost app arms (1h / until relaunch / always); remaining polish is stateful submenu text/dynamic app-name LOOK validation |
| Disable Completions Globally ▸ (1h / until relaunch / always) | snooze (c54) = the 1h arm; "always" = enable toggle (c50) | shipped for 1h / until relaunch / always via `apply_global_disable`; remaining polish is stateful submenu text/dynamic app-name LOOK validation |
| Input Collection in <app> ▸ | `memory` per-app collection override in `prefs` | shipped as a current-app toggle with persistence and run-loop gates; remaining polish is submenu state/dynamic app-name LOOK validation |
| Settings… ⌘, | opens the S2 window | done |
| Visit Website / Contact Support | repo URLs through the portable `ShellHost::open_url` seam | shipped as one-shot tray actions with exact URL tests; live menu-click/browser-opening LOOK remains |
| Quit | exists | done |

## Phase S2 — Settings window skeleton + panes

Sidebar + pane layout (NSWindow + NSTableView/NSStackView or SwiftUI-free
AppKit). Pane order mirrors Cotypist. Every toggle persists through
`persist_setting` → `config.env` → read at launch (env still wins).

| Pane | Cotypist contents | compme backing | Gap |
|---|---|---|---|
| Setup | permission states (AX, Screen Recording), model downloaded, macOS text-suggestions off, clipboard context | `accessibility_trusted`, `screen_recording_permission`, model_select, compat | pane only; "disable macOS suggestions" helper is new — **[2026-06-10] DONE** (checklist + Grant/Request/Reveal buttons + 480ms visible-only poll) |
| General | launch-at-login; menu-bar icon toggle; accessory button; model picker + folder reveal; enable-by-default; max length (S/M/L); autocorrect toggles | SMAppService (bundle exists, c80); tray exists; model_select + `COMPME_MODEL_PATH`; `COMPME_ENABLED`; `COMPME_MAX_WORDS`; `COMPME_AUTOCORRECT` | accessory floating button = new feature (defer) — **[2026-06-10] DONE** for 3 live switches (mid-line/autocorrect/trailing-space); launch-at-login wiring done via SMAppService; **[2026-06-17] model catalog/download shipped in Setup** with picker, RAM labels/gate, license gate, SHA verify, and dest-exists guard; **[2026-07-10, current main]** Setup has exactly one model-location action, **Show Models Folder**, and no **Reveal Model in Finder**; residual model work is recovery/visual LOOK validation |
| Context | screenshots-for-context (+appearance sub-toggle); clipboard | `COMPME_SCREEN_CONTEXT`, `COMPME_CLIPBOARD_CONTEXT` | dedicated Context tab with clipboard + screen-OCR switches shipped; screen enable starts OCR live when Screen Recording is granted, while denial reverts the switch, and screen disable immediately drops the worker, clears cached OCR, and gates new submissions; appearance sub-toggle has no equivalent (defer) |
| Personalization | collect typing history; store-without-accepts; word-choice strength slider; existing-data count + Delete All; custom AI instructions editor | `memory` modes (AcceptedOnly/AllMonitored!), `count`/`delete_all`; personalization 6-stop strength; `COMPME_INSTRUCTIONS`, `COMPME_INSTRUCTIONS_APPS` / `_APP_*`, `COMPME_INSTRUCTIONS_DOMAINS` / `_DOMAIN_*` | global instructions, sender identity, and 6-stop strength controls shipped in the dedicated Personalization tab; layout/edit persistence is live-validated, and the remaining LOOK is a visible steering effect. Memory mode/global delete controls are tracked separately from the profile editor |
| Emoji | enable; skin tone; **include neutral variant**; gender | `COMPME_EMOJI`, `_SKIN_TONE`, `_GENDER` | enable switch plus skin-tone and gender popups shipped in a dedicated Emoji tab; `includeVanillaVariants` is unmodeled and deferred until multi-candidate replacement display exists |
| Shortcuts | word key (+trailing-space toggle); full key; force-activate; per-app temp toggle shortcut; global toggle shortcut | `AcceptKeymap` (c13) + `COMPME_TRAILING_SPACE`; `KeyRecorderField` rows persist live rebinds through `COMPME_ACCEPT_*` config; always-on force/toggle/grammar-check hotkeys are config-backed at startup | recorder UI and live rebind are implemented for Word, Full, and Grammar accept; modifier persistence is synthetic-validated. Force-activate, per-app toggle, global toggle, and grammar-check remain config-only startup bindings whose physical dispatch needs LOOK validation |
| App Settings | per-app list (usage counts) with enable/mid-line/autocorrect/Tab-disable, compat mode, per-app instructions, per-app history | `prefs` overrides + `tab_disabled` tap suppression; `memory` per-app counts; personalization per-app maps (config-wired; editor missing) | Apps tab ships recorded rows, count/delete, and On/Tab/Mid/AC/GF policy columns; column readability is live-validated. Remaining LOOK is name truncation and physical toggle-to-behavior confirmation |
| Labs | mid-line toggle | `COMPME_MIDLINE` | pane only — **[2026-06-10] DONE** (shipped as a switch in the General tab — the Labs pane content moved to General) |
| Statistics | today + 30-day charts (range/group/metric) | `stats` crate (counts/words/latency) — menu line shipped c51 | current scope complete: sparkline rows + lifetime row + stats.env persistence + range/grouping popups and LOOK evidence shipped; a single metric picker was closed by design because separate rows preserve comparison |
| About | version, acks, links | LICENSE, deps | pane only; states the no-analytics/telemetry guarantee — **[2026-06-10] DONE** |

## Build order (each loop-tick-sized unless noted)

1. **S1 tray submenus** — per-app timed disable + global submenu + per-app
   input-collection (pure prefs additions + tray plumbing; pattern = c54
   snooze). **[2026-06-15 DONE] Per-app timed disable ▸ DONE; the GLOBAL
   disable submenu (For 1 Hour / Until Relaunch / Always) IS built
   (global submenu + `DisableArm` in `crates/platform_macos/src/tray.rs`, mapped
   and consumed through `apply_global_disable` in `crates/app/src/run_loop.rs`);
   the per-app disable counterpart also exists
   (`crates/platform_macos/src/tray.rs`). Flat Snooze-1h still coexists. Input-collection =
   single toggle (works, persists; stateful submenu text is the only polish
   residual). _(Supersedes the stale 2026-06-11 note that claimed the global
   submenu was not built — it predated the 06-11 build.)_**
2. ~~Emoji `includeVanillaVariants`~~ **DEFERRED (corrected 2026-06-10)**:
   not a small crate change — an alternate vanilla glyph has no display path
   in the single-ghost replacement pipeline. Revisit when a multi-candidate
   replacement display exists.
3. **Launch-at-login** via SMAppService (bundle exists; default-off, D13).
   **[2026-06-10] DONE** (wired via SMAppService).
4. **S2 window skeleton** + the pure panes first (Labs, Emoji, Context,
   Personalization — backing complete, persistence via persist_setting).
   **[2026-06-15] PARTIAL** — skeleton DONE (8 tabs via NSTabView); Labs DONE
   as a General-tab switch; Context DONE for clipboard + screen-OCR switches;
   Emoji DONE for the enable switch, skin-tone popup, and gender popup;
   Personalization controls and the Context appearance sub-toggle deferred.
5. **Shortcuts pane** + keymap threading (closes the c13 residual) —
   **[2026-06-15] DONE for recorder UI/live rebind** via `KeyRecorderField`
   rows and run-loop persistence for Word, Full, and Grammar accept. Force-
   activate, per-app toggle, global toggle, and grammar-check shortcuts are
   implemented as config-only startup bindings; adding recorder rows for those
   always-on shortcuts is a future UI enhancement, not part of the shipped pane.
6. **App Settings pane** (largest; needs the new per-app prefs fields).
7. Statistics charts; Setup/onboarding pane; About. **[2026-06-10] DONE**
   (Statistics DONE-MVP as sparklines; Setup and About panes shipped).
8. Out of scope here: model catalog/download manager (§15 D14; since shipped
   into the Setup tab — download button c122, sha verify c126, license gate
   c127 **[2026-06-12]**), accessory
   floating button, Sparkle/appcast auto-update client — separate; GitHub
   Release handoff + update manifest are shipped.

GUI panes require the live-LOOK validation listed in the status header (human or
scripted screenshot reads); underlying toggles stay env/config-file-backed so
their behavior remains headless-testable.
