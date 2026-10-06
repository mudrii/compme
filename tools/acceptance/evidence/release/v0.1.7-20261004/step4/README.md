# Memory and Computer Use follow-up — 2026-10-06

Candidate source: `b2ad7a91d15d8aca8ea8e0e5d465e0fde56df05b`.
Bundle executable SHA-256:
`2f9e37671eea81806368ac69eb553118082466bcfad9cf1a7eb17dac883d9b53`.
The isolated configuration used a fresh encrypted test store and an explicit
public test key; no production memory store was changed. Full Local Gate passed
57/57 commands with zero skips. All five native CI lanes passed
[run 37261099778](https://github.com/mudrii/compme/actions/runs/37261099778).

## Owner-operated checks with captured readback

- Accepted completions: a physical Full accept inserted text and added one
  TextEdit record; ordinary typing added none.
- All monitored typing: typing without acceptance increased records 1 → 5.
- Off: typing and physical Full acceptance left five existing records unchanged.
- Mode/count persisted across pane reopening and candidate relaunch. The owner
  confirmed Off and five TextEdit records in Apps after relaunch.
- Cancelled app deletion retained five rows; confirmed app deletion removed all
  five. The owner confirmed that the stale Apps count cleared. Further Off-mode
  typing left the store empty.
- AllMonitored collected four fresh records after re-enabling. The proposed
  unfinished marker was already stored (4 → 5), with a newline in runtime
  readback; the unfinished-buffer precondition was not established.
- App deletion while AllMonitored emptied the store. Subsequent typing produced
  exactly one decrypted test record, `FreshAfterDeleteIota\n`, without the old
  marker. This proves the observed stored-text behavior, not live prompt-context
  clearing or an unfinished-buffer case.
- Cancelled global erase preserved that record; confirmed global erase emptied
  the store. Only TextEdit was represented in this global-erase leg.

One Off-mode completion attempt produced no visible suggestion; backtick was
inserted literally. Retrying the previously successful prefix produced a real
Full acceptance without recording. Both attempts are retained in results.json.

## Computer Use checks

All nine panes were inspected through AX readback and session screenshots, with
no visible clipping. General enabled/midline/trailing-space switches applied,
persisted across reopening, and restored; runtime Disabled/Ready events agree.
Cross-app context enabled, survived reopening, and restored disabled.
Personalization instructions committed and survived reopening, then were cleared.
Output steering is unverified. Clipboard and OCR stayed disabled.

The shortcut recorder captured Shift+F7, persisted `shift+98`, and registered
Carbon with matching debug logs. Shift+F6 collision was rejected, reserved Down
did not change the binding, Escape cancelled capture, and Shift+F5 was restored.
This is synthetic recorder evidence, not physical suggestion acceptance.
Apps rendered `No recorded inputs yet` without stale rows.

Computer Use changed TextEdit and a native Chrome local textarea, but these
synthetic actions did not establish reliable foreground observation by the
product. TextEdit monitored collection remained at zero despite changed AX
readback, including after AX Raise. Native Chrome showed no visible suggestion.
A read-only NSWorkspace sample reported Chrome while Settings was the targeted
surface. These runtime probes are inconclusive; neither a pass nor a product
regression is inferred. Fixture tabs were closed and the local server stopped.
Memory is Off; the store is empty; temporary preferences were restored.

## Remaining gate disposition

No complete ledger row is newly closed: **21 of 22 remain open**. The table
lists the remaining evidence for every row; it does not claim that an unexecuted
probe failed. Separate Tier-4/model/compatibility requirements also still apply.

| Runner gate | Current disposition / missing evidence |
|---|---|
| apps-policy-toggle-look | Stale-count repair verified; multi-row layout and live suggestion/correction dismissal still open. |
| personalization-pane-look | UI persistence verified; next-request steering open. |
| menu-bar-icon-look | Light/dark and runtime icon-state appearances not captured in this pass. |
| shortcuts-recorder-look | Synthetic capture, collision, cancellation and debug registration verified; physical live accept and full reopen resynchronization open. |
| always-on-hotkeys-physical-look | Requires hardware presses; synthetic input cannot close it. |
| setup-model-picker-look | Previously closed for applicable host legs; current single-folder-control layout reconfirmed. |
| nine-tab-settings-walkthrough | Pane layout and basic memory-mode behavior advanced; privacy/context/domain and buffer residuals remain. |
| full-autocorrect-prose-code-look | Prose offer/code-editor rejection not established under reliable product foreground. |
| cross-app-previous-inputs-look | Context toggle verified; two-app privacy-safe sources=recent readback and clearing open. |
| selection-thesaurus-look | Selected-word presentation, cycle, accept and stale refusal still open. |
| tray-external-links-look | Native tray destinations not exercised in this pass. |
| caret-marker-chromium-forks-calibration | Brave, Edge and Vivaldi installed during the follow-up; alignment remains unverified. |
| caret-marker-chrome-marker | Native Chrome fixture readback captured; visible ghost and Marker diagnostics absent, so inconclusive. |
| caret-marker-chromium-marker | Chromium unavailable through Homebrew because its cask is disabled for a Gatekeeper failure; no bypass attempted. |
| caret-marker-electron-marker | Electron marker-source proof not established in this pass. |
| sidebar-only-editor-assistant-look | Named VS Code/Cursor/Windsurf main-editor versus assistant-field matrix not executed. |
| encrypted-memory-all-monitored-live | Collection/mode/erase legs advanced; granted privacy scenarios, unfinished buffer, domain and prompt-context clearing remain. |
| grammar-fix-textedit-look | Current-candidate physical underline/banner/accept/staleness observations remain. |
| mirror-window-firefox-zen-look | Mirror placement and monitor-capture exclusion remain. |
| setup-needed-docs-arc-onboarding | Arc installed; opens at Sign In to Arc. Unsupported Google Docs onboarding remains unverified. |
| multi-candidate-cycle-physical-look | Requires hardware Down/accept with visible candidates. |
| input-monitoring-revoked-carbon-accept | Permission-revocation scenario and physical Carbon acceptance remain conditional; no OS permission was changed. |

The candidate remains **not ready to tag**. No tag, release publication,
notarization, or cask finalization was performed.

Machine-readable probe results are in [results.json](results.json). The two
event logs retain only relevant runtime events, excluding general browser
activity and prompt dumps. Screenshots inspected through Computer Use are
session evidence; standalone screenshot files are not included here.

## Final preparation follow-up — 2026-10-06

Installed official Homebrew casks for Brave, Edge, Vivaldi and Arc. Installation
is a prerequisite only; no browser acceptance gate is closed by installation.
Arc requires sign-in. Chromium's disabled cask prevents this installation route.
The named Windsurf application was not found; the current cask alias resolves
to Devin Desktop, which was not substituted for the named test target.

A Finder Open action reached the disposable TextEdit artifact, and synthetic
typing changed its readback, but subsequent NSWorkspace foreground sampling
still reported Finder. A dedicated Brave fixture likewise did not establish
stable foreground/visible-overlay proof. No product failure or pass is inferred.
The owner was asked to select TextEdit manually and leave it foreground; that
precondition has not yet been confirmed.

GitHub governance validation passed with the validator's documented accepted
gaps. The release environment contains all six required signing/notary secret
names; their values and operational validity remain untested until the real
release pipeline. Existing current-head native CI passed all five lanes.
