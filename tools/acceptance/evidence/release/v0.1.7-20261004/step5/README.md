# Residual checklist cleanup and startup tests — 2026-10-06

This follow-up preserves earlier dated evidence and replaces repeated broad
instructions with a consolidated residual worklist. No acceptance gate is waived.
The candidate executable matches step 4; the source tree has only documentation
and evidence changes since that binary. All 21 open IDs remain open.

## Newly executed tests

- Off with no path: bounded production launch exited successfully; no database
  was created. The first run lacked a before-run Keychain baseline.
- Off with an explicitly absent database: Keychain metadata-only lookup returned
  item-not-found (44) both before and after. No secret value was requested. The
  database and parent directory stayed absent; bounded launch exited 0.
  No-store/no-key creation passes; native prompt visibility remains unobserved.
- AllMonitored requested at startup with no path: runtime reported memory disabled
  and exited 0. This does not establish in-app picker rollback or persisted Off.
- Input Monitoring preflight: granted=true, revoked=false. Revoked-state probe
  stays conditional; no permission was requested or changed.
- Before restarting, Shortcuts reopened with effective Word Shift+F5, Full
  backtick and Grammar Shift+F6; About readback showed v0.1.7 and local-inference
  text. Recorder physical capture and live routing are still open.

TextEdit's disposable document was reset to one harmless completion prefix.
A subsequent read-only foreground sample still named Zen, not TextEdit. A Zen
local-fixture attempt stopped on a user-change interruption. No fixture became
established and no browser result is claimed. Personal browser content was not
saved into these evidence files. The temporary HTTP server was stopped and the
baseline candidate/configuration restored with Memory Off.

## Consolidated residual sessions

| Session | Open runner IDs | Remaining work; reuse earlier evidence |
|---|---|---|
| Settings/tray | apps-policy-toggle-look; personalization-pane-look; menu-bar-icon-look; nine-tab-settings-walkthrough; tray-external-links-look | Two-row Apps readability and live policy dismissal; next-request steering; light/dark and status icon; remaining Context/privacy/login-item behavior; exact once-only tray links. Basic pane layout and persisted controls need not be repeated. Fallback title is conditional on image failure; no normal-bundle corruption required. |
| Physical TextEdit | shortcuts-recorder-look; always-on-hotkeys-physical-look; grammar-fix-textedit-look; selection-thesaurus-look; multi-candidate-cycle-physical-look | Physical recorder capture/rebind and accept; four global hotkeys; grammar exact replacement/staleness; selected synonym cycle/range/staleness; multi-candidate order/wrap/accept. Share Grammar and Down observations where their distinct preconditions hold. |
| Memory/Context | encrypted-memory-all-monitored-live; cross-app-previous-inputs-look; nine-tab-settings-walkthrough | Secure-input/snooze/volatile identity; missing-path picker rollback; Off no-hydration; two-app context isolation and deletion; genuinely unfinished buffers; global clear/fresh-only collection; two browsers with real detected domains and domain clear/non-resurrection; Clipboard/OCR opt-in and disable-clear. Basic mode/count/cancel/erase results remain reusable. |
| Editors | sidebar-only-editor-assistant-look; full-autocorrect-prose-code-look; caret-marker-electron-marker | VS Code/Cursor/Windsurf main blocked and labelled assistant allowed. Reuse an allowed Electron assistant for Marker geometry and one blocked editor main pane for autocorrect suppression; TextEdit proves positive spelling replacement. |
| Browsers | caret-marker-chromium-forks-calibration; caret-marker-chrome-marker; caret-marker-chromium-marker; mirror-window-firefox-zen-look; setup-needed-docs-arc-onboarding | Three fork placements; Chrome and Chromium actual Marker diagnostics plus visible alignment; Firefox/Zen mirrors and capture exclusion; Arc/Docs unsupported-state onboarding/no request. Chromium availability and Arc sign-in remain prerequisites. Correct fallback geometry does not close a Marker-specific row. |
| Permission state | input-monitoring-revoked-carbon-accept | Conditional on established revoked state with Accessibility retained. Production Carbon does not require Input Monitoring. Runner can script this when already revoked; do not describe it as invariably physical. |

Windsurf is not substituted by its renamed cask target. Missing applications are
blocked prerequisites rather than passed or automatically inapplicable legs.
The already-closed model picker need not be repeated; low-RAM refusal remains
inapplicable on this host. Optional replacement matrices, native updater work,
and exhaustive combinations are not new requirements of these 21 groups.

The fresh A2 13-row matrix, G7 before/after physical acceptance, remaining
Tier-4 observations and actual release sign/notarize/publish/post-verify remain
separate. Existing policy/practice discrepancy Qfd G11 is an owner decision,
not permission to label unexecuted checks passed. No tag was created.

Full Local Gate completed: **57 commands run, 0 skipped, exit 0**.
The log fingerprint and completion timestamp are recorded in results.json.
