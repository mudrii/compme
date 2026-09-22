//! `compme` — the integration binary, one run loop for every target OS.
//!
//! Wires the proven-in-isolation parts into one running process:
//! `PlatformAdapterImpl` (focus/caret/accept, context reads, inserts) and
//! `OverlayPresenterImpl` (ghost text), which the `shell` module binds per
//! target at compile time — `platform_macos` on macOS (the shipped product),
//! `platform_windows` on Windows, `platform_linux` on Linux, each within its
//! adapter's documented boundary — plus `Engine` (the deterministic state
//! machine) and a `LocalModel` (inference on a dedicated thread).
//!
//! See `docs/superpowers/specs/2026-06-06-p0-mvp-integration-design.md`.

mod about;
mod adapter;
mod builders;
mod config;
mod context_policy;
mod feature_policy;
mod inference;
mod loop_state;
mod model_picker;
mod model_select;
mod run_loop;
mod screen_ocr;
mod settings_runtime;
mod setup_state;
mod shell;
mod status;
mod url_actions;
mod wiring;

pub(crate) fn write_stderr(args: std::fmt::Arguments<'_>) {
    use std::io::Write;

    let _ = writeln!(std::io::stderr().lock(), "{args}");
}

fn main() {
    if let Err(err) = run_loop::run() {
        eprintln!("compme: fatal: {err}");
        std::process::exit(1);
    }
}
