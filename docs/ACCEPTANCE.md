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
- grant Input Monitoring permission to the terminal for event-tap gates
- open TextEdit and focus an editable document
- avoid password fields and apps that enable global Secure Input

The runner preflights:

- screen lock state
- global Secure Input PID
- required example binaries

Use `--force` only when intentionally collecting blocked harness logs.

### Default Gates

By default the runner builds the `platform_macos` examples, builds the
`complete-me` product binary, and runs:

- `textedit-read`
- `textedit-insert-synthetic`
- `textedit-insert-clipboard`
- `textedit-insert-axset`
- `caret-marker-textedit-any`
- `accept-insert-full`
- `accept-insert-word`
- `e2e-complete-me-pipeline`
- `e2e-complete-me-word-remainder`
- `popup-fallback-fixture`
- `accept-tap-inactive`
- `accept-tap-full`
- `accept-tap-word`
- `accept-tap-delayed-hide`
- `overlay-presenter`

If TextEdit is not running, TextEdit-dependent gates are skipped instead of
misreporting app-focus failures as product failures.

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
  + ~500 ms re-poll exercised live; Input Monitoring has no public prompt API.
- **Coordinate diagnostics** — `COMPLETE_ME_DIAG_COORDS=1` prints display scales +
  caret rect; measured scale 1.0 on the built-in display (no offset). True-2× /
  multi-monitor (scale > 1) is hardware-bound and unverified.
- **Config surface** — `config.env` layered under env (`env > file > default`),
  `SIGUSR1` toggle, debounce/max-words/max-tokens/heartbeat keys (unit-tested in
  `crates/app`).

> **[CORR 06-08] Accept-key harness:** after the accept-key flip (Tab → next word,
> grave → full), the `accept_tap_acceptance` and `accept_insert_acceptance` harnesses
> now post the key matching the requirement — **grave (keycode 50) for `full`**, Tab
> for `word` — so the `accept-tap-full` / `accept-insert-full` gates exercise the real
> grave→full path. Unit tests cover grave→full; the live desktop run of these two gates
> is still pending (FFI/GUI manual acceptance).

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
- missing Accessibility/Input Monitoring permission
- wrong focused target
- transient AX observer setup failures

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
