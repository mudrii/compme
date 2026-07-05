# Acceptance

This document describes the current automated and live acceptance checks for
Compme.

## Automated Gates

Root workspace:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets -- --test-threads=1
cargo build --workspace --all-targets
```

Acceptance harness syntax and deterministic self-tests:

```sh
bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh
tools/bundle/check-bundle-metadata.sh
tools/bundle/make-app.sh --self-test
tools/acceptance/e2e-complete-me.sh --self-test
tools/acceptance/missing-model-startup.sh --self-test
tools/acceptance/missing-model-startup.sh
tools/acceptance/run-a1b-live-gates.sh --self-test
tools/acceptance/run-a2-compat-gates.sh --self-test
tools/release/check-model-client-features.sh
bash tools/release/check-model-gates.sh
tools/release/run-model-gates.sh --self-test
tools/release/update-cask.sh --self-test
tools/release/finalize-cask.sh --self-test
tools/release/notarize-app.sh --self-test
tools/release/write-update-manifest.sh --self-test
```

Spike workspace:

```sh
cd tools/spike
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --bins
```

The root test gate must use `--all-targets` because `platform_macos` keeps
acceptance regression tests in example targets. It is serialized because several
macOS pasteboard checks share process-wide OS state.

Model-backed local gates:

```sh
COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1
cd tools/spike
COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1
```

These ignored suites need the GGUF files and Metal GPU. Without
`COMPME_REQUIRE_MODEL_TESTS=1` they skip absent models for developer convenience;
with it, a missing model is a failed acceptance gate.

The Release workflow must invoke `tools/release/run-model-gates.sh` before
publishing a tag; `tools/release/check-model-gates.sh` is the machine check that
prevents the workflow from silently dropping that model-backed gate script.

## Live macOS Acceptance Runner

The live runner is:

```sh
tools/acceptance/run-a1b-live-gates.sh
```

It validates current A1b macOS adapter behavior against real macOS services and
focused GUI applications.

### Preconditions

Before running:

- unlock the macOS session
- grant Accessibility permission to the terminal
- open TextEdit and focus a plain editable document; for the Option-Tab
  passthrough gate, use a normal text insertion point where a literal Tab inserts
  exactly `\t`
- avoid password fields and apps that enable global Secure Input

The runner preflights:

- screen lock state
- global Secure Input PID
- required example binaries

Use `--force` only when intentionally collecting blocked harness logs.

### Automated Default Gates

By default the runner builds the `platform_macos` examples, builds the
`compme` product binary, and runs the deterministic/scriptable gates:

- `textedit-read`
- `textedit-insert-synthetic`
- `textedit-insert-clipboard`
- `textedit-insert-axset`
- `caret-marker-textedit-any`
- `accept-insert-full`
- `accept-insert-word`
- `accept-insert-option-tab`
- `e2e-compme-pipeline`
- `e2e-compme-word-remainder`
- `accept-tap-inactive`
- `accept-tap-full`
- `accept-tap-word`
- `accept-tap-escape`
- `accept-tap-option-tab`
- `accept-tap-cycle`
- `accept-tap-delayed-hide`
- `popup-fallback-fixture`
- `overlay-presenter`
- `overlay-correction-presenter`

If TextEdit is not running, TextEdit-dependent gates are skipped instead of
misreporting app-focus failures as product failures. These are mandatory skips:
the run fails as incomplete by default, and `--allow-incomplete` is only for
intentionally partial target-specific runs.

`accept-insert-option-tab` is the public TextEdit-backed passthrough gate for
the Option+Tab contract: the harness arms a visible word suggestion, posts
Option+Tab, confirms no accept callback fired, and reads the focused TextEdit
document to prove the native key event reached TextEdit. Plain-text targets
insert a literal tab; rich TextEdit may handle the same passed-through key as
list indentation (`\t⁃\t...`) instead. Both are valid evidence that Compme did
not consume the Option+Tab or map it to Word accept.

**[2026-06-11] Scripted Carbon gates REBUILT.** The long-standing claim that
synthetic key posts cannot fire `RegisterEventHotKey` was re-tested and is
STALE: the earlier failure was the missing NSApp event pump, not a
synthetic-event filter. The `accept_tap_acceptance` and
`accept_insert_acceptance` harnesses now pump NSApp events in their wait loops
(the same cycle-41 fix the product binary carries), so synthetic key posts
dispatch to the installed hotkey handler. Validated live: synthetic
Tab/grave/Esc/Down each fired through the rebuilt tap harness (`SUMMARY
controls=[Accept(Word)]` / `[Accept(Full)]` / `[Dismiss]` / `[Cycle]`, exit
0). The runner's `accept-tap-*`, `accept-insert-*`, and `e2e-compme-*` gates
are scripted default gates (`manual=0`). Two operator notes:
(1) the gates assert an EXACT control set and the Carbon hotkeys are
system-wide, so keep hands off the keyboard during a run — ambient
Tab/grave/Esc/Down presses contaminate the captured controls (the runner
retries mismatches up to `--retries`); (2) for manual physical runs of these
gates, press the key exactly once — a double-tap now fails the exact-match
check that the old `contains()` check tolerated.

### Manual Physical Carbon Gates

Run these on an unlocked GUI session with Accessibility granted and no global
Secure Input process. Launch `compme`, focus TextEdit (or the app named by
the gate), type enough text to show a suggestion, then use a physical keyboard:

- grave/key-above-Tab accepts the full shown completion
- Tab accepts the next word and leaves the remainder visible
- Esc hides the suggestion and suppresses that field until refocus/edit
- Down cycles candidates without insertion
- Option+Tab passes through to the app without accepting; plain-text targets
  insert a literal Tab, while rich targets may apply their native Option+Tab
  behavior such as list indentation
- revoke Input Monitoring and confirm accept behavior is unchanged

Record the product log line, target app, keyboard action, and resulting field
contents for each run. These manual runs are supplemental UX confirmation; the
scripted default gates are the repeatable evidence for current Carbon hotkey
dispatch.

**PASSED 2026-06-10** (TextEdit, physical keyboard, two runs — one on a
secondary display): grave full-accept replaced `:smile` with the emoji
(`accept Full`), Tab word-accept advanced the caret and re-placed the
remainder ghost correctly (`accept Word`), Esc dismissed (`dismiss (Esc)`),
Down fired (`cycle candidate`; visible rotation needs
`COMPME_CANDIDATES>1` — the default single candidate has nothing to
rotate; with `COMPME_CANDIDATES=5` the user confirmed visible
candidate rotation live — that run's log was overwritten by a later
launch, so the evidence is the observation plus the earlier logged
`cycle candidate` firings). Option+Tab passthrough was
subsequently validated by a scripted run (2026-06-10): with a ghost
armed, synthetic Option+Tab produced ZERO `carbon hotkey fired` lines
(a modifier combo never matches the modifiers=0 registration) and
TextEdit handled the key natively (inserted its list bullet), while a
plain Tab immediately after fired and accepted normally. Not yet
exercised: the revoke-Input-Monitoring re-check. Watch-item: one screenshot showed a
remainder ghost overlapping typed text after a word-accept; not reproduced
in the logged runs (caret advance was correct) — needs a repro with the log
preserved.

### Optional Gates

Browser marker geometry:

```sh
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete --browser-pid <pid>
```

External popup fallback:

```sh
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete --popup-pid <pid>
```

External popup fallback requires:

- popup-mode capabilities
- `AxSet` insertion
- post-insert readback proving the field value changed by the expected text

This prevents a capability-only pass from masking a failed insertion path.

### P1 Quality Validation (manual / scriptable, not in the default gate list)

The P1 "MVP quality" items (`docs/superpowers/specs/2026-06-07-p1-mvp-quality-design.md`)
are validated outside the A1b gate runner:

- **Tray instantiation** — launch `compme`; the `NSStatusItem` appears and the
  binary exits clean (smoke, no panic / "tray unavailable").
- **Enable/disable toggle + gating + dismiss** — scripted via `SIGUSR1` (the
  scriptable equivalent of the tray's Enable item): `Ready` → `SIGUSR1` →
  `Disabled enabled=false` (suggestions gated, ghost dismissed) → `SIGUSR1` →
  `Ready`. A literal tray-menu mouse click stays manual (accessory-policy item
  exposes no menu to System Events).
- **Permissions** — `accessibility_trusted()` true when granted; status derivation
  + ~500 ms re-poll exercised live. Production accept keys use Carbon hotkeys
  and no longer require Input Monitoring; historical A0 CGEventTap probes still do.
  Carbon hotkey consumption was a manual physical-key gate; **[CORR 2026-06-12]**
  synthetic posts DO fire `RegisterEventHotKey` with the NSApp pump in place
  (see the [CORR 2026-06-10] note above) and the scripted gates were rebuilt
  2026-06-11 — physical runs remain the final UX confirmation.
- **Coordinate diagnostics** — `COMPME_DIAG_COORDS=1` prints display scales +
  caret rect; measured scale 1.0 on the built-in display (no offset). True-2× /
  multi-monitor (scale > 1) caret mapping was later measured on two displays and
  the backing-scale helper is unit-proven (design spec §15 G7). **Live-confirmed
  2026-06-10**: ghost placement worked on both the built-in Retina panel and an
  external display in one session (two caret clusters in the debug log, both
  overlay frames onscreen; user confirmed visually on each) — G7 is
  live-re-confirmed.
- **Config surface** — `config.env` layered under env (`env > file > default`),
  `SIGUSR1` toggle, debounce/max-words/max-tokens/heartbeat keys (unit-tested in
  `crates/app`).

> **[CORR 06-12] Accept-key evidence boundary:** the current default runner
> exercises transient Carbon hotkeys through the rebuilt scripted harnesses.
> Physical keyboard runs remain UX confirmation, and the Input Monitoring
> revoked check remains a permission-state spot-check: the runner only scripts it
> when read-only preflight shows Input Monitoring is already revoked, and
> otherwise leaves it as a manual checklist item.

### Standalone Grammar-Fix LOOK Gate

The grammar/spell-fix implementation has deterministic coverage for request
gating, correction vetting, scalar range conversion, correction-only accept
routing, fail-closed range replacement, and macOS underline/banner geometry. The
scripted `overlay-correction-presenter` gate exercises the correction banner and
underline presenter directly. The remaining product LOOK gate is visual and
requires an unlocked macOS GUI session with Accessibility permission:

- launch `compme` with `COMPME_GRAMMAR_FIX=1`,
  `COMPME_GRAMMAR_CHECK_KEY=<trigger>`, and
  `COMPME_GRAMMAR_ACCEPT_KEY=<accept>`
- focus TextEdit, type a single-word typo such as `teh`, place the caret in or
  just after the word, and press the grammar trigger
- confirm a thin underline appears under the word and a correction banner appears
  above it without moving focus or swallowing normal completion accept keys
- press the grammar accept key and confirm the original word is replaced in place
  with the vetted correction, with no duplicate suffix or left-fragment leak
- move the caret or edit the field before accepting and confirm the stale
  correction no longer applies

### Settings LOOK Gates

These Settings-window checks are manual because they depend on visible AppKit
layout and live control interaction. The runner-pinned IDs are
`apps-policy-toggle-look`, `personalization-pane-look`, `menu-bar-icon-look`,
`shortcuts-recorder-look`, `setup-model-picker-look`, and
`nine-tab-settings-walkthrough`:

- **Apps policy grid** — open Settings > Apps with at least two rows; verify the
  On / Tab / Mid / AC / GF checkbox columns do not overlap or truncate app names,
  toggle `Enabled` and `Grammar fix` for the focused app, confirm visible
  suggestions/corrections dismiss, and confirm persisted `COMPME_*_APPS` config
  changes.
- **Personalization pane** — open Settings > Personalization; edit global
  instructions, sender identity, and strength, confirm the multi-line field
  commits and persists, and confirm the next request uses updated steering
  without relaunch.
- **Menu bar / Shortcuts / Setup / nine-tab walkthrough** — confirm the tray
  icon/status, modifier-combo recorder, setup model picker, and all nine panes
  match the detailed Live UI LOOK checklist below.

### Useful Options

```text
--dry-run
--force
--allow-incomplete
--allow-manual
--skip-build
--skip-e2e
--skip-textedit
--self-test
--textedit-pid PID
--popup-pid PID
--browser-pid PID
--timeout-ms MS
--short-timeout-ms MS
--retries N
--gate-pause-ms MS
--log-dir DIR
```

Use `--allow-manual` only after executing and recording the MANUAL checklist
lines emitted by the runner. Omit it for unattended readiness runs; unresolved
manual gates fail by default.

### Logs

Logs are written under:

```text
tools/acceptance/logs/a1b-live-YYYYMMDD-HHMMSS/
```

Each gate writes a dedicated log. Retryable observer gates use
`.attempt-N.log` suffixes.

Failure classification looks for common blockers:

- locked screen
- global Secure Input
- missing Accessibility permission
- wrong focused target
- transient AX observer setup failures

## A2 Compatibility And Context Smoke Gates

Run:

```sh
tools/acceptance/run-a2-compat-gates.sh <kind>
```

Supported kinds:

- `works`
- `unsupported`
- `terminal-cmd`
- `terminal-nlp`
- `clipboard`
- `screen`

These are request-path smoke probes for a supplied PID and kind, not a full
per-app compatibility matrix runner. The `clipboard` and `screen` gates enable
`COMPME_DIAG_CONTEXT=1`; clipboard requires the marker
`CLIPBOARD-CONTEXT-MARKER` to reach the submit path, and screen requires
non-empty OCR context. The screen gate also requires Screen Recording
permission and visible text on the focused display.

## A2 Local-Replacement Live Gate (emoji / autocorrect / British English)

The local-replacement pipeline (`offer_replacement` → `Command::Replace` → AxSet
range-replace) is unit/build-verified and covered by the rebuilt scripted Carbon
live gates. Synthetic key posts do fire the Carbon accept path when the NSApp
event pump is active (same correction as A1b above), so scripted runs are valid
coverage for the accept edge.

For optional physical confirmation, run `compme` with the feature enabled, focus
an **AxSet** field (e.g. TextEdit — the offer is gated to AxSet-capable fields;
SyntheticKeys/Clipboard apps are a separate backspace-synthesis residual), then
with a physical keyboard:

- `COMPME_EMOJI=1` — type `:smile`, accept → the `:smile` is deleted and the
  emoji glyph (skin-tone/gender per `_SKIN_TONE`/`_GENDER`) is inserted.
- `COMPME_AUTOCORRECT=1` — type a known typo (e.g. `teh`), accept → replaced
  with the correction (`the`).
- `COMPME_BRITISH_ENGLISH=1` — type a US-only spelling (e.g. `color`), accept
  → replaced with the UK form (`colour`).

Confirm the typed token is deleted (not left as `:smile😄`), the field value is
correct, and that the offer is **suppressed** in an excluded app / while snoozed /
in a terminal shell-command line / when the tray is disabled (shared suggestion
gating). Record the product log line, target app, keyboard action, and resulting
field contents for each.

**PASSED 2026-06-10** (live run, TextEdit/AxSet, physical keyboard,
`COMPME_DEBUG=1` logs):

- **emoji** — typed `:smile`, ghost `😄` offered + placed on the caret line,
  physical Tab accepted: log `carbon hotkey fired id=1` → `accept Word`, field
  left-context after accept = `"😄\n"` (typed token deleted, no `:smile😄`).
- **autocorrect** — typed `teh`, ghost `the` offered, Tab accepted: field
  left-context = `"…\nthe\n"`.
- **British English** — typed `color`, ghost `colour` offered + placed; Esc
  (hotkey id=3) dismissed it correctly in one run, and on-line placement was
  re-confirmed in a later run. Its accept is byte-identical shared path
  (`replacement_offer` → `Command::Replace` → AxSet) with the two verified
  accepts above.
- Placement was live-calibrated during this gate: the AX caret rect's bottom
  edge is the caret line's top (see `overlay_frame_for_text`), the box hugs the
  line, and the ghost font tracks the line height.

Residuals (unchanged): SyntheticKeys/Clipboard backspace-synthesis (non-AxSet
apps); the suppression spot-checks (excluded app / snoozed / terminal / tray
off) were not separately exercised live — the gating is shared with model
completions and unit-tested (`suggestion_gates_apply_to_local_replacements_too`).

**RE-VALIDATED 2026-06-10 (post R2-5 restructure)** — after the Carbon handler
moved to a process-lifetime install with a swappable handler slot (one
`InstallEventHandler` per process, per-arm hotkey registrations only), a live
TextEdit run re-confirmed the full path: all four hotkeys registered per arm
cycle (Tab 48 / grave 50 / Esc 53 / Down 125), physical Tab dispatched
(`carbon hotkey fired signature=0x636d414b id=1`) and accepted (`accept Word`,
twice), emoji ghost offered/placed, and per-app exclusion held
(`com.mitchellh.ghostty … suggestions disabled`). The restructure did not
regress hotkey dispatch. Not exercised in that run: snooze click, keychain key
creation, toggle-relaunch persistence (separate residuals).

**Follow-up runs same day closed three of those**: (1) keychain key
created on first use and reused across three runs (one login-keychain
`genp` entry, store reopened over existing records); (2) two Full-accept
records landed and the db shows no typed plaintext under `strings` —
ciphertext-only-on-disk holds live; (3) the tray Enable toggle persisted
`COMPME_ENABLED=false` to config.env and a relaunch started
`status=Disabled` directly from it. A later run validated the
snooze flow live: the tray click logged `suggestions snoozed for 60
minutes`, the render state flipped `snoozed=true`, and a fully typed
`:smile` while snoozed produced `decision=None` on every keystroke with
zero ghosts or model requests — the unified prefs gate blocks local
replacements and model completions alike. (The ghost-dismiss-on-snooze
edge ran with no ghost visible, so it stays unit-covered only.)

**AxSet readback fallback VALIDATED LIVE 2026-06-10** in iTerm2 (a scripted,
fully autonomous run) for a plain insert after a silently ignored AX write: the
log showed `AxSet write silently ignored — falling back to synthetic input`, and
the follow-on synthetic insertion landed in the target. This does **not** close
non-AxSet replacement support: local replacements remain AxSet-only because
SyntheticKeys/Clipboard cannot atomically delete `replace_left` and insert the
replacement. The shared non-AxSet backspace-synthesis replacement residual above
therefore remains open.

## Live UI LOOK Gates (Settings window / tray)

These are macOS-only supplemental LOOK checklists. They are driven by launching
the product binary with debug logging and exercising the AppKit UI by hand (the
AppKit glue is render-only, so LOOK + log evidence is the contract — the pure
halves are unit-tested). They are not the runner-pinned remaining manual gate
list; the runner-emitted manual gates are tracked under **Pending Manual
Gates** below. Launch once and watch the log:

```sh
cd ~/src/compme
COMPME_EMOJI=1 COMPME_DEBUG=1 cargo run -p app 2>&1 | tee /tmp/cm.log
```

Open Settings from the tray menu (or the tray's Settings item). The window has
nine tabs in display order (`pane_titles`): **Setup, General, Personalization,
Apps, Context, Emoji, Shortcuts, Statistics, About**. Setup, Apps, Context,
Emoji, Statistics, and Shortcuts rows are recomposed by the run loop right
before each show; General switches re-read their atomics on every show.

### Menu-bar icon LOOK gate

- The tray status item shows a **caret + double-chevron template image**
  (`assets/tray-icon.png`, "auto-complete forward"), not the old `CM…` text.
- Because it is set as a template image (`setTemplate(true)`), macOS tints it to
  match the menu bar: confirm it renders correctly in both light and dark menu
  bars. The `CM…` title is now only a decode-failure fallback.

### Shortcuts tab + modifier-combo recorder LOOK gate

The Shortcuts pane lists the current accept bindings with macOS modifier-glyph
labels (⌃⌥⇧⌘ via `accept_key_modifier_glyphs` / `keycode_label_with_mods`) and
carries a recorder box per role (Word / Full / Grammar accept).

1. Settings → **Shortcuts** → click a recorder box → press a modifier combo,
   e.g. **Shift+F5**. The box label updates to **`⇧F5`**.
2. The log shows the recorder capture then the live re-registration:
   - `compme: recorder keyDown role=Word keycode=96 mask=512`
     (F5 = keycode 96; Carbon Shift mask = 512)
   - `compme: carbon hotkey registered id=1 keycode=96 modifiers=512`
     (id=1 is the Word/Tab slot)
3. The new combo **accepts live** in TextEdit (type to a suggestion, press
   Shift+F5 → `accept Word`).
4. The **Grammar accept** row can be rebound the same way; it persists
   `COMPME_GRAMMAR_ACCEPT_KEY` and is used only for correction presentations,
   not normal Word/Full ghosts.
5. Fixed-key behavior holds inside the recorder: **Esc cancels** the recording
   (reverts to the role's current key), and **Down is rejected silently**
   (reserved cycle key) — neither can be rebound. A capture that collides with
   another role's `(keycode, mask)` shows "In use — press another".
6. Reopen Settings: the Shortcuts pane **re-syncs to the effective keymap**
   (`effective_accept_keys_with_mods_and_grammar()`), so the glyph labels
   reflect the live binding, not a stale default.

Record the box label, the two log lines, and the live accept for each rebind.

### Setup tab model picker LOOK gate

The Setup tab carries a **"Model to download:"** popup whose items are the
catalog rows in order (`model_menu_titles`), each suffixed with a RAM-fit
label for this machine's available memory (`ram_verdict`):

- `· fits`
- `· tight — may swap under load`
- `· exceeds available memory`

`fits` and `tight` entries may download; `exceeds available memory` is a hard
download block. To exercise:

1. Open the popup and confirm one row per catalog entry
   (`qwen2.5-0.5b-q4_k_m`, `llama-3.2-1b-q4_k_m`, `qwen2.5-1.5b-q4_k_m`,
   `gemma-2-2b-q4_k_m`), each carrying a fit suffix.
2. Pick a **non-recommended** model that fits or is tight and click Download →
   that model downloads (log: `downloading <model> (<MB> MB) — progress in this
   log`), proving the picker index drives the target, not just `recommended()`.
3. On a machine below a model's minimum RAM, picking that row and clicking
   Download logs a blocked message and does not enqueue a fetch.
4. Re-click Download on a model already on disk → the dest-exists guard logs
   `<model> already downloaded at <path> — delete it to re-download` (no
   re-fetch / clobber).
5. Pick an **encumbered** model (`llama-3.2-1b-q4_k_m` /
   `gemma-2-2b-q4_k_m`) with no prior acceptance → the **license click-through
   prompt** appears (the `download_gate` `NeedsLicense` path) before any fetch.
   Today's recommended default is unencumbered, so a plain run never prompts.

### Nine-tab Settings walkthrough

A quick LOOK pass over all nine panes:

- **Setup** — readiness checklist (Accessibility / Screen Recording / model
  file) plus the model picker + Download button; rows re-probe on each show.
- **General** — the master Enabled switch (the same atomic as the tray
  checkmark) plus autocorrect / trailing-space / Labs mid-line switches; flips
  live-apply and persist.
- **Personalization** — a strength popup plus editors for the global steering
  instructions and sender identity (name / email), driven by the
  `PersonalizationEdit` enum; edits live-apply and persist. (Pane builds;
  per-app/per-domain instruction editing is a follow-up.)
- **Apps** — per-app recorded-input counts from the encrypted memory store;
  count rows (`app — N`) carry a Delete button, status/empty rows do not.
- **Context** — Clipboard and Screen Context switches match their current
  config-backed state; disabling either clears the cached context source and
  gates new submissions.
- **Emoji** — the enable switch, skin-tone popup, and Gender popup render in
  the dedicated pane and persist through `COMPME_EMOJI*` keys.
- **Shortcuts** — current bindings (glyph-labelled) + the recorder boxes above.
- **Statistics** — shown / accepted / words / lifetime rows.
- **About** — static version / license / no-telemetry / repo / credits text.

## Example Acceptance Binaries

The live runner uses `platform_macos` examples:

- `textedit_observer_acceptance`
- `caret_marker_acceptance`
- `accept_tap_acceptance`
- `accept_insert_acceptance`
- `input_monitoring_preflight_acceptance`
- `popup_fallback_acceptance`
- `overlay_presenter_acceptance`

Build them with:

```sh
cargo build -p platform_macos --examples
```

## Spike Manual Acceptance

`tools/spike/MANUAL-ACCEPTANCE.md` records the A0 manual probe results for:

- AX reads
- caret geometry
- event taps
- split observer/consumer taps
- AppKit overlay
- read -> infer -> overlay smoke path

Those records are historical evidence. New production acceptance should prefer
the root `platform_macos` examples and `tools/acceptance/run-a1b-live-gates.sh`.

## Manual/Live Gate Ledger [added 2026-06-10]

`tools/acceptance/run-a1b-live-gates.sh` now emits each remaining manual/live
gate as a `MANUAL ...` checklist line after the deterministic gates. The
runner's `--self-test` pins that checklist so these public-behavior gaps cannot
quietly disappear from the acceptance surface.

Exact runner-emitted manual gate IDs:

- `apps-policy-toggle-look`
- `personalization-pane-look`
- `menu-bar-icon-look`
- `shortcuts-recorder-look`
- `always-on-hotkeys-physical-look`
- `setup-model-picker-look`
- `nine-tab-settings-walkthrough`
- `caret-marker-chromium-forks-calibration`
- `caret-marker-chrome-marker`
- `caret-marker-chromium-marker`
- `caret-marker-electron-marker`
- `encrypted-memory-all-monitored-live`
- `grammar-fix-textedit-look`
- `input-monitoring-revoked-carbon-accept`

- **Settings timed/behavioral gate (completed 2026-06-17):** live AppKit runs
  against disposable `COMPME_CONFIG` roots verified General switches apply
  immediately and persist across relaunch (`Enable/Mid-line/Autocorrect/Trailing
  space` reopened as `0/1/1/1`, with `status=Disabled enabled=false` on
  relaunch), Setup re-probes visible rows within the 480 ms budget (temporary
  model symlink removal flipped `✓ Model file` to `✗ Model file` at the first
  100 ms poll), and Apps renders recorded-input rows from an encrypted-memory
  store. A disposable `com.apple.TextEdit — 1` row opened the **Delete recorded
  inputs?** alert with **Cancel** and **Delete** buttons; Cancel preserved the
  row, and the production prompt adds Cancel as the first/default NSAlert
  button.
- **Statistics range/grouping LOOK gate (completed 2026-06-17):** Settings →
  **Statistics** showed side-by-side range and grouping popups in the header
  row. A live run changed range to **Last 14 days** and grouping to **Weekly**;
  reopening Settings preserved both selections and recomposed the rows as
  weekly two-bar sparklines with the Lifetime row still visible.
- **Emoji gender LOOK gate (completed 2026-06-17):** Settings → **Emoji**
  showed **Gender** directly below **Skin tone**, exposed **Neutral / Female /
  Male**, persisted a live change to `COMPME_EMOJI_GENDER=female`, and reopened
  with **Female** selected. The stale-ghost invalidation edge is covered by
  `emoji_gender_edge_invalidates_stale_visible_suggestion`, which routes the
  same settings watcher through `engine.on_dismiss()`.
- **Encrypted memory accepted-only live gate (completed 2026-06-10):**
  `COMPME_MEMORY=accepted` + `COMPME_MEMORY_PATH` run without
  `COMPME_MEMORY_KEY` created and reused one `com.compme.memory` login-keychain
  entry across three runs; two Full-accept records landed for TextEdit, and
  `strings` over the database showed no typed plaintext.
- **Encrypted memory AllMonitored live gate (pending):**
  `COMPME_MEMORY=all` run that validates redactable monitored typed runs
  assembled from inserted deltas without storing pre-existing field text, plus
  secure input, disabled/snoozed, app/domain excluded, volatile `pid:N`, and
  per-app collection-off blocks. **[2026-06-17]** unit coverage now includes
  file-backed AllMonitored redacted inserted-delta persistence with a raw DB
  scan proving neither the original email nor `[redacted-email]` is present on
  disk, plus store-effect checks that disabled and excluded-app policies create
  no encrypted rows. **[2026-06-17 live partial]:** a disposable TextEdit
  `AXTextArea` product-loop run with `COMPME_MEMORY=all`, a temp SQLite DB, and
  an explicit temp key stored one row for `com.apple.TextEdit`; decrypt-readback
  was exactly ` typed [redacted-email] ` after a baseline containing
  `alice@example.com`, and a raw DB scan found neither the raw emails nor the
  redacted marker. Four additional disposable TextEdit product-loop runs
  confirmed `rows=0` for global disabled (`COMPME_ENABLED=false`), per-app
  disabled (`COMPME_DISABLED_APPS=com.apple.TextEdit`), hard app exclude
  (`COMPME_EXCLUDED_APPS=com.apple.TextEdit`), and per-app collection-off
  (`COMPME_NO_COLLECT_APPS=com.apple.TextEdit`). A fifth disposable run typed a
  no-whitespace monitored buffer, toggled disabled via `SIGUSR1` before the
  boundary, then typed the boundary and confirmed `rows=0`. A disposable Chrome
  `AXTextArea` run on `127.0.0.1` with `COMPME_EXCLUDED_DOMAINS=127.0.0.1`
  logged `domain=127.0.0.1`, blocked requests with `prefs_ok=false`, and kept
  the store at `rows=0`. The remaining manual/live residual is secure input,
  snoozed policy transition, and volatile `pid:N` confirmation in a GUI session.
- **Input Monitoring revoked spot-check (pending/conditional):** with
  Accessibility still granted, revoke Input Monitoring and confirm the transient
  Carbon accept path keeps working. `run-a1b-live-gates.sh` uses
  `CGPreflightListenEventAccess()` to automate this only when the current
  process is already revoked; it never requests or changes the permission. This
  is a permission-state confirmation, not a requirement for the production
  accept path.
- **Lifetime stats relaunch gate (completed 2026-06-17):** a disposable
  `COMPME_CONFIG` run with `COMPME_STUB_COMPLETION=' world'` drove TextEdit
  through the production Carbon Tab accept path (`accept Word`), quit via the
  status menu, and wrote `stats.env` with `STATS_ACCEPTED=1` and
  `STATS_WORDS=1`. Relaunching against the same temp config and opening
  Settings → **Statistics** showed `Lifetime 1 words · 1 accepted`, proving the
  prior-session baseline is loaded into the UI.
