# Acceptance

This document describes the current automated and live acceptance checks for
Complete Me.

## Automated Gates

Root workspace:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --all-targets
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
acceptance regression tests in example targets.

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
`complete-me` product binary, and runs the deterministic/scriptable gates:

- `textedit-read`
- `textedit-insert-synthetic`
- `textedit-insert-clipboard`
- `textedit-insert-axset`
- `caret-marker-textedit-any`
- `popup-fallback-fixture`
- `overlay-presenter`

If TextEdit is not running, TextEdit-dependent gates are skipped instead of
misreporting app-focus failures as product failures.

The historical `accept-tap-*`, `accept-insert-*`, and `e2e-complete-me-*`
harnesses are no longer default automated evidence for product accept-key
consumption after the Carbon migration. **[CORR 2026-06-10]** The long-standing
claim that synthetic key posts cannot fire `RegisterEventHotKey` was
re-tested and is STALE: with the NSApp event pump in place (the cycle-41
fix), a System Events synthetic Tab DID fire the Carbon hotkey and accepted
live (`carbon hotkey fired id=1` → `accept Word`, scripted run, Chrome
textarea). The historical "registered but never fired" evidence was the
missing event pump, misattributed to synthetic-event filtering. Scripted
accept-key tests are therefore possible again; keep physical-key runs as the
final UX confirmation for gate records.
The runner reports these as `MANUAL`, not `SKIP`, when they are the remaining
Carbon evidence requirement. `--self-test` pins the runner's blocker
classification logic.

### Manual Physical Carbon Gates

Run these on an unlocked GUI session with Accessibility granted and no global
Secure Input process. Launch `complete-me`, focus TextEdit (or the app named by
the gate), type enough text to show a suggestion, then use a physical keyboard:

- grave/key-above-Tab accepts the full shown completion
- Tab accepts the next word and leaves the remainder visible
- Esc hides the suggestion and suppresses that field until refocus/edit
- Down cycles candidates without insertion
- Option+Tab passes a literal Tab through to the app
- revoke Input Monitoring and confirm accept behavior is unchanged

Record the product log line, target app, keyboard action, and resulting field
contents for each run. These manual runs are the authoritative evidence for
current Carbon hotkey consumption.

**PASSED 2026-06-10** (TextEdit, physical keyboard, two runs — one on a
secondary display): grave full-accept replaced `:smile` with the emoji
(`accept Full`), Tab word-accept advanced the caret and re-placed the
remainder ghost correctly (`accept Word`), Esc dismissed (`dismiss (Esc)`),
Down fired (`cycle candidate`; visible rotation needs
`COMPLETE_ME_CANDIDATES>1` — the default single candidate has nothing to
rotate; with `COMPLETE_ME_CANDIDATES=5` the user confirmed visible
candidate rotation live — that run's log was overwritten by a later
launch, so the evidence is the observation plus the earlier logged
`cycle candidate` firings). Not yet exercised: Option+Tab literal passthrough and the
revoke-Input-Monitoring re-check. Watch-item: one screenshot showed a
remainder ghost overlapping typed text after a word-accept; not reproduced
in the logged runs (caret advance was correct) — needs a repro with the log
preserved.

### Optional Gates

Browser marker geometry:

```sh
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --browser-pid <pid>
```

External popup fallback:

```sh
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --popup-pid <pid>
```

External popup fallback requires:

- popup-mode capabilities
- `AxSet` insertion
- post-insert readback proving the field value changed by the expected text

This prevents a capability-only pass from masking a failed insertion path.

### P1 Quality Validation (manual / scriptable, not in the default gate list)

The P1 "MVP quality" items (`docs/superpowers/specs/2026-06-07-p1-mvp-quality-design.md`)
are validated outside the A1b gate runner:

- **Tray instantiation** — launch `complete-me`; the `NSStatusItem` appears and the
  binary exits clean (smoke, no panic / "tray unavailable").
- **Enable/disable toggle + gating + dismiss** — scripted via `SIGUSR1` (the
  scriptable equivalent of the tray's Enable item): `Ready` → `SIGUSR1` →
  `Disabled enabled=false` (suggestions gated, ghost dismissed) → `SIGUSR1` →
  `Ready`. A literal tray-menu mouse click stays manual (accessory-policy item
  exposes no menu to System Events).
- **Permissions** — `accessibility_trusted()` true when granted; status derivation
  + ~500 ms re-poll exercised live. Production accept keys use Carbon hotkeys
  and no longer require Input Monitoring; historical A0 CGEventTap probes still do.
  Carbon hotkey consumption is a manual physical-key gate because macOS synthetic
  key posts do not fire `RegisterEventHotKey` the way real keyboard input does.
- **Coordinate diagnostics** — `COMPLETE_ME_DIAG_COORDS=1` prints display scales +
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

> **[CORR 06-09] Accept-key evidence boundary:** the 2026-06-08 synthetic
> harnesses closed the old consuming-CGEventTap path only. Production accept
> keys now use transient Carbon hotkeys; deterministic tests cover key mapping,
> engine accept/dismiss/cycle behavior, and insertion strategies, but physical
> keyboard runs are required to close Carbon product consumption. Design spec
> §15 G6/I11 is the authoritative record and must distinguish old tap evidence
> from current Carbon evidence.

### Useful Options

```text
--dry-run
--force
--skip-build
--skip-textedit
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

## A2 Compatibility And Context Gates

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

The `clipboard` and `screen` gates enable `COMPLETE_ME_DIAG_CONTEXT=1`; clipboard
requires the marker `CLIPBOARD-CONTEXT-MARKER` to reach the submit path, and
screen requires non-empty OCR context. The screen gate also requires Screen
Recording permission and visible text on the focused display.

## A2 Local-Replacement Live Gate (emoji / autocorrect / British English)

The local-replacement pipeline (`offer_replacement` → `Command::Replace` → AxSet
range-replace) is unit/build-verified but its physical-key accept is a **manual
live §16 gate** (synthetic key posts don't fire the Carbon accept the way real
input does, same boundary as the A1b accept gates).

Run `complete-me` with the feature enabled, focus an **AxSet** field (e.g.
TextEdit — the offer is gated to AxSet-capable fields; SyntheticKeys/Clipboard
apps are a separate backspace-synthesis residual), then with a physical keyboard:

- `COMPLETE_ME_EMOJI=1` — type `:smile`, accept → the `:smile` is deleted and the
  emoji glyph (skin-tone/gender per `_SKIN_TONE`/`_GENDER`) is inserted.
- `COMPLETE_ME_AUTOCORRECT=1` — type a known typo (e.g. `teh`), accept → replaced
  with the correction (`the`).
- `COMPLETE_ME_BRITISH_ENGLISH=1` — type a US-only spelling (e.g. `color`), accept
  → replaced with the UK form (`colour`).

Confirm the typed token is deleted (not left as `:smile😄`), the field value is
correct, and that the offer is **suppressed** in an excluded app / while snoozed /
in a terminal shell-command line / when the tray is disabled (shared suggestion
gating). Record the product log line, target app, keyboard action, and resulting
field contents for each.

**PASSED 2026-06-10** (live run, TextEdit/AxSet, physical keyboard,
`COMPLETE_ME_DEBUG=1` logs):

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
`COMPLETE_ME_ENABLED=false` to config.env and a relaunch started
`status=Disabled` directly from it. A later run validated the
snooze flow live: the tray click logged `suggestions snoozed for 60
minutes`, the render state flipped `snoozed=true`, and a fully typed
`:smile` while snoozed produced `decision=None` on every keystroke with
zero ghosts or model requests — the unified prefs gate blocks local
replacements and model completions alike. (The ghost-dismiss-on-snooze
edge ran with no ghost visible, so it stays unit-covered only.) Still
open live: backspace-synthesis in a non-AxSet app.

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
