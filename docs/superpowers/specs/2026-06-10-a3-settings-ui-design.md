# A3 Settings UI + Tray Menu — Cotypist-Reference Plan

> **Status annotations updated 2026-06-10 (cycles c95-c115):** window shipped
> as 6 tabs (Setup/General/Apps/Shortcuts/Statistics/About) via NSTabView.

Reference: 13 Cotypist 2026.1 screenshots captured 2026-06-10 (tray menu +
every settings pane). This maps each surface to compme's existing backing and
sequences the build. Native AppKit, no web view (engine-macos §9 A3 "no
Tauri"). Most logic already exists as crates + `COMPME_*` env config; the UI
is a front-end over `config.env` via `config::persist_setting` (c50) plus the
live tray flags.

## Deliberate divergences (Project Scope)

- **No Subscription pane** — compme is Apache-2.0, all features open.
- **No telemetry toggle** — Cotypist's "share anonymous usage statistics"
  pane has no compme equivalent because nothing is ever sent (local-only).
  The About pane states this instead.
- **No Sparkle/Check-for-Updates initially** — updater is its own A3 ship
  item (engine-macos §9).

## Phase S1 — Tray menu extensions (loop-doable now)

Cotypist tray: per-app disable submenu, global disable submenu, per-app
input-collection submenu, Settings (⌘,), support links, updates, Quit.

| Item | Backing | Status |
|---|---|---|
| Disable Completions in <frontmost app> ▸ (1h / until relaunch / always) | `prefs` per-app exclude + snooze-style timer keyed by app | exclude exists; per-app TIMED disable is new pure prefs work |
| Disable Completions Globally ▸ (1h / until relaunch / always) | snooze (c54) = the 1h arm; "always" = enable toggle (c50) | submenu wiring only |
| Input Collection in <app> ▸ | `memory` per-app — needs per-app collection override in prefs | new pure prefs field + gate |
| Settings… ⌘, | opens the S2 window | done |
| Visit Website / Contact Support | repo URLs, `NSWorkspace.open` | trivial |
| Quit | exists | done |

## Phase S2 — Settings window skeleton + panes

Sidebar + pane layout (NSWindow + NSTableView/NSStackView or SwiftUI-free
AppKit). Pane order mirrors Cotypist. Every toggle persists through
`persist_setting` → `config.env` → read at launch (env still wins).

| Pane | Cotypist contents | compme backing | Gap |
|---|---|---|---|
| Setup | permission states (AX, Screen Recording), model downloaded, macOS text-suggestions off, clipboard context | `accessibility_trusted`, `screen_recording_permission`, model_select, compat | pane only; "disable macOS suggestions" helper is new — **[2026-06-10] DONE** (checklist + Grant/Request/Reveal buttons + 480ms visible-only poll) |
| General | launch-at-login; menu-bar icon toggle; accessory button; model picker + reveal; enable-by-default; max length (S/M/L); autocorrect toggles | SMAppService (bundle exists, c80); tray exists; model_select + `COMPME_MODEL_PATH`; `COMPME_ENABLED`; `COMPME_MAX_WORDS`; `COMPME_AUTOCORRECT` | launch-at-login wiring; model CATALOG/download mgr (§15 D14 — big, own item); accessory floating button = new feature (defer) — **[2026-06-10] DONE** for 3 live switches (mid-line/autocorrect/trailing-space); launch-at-login wiring done via SMAppService; model catalog still separate D14 |
| Context | screenshots-for-context (+appearance sub-toggle); clipboard | `COMPME_SCREEN_CONTEXT`, `COMPME_CLIPBOARD_CONTEXT` | env/config backing shipped; no Context tab or settings switches yet; appearance sub-toggle has no equivalent (defer) |
| Personalization | collect typing history; store-without-accepts; word-choice strength slider; existing-data count + Delete All; custom AI instructions editor | `memory` modes (AcceptedOnly/AllMonitored!), `count`/`delete_all`; personalization 6-stop strength; `COMPME_INSTRUCTIONS` | backing shipped for memory modes/instructions/strength; Apps tab count/delete UI shipped; Personalization controls for mode, instructions, and strength remain deferred |
| Emoji | enable; skin tone; **include neutral variant**; gender | `COMPME_EMOJI`, `_SKIN_TONE`, `_GENDER` | `includeVanillaVariants` is the one unmodeled item (§8 note) — small emoji-crate addition — **[2026-06-10] NOT SHIPPED** (deferred) |
| Shortcuts | word key (+trailing-space toggle); full key; force-activate; per-app temp toggle shortcut; global toggle shortcut | `AcceptKeymap` (c13) + `COMPME_TRAILING_SPACE`; `KeyRecorderField` rows persist live rebinds through `COMPME_ACCEPT_*` config | recorder UI + live rebind shipped; force-activate + the two toggle shortcuts remain new hotkeys — **[2026-06-15] PARTIAL** |
| App Settings | per-app list (usage counts) with enable/mid-line/autocorrect/Tab-disable, compat mode, per-app instructions, per-app history | `prefs` overrides + `tab_disabled` (unwired); `memory` per-app counts; personalization per-app maps (unwired, c1 #5) | per-app mid-line/autocorrect overrides are new prefs fields; pane is the largest — **[2026-06-10] PARTIAL** (Apps tab: per-app counts + confirmed per-row Delete; per-app override FIELDS live since c91 but per-app UI rows deferred) |
| Labs | mid-line toggle | `COMPME_MIDLINE` | pane only — **[2026-06-10] DONE** (shipped as a switch in the General tab — the Labs pane content moved to General) |
| Statistics | today + 30-day charts (range/group/metric) | `stats` crate (counts/words/latency) — menu line shipped c51 | chart view; longer retention than 30d if ranges grow — **[2026-06-10] DONE-MVP** (sparkline rows + lifetime row + stats.env persistence; chart view = sparklines, range/group/metric controls deferred) |
| About | version, acks, links | LICENSE, deps | pane only; states the no-telemetry guarantee — **[2026-06-10] DONE** |

## Build order (each loop-tick-sized unless noted)

1. **S1 tray submenus** — per-app timed disable + global submenu + per-app
   input-collection (pure prefs additions + tray plumbing; pattern = c54
   snooze). **[2026-06-11 CORRECTED — the 06-10 "DONE" overclaimed] Per-app
   timed disable ▸ DONE; input-collection = single toggle (works, persists;
   stateful submenu is polish); the GLOBAL disable submenu (1h / until
   relaunch / always) was NOT built — only flat Snooze-1h + the Enabled
   item exist. Global until-relaunch arm has no surface.**
2. ~~Emoji `includeVanillaVariants`~~ **DEFERRED (corrected 2026-06-10)**:
   not a small crate change — an alternate vanilla glyph has no display path
   in the single-ghost replacement pipeline. Revisit when a multi-candidate
   replacement display exists.
3. **Launch-at-login** via SMAppService (bundle exists; default-off, D13).
   **[2026-06-10] DONE** (wired via SMAppService).
4. **S2 window skeleton** + the pure panes first (Labs, Emoji, Context,
   Personalization — backing complete, persistence via persist_setting).
   **[2026-06-10] PARTIAL** — skeleton DONE (6 tabs via NSTabView); Labs DONE
   as a General-tab switch; Emoji/Context/Personalization panes deferred.
5. **Shortcuts pane** + keymap threading (closes the c13 residual) —
   **[2026-06-15] DONE for recorder UI/live rebind** via `KeyRecorderField`
   rows and run-loop persistence. Remaining shortcut gaps are force-activate
   and per-app/global toggle shortcuts.
6. **App Settings pane** (largest; needs the new per-app prefs fields).
7. Statistics charts; Setup/onboarding pane; About. **[2026-06-10] DONE**
   (Statistics DONE-MVP as sparklines; Setup and About panes shipped).
8. Out of scope here: model catalog/download manager (§15 D14; since shipped
   into the Setup tab — download button c122, sha verify c126, license gate
   c127 **[2026-06-12]**), accessory
   floating button, updater — separate items.

GUI panes are live-LOOK validated (human or scripted screenshot reads);
underlying toggles stay env/config-file-backed so every behavior remains
headless-testable.
