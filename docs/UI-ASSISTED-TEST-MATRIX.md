# UI Assisted Test Matrix

Goal: user-assisted screenshots and logs for every user-facing Compme surface.
Each batch stays open until screenshots/logs prove pass or a fix lands.

Start a test session with:

```sh
tools/acceptance/run-ui-assisted-session.sh
```

## Batch 1 — Launch, Tray, Settings Layout

Evidence required:
- Screenshot: menu-bar icon with menu open.
- Screenshots: Settings tabs `Setup`, `General`, `Personalization`, `Apps`,
  `Context`, `Emoji`, `Shortcuts`, `Statistics`, `About`.
- Log: `tail -80 /tmp/cm-ui.log`.

Check:
- Tray uses the caret + double-chevron template icon, not text fallback.
- Menu items fit and expose Settings, Enable/disable, update, and quit actions.
- All nine Settings tabs fit without clipped text, overlap, or stale labels.
- Setup shows permission/model rows and model picker.
- General toggles render and are readable.
- Personalization multiline instructions, sender fields, and strength control fit.
- Apps grid columns `On`, `Tab`, `Mid`, `AC`, `GF` fit and do not overlap names.
- Context switches render for clipboard and screen context.
- Emoji enable, skin-tone, and gender controls render.
- Shortcuts shows Word, Full, and Grammar accept recorders with glyph labels.
- Statistics shows session/lifetime rows and range/grouping controls.
- About shows version/license/no-telemetry/repo/credits text.

Evidence:
- Settings screenshots received 2026-07-06 under `~/Pictures/Diff/`:
  - `Screenshot 2026-07-06 at 13-55-38.png` — Setup
  - `Screenshot 2026-07-06 at 13-55-47.png` — General
  - `Screenshot 2026-07-06 at 13-55-55.png` — Personalization
  - `Screenshot 2026-07-06 at 13-56-09.png` — Apps
  - `Screenshot 2026-07-06 at 13-56-14.png` — Context
  - `Screenshot 2026-07-06 at 13-56-20.png` — Emoji
  - `Screenshot 2026-07-06 at 13-56-25.png` — Shortcuts
  - `Screenshot 2026-07-06 at 13-56-31.png` — Statistics
  - `Screenshot 2026-07-06 at 13-56-38.png` — About
- `/tmp/cm-ui.log` tail showed TextEdit request, ghost, and Carbon hotkey
  registration activity with no crash in the visible tail.
- Menu screenshots received 2026-07-06 under `~/Pictures/Diff/`:
  - `Screenshot 2026-07-06 at 14-00-42.png` — menu-bar icon crop
  - `Screenshot 2026-07-06 at 14-00-54.png` — open tray menu

Result:
- Settings layout pass: no visible clipping, overlap, or stale labels found in
  the 9 tab screenshots.
- Tray/menu pass: icon is image-based rather than `CM...` text fallback, and
  menu items fit with status, enable, app/input toggles, global disable,
  snooze, Settings, updates, and Quit visible.

Status: pass.

## Batch 2 — Settings Interaction And Persistence

Evidence required:
- Screenshot before/after changing General toggles.
- Screenshot before/after editing Personalization fields.
- Screenshot before/after changing Emoji gender/skin tone.
- Screenshot Shortcuts after rebinding Word, Full, and Grammar accept keys.
- Log excerpts showing persistence and hotkey re-registration.

Check:
- General toggles live-apply and persist after closing/reopening Settings.
- Personalization edits commit and persist.
- Emoji changes persist and dismiss stale visible suggestions.
- Shortcut recorders capture modifier combos, reject reserved Down, cancel on Esc,
  show collision feedback, and persist on reopen.

Findings from 2026-07-06 user-assisted pass:
- General toggles persisted after close/reopen.
- Emoji skin tone and gender persisted after close/reopen.
- Personalization text fields did not save reliably after editing.
- Shortcut recorder captured F5/F6 but lost the held Shift modifier.
- Follow-up validation found Shift+F6/F7 working, but Shift+F5 still sometimes
  arrived as Shift down, Shift up, then bare F5 in AppKit recorder events.
- Second follow-up found the assisted launcher was registering global
  grammar-check as Shift+F5, so Carbon consumed that chord before the recorder
  saw `keyDown`.
- Selected tab highlight had a tight/cropped top edge in the native tab strip.
- Latest screenshot pass showed the native tab strip still clipped the first
  selected tab on first open and later degraded to a line-like highlight.
- Follow-up screenshot showed the segmented tab row fixed selection formatting,
  but General-pane text started too close to the tabs.

Fixes validated 2026-07-07 (scripted UI-assisted session, screenshots at
/tmp/cm-tab-*.png + /tmp/cm-reopen-*.png, isolated-config readback as ground
truth):
- Segmented tab row renders a proper rounded selection highlight on first open;
  General first toggle row has clear space under the tabs (all 9 pane
  screenshots re-taken, no clipping/overlap).
- General toggles live-persist (`COMPME_MIDLINE=1`, `COMPME_AUTOCORRECT=0`
  appeared in the isolated config immediately on click).
- Personalization edits flush on tab-switch without Enter
  (`COMPME_INSTRUCTIONS` + `COMPME_SENDER_NAME` written; values shown after
  close/reopen).
- Emoji skin tone + gender persist (`COMPME_EMOJI_SKIN_TONE=medium`,
  `COMPME_EMOJI_GENDER=female`).
- Shift+F5 records with its modifier (`COMPME_ACCEPT_WORD_KEY=shift+96`;
  recorder + summary show ⇧F5 after reopen). Synthetic keystroke path; the
  physical-key edge stays covered by `always-on-hotkeys-physical-look`.
- Note: recorder fields expose no AX sub-elements (custom NSView); labels for
  the tab segments are exposed via AXDescription — VoiceOver-readable.

Status: pass.

## Batch 3 — TextEdit Completion Flow

Evidence required:
- Screenshot: ghost completion in TextEdit.
- Screenshot/log: Word accept leaves remainder visible.
- Screenshot/log: Full accept inserts full suggestion.
- Screenshot/log: Esc dismisses suggestion.
- Screenshot/log: Down cycles candidates when `COMPME_CANDIDATES>1`.

Check:
- Ghost is aligned with caret.
- Tab/Word accept, full accept, Esc, and Down behavior match docs.
- Option+Tab passes through to TextEdit instead of accepting.

Status: pass (2026-07-07 scripted assisted session). Ghost at caret verified by
screenshot; word accept "hello world " + remainder; full accept "hello world";
Esc dismissed; Option+Tab passed through (TextEdit consumed the chord). Found
and fixed live: seam space lost by candidate normalization ("helloworld") —
commit 7c80b51. Down-cycle stays with the physical gate (isolated launch env
carries no COMPME_CANDIDATES override).

## Batch 4 — Local Replacement Features

Evidence required:
- Screenshot/log: `:smile` to emoji.
- Screenshot/log: `teh` autocorrects to `the`.
- Screenshot/log: `color` offers British `colour`.
- Screenshot/log: thesaurus suggestion appears for a supported word.

Check:
- Replacement deletes the typed token and inserts only the replacement.
- Feature is suppressed when disabled or app policy blocks it.

Status: pass (2026-07-07 scripted assisted session, doc readback): ":smile" ->
😄 exactly; "I saw teh" -> "I saw the"; "my favorite color" -> "my favorite
colour"; "the results are good" -> "the results are great" (4 thesaurus
candidates offered). Policy-suppression legs remain covered by deterministic
tests.

## Batch 5 — Grammar Fix Flow

Evidence required:
- Screenshot: `teh` underlined with correction banner.
- Screenshot/log: grammar accept replaces the word in place.
- Screenshot/log: moving caret/editing before accept prevents stale replacement.

Check:
- Grammar trigger is separate from normal completion.
- Correction banner does not steal focus.
- Word/Full accept keys do not accept correction unless bound as grammar accept.

Status: pass (2026-07-07 scripted assisted session, real model). Found and
fixed live: corrections NEVER surfaced — the instruction-style prompt fed the
base (non-instruct) model, whose continuations always failed vetting (commit
5126509: few-shot prompt + first-token extraction in vet_correction).
After the fix: "I saw teh cat today" with the caret on the mid-sentence typo →
ctrl+opt+F12 → underline on "teh" + "the" banner (screenshot
/tmp/cm-b5-grammar2.png) → ⇧F6 → "I saw the cat today" (exact in-place range,
log "accept Correction"). Staleness leg: correction for "recieve" produced,
caret moved one right, ⇧F6 → document unchanged (stale correction refused).

## Batch 6 — Browser, Domain, Terminal, Context

Evidence required:
- Screenshot/log: Chrome/Safari/Brave domain allow.
- Screenshot/log: browser-domain exclude blocks configured host.
- Screenshot/log: terminal command line blocks, natural-language prompt allows.
- Screenshot/log: clipboard context reaches submit path.
- Screenshot/log: screen OCR context reaches submit path when permission granted.

Check:
- Browser domain is host-only, not raw URL with path/query.
- Context features are opt-in and visible in logs.
- Terminal gating matches command vs natural-language prompt.

Status: pass (2026-07-07 scripted assisted session):
- Terminal gating: Terminal.app shell command line -> "request blocked ...
  terminal_ok=false"; natural-language line -> request submitted
  (terminal_ok=true). Both legs log-proven. WezTerm exposes no AX text value
  -> fail-closed (no suggestions), as designed.
- Clipboard context: opt-in via Context pane (persisted
  COMPME_CLIPBOARD_CONTEXT=1, "clipboard context enabled" logged); submit-path
  proof via COMPME_DIAG_CONTEXT=1: prompt_context="sources=clipboard chars=39
  clipboard_chars=39" matching the copied marker.
- Screen OCR: live pass after a live-found fix — the OCR thread panicked in
  every debug build (objc2 rejected the CGImageRef passed as a void pointer);
  after the CGImageOpaque encoding fix, prompt_context="sources=screen
  chars=160 screen_chars=160" reached the submit path with no panic.
- Browser domain live rows (Safari on github.com/mudrii/compme):
  exclude leg — COMPME_EXCLUDED_DOMAINS=github.com -> "request blocked ...
  prefs_ok=false" with domain=github.com (host-only despite the /mudrii/compme
  path); allow leg — no exclusion -> request submitted, prefs_ok=true, same
  host-only domain. Chrome/Brave variants remain with the A2 matrix rows.

Physical-leg follow-up (2026-07-07, scripted with synthetic keys — hardware
purity stays with the runner's physical gates):
- Multi-candidate cycle: COMPME_CANDIDATES=3, real model produced 3 candidates;
  Down visibly switched the TextEdit ghost ("sunny and warm." -> "rainy.",
  screenshots /tmp/cm-cyc*.png, log "cycle candidate"); word accept + Esc
  dismiss re-proven in the same run.
- Chrome (local textarea fixture): AXManualAccessibility wake works, focus
  binds as AXTextArea and a completion request was submitted (3 candidates
  returned) — but Chrome delivered a fresh AX element per focus notification
  and every bind went StaleField within the churn (661 StaleField lines), so
  the ghost was discarded before rendering. FIXED same day with CFHash-based
  element identity: live retest shows 0 churn StaleFields and the full
  bind→request→ghost→accept→insert pipeline in a Chrome textarea (screenshots
  /tmp/cm-chrome-ghost.png, /tmp/cm-chrome-after.png). Ghost anchors at the
  window rect (Chrome returns no caret rect on this path) — precise caret
  anchoring remains the caret-marker-chrome-marker calibration gate.
- VS Code: Monaco exposes only AXWebArea until VS Code's screen-reader mode is
  on -> fail-closed, no suggestions (matches the unsupported compat class).
- Firefox/Zen: mirror-mode ENGAGEMENT log-proven for both bundle ids
  ("renders via a mirror window (inline overlay unsupported)"); Gecko does not
  wake its AX tree for our advisory AXManualAccessibility set, so the visual
  mirror render remains with the mirror-window-firefox-zen-look human gate.

## Batch 7 — Memory, Stats, Setup, Release Update Surface

Evidence required:
- Screenshot/log: accepted-only or all-monitored memory creates app row.
- Screenshot/log: Apps delete row alert has Cancel/Delete and works.
- Screenshot: Statistics lifetime/session values after an accept.
- Screenshot: Setup model picker rows with RAM verdicts.
- Screenshot/log: already-downloaded model does not re-download.
- Screenshot: Check for Updates opens latest release page.

Check:
- Memory stores no plaintext in DB scan when tested.
- Stats survive relaunch.
- Model picker target follows selected row and honors license/RAM/dest guards.
- Update menu opens release page without crashing.

Status: pass (2026-07-07 scripted assisted session):
- Memory: COMPME_MEMORY=accepted + PATH/KEY -> "encrypted memory enabled
  (mode=AcceptedOnly)"; after a real-model accept the Apps pane shows
  "com.apple.TextEdit — 1" with the On/Tab/Mid/AC/GF policy row + Delete;
  Delete opens "Delete recorded inputs?" with Cancel (focused default) /
  Delete; Cancel preserved the row. Fail-closed proven en route:
  COMPME_MEMORY without COMPME_MEMORY_PATH logs "memory disabled".
- Stats: live counts after session accepts (Shown 5 / Accepted 1 / Words 8,
  Lifetime 21 words · 14 accepted — lifetime carried across relaunches).
- Setup: checklist + picker with "fits" RAM verdict captured in Batch 1/2
  passes; no re-download attempted (model already on disk, gate covered
  deterministically).
- Check for Updates…: frontmost switched to the default browser (releases
  page handoff), no crash.
