//! `complete-me` — the macOS MVP integration binary.
//!
//! Wires the proven-in-isolation parts into one running process:
//! `MacosPlatformAdapter` (focus/caret/accept + AX reads/inserts),
//! `MacosOverlayPresenter` (ghost text), `Engine` (the deterministic state
//! machine), and a `LocalModel` (inference on a dedicated thread).
//!
//! See `docs/superpowers/specs/2026-06-06-p0-mvp-integration-design.md`.

mod adapter;
mod inference;
mod model_select;
mod wiring;

use std::sync::Arc;

use adapter::SharedAdapter;
use platform::PlatformAdapter;
use platform_macos::MacosPlatformAdapter;

fn main() {
    // Slice 1 placeholder: prove the shared adapter wiring compiles and the
    // binary links. The full run loop arrives in a later slice.
    match MacosPlatformAdapter::new() {
        Ok(adapter) => {
            let shared = SharedAdapter::new(Arc::new(adapter));
            let env = shared.environment();
            println!("complete-me: {:?}", env.os);
        }
        Err(err) => {
            eprintln!("complete-me: failed to start adapter: {err:?}");
            std::process::exit(2);
        }
    }
}
