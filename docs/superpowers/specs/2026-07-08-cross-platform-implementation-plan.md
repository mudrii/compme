# Cross-platform implementation plan — Windows + Linux adapters

**Date:** 2026-07-08 · **Status:** Phase 0 ✅ shipped 2026-07-08; phases 1–6 pending/env-gated
**Prereqs:** clean `main` (builds, clippy clean, ≈1846 tests green).
**Supersedes:** nothing — details ROADMAP §1.1's pending half. ROADMAP stays the
status ledger; this doc is the execution guide.

Evidence base: full-codebase analysis 2026-07-08 (three-agent sweep: contract
inventory, macOS-leakage audit, build/CI/packaging audit), every claim
re-verified by grep against `b367f0f`. Status was reconciled again 2026-07-10
through implementation commit `a5781fc` and documentation commit `58debca`:
Phase 0 remains shipped, macOS v0.1.4 is released, and the real
Windows/Linux adapters plus Phases 3–6 remain pending.

**Release boundary:** v0.1.4 is tag `18b8dc0`. The runtime/release hardening,
A2 local/manual-only automation policy, and single model-location-control fix
described from later `main` are post-tag and require a subsequent release.
The post-tag shell hardening also replaced Windows `cmd /C start` URL launch
with `ShellExecuteW` (preserving metacharacters verbatim) and made Linux
`xdg-open` child reaping non-blocking; the pre-Phase-0 baseline below retains
the original launcher description only as historical derivation context.

## Verified Phase 0 baseline (historical)

This records the pre-Phase-0 state used to derive tasks 0.1–0.4. Current status
is the header above plus `docs/ROADMAP.md`; do not read the G1–G6 baseline gaps
below as still-open work after their corresponding Phase 0 item is marked shipped.

- `platform::PlatformAdapter` (15 methods), `ShellHost` (8 required + 9
  defaulted), `OverlayPresenter`, `TrayHandle` — contract complete and
  portable-shaped. Enums already carry the non-mac variants:
  `KeyInterceptMode::{LowLevelHook, XGrabKey, FocusScopedInhibit, ImeOwnsKey}`,
  `OverlayPlacement::{LayeredWindow, OverrideRedirect, LayerShell, ImeCandidate}`,
  `InsertStrategy::{SyntheticKeys, Clipboard, ImeCommit}`,
  `OffsetEncoding::{Utf16CodeUnits, UnicodeScalars, Utf8Bytes}`.
- `platform_windows` / `platform_linux`: 18 fail-closed `unsupported()` stubs
  each; only `environment()`, `front_app()→None`, `pump_events→sleep`,
  `open_url` (`cmd /C start` / `xdg-open`), overlay `hide()` are real. Every
  stub's doc comment names its target OS API.
- `app` selects adapters via Cargo target gates (`crates/app/Cargo.toml:32-42`);
  no unconditional Apple dep anywhere outside `platform_macos`.
- CI: `windows-latest` + `ubuntu-latest` run fmt/clippy/full tests
  (`--exclude platform_macos`) + `build -p app` on every push.
- Config dirs per-OS and unit-tested; atomic write/single-instance lock
  portable (MoveFileEx/LockFileEx semantics via stdlib).
- llama-cpp-2 `=0.1.146`: Metal on macOS, **CPU-only elsewhere** (Vulkan needs
  SDK at build time; `dynamic-backends` build.rs racy at this pin).
- Known shared-code gaps (all verified):
  - G1 `InsertStrategy::AxSet` is the only variant passing
    `supports_atomic_range_replace()` (platform/src/lib.rs:175); gates at
    run_loop.rs:674, engine_core:865, engine_core:1011. UIA/AT-SPI atomic edits
    have no variant to report → replacements/grammar-fix dead on Win/Linux.
  - G2 File hardening (0600/0700) is `cfg(unix)`-only: memory db + sidecars
    (memory/src/lib.rs:140-179), config atomic_write (config.rs:232-245),
    model_fetch .part. On Windows these land with inherited ACLs; memory db
    carries a plaintext `app` metadata column.
  - G3 `install_signal_handlers` is a no-op off-unix (run_loop.rs:96-100): no
    Ctrl-C/console shutdown, no SIGUSR1-style toggle on Windows.
  - G4 About-pane credits list macOS-only crates unconditionally (about.rs).
  - G5 Persisted key-chord format uses macOS modifier bit masks (shell.rs);
    Win/Linux adapters need a mask-translation layer.
  - G6 No packaging/signing/update path off macOS; release.yml not
    OS-parameterized.

## Phase 0 — shared-code pre-work (✅ SHIPPED; historical task text)

Small, independently shippable, all gate-green on macOS. Do these before any
adapter work so the contract is final when real impls land.

0.1 **`InsertStrategy::NativeRangeSet`** (fixes G1)
   - Add variant to `platform::InsertStrategy`; opt into
     `supports_atomic_range_replace()` (one site, post-d4c978f).
   - Semantics doc: "adapter-native atomic range replacement (UIA
     TextPattern/ValuePattern, AT-SPI EditableText); same contract as AxSet:
     all-or-nothing, verified readback where the API allows".
   - macOS behavior unchanged (never reports it).
   - Tests: extend `only_axset_supports_atomic_range_replace` →
     `only_atomic_strategies_support_range_replace` (AxSet + NativeRangeSet
     true, rest false); engine_core gate test with NativeRangeSet caps arming a
     replacement; platform_macos `refuse_non_atomic_replacement` unaffected
     (checks replace_left, not variant).
   - Touchpoints: platform/src/lib.rs enum + predicate + ux_mode docs;
     platform_windows/linux doc comments (state intended variant); ROADMAP.
   - Effort: S (½ day).

0.2 **Windows file hardening seam** (fixes G2)
   - Add `platform::fs_harden` (or `ShellHost` method) `harden_owner_only(path,
     is_dir)`: unix chmod (current behavior moved), Windows
     `SetNamedSecurityInfoW` owner-only DACL via `windows` crate — BUT the
     `windows` dep only activates inside `platform_windows`; shared crates call
     through a small trait/fn-pointer seam injected at store construction
     (memory::Store::open takes it today implicitly — add explicit param or
     callback in app wiring).
   - Simplest shape honoring "stdlib first": keep unix inline as-is; add a
     `#[cfg(windows)]` call into `platform_windows::harden_owner_only` from the
     three sites via a tiny `platform` re-export that is a no-op until the real
     impl lands. Fail-open with eprintln (posture, not correctness) — decide at
     review; memory db should arguably fail-closed.
   - Tests: unix behavior unchanged (existing 0600/0700 pins); Windows unit
     test in platform_windows asserting DACL owner-only (runs in CI windows
     job).
   - Effort: M (1-2 days incl. CI verification).

0.3 **Windows console-control handler** (fixes G3)
   - `SetConsoleCtrlHandler` in platform_windows behind the shell; wire
     `install_signal_handlers`'s `#[cfg(windows)]` arm to it (graceful shutdown
     parity with SIGINT/SIGTERM). Headless toggle (SIGUSR1 equivalent): named
     event `Global\compme-toggle` waited on a thread — defer to Phase 2 if not
     trivial.
   - Effort: S-M (1 day).

0.4 **Cosmetics** (fixes G4, doc drift)
   - About credits: cfg-gate the Apple-crate ACK entries.
   - Reword "CFRunLoop/AppKit/Keychain" comments in shared code to
     trait-neutral ("host event pump", "OS key store").
   - Effort: S (hours).

Phase 0 exit: contract frozen for adapter work; ROADMAP 1.1 updated; all
gates green on 3-OS CI.

## Phase 1 — Windows adapter (env-gated: needs a Windows dev machine)

Order chosen so each step yields a testable increment. The `windows` dep is
already active and pinned (`=0.61.3`, Phase 0.2/0.3, carrying
Foundation/Security/Console features) — extend its feature list with the
adapter features (`Win32_UI_Accessibility`, `Win32_UI_Input_KeyboardAndMouse`,
`Win32_UI_WindowsAndMessaging`). Feature-flag nothing; the crate is already
target-gated.

1.1 **Event pump + environment**
   - Real `pump_events`: `MsgWaitForMultipleObjectsEx` + `PeekMessage` loop
     honoring the heartbeat; `environment()` real version via
     `RtlGetVersion`.
   - `physical_memory_bytes`: `GlobalMemoryStatusEx`.
   - Acceptance: app boots, idles, quits cleanly on Windows (manual;
     scripted smoke in 1.7).

1.2 **UIA read path** (focus → capabilities → read_context → caret_rect)
   - `IUIAutomation` singleton on a dedicated STA thread (UIA callbacks have
     apartment requirements — mirror platform_macos's worker-thread pattern:
     one owner thread, mpsc request/reply, generation-stamped FieldHandles).
   - `AddFocusChangedEventHandler` → `subscribe_focus`; element runtime-id +
     pid → `FieldHandle{app, pid, element_id, generation}`.
   - `capabilities()`: TextPattern/ValuePattern presence → readable/writable;
     `IsPassword` → `SecurityState::SecureField`; insert_strategy:
     `NativeRangeSet` when TextPattern+ValuePattern allow ranged set, else
     `SyntheticKeys`; `KeyInterceptMode::LowLevelHook`;
     `OverlayPlacement::LayeredWindow`; `coords_global_screen=true`;
     offsets `Utf16CodeUnits`.
   - `read_context()`: TextPattern document range + selection endpoints
     materialized to UTF-16 offsets (UIA ranges are opaque — walk with
     `CompareEndpoints`/`MoveEndpointByUnit`); clamp + never panic.
   - `caret_rect()`: selection range `GetBoundingRectangles`, degenerate
     range fallback to `TextPattern2::GetCaretRange`.
   - `subscribe_caret`: `TextSelectionChanged` UIA event → rect recompute,
     coalesced (port CaretCoalescer pattern).
   - Acceptance: notepad + WordPad + Chromium/Electron + WinUI TextBox read
     correctly; UTF-16 surrogate tests (emoji in field) pass.

1.3 **Accept tap**
   - `SetWindowsHookEx(WH_KEYBOARD_LL)` on its own thread with message pump;
     Tab/Esc swallow only while `set_suggestion_visible(true)` — port the
     teardown_generation pattern from platform_macos AcceptTapController.
   - `set_accept_action`, `hide_suggestion_after`, `rearm` (keymap change →
     re-register); chord translation layer: persisted macOS mask bits →
     VK codes + MOD_* (fixes G5, keep translation in platform_windows).
   - Risk: LL hooks are process-trust sensitive (UIPI: no injection into
     elevated windows) — document; accept tap silently inert over elevated
     apps ⇒ ux_mode Hotkey fallback.

1.4 **Insert paths**
   - `insert` / `insert_replacing`: TextPattern selection collapse + either
     `ValuePattern::SetValue` full-value swap with caret restore (only when
     value snapshot matches — expected-text guard like
     insert_replacing_range) or `SendInput` synthetic keys (backspace×N +
     text via `KEYEVENTF_UNICODE`).
   - `insert_replacing_range`: report `NativeRangeSet` only where the control
     supports it (rich edits, WinUI); expected-text verification before
     replace; readback after (fail-open documented like macOS).
   - Acceptance: emoji `:smile`, typo fix, US→UK, grammar-fix range replace
     across notepad/Chromium/Word-online.

1.5 **Overlay**
   - `WS_EX_LAYERED|WS_EX_TRANSPARENT|WS_EX_NOACTIVATE|WS_EX_TOOLWINDOW`
     topmost window, `UpdateLayeredWindow` with per-pixel alpha; DirectWrite
     text run for ghost/correction; per-monitor DPI v2 awareness
     (`display_scales` real values).
   - Acceptance: ghost anchors at caret rect on mixed-DPI dual monitor.

1.6 **ShellHost services**
   - Memory key: DPAPI `CryptProtectData` user-scope blob in
     `%APPDATA%\compme\` (or CredWrite — pick DPAPI: no credential-manager UI
     surface), zeroize after use.
   - `confirm`: TaskDialogIndirect; `reveal_file`: `explorer /select,`;
     `set_launch_at_login`: HKCU Run key write/delete;
     `open_permission_settings`: n/a → `Ok` no-op documented (no TCC
     equivalent; UIA needs no consent for non-elevated).
   - Tray: Shell_NotifyIcon + popup menu mapping TrayFlags/SettingsFlags;
     settings window can stay webconfig-driven (browser open) initially.
   - Clipboard read (`read_clipboard_text`): `GetClipboardData(CF_UNICODETEXT)`.
   - Deep links: `compme://` via HKCU URL protocol registration; single
     instance already portable.
   - G3 toggle: named-event listener thread.

1.7 **Windows CI upgrades**
   - Extend windows job: run platform_windows unit tests (now real), plus a
     headless UIA smoke against a spawned notepad (best-effort; skip on
     runner-image variance, keep as scheduled job if flaky).
   - Add `cargo clippy -p platform_windows --all-targets -D warnings` with
     the `windows` dep active (already covered by workspace clippy once dep
     activates).

Effort: 4-8 weeks single dev with Windows hardware. Exit: ROADMAP 1.1
Windows flips ✅; acceptance matrix doc extended with Windows column.

## Phase 2 — Linux adapter, X11-first (env-gated)

Activate `atspi` (D-Bus) + `x11rb` deps. Wayland is Phase 3 — do not block
X11 on it.

2.1 **AT-SPI2 read path**: `atspi` crate over session bus; requires
    `org.a11y.Status.IsEnabled` (prompt user if off — the Linux analog of
    TCC). Focus events via `object:state-changed:focused` /
    `window:activate`; `Text`/`EditableText` interfaces for read/insert;
    offsets are characters → report `UnicodeScalars` (exercises non-UTF-16
    conversion paths for the first time — expect engine-side latent bug
    flush; the context-crate contract already wants scalars).
2.2 **Caret geometry**: `Text.GetCharacterExtents` at caret offset,
    `coords_global_screen=true` (CoordType::Screen).
2.3 **Accept tap (X11)**: `XGrabKey` on Tab/Esc only-while-visible is too
    invasive (grabs are exclusive) → prefer XTEST-passive approach:
    `x11rb` + XInput2 raw key events with a synthetic re-send suppress, or
    fall back to `KeyInterceptMode::FocusScopedInhibit`. Decision spike
    first (2 days, prototype both; AT-SPI device listeners are deprecated,
    libei is future-proof but compositor-gated).
2.4 **Insert**: `EditableText.InsertText/DeleteText` (report
    `NativeRangeSet`); XTEST synthetic fallback where EditableText absent.
2.5 **Overlay**: override-redirect X11 window (x11rb), ARGB visual for
    alpha; font rendering via existing text stack choice (pango or
    tiny-skia+fontdue — pick in spike; smallest dep wins).
2.6 **ShellHost**: libsecret (Secret Service D-Bus) key store — fail-closed
    when absent (headless servers); `/proc/meminfo`; zenity/kdialog confirm
    fallback chain with `eprintln` last resort; XDG autostart .desktop;
    tray via StatusNotifierItem D-Bus (ksni crate or hand-rolled) —
    degrade to none when no SNI host.
2.7 **CI**: ubuntu job gains `dbus` + `at-spi2-core` + Xvfb service; run
    platform_linux tests under `xvfb-run` with a11y bus launched; keep
    deterministic (no real desktop apps — test against a GTK fixture app
    spawned in Xvfb, gtk3 example checked into tools/acceptance).

Effort: 4-8 weeks after the 2.3 spike resolves. Exit: X11 desktops
(GNOME-Xorg, KDE-Xorg, XFCE) functional.

## Phase 3 — Wayland strategy

No global key grab, restricted synthetic input. Options, in preference
order (contract variants already exist):
1. **IME path**: implement as an input-method via `zwp_input_method_v2`
   (`InsertStrategy::ImeCommit`, `KeyInterceptMode::ImeOwnsKey`,
   `OverlayPlacement::ImeCandidate`) — cleanest, but compositor support
   varies (wlroots yes; GNOME needs IBus route).
2. **Portal/global-shortcuts**: `org.freedesktop.portal.GlobalShortcuts`
   for accept key + AT-SPI read path (AT-SPI works on Wayland for
   GTK/Qt apps) + layer-shell overlay (`zwlr_layer_shell_v1`; GNOME
   lacks it → fallback popup).
3. Accept degraded `UxMode::Hotkey` on GNOME-Wayland initially.
Decision spike (1 week) prototyping 1 vs 2 on GNOME + KDE + sway before
committing. Effort after spike: 3-6 weeks.

## Phase 4 — model runtime off-mac

- Add optional GPU features: `model_client` feature `vulkan` (Linux/Windows)
  and `cuda` (opt-in, huge toolchain) forwarding to llama-cpp-2; CI installs
  Vulkan SDK in a scheduled (not per-push) job to keep push CI fast.
- Revisit `dynamic-backends` when llama-cpp-sys-2 build.rs race
  (hard_link AlreadyExists, 0.1.146:1110) is fixed upstream; until then
  static per-backend builds.
- CPU-only remains the default fallback — correctness identical, latency
  gate numbers re-baselined per-OS (the 300ms budget may need a larger
  first-token allowance on CPU; measure before changing).

## Phase 5 — packaging & distribution

Windows: `cargo build --release` exe → MSIX (preferred: store-ready,
auto-update via App Installer) or NSIS fallback; Authenticode via Azure
Trusted Signing (secretless-job split mirrors macOS design: build cold on
untrusted runner, sign in protected environment); winget manifest PR
automation mirroring finalize-cask.sh.
Linux: AppImage first (single artifact, no repo infra), then Flathub
(sandbox: a11y bus + input portals need holes — document). .desktop +
icon + AppStream metainfo.
release.yml: refactor to per-OS job chains (preflight → validate →
prebuild(os) → sign(os) → publish collects all artifacts); keep the
prerelease containment semantics (hyphenated tags: prerelease + no
cask/winget/Flathub push).
Updater: per-OS story (MSIX auto-update / AppImageUpdate / Sparkle-style
later); manifest already published, add signature verification before any
in-app consumption (ROADMAP 1.2 note stands).

## Phase 6 — acceptance + docs

- Extend `ACCEPTANCE.md`'s Manual/Live Gate Ledger with Windows/Linux rows; port
  tools/acceptance e2e to per-OS variants (osascript → PowerShell UIA
  script / dogtail+Xvfb).
- ACCEPTANCE.md per-OS gates; ARCHITECTURE.md adapter chapters. Keep README's
  current support table honest (macOS released; Windows/Linux scaffold-only),
  then extend it with per-desktop UxMode expectations as real adapters land
  (GNOME-Wayland: Hotkey until Phase 3 lands, etc.).

## Cross-cutting rules

- Every adapter method lands with: unit test (fail-closed → real behavior
  flip), doc comment updated from "not yet implemented" to contract notes,
  ROADMAP 1.1 evidence anchor.
- Port the macOS worker-thread pattern (single owner thread + mpsc +
  generation stamps + poison-recovery) — UIA STA and D-Bus both need it;
  do not invent a second concurrency idiom.
- No new abstraction layers: implement against the existing 15-method
  trait; if a method's shape fights an OS API, change the trait (one
  place) rather than wrapping.
- Fail-closed stays the default for anything security-adjacent (secure
  fields, key store absent, a11y permission missing).

## Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Wayland fragmentation | Linux UX degraded on GNOME | Phase 3 spike; ship X11 first; honest docs |
| UIA coverage gaps (Chromium off-screen text) | wrong read_context | per-app compat tiers already exist (compat crate) — extend to Windows apps |
| LL-hook vs elevated windows (UIPI) | accept key dead over admin apps | detect + UxMode::Hotkey fallback |
| llama build.rs race pins CPU-only | slow inference off-mac | upstream fix tracked; Vulkan static feature in scheduled CI |
| CI runners can't run real a11y stacks | untested adapter paths | Xvfb+at-spi fixture app (Linux); scheduled best-effort UIA smoke (Windows); manual matrix in docs |
| Win ACL hardening wrong → silent posture gap | memory db readable | Phase 0.2 lands with an asserting Windows CI test, not fire-and-forget |
