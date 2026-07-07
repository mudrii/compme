# Cross-Platform Foundation Implementation Plan (Steps 1 + 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every crate except the OS adapters compile and test green on Windows + Linux CI (Phase A), then extend the platform contract with the shell seams (key store, tray, pump, prompts, deep links, autostart, permissions, clipboard, OCR, open-url) so the `app` binary itself compiles on all three OSes with fail-closed stubs (Phase B).

**Architecture:** Phase A removes the three portable-code breaks inside `app` (macOS config path, `libc::flock` lock, un-gated signal handlers) and widens the Windows/Linux CI jobs from the stub crates to the whole portable workspace. Phase B adds `platform::shell` (a `ShellHost` trait + `TrayHandle` + shared pure data), implements it in `platform_macos` by wrapping existing free functions, adds fail-closed impls to `platform_windows`/`platform_linux`, funnels ALL remaining `platform_macos::` references in `app` through one cfg-selected `crate::shell` module, then flips `app`'s Cargo deps to target-gated. macOS behavior changes zero — Phase B is a pure seam refactor gated by the existing ~1700-test suite.

**Tech Stack:** Rust 1.96.1 workspace, stdlib only (no new external deps — `std::fs::File::try_lock` replaces `libc::flock`), GitHub Actions (`windows-latest`, `ubuntu-latest` jobs already exist in `.github/workflows/ci.yml`).

**Non-goals (explicitly parked, do not build):**
- Real Windows/Linux adapters (UIA / AT-SPI2) — ROADMAP 1.1, needs real Win/Linux machines.
- Windows ACL equivalent of the `#[cfg(unix)]` 0600/0700 hardening — accepted degradation, documented.
- `compat` crate terminal-path heuristics for Win/Linux — behavioral tuning, needs real data.
- Non-macOS packaging (MSI/deb) — ROADMAP 1.2.
- Porting the AppKit settings window — it stays macOS-only behind the facade; long-term answer is webconfig-driven settings.

**Working rules (from CLAUDE.md):** work on `main`, commit directly, all of `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` green before every commit. Run `graphify update .` after code changes.

---

# Phase A — unblock the portable workspace on Windows/Linux CI

Everything in Phase A is buildable and testable on the macOS dev machine; the CI change in Task 4 is the proof gate.

### Task 1: Per-OS config directory

`config_file_path_from` hardcodes `$HOME/Library/Application Support/compme` (`crates/app/src/config.rs:117-128`). Make the OS branch explicit and testable on any host by threading `std::env::consts::OS` through a pure function (same lookup-injection pattern the file already uses).

**Files:**
- Modify: `crates/app/src/config.rs:110-128` (function + new tests in the existing `mod tests`)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/app/src/config.rs`:

```rust
#[test]
fn config_path_macos_uses_application_support() {
    let lookup = |key: &str| match key {
        "HOME" => Some("/Users/u".to_string()),
        _ => None,
    };
    assert_eq!(
        config_file_path_for(&lookup, "macos").unwrap(),
        PathBuf::from("/Users/u/Library/Application Support/compme/config.env")
    );
}

#[test]
fn config_path_linux_prefers_xdg_config_home() {
    let lookup = |key: &str| match key {
        "XDG_CONFIG_HOME" => Some("/home/u/.cfg".to_string()),
        "HOME" => Some("/home/u".to_string()),
        _ => None,
    };
    assert_eq!(
        config_file_path_for(&lookup, "linux").unwrap(),
        PathBuf::from("/home/u/.cfg/compme/config.env")
    );
}

#[test]
fn config_path_linux_falls_back_to_dot_config() {
    // Empty XDG_CONFIG_HOME must fall back too (XDG spec: empty == unset).
    let lookup = |key: &str| match key {
        "XDG_CONFIG_HOME" => Some(String::new()),
        "HOME" => Some("/home/u".to_string()),
        _ => None,
    };
    assert_eq!(
        config_file_path_for(&lookup, "linux").unwrap(),
        PathBuf::from("/home/u/.config/compme/config.env")
    );
}

#[test]
fn config_path_windows_uses_appdata() {
    let lookup = |key: &str| match key {
        "APPDATA" => Some(r"C:\Users\u\AppData\Roaming".to_string()),
        _ => None,
    };
    assert_eq!(
        config_file_path_for(&lookup, "windows").unwrap(),
        PathBuf::from(r"C:\Users\u\AppData\Roaming").join("compme").join("config.env")
    );
}

#[test]
fn config_override_wins_on_every_os() {
    let lookup = |key: &str| match key {
        "COMPME_CONFIG" => Some("/tmp/override.env".to_string()),
        _ => None,
    };
    for os in ["macos", "linux", "windows"] {
        assert_eq!(
            config_file_path_for(&lookup, os).unwrap(),
            PathBuf::from("/tmp/override.env")
        );
    }
}

#[test]
fn config_path_none_without_home() {
    let lookup = |_: &str| None;
    for os in ["macos", "linux", "windows"] {
        assert!(config_file_path_for(&lookup, os).is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p app config_path -- --nocapture`
Expected: FAIL — `config_file_path_for` not found.

- [ ] **Step 3: Implement**

Replace `config_file_path_from` (`config.rs:117-128`) with:

```rust
/// Lookup-injected core of [`config_file_path`] (the `Config::from_lookup`
/// pattern): testable without mutating the process environment — `set_var`
/// races parallel tests and is `unsafe` under edition 2024.
fn config_file_path_from(lookup: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    config_file_path_for(lookup, std::env::consts::OS)
}

/// Per-OS config home. `os` is `std::env::consts::OS` in production; injected
/// so every branch is testable on any host.
fn config_file_path_for(
    lookup: &impl Fn(&str) -> Option<String>,
    os: &str,
) -> Option<PathBuf> {
    if let Some(path) = lookup("COMPME_CONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let dir = match os {
        "windows" => PathBuf::from(lookup("APPDATA")?).join("compme"),
        "macos" => PathBuf::from(lookup("HOME")?).join("Library/Application Support/compme"),
        // Linux and other unix: XDG, empty value == unset per the XDG basedir spec.
        _ => match lookup("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(xdg) => PathBuf::from(xdg).join("compme"),
            None => PathBuf::from(lookup("HOME")?).join(".config/compme"),
        },
    };
    Some(dir.join("config.env"))
}
```

Also update the doc comment on `config_file_path` (`config.rs:107-109`) to say "per-OS config home (`~/Library/Application Support` on macOS, XDG on Linux, `%APPDATA%` on Windows)".

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p app config_path`
Expected: PASS (6 new tests). Then full crate: `cargo test -p app` — no regressions (existing macOS-path tests still pass because macOS branch is unchanged).

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -p app
git add crates/app/src/config.rs
git commit -m "feat: per-OS config directory (XDG on Linux, APPDATA on Windows)"
```

### Task 2: Instance lock via std `File::try_lock` (drop `libc::flock`)

`try_acquire_instance_lock` (`crates/app/src/config.rs:307-336`) uses `std::os::unix::io::AsRawFd` + `libc::flock` — a hard Windows compile break. Rust 1.89 stabilized `std::fs::File::try_lock()`, which is `flock(LOCK_EX | LOCK_NB)` on unix and `LockFileEx` on Windows — identical semantics (kernel releases on any exit), zero deps. Toolchain is pinned 1.96.1, so it's available.

**Files:**
- Modify: `crates/app/src/config.rs:307-336`

- [ ] **Step 1: Check existing coverage**

Run: `cargo test -p app instance_lock`
Note which tests exist (there are lock tests around `config.rs:631+`). They pin the public contract (`Held` vs `Io`) — they must pass unchanged after the swap. If a test references the private `instance_lock_error_from` helper, it will be updated in Step 2 (the helper dies).

- [ ] **Step 2: Swap the implementation**

Replace the body of `try_acquire_instance_lock` and delete `instance_lock_error_from` (`config.rs:307-336`):

```rust
/// Try to become THE compme instance: an exclusive, non-blocking OS file lock
/// on `path` (parent dirs created). `std::fs::File::try_lock` is
/// `flock(LOCK_EX | LOCK_NB)` on unix and `LockFileEx` on Windows — on every
/// OS the kernel releases it on ANY exit (crash included), the reason this is
/// a file lock and not a pid file. Launch-method-agnostic: a
/// LaunchServices-spawned copy and a direct-exec'd binary contend on the same
/// file (the c92 finding — two instances would double AX observers and hotkey
/// registrations).
///
/// On [`InstanceLockError::Io`] the app fails closed before installing AX
/// observers or hotkeys. Running unguarded can double-observe private context
/// and double-insert completions.
pub fn try_acquire_instance_lock(path: &Path) -> Result<InstanceLock, InstanceLockError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| InstanceLockError::Io(e.to_string()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| InstanceLockError::Io(e.to_string()))?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(InstanceLockError::Held),
        Err(std::fs::TryLockError::Error(err)) => Err(InstanceLockError::Io(err.to_string())),
    }
}
```

`InstanceLock`, `InstanceLockError`, and `instance_lock_path` stay as-is.

- [ ] **Step 3: Verify no `libc` left outside signal code**

Run: `grep -rn "libc::" crates/app/src --include='*.rs' | grep -v '^.*#\[cfg(test)\]'`
Expected: only the signal-handler block in `run_loop.rs:81-98` (handled in Task 3). If anything else appears, gate or replace it the same way before proceeding.

- [ ] **Step 4: Run tests**

Run: `cargo test -p app instance_lock && cargo test -p app config`
Expected: PASS — same public contract, `Held` on second lock, `Io` on unwritable dir.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -p app
git add crates/app/src/config.rs
git commit -m "fix: instance lock via std File::try_lock (portable, drops libc::flock)"
```

### Task 3: cfg-gate signal handlers, target-gate `libc`

`install_signal_handlers` (`crates/app/src/run_loop.rs:81-99`) calls `libc::signal(SIGINT/SIGTERM/SIGUSR1, ...)` un-gated — `SIGUSR1` does not exist on Windows. Gate the whole block `#[cfg(unix)]` with a documented no-op twin, and move the `libc` dependency under a unix target gate.

**Files:**
- Modify: `crates/app/src/run_loop.rs:81-99`
- Modify: `crates/app/Cargo.toml:32`

- [ ] **Step 1: Gate the handlers**

Replace `run_loop.rs:81-99` with:

```rust
#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    // Async-signal-safe: only a relaxed atomic store.
    STOP.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
extern "C" fn on_toggle(_sig: libc::c_int) {
    TOGGLE.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
fn install_signal_handlers() {
    let stop = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    let toggle = on_toggle as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installing handlers that only set atomic flags is safe.
    unsafe {
        libc::signal(libc::SIGINT, stop);
        libc::signal(libc::SIGTERM, stop);
        libc::signal(libc::SIGUSR1, toggle);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {
    // ponytail: no console-signal hooks off unix yet — STOP/TOGGLE stay false,
    // shutdown is the tray Quit item. SetConsoleCtrlHandler lands with the
    // real Windows adapter (ROADMAP 1.1).
}
```

The call site (`run_loop.rs:3690`) is unchanged.

- [ ] **Step 2: Target-gate the dep**

In `crates/app/Cargo.toml`, delete `libc = "0.2"` from `[dependencies]` and add:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 3: Verify**

Run: `cargo build --locked -p app && cargo test --locked -p app`
Expected: PASS — on macOS nothing changed (macOS is unix; both cfg arms resolve as before).

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings
git add crates/app/src/run_loop.rs crates/app/Cargo.toml
git commit -m "fix: cfg(unix)-gate signal handlers, target-gate libc dep"
```

### Task 4: Widen Windows/Linux CI from stub crates to the portable workspace

The `windows` job (`.github/workflows/ci.yml:166-199`) and `linux` job (`ci.yml:201-233`) only gate `-p platform_windows` / `-p platform_linux`. Every crate except `app` and `platform_macos` is portable — gate them all, so a portability regression in `engine`/`memory`/`redaction`/etc. turns CI red the day it lands.

**Files:**
- Modify: `.github/workflows/ci.yml:183-199` (windows job steps), `ci.yml:218-233` (linux job steps)

- [ ] **Step 1: Rewrite the scoped steps**

In BOTH jobs, replace the Format/Clippy/Test/Build steps (windows: `ci.yml:189-199`, linux: `ci.yml:223-233`) with (keep each job's existing checkout/toolchain/cache steps and adjust only the `-p` scoping; shown here for windows — linux is identical except the final `-p platform_linux`):

```yaml
      # The whole workspace minus the two crates that cannot build here:
      # `app` (hard-wired to platform_macos until the Phase B seam lands) and
      # `platform_macos` (Apple frameworks). This is the portability gate for
      # engine/memory/redaction/model_client/etc. — a unix-only or mac-only
      # leak in any shared crate turns this job red.
      - name: Format (workspace)
        run: cargo fmt --all -- --check

      - name: Clippy portable workspace (deny warnings)
        run: cargo clippy --locked --workspace --exclude app --exclude platform_macos --all-targets -- -D warnings

      - name: Test portable workspace
        run: cargo test --locked --workspace --exclude app --exclude platform_macos

      - name: Build adapter crate
        run: cargo build --locked -p platform_windows
```

Update both job `name:` fields to `portable workspace + platform_windows (Windows)` / `portable workspace + platform_linux (Linux)`. Rewrite the now-stale scoping comments (`ci.yml:183-188` and `:218-222`) to match.

- [ ] **Step 2: Known risk — `model_client` native build**

`model_client` builds `llama-cpp-2` with `dynamic-backends`/`vulkan` features off-macOS (`crates/model_client/Cargo.toml:18`). GitHub runners have cmake; if the vulkan feature demands SDK headers the job fails on that crate. Contingency (apply ONLY if the first CI run is red on llama build): add `--exclude model_client` to the clippy+test steps of the failing job with a comment `# llama-cpp vulkan needs SDK headers this runner lacks — re-include when the real adapter lands with its system-deps install step`, and `log` it in the commit message. Do not pre-emptively exclude.

- [ ] **Step 3: Validate YAML locally, push, watch CI**

```bash
ruby -ryaml -e 'YAML.load_file(".github/workflows/ci.yml")' && echo OK
git add .github/workflows/ci.yml
git commit -m "ci: gate the portable workspace on Windows + Linux runners"
git push
gh run watch --exit-status
```

Expected: all jobs green (or apply the Step 2 contingency and re-push).

**Phase A exit criterion:** Windows + Linux CI jobs compile and test ~22 of 25 crates. `app` + `platform_macos` remain macOS-only — that is Phase B.

---

# Phase B — the shell contract

The `PlatformAdapter`/`OverlayPresenter` traits cover field text I/O only. The app's whole product shell reaches into `platform_macos` directly at ~50 sites (adapters, tray, keychain, pump, permissions, prompts, deep links, settings/keymap). Phase B routes every one of those through two seams:

1. **`platform::shell`** — a new `ShellHost` trait (+ `TrayHandle`, `ConfirmPrompt`, and the pure data `TrayFlags`/`DisableArm`) for everything with a sane cross-platform meaning.
2. **`crate::shell` in `app`** — ONE cfg-selected module that is the only place allowed to name a `platform_*` crate. Things with no cross-platform meaning yet (AppKit settings window, mac keymap registration) are re-exported through it on macOS and given inert twins in the stub arm.

Design rules (match the existing `platform` crate conventions):
- Fail-closed defaults: privacy/secure probes default to the safe answer; capability probes default to "absent".
- Stub methods return `PlatformError::UnsupportedField { reason: "platform_{os}::{method} not yet implemented (Tier 1.1 scaffold)" }` via each stub crate's existing `unsupported()` helper — never panic.
- macOS behavior is bit-for-bit unchanged: every mac impl is a one-line wrapper over the existing free function. The 1700-test suite is the regression gate.

### Task 5: `platform::shell` — trait + shared data

**Files:**
- Create: `crates/platform/src/shell.rs`
- Modify: `crates/platform/src/lib.rs` (add `pub mod shell;` near the top, after the existing module docs)

- [ ] **Step 1: Write the failing default-behavior tests**

Bottom of the new `crates/platform/src/shell.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal impl providing only the required methods — pins that every
    /// default is fail-closed/absent, so a not-yet-ported platform inherits
    /// safe behavior (same pattern as the PlatformAdapter default tests).
    struct BareHost;
    impl ShellHost for BareHost {
        fn pump_events(&self, _heartbeat: std::time::Duration) {}
        fn physical_memory_bytes(&self) -> u64 {
            1
        }
        fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
            Ok(())
        }
        fn open_permission_settings(&self) -> Result<(), PlatformError> {
            Ok(())
        }
        fn reveal_file(&self, _path: &std::path::Path) -> Result<(), PlatformError> {
            Ok(())
        }
        fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
            Ok(())
        }
        fn confirm(&self, _prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
            Ok(false)
        }
        fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
            Err(PlatformError::UnsupportedField {
                reason: "bare".into(),
            })
        }
    }

    #[test]
    fn shell_defaults_are_fail_closed() {
        let h = BareHost;
        // No grant concept => trusted (Windows/Linux need no AX-style grant).
        assert!(h.accessibility_trusted());
        // Privacy probes fail SAFE: secure-input unknown => not secure-flagged
        // is the fail-open direction the engine already treats as "probe per
        // field"; screen capture unknown => absent (OCR stays off).
        assert!(!h.secure_input_enabled());
        assert!(!h.screen_capture_permission());
        assert!(!h.request_screen_capture_permission());
        assert_eq!(h.bundle_id_for_pid(1), None);
        assert_eq!(h.read_clipboard_text(), None);
        assert_eq!(h.screen_context_text(None, 100), None);
        assert!(h.display_scales().is_empty());
    }

    #[test]
    fn shell_host_is_object_safe_and_send_sync() {
        fn takes(_: std::sync::Arc<dyn ShellHost>) {}
        takes(std::sync::Arc::new(BareHost));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p platform shell`
Expected: FAIL — module/trait don't exist yet.

- [ ] **Step 3: Implement the module**

`crates/platform/src/shell.rs`:

```rust
//! Host shell services beyond field text I/O — the second half of the
//! cross-platform contract (ROADMAP 1.1). `PlatformAdapter` covers
//! focus/caret/read/insert; `ShellHost` covers the product shell around it:
//! event pumping, permissions, clipboard, OCR context, OS integration,
//! modal confirms, and the memory-key store. One impl per OS.
//!
//! Threading: `ShellHost` is `Send + Sync`; methods that present UI enforce
//! their platform's threading contract internally (macOS: main-thread check
//! returning `CannotComplete`, the same contract `OverlayPresenter` uses).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{PlatformError, ScreenRect};

/// A blocking modal confirmation. The confirming button must NOT be the
/// default — Return/Enter declines (the safe answer), matching the existing
/// macOS prompts.
pub struct ConfirmPrompt<'a> {
    pub title: &'a str,
    pub message: &'a str,
    /// Label of the confirming (non-default) button, e.g. "Allow", "Delete".
    pub confirm_label: &'a str,
}

pub trait ShellHost: Send + Sync {
    /// Drain queued native UI events, then service the platform main loop for
    /// at most `heartbeat` (paces the run loop; on macOS this is
    /// `pump_app_events()` + a CFRunLoop tick, elsewhere it may just sleep).
    fn pump_events(&self, heartbeat: Duration);

    /// Whether the process holds the OS grant required to observe and inject
    /// text. Default `true`: Windows (UIA) and Linux (AT-SPI2) need no
    /// macOS-style Accessibility trust grant.
    fn accessibility_trusted(&self) -> bool {
        true
    }
    /// Fire the OS permission prompt for that grant, if one exists. Returns
    /// the (possibly unchanged) grant state.
    fn prompt_accessibility_trust(&self) -> bool {
        true
    }
    /// Whether the OS reports a global secure-input session. Default `false`
    /// — per-field secure detection stays `Capabilities::security_state`;
    /// this is only the global kill-switch probe.
    fn secure_input_enabled(&self) -> bool {
        false
    }
    /// Screen-capture grant probe / request (gates OCR context). Fail-closed:
    /// unknown means absent, OCR stays off.
    fn screen_capture_permission(&self) -> bool {
        false
    }
    fn request_screen_capture_permission(&self) -> bool {
        false
    }

    fn physical_memory_bytes(&self) -> u64;
    fn bundle_id_for_pid(&self, _pid: i32) -> Option<String> {
        None
    }
    fn read_clipboard_text(&self) -> Option<String> {
        None
    }
    /// OCR'd text near the caret (macOS: Vision). `None` = unavailable.
    fn screen_context_text(
        &self,
        _caret_rect: Option<ScreenRect>,
        _max_chars: usize,
    ) -> Option<String> {
        None
    }
    fn display_scales(&self) -> Vec<(ScreenRect, f64)> {
        Vec::new()
    }

    /// Open `url` with the OS default handler. Non-blocking (spawn, don't wait).
    fn open_url(&self, url: &str) -> Result<(), PlatformError>;
    /// Open the OS settings pane where the user grants the text-access
    /// permission (macOS: Privacy → Accessibility).
    fn open_permission_settings(&self) -> Result<(), PlatformError>;
    /// Reveal `path` in the OS file browser.
    fn reveal_file(&self, path: &Path) -> Result<(), PlatformError>;
    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError>;

    /// Blocking modal confirm. `Ok(true)` only on an explicit confirm click.
    fn confirm(&self, prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError>;

    /// 32-byte memory-store encryption key from the OS key store, created on
    /// first use (macOS: Keychain; Windows: DPAPI/CredMan; Linux: libsecret —
    /// the latter two are Tier 1.1 scaffold errors today).
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError>;
}

/// Status-area handle (macOS: NSStatusItem). Owned by the main loop; NOT
/// `Send` — UI-thread only, like `OverlayPresenter`.
pub trait TrayHandle {
    fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError>;
    fn set_stats_line(&self, line: &str) -> Result<(), PlatformError>;
}
```

Then MOVE `TrayFlags` and `DisableArm` verbatim from `crates/platform_macos/src/tray.rs:22-63` into `shell.rs` (they are pure `Arc<AtomicBool>`/`Mutex` data — the `use` lines above already import what they need), delete them from `tray.rs`, and add there:

```rust
pub use platform::shell::{DisableArm, TrayFlags};
```

so every existing `platform_macos::{DisableArm, TrayFlags}` import keeps compiling. Add `pub mod shell;` to `crates/platform/src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p platform && cargo test -p platform_macos && cargo build --locked -p app`
Expected: PASS everywhere — the re-export keeps `app`'s existing `platform_macos::DisableArm`/`TrayFlags` imports working untouched.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
git add crates/platform/src/lib.rs crates/platform/src/shell.rs crates/platform_macos/src/tray.rs
git commit -m "feat: platform::shell contract — ShellHost + TrayHandle + shared tray data"
```

### Task 6: `MacosShellHost` — wrap the existing free functions

**Files:**
- Create: `crates/platform_macos/src/shell_host.rs`
- Modify: `crates/platform_macos/src/lib.rs` (add `mod shell_host; pub use shell_host::MacosShellHost;`)
- Modify: `crates/platform_macos/src/ui_prompt.rs` (add one pub wrapper over the private `run_confirm`)

- [ ] **Step 1: Add the generic confirm entry point**

In `ui_prompt.rs`, next to the existing three prompts (which stay until Task 10 deletes them):

```rust
/// Generic confirm for the ShellHost seam: same contract as the named
/// prompts — main-thread only, Cancel is the FIRST/default button (Return
/// declines), nested run loop while modal.
pub fn confirm_prompt(
    title: &str,
    message: &str,
    confirm_label: &str,
) -> Result<bool, PlatformError> {
    run_confirm(
        "confirm prompt requires the main thread",
        title,
        message,
        confirm_label,
    )
}
```

(Check `run_confirm`'s exact parameter order at the top of `ui_prompt.rs` — mirror what `confirm_deep_link_prompt` at `ui_prompt.rs:42-53` passes.)

- [ ] **Step 2: Implement the host**

`crates/platform_macos/src/shell_host.rs`:

```rust
//! `ShellHost` for macOS: every method is a thin wrapper over the free
//! functions this crate already ships — zero new platform behavior, only the
//! seam. Threading contracts (main-thread checks) live in the wrapped
//! functions, unchanged.

use std::path::Path;
use std::time::Duration;

use platform::shell::{ConfirmPrompt, ShellHost, TrayHandle};
use platform::{PlatformError, ScreenRect};

// SAFETY-free unit struct: all wrapped functions are process-global.
#[derive(Debug, Default)]
pub struct MacosShellHost;

impl MacosShellHost {
    pub fn new() -> Self {
        Self
    }
}

impl ShellHost for MacosShellHost {
    fn pump_events(&self, heartbeat: Duration) {
        crate::pump_app_events();
        // SAFETY: `kCFRunLoopDefaultMode` is a Core Foundation extern static.
        let mode = unsafe { core_foundation::runloop::kCFRunLoopDefaultMode };
        core_foundation::runloop::CFRunLoop::run_in_mode(mode, heartbeat, false);
    }
    fn accessibility_trusted(&self) -> bool {
        crate::accessibility_trusted()
    }
    fn prompt_accessibility_trust(&self) -> bool {
        crate::prompt_accessibility_trust()
    }
    fn secure_input_enabled(&self) -> bool {
        crate::secure_input_enabled()
    }
    fn screen_capture_permission(&self) -> bool {
        crate::screen_recording_permission()
    }
    fn request_screen_capture_permission(&self) -> bool {
        crate::request_screen_recording_permission()
    }
    fn physical_memory_bytes(&self) -> u64 {
        crate::physical_memory_bytes()
    }
    fn bundle_id_for_pid(&self, pid: i32) -> Option<String> {
        crate::bundle_id_for_pid(pid)
    }
    fn read_clipboard_text(&self) -> Option<String> {
        crate::read_pasteboard_text()
    }
    fn screen_context_text(
        &self,
        caret_rect: Option<ScreenRect>,
        max_chars: usize,
    ) -> Option<String> {
        crate::screen_context_text(caret_rect, max_chars)
    }
    fn display_scales(&self) -> Vec<(ScreenRect, f64)> {
        crate::display_scales()
    }
    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(drop)
            .map_err(|e| PlatformError::CannotComplete {
                reason: format!("open {url}: {e}"),
            })
    }
    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        self.open_url(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        )
    }
    fn reveal_file(&self, path: &Path) -> Result<(), PlatformError> {
        crate::reveal_file_in_finder(path)
    }
    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError> {
        crate::login_item::set_launch_at_login(enabled)
    }
    fn confirm(&self, prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        crate::ui_prompt::confirm_prompt(prompt.title, prompt.message, prompt.confirm_label)
    }
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        crate::keychain::KeychainKeyStore::new().load_or_create_memory_key()
    }
}

impl TrayHandle for crate::MacosTray {
    fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError> {
        crate::MacosTray::set_status(self, title, status_line, enabled, needs_accessibility)
    }
    fn set_stats_line(&self, line: &str) -> Result<(), PlatformError> {
        crate::MacosTray::set_stats_line(self, line)
    }
}
```

Adjust module paths to reality while implementing (`crate::login_item::` vs a re-export at crate root — check `platform_macos/src/lib.rs`'s `pub use` list and call whatever `app` calls today).

- [ ] **Step 3: Verify (smoke test)**

Append to `shell_host.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use platform::shell::ShellHost;

    #[test]
    fn physical_memory_is_nonzero() {
        assert!(MacosShellHost::new().physical_memory_bytes() > 0);
    }

    #[test]
    fn pump_events_returns_within_heartbeat_scale() {
        // Off the main AppKit thread the pump must still return (CFRunLoop
        // in this thread's default mode) — pins that pacing can't hang.
        let start = std::time::Instant::now();
        MacosShellHost::new().pump_events(Duration::from_millis(10));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
```

Run: `cargo test -p platform_macos shell_host`
Expected: PASS.

- [ ] **Step 4: Gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -p platform_macos
git add crates/platform_macos/src/shell_host.rs crates/platform_macos/src/lib.rs crates/platform_macos/src/ui_prompt.rs
git commit -m "feat: MacosShellHost — ShellHost/TrayHandle impls wrapping existing shell functions"
```

### Task 7: Fail-closed `ShellHost` + `OverlayPresenter` in the stub crates

Mirror the crates' existing scaffold style (`crates/platform_windows/src/lib.rs`, `crates/platform_linux/src/lib.rs`): every unsupported method routes through the existing `unsupported(method)` helper; reason strings and tests pin the behavior. Shown for Windows; Linux is identical with `platform_linux`/`LinuxShellHost`/`xdg-open`.

**Files:**
- Modify: `crates/platform_windows/src/lib.rs`
- Modify: `crates/platform_linux/src/lib.rs`

- [ ] **Step 1: Write the failing tests** (append to each crate's test module)

```rust
#[test]
fn shell_host_is_fail_closed() {
    use platform::shell::{ConfirmPrompt, ShellHost};
    let h = WindowsShellHost::new();
    assert!(!h.secure_input_enabled());
    assert!(!h.screen_capture_permission());
    assert!(h.load_or_create_memory_key().is_err(), "no key store yet — must fail closed");
    assert!(h
        .confirm(&ConfirmPrompt { title: "t", message: "m", confirm_label: "c" })
        .is_err());
    assert!(h.set_launch_at_login(true).is_err());
    assert!(h.reveal_file(std::path::Path::new("x")).is_err());
    assert!(h.open_permission_settings().is_err());
    // pump must return promptly (plain sleep pacing).
    let start = std::time::Instant::now();
    h.pump_events(std::time::Duration::from_millis(5));
    assert!(start.elapsed() >= std::time::Duration::from_millis(5));
}

#[test]
fn overlay_is_fail_closed_and_hide_is_idempotent() {
    use platform::OverlayPresenter;
    let mut o = WindowsOverlayPresenter::new();
    assert!(o
        .show_ghost(ScreenRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, "g")
        .is_err());
    assert!(o.update_ghost("g").is_err());
    o.hide().expect("hide is contractually idempotent-success");
    o.hide().expect("second hide too");
}
```

(Match `show_ghost`/`update_ghost` signatures to `crates/platform/src/lib.rs:566+` exactly — check `&str` vs `&String` and the `ScreenRect` argument order before writing.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p platform_windows shell && cargo test -p platform_linux shell`
Expected: FAIL — types don't exist.

- [ ] **Step 3: Implement**

Append to `crates/platform_windows/src/lib.rs` (reusing the file's `unsupported()` helper):

```rust
/// SCAFFOLD (ROADMAP Tier 1.1): fail-closed ShellHost. Real impl:
/// GlobalMemoryStatusEx (RAM), OpenClipboard (clipboard), DPAPI/CredWrite
/// (key store), SetConsoleCtrlHandler + a message pump (events), TaskDialog
/// (confirm), Startup-folder/registry Run key (autostart).
#[derive(Debug, Default)]
pub struct WindowsShellHost;

impl WindowsShellHost {
    pub fn new() -> Self {
        Self
    }
}

impl platform::shell::ShellHost for WindowsShellHost {
    fn pump_events(&self, heartbeat: std::time::Duration) {
        // ponytail: no native message pump yet — plain sleep paces the loop.
        std::thread::sleep(heartbeat);
    }
    fn physical_memory_bytes(&self) -> u64 {
        0 // scaffold: unknown RAM — callers already treat 0 GB as "no RAM info"
    }
    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        // `start`'s first quoted arg is the window title — keep "" so URLs
        // containing & survive cmd parsing.
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(drop)
            .map_err(|e| PlatformError::CannotComplete {
                reason: format!("start {url}: {e}"),
            })
    }
    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Err(unsupported("open_permission_settings"))
    }
    fn reveal_file(&self, _path: &std::path::Path) -> Result<(), PlatformError> {
        Err(unsupported("reveal_file"))
    }
    fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
        Err(unsupported("set_launch_at_login"))
    }
    fn confirm(
        &self,
        _prompt: &platform::shell::ConfirmPrompt<'_>,
    ) -> Result<bool, PlatformError> {
        Err(unsupported("confirm"))
    }
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Err(unsupported("load_or_create_memory_key"))
    }
}

/// SCAFFOLD (ROADMAP Tier 1.1): fail-closed overlay. Real impl: layered
/// click-through topmost window (WS_EX_LAYERED | WS_EX_TRANSPARENT).
#[derive(Debug, Default)]
pub struct WindowsOverlayPresenter;

impl WindowsOverlayPresenter {
    pub fn new() -> Self {
        Self
    }
}

impl platform::OverlayPresenter for WindowsOverlayPresenter {
    fn show_ghost(&mut self, _anchor: ScreenRect, _text: &str) -> Result<(), PlatformError> {
        Err(unsupported("show_ghost"))
    }
    fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
        Err(unsupported("update_ghost"))
    }
    fn hide(&mut self) -> Result<(), PlatformError> {
        Ok(()) // hide is contractually idempotent — nothing shown, nothing to do
    }
}
```

Linux mirror: `LinuxShellHost` (`open_url` via `xdg-open`, doc comment naming `/proc/meminfo`, libsecret, zenity/GTK dialog, XDG autostart .desktop as the future impls) + `LinuxOverlayPresenter` (doc: wlr-layer-shell / override-redirect X11).

- [ ] **Step 4: Run tests**

Run: `cargo test -p platform_windows && cargo test -p platform_linux`
Expected: PASS, including all pre-existing scaffold tests.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
git add crates/platform_windows/src/lib.rs crates/platform_linux/src/lib.rs
git commit -m "feat: fail-closed ShellHost + OverlayPresenter scaffolds for Windows/Linux"
```

### Task 8: `crate::shell` — the one cfg boundary in `app`

After this task the module exists and the macOS arm re-exports everything; migration of call sites is Tasks 9-13. The stub arm is completed incrementally as those tasks reveal the exact surface — it only has to COMPILE at the end of Task 14 (CI proves it), never run UI.

**Files:**
- Create: `crates/app/src/shell/mod.rs`
- Create: `crates/app/src/shell/macos.rs`
- Create: `crates/app/src/shell/stub.rs`
- Modify: `crates/app/src/main.rs:10-20` (add `mod shell;`)

- [ ] **Step 1: Build the symbol inventory (mechanical, do not skip)**

```bash
grep -n "platform_macos::" crates/app/src/*.rs | grep -v "^crates/app/src/shell/" > /tmp/pm-inventory.txt
grep -n "use platform_macos" crates/app/src/*.rs >> /tmp/pm-inventory.txt
```

Known inventory as of this plan (verify against the grep — anything new follows the same pattern):
- `adapter.rs:18` — `MacosPlatformAdapter` (type default)
- `run_loop.rs:31-36` — `DisableArm, accessibility_trusted, bundle_id_for_pid, display_scales, prompt_accessibility_trust, read_pasteboard_text, request_screen_recording_permission, screen_recording_permission, secure_input_enabled, MacosOverlayPresenter, MacosPlatformAdapter, MacosTray, TrayFlags`
- `run_loop.rs` body — `parse_accept_key` (368-372, 7388+), `KeymapError` (934), `PersonalizationEdit` (1225+), `APPS_ROWS`/`APP_POLICY_FIELDS`/`STATS_ROWS`/`SETUP_ROWS` (2692+), `keycode_label_with_mods` (2697), `SettingsFlags`/`physical_memory_bytes` (2809-2812), `effective_accept_keys_with_mods_and_grammar` (2863), `set_accept_keymap_from_config_with_mods` (3514), `set_shortcut_bindings_from_config` (3538), `install_url_event_handler` (3852), `set_launch_at_login` (3872), `keychain::KeychainKeyStore` (3890), `MacosTray::new` (3967), `MacosSettingsWindow::new` (4032), `set_tab_hotkey_suppressed` (4098), `policy_restore_needed` (4682), `reveal_file_in_finder` (4718), `format_accept_key` (4752+), `confirm_delete_app_prompt` (4821), `confirm_license_prompt` (4975), `confirm_deep_link_prompt` (5243), `pump_app_events` (5631), `ShortcutBindings`/`effective_shortcut_bindings`/`set_shortcut_bindings` (5682-5694, tests)
- `screen_ocr.rs:22` — `screen_context_text`

- [ ] **Step 2: Create the module skeleton**

`crates/app/src/shell/mod.rs`:

```rust
//! Compile-time platform binding — the ONLY app module allowed to name a
//! `platform_*` crate. Everything else imports from `crate::shell`, so the
//! Windows/Linux port is: fill in `stub.rs` (or split it per-OS) and flip
//! the Cargo target gates. Free functions with a cross-platform meaning go
//! through `platform::shell::ShellHost` instead; what lives here is either
//! construction (adapter/overlay/tray/host) or macOS-only surface that has
//! no contract yet (AppKit settings window, keymap registration).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::*;
```

`crates/app/src/shell/macos.rs` (grow this in Tasks 9-13; the full expected end state):

```rust
use std::sync::Arc;

use platform::shell::{ShellHost, TrayFlags, TrayHandle};
use platform::PlatformError;

pub type PlatformAdapterImpl = platform_macos::MacosPlatformAdapter;
pub type OverlayPresenterImpl = platform_macos::MacosOverlayPresenter;
pub type SettingsWindow = platform_macos::MacosSettingsWindow;
pub type UrlHandlerGuard = platform_macos::UrlEventHandler;

pub fn make_shell() -> Arc<dyn ShellHost> {
    Arc::new(platform_macos::MacosShellHost::new())
}

pub fn make_tray(flags: TrayFlags) -> Result<Box<dyn TrayHandle>, PlatformError> {
    platform_macos::MacosTray::new(flags).map(|t| Box::new(t) as Box<dyn TrayHandle>)
}

pub use platform_macos::install_url_event_handler;

// macOS-only surface with no cross-platform contract yet: the AppKit
// settings window and the Carbon keymap/shortcut registry. Re-exported
// name-for-name; `stub.rs` provides inert twins with identical signatures.
pub use platform_macos::{
    effective_accept_keys_with_mods_and_grammar, effective_shortcut_bindings,
    format_accept_key, keycode_label_with_mods, parse_accept_key, policy_restore_needed,
    set_accept_keymap_from_config_with_mods, set_shortcut_bindings,
    set_shortcut_bindings_from_config, set_tab_hotkey_suppressed, KeymapError,
    PersonalizationEdit, SettingsFlags, ShortcutBindings, APPS_ROWS, APP_POLICY_FIELDS,
    SETUP_ROWS, STATS_ROWS,
};
```

`crates/app/src/shell/stub.rs` initial content (adapter/overlay/host/tray only; the settings twins land in Task 13):

```rust
use std::sync::Arc;

use platform::shell::{ShellHost, TrayFlags, TrayHandle};
use platform::PlatformError;

#[cfg(windows)]
pub type PlatformAdapterImpl = platform_windows::WindowsAdapter;
#[cfg(windows)]
pub type OverlayPresenterImpl = platform_windows::WindowsOverlayPresenter;
#[cfg(target_os = "linux")]
pub type PlatformAdapterImpl = platform_linux::LinuxAdapter;
#[cfg(target_os = "linux")]
pub type OverlayPresenterImpl = platform_linux::LinuxOverlayPresenter;

#[cfg(windows)]
pub fn make_shell() -> Arc<dyn ShellHost> {
    Arc::new(platform_windows::WindowsShellHost::new())
}
#[cfg(target_os = "linux")]
pub fn make_shell() -> Arc<dyn ShellHost> {
    Arc::new(platform_linux::LinuxShellHost::new())
}

pub fn make_tray(_flags: TrayFlags) -> Result<Box<dyn TrayHandle>, PlatformError> {
    // Same failure path the run loop already handles when MacosTray::new
    // errors: the app runs headless-degraded, it does not crash.
    Err(PlatformError::UnsupportedField {
        reason: "tray not yet implemented (Tier 1.1 scaffold)".into(),
    })
}

/// Deep links: no scheme registration off macOS yet — inert guard, install Err.
pub struct UrlHandlerGuard;
pub fn install_url_event_handler(
    _on_url: Arc<platform::shell::UrlCallback>,
) -> Result<UrlHandlerGuard, PlatformError> {
    Err(PlatformError::UnsupportedField {
        reason: "deep links not yet implemented (Tier 1.1 scaffold)".into(),
    })
}
```

Prerequisite for that signature: `platform_macos::install_url_event_handler` takes `Arc<UrlCallback>` (`url_events.rs:73-75`). Read `UrlCallback`'s definition in `url_events.rs`; move the alias (expected `pub type UrlCallback = dyn Fn(String) + Send + Sync;` — adopt the real shape) into `crates/platform/src/shell.rs` and leave `pub use platform::shell::UrlCallback;` in `url_events.rs` — the same move-and-re-export pattern as `TrayFlags` in Task 5.

- [ ] **Step 3: Compile check (macOS arm only — stub arm compiles in CI at Task 14)**

Run: `cargo build --locked -p app && cargo test --locked -p app`
Expected: PASS — module exists, nothing consumes it yet.

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings
git add crates/app/src/shell crates/app/src/main.rs
git commit -m "feat: crate::shell cfg boundary in app (macOS arm + stub skeleton)"
```

### Task 9: Migrate run_loop free functions + pump to `ShellHost`

Pure mechanical substitution; the test suite is the gate. The `shell` handle is created once at the top of `run()` and threaded to where it's needed (most uses are inside `run()` itself — one giant function — so it's mostly a local variable).

**Files:**
- Modify: `crates/app/src/run_loop.rs`

- [ ] **Step 1: Construct the host at startup**

Right after the instance lock is acquired (near `run_loop.rs:3689`):

```rust
let shell = crate::shell::make_shell();
```

- [ ] **Step 2: Substitute call sites**

| Old (`platform_macos::` or bare import) | New |
|---|---|
| `accessibility_trusted()` (3697 + secure-poll sites) | `shell.accessibility_trusted()` |
| `prompt_accessibility_trust()` (3700) | `shell.prompt_accessibility_trust()` |
| `secure_input_enabled()` | `shell.secure_input_enabled()` |
| `screen_recording_permission()` (3903 + others) | `shell.screen_capture_permission()` |
| `request_screen_recording_permission()` | `shell.request_screen_capture_permission()` |
| `read_pasteboard_text()` | `shell.read_clipboard_text()` |
| `bundle_id_for_pid(pid)` | `shell.bundle_id_for_pid(pid)` |
| `display_scales()` | `shell.display_scales()` |
| `platform_macos::physical_memory_bytes()` (4007, 2811) | `shell.physical_memory_bytes()` (thread `&dyn ShellHost` or the `Arc` into `settings_flags_from_*` at 2809 — follow the existing parameter-injection style) |
| `platform_macos::pump_app_events(); let mode = unsafe {...}; CFRunLoop::run_in_mode(...)` (5631-5634) | `shell.pump_events(heartbeat);` |
| `Command::new("open").arg(ACCESSIBILITY_SETTINGS_URL).spawn()` (5602) | `shell.open_permission_settings()` (keep the same `eprintln!` error handling; delete the `ACCESSIBILITY_SETTINGS_URL` const at 71-72 — it lives in `MacosShellHost` now) |
| `Command::new("open").arg(url).spawn()` (5610) | `shell.open_url(url)` |
| `platform_macos::reveal_file_in_finder(&model_abs)` (4718) | `shell.reveal_file(&model_abs)` |

Delete the now-dead imports from `run_loop.rs:22` (`core_foundation::runloop`) and prune `run_loop.rs:32-36` accordingly. Where a helper function takes a closure/fn-pointer for one of these (the existing injection pattern, e.g. license-prompt injection at 803), keep the injection — only the production closure body changes to call `shell`.

- [ ] **Step 3: Run the full suite**

Run: `cargo test --locked -p app`
Expected: PASS — behavior identical (every ShellHost method is a wrapper over the exact function previously called).

- [ ] **Step 4: Gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
git add crates/app/src/run_loop.rs
git commit -m "refactor: run_loop shell free functions + event pump behind ShellHost"
```

### Task 10: Prompts, deep links, autostart, keychain through the seam

**Files:**
- Modify: `crates/app/src/run_loop.rs:3852-3897, 4821, 4975, 5243`
- Modify: `crates/platform_macos/src/ui_prompt.rs` (delete the three named prompts once unused)

- [ ] **Step 1: Deep links** (3852): change `platform_macos::install_url_event_handler` → `crate::shell::install_url_event_handler`; the guard binding `let _url_handler = ...` keeps its shape (type is `crate::shell::UrlHandlerGuard` per arm). Error arm unchanged (non-fatal).

- [ ] **Step 2: Autostart** (3872): `platform_macos::set_launch_at_login(enabled)` → `shell.set_launch_at_login(enabled)`. Error arm unchanged (non-fatal, expected for bare cargo binary).

- [ ] **Step 3: Keychain** (3890): the memory-key closure becomes

```rust
let shell_for_key = Arc::clone(&shell);
let memory = open_memory_store(&config.memory, || {
    match shell_for_key.load_or_create_memory_key() {
        Ok(key) => Some(key),
        Err(err) => {
            eprintln!("compme: OS key store memory key unavailable: {err}");
            None
        }
    }
});
```

- [ ] **Step 4: Prompts** — message composition moves app-side, the three call sites become:

```rust
// 4821 (delete recorded inputs):
let confirmed = shell
    .confirm(&platform::shell::ConfirmPrompt {
        title: "Delete recorded inputs?",
        message: &format!("All recorded inputs for {app} will be permanently erased."),
        confirm_label: "Delete",
    })
    .unwrap_or(false);

// 4975 (model license, inside the existing injection closure):
|model, license_name, terms_url| {
    shell
        .confirm(&platform::shell::ConfirmPrompt {
            title: "Accept model license?",
            message: &format!(
                "{model} is distributed under the {license_name}.\n\
                 Downloading requires accepting its terms:\n{terms_url}"
            ),
            confirm_label: "Accept",
        })
        .unwrap_or(false)
}

// 5243 (deep-link apply):
shell
    .confirm(&platform::shell::ConfirmPrompt {
        title: "Allow configuration change?",
        message: &format!("A compme:// link wants to apply {action} for:\n{scope}\n({trust})"),
        confirm_label: "Allow",
    })
    .unwrap_or(false)
```

The wording strings are copied verbatim from `ui_prompt.rs:42-84` — do not reword. Then delete `confirm_deep_link_prompt`, `confirm_license_prompt`, `confirm_delete_app_prompt` from `ui_prompt.rs` (keep `confirm_prompt` + `run_confirm`); fix any `platform_macos` tests that named them.

- [ ] **Step 5: Full suite + commit**

```bash
cargo test --locked && cargo fmt && cargo clippy --locked --all-targets -- -D warnings
git add crates/app/src/run_loop.rs crates/platform_macos/src/ui_prompt.rs
git commit -m "refactor: prompts, deep links, autostart, memory key through ShellHost"
```

### Task 11: `screen_ocr.rs` through the seam

**Files:**
- Modify: `crates/app/src/screen_ocr.rs:22,156` and its constructor + the construction site in `run_loop.rs`

- [ ] **Step 1:** Delete `use platform_macos::screen_context_text;` (`screen_ocr.rs:22`). Add an `Arc<dyn platform::shell::ShellHost>` field to the `ScreenOcr` worker (it already runs on its own thread — `ShellHost: Send + Sync` makes this legal), passed in through `ScreenOcr::new` from `run_loop`'s `Arc::clone(&shell)`.

- [ ] **Step 2:** `screen_ocr.rs:156`: `screen_context_text(request.caret_rect, max_chars)` → `self.shell.screen_context_text(request.caret_rect, max_chars)` (adjust receiver to the actual struct layout around line 156).

- [ ] **Step 3:** Existing screen-OCR tests use fakes/injection already where possible; if a test constructed `ScreenOcr` directly, hand it a tiny `struct NoScreenHost;` implementing `ShellHost` with the Task 5 `BareHost` shape (returns `None` for OCR).

- [ ] **Step 4:**

```bash
cargo test --locked -p app && cargo fmt && cargo clippy --locked --all-targets -- -D warnings
git add crates/app/src/screen_ocr.rs crates/app/src/run_loop.rs
git commit -m "refactor: screen OCR context through ShellHost"
```

### Task 12: Tray through `TrayHandle`

**Files:**
- Modify: `crates/app/src/run_loop.rs:31-36, 3967, and every `tray.` call`

- [ ] **Step 1:** Imports: replace `platform_macos::{MacosTray, TrayFlags}` and `platform_macos::DisableArm` with `platform::shell::{DisableArm, TrayFlags}` (they moved in Task 5; the names/fields are identical).

- [ ] **Step 2:** Construction (3967): `MacosTray::new(flags.clone())` → `crate::shell::make_tray(flags.clone())`. The surrounding `match` keeps its Ok/Err arms — the binding becomes `Box<dyn TrayHandle>`; find every `tray.set_status(...)`/`tray.set_stats_line(...)` call (`grep -n "tray\." crates/app/src/run_loop.rs`) — signatures are unchanged, so they compile as-is through the trait.

- [ ] **Step 3:**

```bash
cargo test --locked -p app && cargo fmt && cargo clippy --locked --all-targets -- -D warnings
git add crates/app/src/run_loop.rs
git commit -m "refactor: tray behind platform::shell::TrayHandle"
```

### Task 13: Settings window + keymap through the `crate::shell` facade

The AppKit settings window and Carbon keymap registry stay macOS-only — no trait, just name-for-name re-exports (already in `shell/macos.rs` from Task 8) plus inert twins in `shell/stub.rs`. Pure-data types move DOWN to `platform` so the twins don't need mirroring.

**Files:**
- Modify: `crates/app/src/run_loop.rs` (imports + `platform_macos::` → `crate::shell::` substitutions)
- Modify: `crates/platform/src/shell.rs` (receive moved pure data)
- Modify: `crates/platform_macos/src/lib.rs`, `crates/platform_macos/src/settings_window.rs` (move + re-export)
- Modify: `crates/app/src/shell/stub.rs` (inert twins)

- [ ] **Step 1: Move pure data down to `platform::shell`**

Read each of these definitions; every one that is pure data (no `objc2`/AppKit/Carbon imports in the type itself) MOVES to `crates/platform/src/shell.rs`, with a `pub use platform::shell::X;` left in `platform_macos` (the `TrayFlags` pattern from Task 5):
- `SettingsFlags` (`settings_window.rs:324` area) — expected pure `Arc`/atomic fields like `TrayFlags`
- `PersonalizationEdit` (`settings_window.rs:309`)
- `KeymapError` (`platform_macos/src/lib.rs:2941`)
- `ShortcutBindings` (`lib.rs:2812` area) and `EffectiveAcceptKeys` (near `lib.rs:3679`)
- consts `APPS_ROWS`, `APP_POLICY_FIELDS`, `APP_POLICY_FIELD_TITLES`, `STATS_ROWS`, `SETUP_ROWS`

Decision rule: if a definition drags AppKit types with it, do NOT move it — leave it mac-side and give `stub.rs` a signature-identical inert twin instead (twin rule below). Update `platform_macos` internal `use` paths after each move; `cargo test -p platform_macos` after each move keeps this honest.

- [ ] **Step 2: Facade the functions**

In `run_loop.rs`, replace every remaining `platform_macos::X` from the Task 8 inventory with `crate::shell::X`: `parse_accept_key`, `format_accept_key`, `keycode_label_with_mods`, `set_accept_keymap_from_config_with_mods`, `set_shortcut_bindings_from_config`, `set_shortcut_bindings`, `effective_shortcut_bindings`, `effective_accept_keys_with_mods_and_grammar`, `set_tab_hotkey_suppressed`, `policy_restore_needed`, `MacosSettingsWindow::new(...)` → `crate::shell::SettingsWindow::new(...)`. After this: `grep -c "platform_macos" crates/app/src/run_loop.rs` must print `0` (test modules included — test helpers migrate the same way).

- [ ] **Step 3: Write the stub twins**

In `shell/stub.rs`, add inert twins with EXACTLY the macOS signatures (compile target: the unmodified `run_loop.rs` call sites). Expected shapes — verify each against the real definition before writing:

```rust
// Keymap registry twins: accept keys can't fire on scaffold platforms
// (subscribe_accept is a stub error), so registration is inert and lookups
// return "nothing bound".
pub fn parse_accept_key(_raw: &str) -> Option<(i64, u32)> {
    None
}
pub fn format_accept_key(keycode: i64, mask: u32) -> String {
    format!("{mask}+{keycode}") // symmetric placeholder; never user-visible while parse returns None
}
pub fn keycode_label_with_mods(code: i64, mask: u32) -> String {
    format_accept_key(code, mask)
}
pub fn set_tab_hotkey_suppressed(_suppressed: bool) {}
pub fn policy_restore_needed(was_visible: bool, visible: bool) -> bool {
    was_visible && !visible // mirror the macOS pure logic if it did not move down in Step 1
}
// set_accept_keymap_from_config_with_mods / set_shortcut_bindings(_from_config) /
// effective_* : mirror return types from platform_macos/src/lib.rs:3571,3679+
// returning Ok(default)/Default::default() — inert success, never Err (a stub
// platform must not spam startup errors for a subsystem that cannot work yet).

/// Settings window twin: never shows; every flag-consuming read sees defaults.
pub struct SettingsWindow;
impl SettingsWindow {
    pub fn new(_flags: SettingsFlags) -> Self {
        Self
    }
    // Mirror ONLY the methods run_loop actually calls:
    //   grep -n "settings_window\." crates/app/src/run_loop.rs
    // giving each the inert behavior (visibility=false, Ok(()), no-op).
}
```

- [ ] **Step 4: Full suite + commit**

```bash
cargo test --locked && cargo fmt && cargo clippy --locked --all-targets -- -D warnings
git add crates/platform/src/shell.rs crates/platform_macos crates/app/src
git commit -m "refactor: settings window + keymap behind crate::shell facade; pure data moved to platform"
```

### Task 14: Flip `app` to target-gated platform deps

Precondition (verify, don't assume): `grep -rn "platform_macos" crates/app/src --include='*.rs' | grep -v "src/shell/macos.rs"` prints nothing.

**Files:**
- Modify: `crates/app/Cargo.toml`
- Modify: `crates/app/src/adapter.rs:18,26`
- Modify: `crates/app/src/run_loop.rs` (adapter/overlay construction sites)

- [ ] **Step 1: Cargo target gates**

In `crates/app/Cargo.toml`: delete the unconditional `platform_macos` and `core-foundation` lines (14, 31 — `core-foundation` became unused in Task 9; confirm with `grep -rn "core_foundation" crates/app/src`) and add:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
platform_macos = { path = "../platform_macos" }

[target.'cfg(windows)'.dependencies]
platform_windows = { path = "../platform_windows" }

[target.'cfg(target_os = "linux")'.dependencies]
platform_linux = { path = "../platform_linux" }
```

(`[target.'cfg(unix)'.dependencies] libc` already exists from Task 3.)

- [ ] **Step 2: Adapter default type**

`adapter.rs:18` — delete `use platform_macos::MacosPlatformAdapter;`. `adapter.rs:26`:

```rust
pub struct SharedAdapter<A: PlatformAdapter = crate::shell::PlatformAdapterImpl>(Arc<A>);
```

Update the module doc (`adapter.rs:1-9`) and the type-param comment (`adapter.rs:20-24`) to say `crate::shell::PlatformAdapterImpl` instead of `MacosPlatformAdapter`. In `run_loop.rs`, the adapter/overlay construction sites (`grep -n "MacosPlatformAdapter\|MacosOverlayPresenter" crates/app/src/run_loop.rs` — should already be `crate::shell::` names after Task 13; if construction was missed, switch to `crate::shell::PlatformAdapterImpl::new()` / `crate::shell::OverlayPresenterImpl::new()`, matching each crate's constructor).

- [ ] **Step 3: Runtime note (document, no code)**

On Windows/Linux the binary now compiles and starts, then `subscribe_focus` returns the scaffold `UnsupportedField` error and `run()` exits with a clear message — fail-closed by construction, exactly the ROADMAP 1.1 contract. No special-casing.

- [ ] **Step 4: Full local gates + commit**

```bash
cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
git add crates/app/Cargo.toml crates/app/src/adapter.rs crates/app/src/run_loop.rs Cargo.lock
git commit -m "feat: app compiles on all targets — platform deps behind cfg gates"
```

### Task 15: CI full-workspace gate + ROADMAP + graph

**Files:**
- Modify: `.github/workflows/ci.yml` (windows + linux jobs from Task 4)
- Modify: `docs/ROADMAP.md:30-66` (section 1.1)

- [ ] **Step 1: Drop the excludes**

In both jobs, the Task 4 steps become full-workspace (keep a `model_client` exclude ONLY if the Task 4 contingency was applied, same comment):

```yaml
      - name: Clippy workspace (deny warnings)
        run: cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings

      - name: Test workspace
        run: cargo test --locked --workspace --exclude platform_macos

      - name: Build app binary
        run: cargo build --locked -p app
```

(`--exclude platform_macos` stays: that crate is genuinely Apple-only and is not in the dependency graph on these targets.)

- [ ] **Step 2: ROADMAP 1.1 status update**

Update the "Pending 🔒" block: the app's adapter selection is DONE (cfg-gated via `crate::shell`), the shell contract (`platform::shell::ShellHost`/`TrayHandle`) is DONE with fail-closed scaffolds; remaining pending = the real UIA / AT-SPI2 adapter internals + real ShellHost impls (key store, tray, pump) — still blocked on Windows/Linux machines. Keep the tier marker `◑🔒`.

- [ ] **Step 3: Refresh the code graph**

Run: `graphify update .`

- [ ] **Step 4: Push and watch all three OS jobs**

```bash
git add .github/workflows/ci.yml docs/ROADMAP.md
git commit -m "ci: full-workspace build+test gate on Windows and Linux; roadmap 1.1 status"
git push
gh run watch --exit-status
```

Expected: macOS, Windows, Linux jobs all green. The Windows/Linux jobs now compile `app` itself — the stub arm of `crate::shell` gets its first real compile here; fix any signature drift the twins have (Task 13 Step 3 shapes) and re-push until green.

**Phase B exit criterion:** full workspace (minus `platform_macos`) builds and tests on all three OSes; `app` binary compiles everywhere and fails closed at runtime off-macOS; zero `platform_macos::` references outside `crates/app/src/shell/macos.rs`; macOS test suite (~1700 tests) unchanged and green.

---

## Execution notes

- **Order is load-bearing:** Tasks 5→8 build seams before Tasks 9→13 consume them; Task 14 only works when the Task 13 precondition grep is clean.
- **The stub arm cannot be compiled locally** (Homebrew rustc, no rustup targets). CI is the compile gate for `shell/stub.rs` — expect one fix-up iteration at Task 15 Step 4; that is planned, not a failure.
- **No behavior drift on macOS:** every migration task ends with the full existing suite. If any macOS test needs a behavioral edit (not a rename/import fix), stop — the seam leaked semantics; re-check the wrapper.
- **Line numbers** cited are as of commit `4d58e42`; re-grep before editing (`run_loop.rs` is 14k lines and shifts).

