//! `compme` — the macOS MVP integration binary.
//!
//! Wires the proven-in-isolation parts into one running process:
//! `PlatformAdapterImpl` (focus/caret/accept + AX reads/inserts),
//! `OverlayPresenterImpl` (ghost text), `Engine` (the deterministic state
//! machine), and a `LocalModel` (inference on a dedicated thread).
//!
//! See `docs/superpowers/specs/2026-06-06-p0-mvp-integration-design.md`.

mod about;
mod adapter;
mod config;
mod inference;
mod model_picker;
mod model_select;
mod run_loop;
mod screen_ocr;
mod setup_state;
mod shell;
mod status;
mod wiring;

fn main() {
    if let Err(err) = run_loop::run() {
        eprintln!("compme: fatal: {err}");
        std::process::exit(1);
    }
}
