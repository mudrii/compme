# P1 MVP Quality — Design

**Date:** 2026-06-07
**Status:** Implemented + live-validated on macOS (M4 Max). **[CORR 06-07]** All P1 items are *implemented*; live validation is complete **except** two hardware/UI-bound checks that stay manual: true-2× / multi-monitor coordinate offset (no second display available) and a literal tray-menu mouse click (the accessory-policy status item exposes no menu to System Events — the Enable toggle is validated via `SIGUSR1` instead). See "Live validation" and "Known environment limits" below.
**Scope:** P1 "MVP quality / correctness" items from the pending list:
- **6** Retina/multi-monitor offset — quantify via diagnostic + manual measurement (coordinate code already exists).
- **7** Tray / menu-bar UI — status + enable/disable toggle.
- **8** App lifecycle / permissions UX — Accessibility prompt + first-run guidance.
- **9** Settings / config surface — user-editable config file for the currently-hardcoded knobs.

**Cotypist parity note (2026-06-07 audit):** P1 completes the MVP control/permission/config layer. It does not complete parity with Cotypist's installed app or current website. Parity work begins after P1 and needs explicit plans for optional Screen Recording / screen-aware context, encrypted local personalization, per-app/per-domain controls, Google Docs setup, browser mirror/Text Metrics guidance, Terminal/iTerm AI-agent prompt support, full shortcut customization, updater/signing, telemetry policy, emoji, typo correction, and advanced overlay/backdrop/mirror UI.

Builds on the implemented P0 integration (`docs/superpowers/specs/2026-06-06-p0-mvp-integration-design.md`), with the post-accept-key-flip live GUI rerun still tracked there.

## Constraints (from the seam map)

- `objc2-app-kit 0.3.2` **default features include** `NSStatusBar`/`NSStatusItem`/`NSMenu`/`NSMenuItem` — no new Cargo feature/dep for the tray.
- `accessibility-sys 0.2.0` exports `AXIsProcessTrusted() -> bool`, `AXIsProcessTrustedWithOptions(CFDictionaryRef) -> bool`, `kAXTrustedCheckOptionPrompt: CFStringRef`.
- Secure input is already detectable internally (`IsSecureEventInputEnabled` Carbon FFI) and surfaces per-field via `Capabilities.security_state`.
- The run loop already owns the shared `NSApplication` (`.Accessory`) on the main thread and pumps the CFRunLoop each ~12ms heartbeat — the tray and any AppKit work piggyback on this.
- **Retina coordinate handling already exists**: `normalize_ax_screen_rect` divides by per-display `backingScaleFactor` when an app reports pixels, and `overlay_frame_for_text` Y-flips against the primary screen height. Item 6 is therefore *measurement/diagnostic*, not new geometry.
- No `serde`/`toml` in the workspace — the config surface stays dependency-free.

## Architecture

### 1. Application status (pure core — `crates/app/src/status.rs`)

A single derived status drives both the tray and suggestion gating.

```rust
pub enum AppStatus {
    Loading,            // model warming up
    Ready,              // suggestions active
    Disabled,           // user toggled off
    Blocked(BlockReason), // Permission | SecureInput
}
pub enum BlockReason { Permission, SecureInput }
```

Pure derivation (priority order):

```rust
pub fn derive_status(trusted: bool, secure: bool, ready: bool, enabled: bool) -> AppStatus
//  !trusted        -> Blocked(Permission)
//  secure          -> Blocked(SecureInput)
//  !ready          -> Loading
//  !enabled        -> Disabled
//  else            -> Ready
```

Helpers (pure): `AppStatus::suggestions_allowed() -> bool` (true only for `Ready`); `menu_title()`/`status_line()` returning the menu-bar text. The tray renders these strings; it owns no policy. **Unit-tested.**

### 2. Shared runtime state

`enabled`, `quit_requested`, `open_settings_requested` are `Arc<AtomicBool>` shared between the run loop and the tray's action target. `enabled` defaults true. The tray's menu actions only flip these atomics; the run loop observes them — keeping all policy on the run-loop side and the objc target trivial.

### 3. Config surface (item 9 — `crates/app/src/config.rs` + `run_loop.rs`)

A user-editable file layered under environment variables, reusing the existing tested `from_lookup`.

- **Location:** `$HOME/Library/Application Support/complete-me/config.env`, overridable with `COMPLETE_ME_CONFIG=<path>`.
- **Format:** dotenv-style `KEY=VALUE`, one per line; `#` comments and blank lines ignored; surrounding whitespace trimmed; first `=` splits key/value. Pure parser `parse_env_file(contents: &str) -> Vec<(String, String)>`. **Unit-tested.**
- **Layering:** the run loop builds a lookup `|key| env::var(key).ok().or_else(|| file_map.get(key).cloned())` and passes it to the existing `Config::from_lookup`. Env wins over file wins over default. No second parse path.
- **New keys** (previously hardcoded constants, now config with the same defaults + validation):
  - `COMPLETE_ME_DEBOUNCE_MS` (default 120, clamp 0..=5000)
  - `COMPLETE_ME_MAX_WORDS` (default 8, clamp 1..=50)
  - `COMPLETE_ME_MAX_TOKENS` (default 24, clamp 1..=200)
  - `COMPLETE_ME_HEARTBEAT_MS` (default 12, clamp 1..=100) — run-loop pump interval.
  - `COMPLETE_ME_MIN_CONTEXT` (default 3, clamp 0..=100) — minimum trimmed left-context chars before a completion is requested (conservative trigger gating; engine-macos design §4 / plan-review F5, added 2026-06-08).
  - `COMPLETE_ME_MIDLINE` (default off; `1`/`true` to enable) — allow completions when the caret splits a word; off by default to protect first-run trust.
  - plus existing `COMPLETE_ME_MODEL_PATH`, `COMPLETE_ME_PROMPT_MODE`.
- Runtime control: `SIGUSR1` toggles enable/disable (headless equivalent of the tray's Enable item; see the tray section).
- Test/gate-only knobs (`ACCEPTANCE_PID`, `STUB_COMPLETION`, `RUN_MS`, `DIAG_COORDS`, `CONFIG`) stay env-sourced (reading them from the file is harmless but not advertised).
- Typed parse/clamp lives in pure helpers (`parse_clamped(raw, default, min, max)`) — **unit-tested**; invalid values fall back to the default rather than failing startup.

### 4. Permissions UX (item 8 — `crates/platform_macos` free fns + run-loop startup)

New free functions in `platform_macos` (process-global, no `&self`):

```rust
pub fn accessibility_trusted() -> bool;          // AXIsProcessTrusted()
pub fn prompt_accessibility_trust() -> bool;     // AXIsProcessTrustedWithOptions({prompt: true})
pub fn secure_input_enabled() -> bool;           // IsSecureEventInputEnabled() != 0
```

Startup flow in `run()`:
1. `accessibility_trusted()`; if false, call `prompt_accessibility_trust()` once to fire the system dialog, log guidance. Continue running (adapter init may still partially work; status shows `Blocked(Permission)`).
2. The tray exposes **Open Accessibility Settings** when blocked; the action sets `open_settings_requested`, and the run loop runs `open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"` (via `std::process::Command`, no AppKit).
3. Secure input **and trust** are re-polled on the same ~500ms throttle (every ~40 heartbeats) and fed into the status derivation. Re-polling trust matters: if the user grants Accessibility while the app is running (via the prompt or the tray's settings affordance), `Blocked(Permission)` clears without a restart. (Per-field secure state continues to be handled by the engine via `Capabilities` on focus.)

Input Monitoring has no public prompt API; failure to install the accept tap already surfaces as a `PlatformError` at startup and is logged with guidance — no extra UI in P1.

Screen Recording is intentionally not requested in P1. Cotypist uses it optionally for screen-aware context; Complete Me should add that in A2+ with local-only processing, a clear off path, and a field-only fallback when the permission is denied.

### 5. Tray / menu-bar UI (item 7 — `crates/platform_macos/src/tray.rs` or in `lib.rs`)

`MacosTray`, constructed on the main thread (reuses the shared `NSApplication`):

- Holds a `Retained<NSStatusItem>` from `NSStatusBar::systemStatusBar()` and a `Retained<NSMenu>`.
- An objc2 `define_class!` action target stores the shared `Arc<AtomicBool>` flags as ivars and implements `toggleEnabled:`, `openSettings:`, `quit:`, each flipping its atomic.
- Menu items: a disabled **status line**, an **Enable/Disable** toggle (checkmark reflects `enabled`), **Open Accessibility Settings** (shown only when `Blocked(Permission)`), **Quit**.
- `set_status(&self, status: AppStatus)` updates the status-bar button title/symbol and the menu (toggle check state, status line text, settings-item visibility) from the pure `AppStatus` strings.

This is the bulk of the AppKit/objc2 glue: **build-verified + live**, not unit-tested. All decision logic stays in the pure `status` module.

### 6. Retina diagnostic (item 6)

- Expose `MacosPlatformAdapter::display_scales() -> Vec<(ScreenRect-ish bounds, f64 scale)>` (wrap existing `active_display_scales`).
- `COMPLETE_ME_DIAG_COORDS=1`: the run loop logs, per caret rect, the resolved `ScreenRect` plus each display's bounds + scale, so a real Retina + external-monitor offset can be measured by inspection.
- The spec records the manual measurement procedure (below). No speculative coordinate code — the existing `normalize_ax_screen_rect` stands until a real offset is observed.

### 7. Engine addition

`Engine::on_dismiss(&mut self) -> Result<Vec<CompletionRequest>, PlatformError>` wrapping the existing `core::Event::Dismiss` so the run loop can hide a showing ghost the instant the user disables the app. **Unit-tested in `crates/engine`.**

### 8. Run-loop integration (`crates/app/src/run_loop.rs`)

- Load config (file + env), build `AppState`, run permissions startup.
- Construct `MacosTray` with the shared flags.
- Each heartbeat: poll secure input (throttled) → `derive_status(trusted, secure, ready, enabled)` → `tray.set_status(...)`; submit requests only when `status.suggestions_allowed()`; when `enabled` transitions true→false, call `engine.on_dismiss()`; if `quit_requested`, break; if `open_settings_requested`, run the settings `open` once and clear it; optional coord diagnostic.
- Teardown unchanged, plus drop the tray before the engine/adapter.

## Manual Retina measurement procedure (item 6)

1. `COMPLETE_ME_DIAG_COORDS=1 COMPLETE_ME_ACCEPTANCE_PID=<TextEdit> ./target/release/complete-me`
2. Focus TextEdit on the **built-in Retina** display, type, read the logged `ScreenRect` vs the visible caret; confirm the ghost lands on the caret.
3. Move the TextEdit window to an **external non-Retina** monitor; repeat. Compare logged display scales and ghost placement.
4. If any offset is observed, record the delta + display scale here; that becomes the input for real geometry work. If none (expected — AX returns points), mark item 6 closed-by-measurement.

## Testing strategy

- **Pure, unit-tested:** `derive_status` (all branches + priority), `AppStatus` strings + `suggestions_allowed`, `parse_env_file` (comments/blank/whitespace/`=`-in-value), `parse_clamped` (default/clamp/invalid), config layering precedence, `Engine::on_dismiss`.
- **Build-verified + live:** `MacosTray` (objc2), AX/secure-input FFI, run-loop glue, the settings `open`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` stay green.
- Live: tray appears in the menu bar, toggle disables suggestions + hides ghost, Quit exits cleanly, blocked state shows when Accessibility is revoked.

## Live validation — 2026-06-07 (Apple M4 Max, macOS 25.5)

- **Tray instantiates** live (status item appears; binary exits clean) — smoke run, no panic, no "tray unavailable".
- **Enable/disable toggle + gating + dismiss** verified live and headless via `SIGUSR1` (a scriptable equivalent of the tray's Enable item): `status=Ready` with completions flowing → SIGUSR1 → `status=Disabled enabled=false` (suggestions gated, showing ghost dismissed) → SIGUSR1 → `status=Ready`. The tray menu's Enable item flips the same `flags.enabled` atomic.
- **Permissions:** `accessibility_trusted()` returns true (granted); status derivation + re-poll exercised live.
- **Retina (item 6):** measured on the built-in display — `display_scales = [(0,0,3840×1600), 1.0]` and caret rect `(619.05, 215.0, 1×14)` global screen points. Scale 1.0 ⇒ AX points equal pixels, `normalize_ax_screen_rect` pass-through correct, **no offset**. The true-2× / multi-monitor case (scale > 1.0) remains hardware-bound (no second display available); the diagnostic + normalization code are ready for it.

### Known environment limits (manual QA only)
- **Visual menu-bar click** of the tray cannot be automated: macOS exposes 0 menu bars for an accessory-policy status item to System Events. The toggle *behavior* is verified via SIGUSR1 + unit tests (`should_dismiss`, `derive_status`, `suggestions_allowed`); only the literal mouse-click on the menu item is manual.
- **Multi-monitor Retina offset** needs a second display.

## Additional config knobs (implemented)

- `COMPLETE_ME_HEARTBEAT_MS` (clamp 1..=100, default 12) — run-loop pump interval.
- `SIGUSR1` toggles enable/disable at runtime (headless control surface alongside the tray).
- Caret read-coalescing is handled at the adapter layer (`CARET_COALESCE_INTERVAL_MS = 25`), not duplicated in the run loop.

## Out of scope (P2+)

Per-app settings/personalization, per-domain browser controls, multi-candidate UI, local encrypted memory, optional Screen Recording / OCR context, native inline-prediction suppression, IME composition, KV-cache reuse, long-lived model actor, sentence/punctuation stop-boundary, Windows/Linux adapters. Automated multi-monitor geometry correction beyond the existing pixel/point guard (revisit only if measurement shows an offset).

Specific Cotypist-alignment backlog:

- Google Docs Accessibility setup detection/onboarding.
- Arc/Dia Text Metrics guidance and Firefox/Zen mirror-window fallback.
- Terminal.app/iTerm AI-agent prompt activation, while leaving normal shell completion alone by default.
- Current compatibility matrix, including Slack partial support, VS Code/Cursor/Windsurf sidebar-chat-only activation, TheBrain support verification, and explicit unsupported status for Pages/Scrivener/Thunderbird/OneNote/BBEdit/Sublime/Ghostty/cmux/Warp unless proven otherwise.
- Full shortcut settings: next-word, full-completion, dismiss, force-activate, temporary app toggle, global toggle, and per-app Tab disable.
- Custom instructions and personalization across global, per-app, and per-domain contexts, including a recheck of current Cotypist strength/tier semantics before choosing 3-level, 6-stop, or intentionally divergent behavior.
- Encrypted local typing-history database with Keychain-protected key, per-app/domain counts, delete-all, and per-scope deletion.
- Typo/suggested-fix behavior separated from full autocorrect, because current public pages describe both and not all help copy agrees.
- Product/tier decision: either implement feature gates for quotas, larger models, clipboard awareness, custom instructions, autocorrect, Labs, and device counts, or document an explicit no-pricing-gates divergence.
- Telemetry decision and policy. P1 has no network telemetry; any future crash/usage reporting needs explicit payload, provider, region, default state, and opt-out/opt-in semantics.
- Signed/notarized release packaging plus native updater artifacts, signing key handling, endpoint format, and failure recovery. Sparkle is the leading candidate because Cotypist ships Sparkle; any non-Sparkle updater needs an explicit A3 decision.

## Decisions (from brainstorming)

- Config: dotenv-style `key=value` file layered under env via the existing `from_lookup` (no new deps).
- Tray: full interactive menu (status + toggle + open-settings + quit) via an objc2 `define_class!` target.
- Permissions: prompt on launch + blocked status + open-settings menu item.
- Retina: diagnostic + manual measurement (existing geometry kept).
