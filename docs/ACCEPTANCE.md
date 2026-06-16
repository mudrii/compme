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
bash -n tools/acceptance/*.sh
tools/acceptance/e2e-complete-me.sh --self-test
tools/acceptance/run-a1b-live-gates.sh --self-test
tools/acceptance/run-a2-compat-gates.sh --self-test
bash tools/release/check-model-gates.sh
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
- open TextEdit and focus an editable document
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

If TextEdit is not running, TextEdit-dependent gates are skipped instead of
misreporting app-focus failures as product failures. These are mandatory skips:
the run fails as incomplete by default, and `--allow-incomplete` is only for
intentionally partial target-specific runs.

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
- Option+Tab passes a literal Tab through to the app
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
> revoked check remains a manual permission-state spot-check.

### Useful Options

```text
--dry-run
--force
--allow-incomplete
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

**Backspace-synthesis VALIDATED LIVE 2026-06-10** via the AxSet
readback fallback in iTerm2 (a scripted, fully autonomous run): typed
`:smile`, synthetic Tab accepted, the log shows `AxSet write silently
ignored — falling back to synthetic input`, and reading the iTerm
session contents back showed the prompt holding `😄` alone — the typed
token was backspaced away and the emoji synthetically typed. The
cycle-47 machinery's live residual is closed.

## Live UI LOOK Gates (Settings window / tray)

These are macOS-only manual gates. They are driven by launching the product
binary with debug logging and exercising the AppKit UI by hand (the AppKit glue
is render-only, so LOOK + log evidence is the contract — the pure halves are
unit-tested). Launch once and watch the log:

```sh
cd ~/src/compme
COMPME_EMOJI=1 COMPME_DEBUG=1 cargo run -p app 2>&1 | tee /tmp/cm.log
```

Open Settings from the tray menu (or the tray's Settings item). The window has
eight tabs in display order (`pane_titles`): **Setup, General, Apps, Context,
Emoji, Shortcuts, Statistics, About**. Setup, Apps, Context, Emoji, Statistics,
and Shortcuts rows are recomposed by the run loop right before each show;
General switches re-read their atomics on every show.

### Menu-bar icon LOOK gate

- The tray status item shows a **caret + double-chevron template image**
  (`assets/tray-icon.png`, "auto-complete forward"), not the old `CM…` text.
- Because it is set as a template image (`setTemplate(true)`), macOS tints it to
  match the menu bar: confirm it renders correctly in both light and dark menu
  bars. The `CM…` title is now only a decode-failure fallback.

### Shortcuts tab + modifier-combo recorder LOOK gate

The Shortcuts pane lists the current accept bindings with macOS modifier-glyph
labels (⌃⌥⇧⌘ via `accept_key_modifier_glyphs` / `keycode_label_with_mods`) and
carries a recorder box per role (Word / Full).

1. Settings → **Shortcuts** → click a recorder box → press a modifier combo,
   e.g. **Shift+F5**. The box label updates to **`⇧F5`**.
2. The log shows the recorder capture then the live re-registration:
   - `compme: recorder keyDown role=Word keycode=96 mask=512`
     (F5 = keycode 96; Carbon Shift mask = 512)
   - `compme: carbon hotkey registered id=1 keycode=96 modifiers=512`
     (id=1 is the Word/Tab slot)
3. The new combo **accepts live** in TextEdit (type to a suggestion, press
   Shift+F5 → `accept Word`).
4. Fixed-key behavior holds inside the recorder: **Esc cancels** the recording
   (reverts to the role's current key), and **Down is rejected silently**
   (reserved cycle key) — neither can be rebound. A capture that collides with
   the other role's `(keycode, mask)` shows "In use — press another".
5. Reopen Settings: the Shortcuts pane **re-syncs to the effective keymap**
   (`effective_accept_keys_with_mods()`), so the glyph labels reflect the live
   binding, not a stale default.

Record the box label, the two log lines, and the live accept for each rebind.

### Setup tab model picker LOOK gate

The Setup tab carries a **"Model to download:"** popup whose items are the
catalog rows in order (`model_menu_titles`), each suffixed with a RAM-fit
advisory for this machine's available memory (`ram_verdict`):

- `· fits`
- `· tight — may swap under load`
- `· exceeds available memory`

The advisory is **advisory only** — it never blocks a download. To exercise:

1. Open the popup and confirm one row per catalog entry
   (`qwen2.5-0.5b-q4_k_m`, `llama-3.2-1b-q4_k_m`, `qwen2.5-1.5b-q4_k_m`,
   `gemma-2-2b-q4_k_m`), each carrying a fit suffix.
2. Pick a **non-recommended** model and click Download → that model downloads
   (log: `downloading <model> (<MB> MB) — progress in this log`), proving the
   picker index drives the target, not just `recommended()`.
3. Re-click Download on a model already on disk → the dest-exists guard logs
   `<model> already downloaded at <path> — delete it to re-download` (no
   re-fetch / clobber).
4. Pick an **encumbered** model (`llama-3.2-1b-q4_k_m` /
   `gemma-2-2b-q4_k_m`) with no prior acceptance → the **license click-through
   prompt** appears (the `download_gate` `NeedsLicense` path) before any fetch.
   Today's recommended default is unencumbered, so a plain run never prompts.

### Eight-tab Settings walkthrough

A quick LOOK pass over all eight panes:

- **Setup** — readiness checklist (Accessibility / Screen Recording / model
  file) plus the model picker + Download button; rows re-probe on each show.
- **General** — the master Enabled switch (the same atomic as the tray
  checkmark) plus autocorrect / trailing-space / Labs mid-line switches; flips
  live-apply and persist.
- **Apps** — per-app recorded-input counts from the encrypted memory store;
  count rows (`app — N`) carry a Delete button, status/empty rows do not.
- **Shortcuts** — current bindings (glyph-labelled) + the recorder boxes above.
- **Statistics** — shown / accepted / words / lifetime rows.
- **About** — static version / license / no-telemetry / repo / credits text.

## Example Acceptance Binaries

The live runner uses `platform_macos` examples:

- `textedit_observer_acceptance`
- `caret_marker_acceptance`
- `accept_tap_acceptance`
- `accept_insert_acceptance`
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

## Pending Manual Gates [added 2026-06-10]

- **Settings window LOOK gate [updated 2026-06-14]:** the structural pieces are
  now covered by the "Live UI LOOK Gates" section above (6 tabs render in
  Cotypist order; the modifier-combo recorder, Shortcuts glyph re-sync, model
  picker, and tray template icon each have a procedure). Still pending as a
  timed/behavioral spot-check: General switches live-flip + persist across a
  relaunch; Setup rows re-probe ≤480 ms while open; the Apps **Delete** button
  prompts with **Cancel** as the default button.
- **Encrypted memory accepted-only live gate (completed 2026-06-10):**
  `COMPME_MEMORY=accepted` + `COMPME_MEMORY_PATH` run without
  `COMPME_MEMORY_KEY` created and reused one `com.compme.memory` login-keychain
  entry across three runs; two Full-accept records landed for TextEdit, and
  `strings` over the database showed no typed plaintext.
- **Encrypted memory AllMonitored live gate (pending):**
  `COMPME_MEMORY=all` run that validates redactable monitored typed runs
  assembled from inserted deltas without storing pre-existing field text, plus
  secure input, disabled/snoozed, app/domain excluded, volatile `pid:N`, and
  per-app collection-off blocks. This remains manual/live blocked until a
  dedicated runner can create a GUI target and inspect the encrypted store.
- **Input Monitoring revoked spot-check (pending):** with Accessibility still
  granted, revoke Input Monitoring and confirm the transient Carbon accept path
  keeps working. This is a manual permission-state confirmation, not a
  requirement for the production accept path.
- **Lifetime stats gate (pending) [updated 2026-06-12, c128]:** `stats.env` is
  written by a 5-minute periodic flush during the run (quit = final flush);
  gate: accept ≥1 suggestion, quit, relaunch shows Lifetime row including the
  prior session. This remains manual/live blocked until a runner can drive a
  suggestion accept, terminate the app, relaunch, and read the UI.
