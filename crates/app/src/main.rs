//! `compme` — the macOS MVP integration binary.
//!
//! Wires the proven-in-isolation parts into one running process:
//! `MacosPlatformAdapter` (focus/caret/accept + AX reads/inserts),
//! `MacosOverlayPresenter` (ghost text), `Engine` (the deterministic state
//! machine), and a `LocalModel` (inference on a dedicated thread).
//!
//! See `docs/superpowers/specs/2026-06-06-p0-mvp-integration-design.md`.

mod adapter;
mod config;
mod inference;
mod model_select;
mod run_loop;
mod screen_ocr;
mod setup_state;
mod status;
mod wiring;

fn main() {
    if let Err(err) = run_loop::run() {
        eprintln!("compme: fatal: {err}");
        std::process::exit(1);
    }
}
