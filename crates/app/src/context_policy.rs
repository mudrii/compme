//! Context-source bounds and live lifecycle transitions.
//!
//! The run loop supplies platform operations; this module owns the state
//! invariants shared by startup and Settings changes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::inference::ScreenContext;

/// Bounded best-effort wait for exact-stamped screen OCR. Vision can be slower
/// than this; late OCR is dropped rather than making suggestion latency unbounded.
pub(crate) const SCREEN_CONTEXT_WAIT_MS: u64 = 250;
/// Per-source character bound when previous-input context is enabled truthily.
pub(crate) const DEFAULT_CONTEXT_MAX_CHARS: usize = 160;

/// The completion worker's context char bound. Clipboard/screen context need
/// a positive bound even when previous-input context is off — with
/// `context_max_chars == 0` the worker's block builder returns `""` and the
/// enabled auxiliary sources would be a silent no-op. An explicit positive
/// bound always wins.
pub(crate) fn context_bound_chars(clipboard: bool, screen_active: bool, max_chars: usize) -> usize {
    if (clipboard || screen_active) && max_chars == 0 {
        DEFAULT_CONTEXT_MAX_CHARS
    } else {
        max_chars
    }
}

pub(crate) fn settings_context_bound_chars(max_chars: usize) -> usize {
    // Settings can enable clipboard context after launch. Keep the inference
    // worker's bound positive enough for that later enable; with no cells
    // populated, the generated context block remains empty.
    context_bound_chars(true, false, max_chars)
}

pub(crate) fn apply_clipboard_context_edge(on: bool, clipboard_cell: &Mutex<Option<String>>) {
    if !on {
        *clipboard_cell.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScreenContextEdge {
    Disabled,
    Enabled,
    RevertedDenied,
    RevertedSpawnFailed,
}

pub(crate) struct ScreenContextToggleState<'a, T> {
    pub(crate) config_screen_context: &'a mut bool,
    pub(crate) ui_flag: &'a AtomicBool,
    pub(crate) screen_cell: &'a Mutex<Option<ScreenContext>>,
    pub(crate) screen_ocr: &'a mut Option<T>,
}

pub(crate) fn apply_screen_context_edge<T>(
    on: bool,
    state: ScreenContextToggleState<'_, T>,
    mut set_wait_ms: impl FnMut(u64),
    screen_recording_permission: impl FnOnce() -> bool,
    spawn_screen_ocr: impl FnOnce() -> Result<T, String>,
) -> ScreenContextEdge {
    if !on {
        *state.screen_cell.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *state.screen_ocr = None;
        set_wait_ms(0);
        return ScreenContextEdge::Disabled;
    }

    if !screen_recording_permission() {
        *state.config_screen_context = false;
        state.ui_flag.store(false, Ordering::Relaxed);
        *state.screen_ocr = None;
        set_wait_ms(0);
        return ScreenContextEdge::RevertedDenied;
    }

    match spawn_screen_ocr() {
        Ok(ocr) => {
            *state.screen_ocr = Some(ocr);
            set_wait_ms(SCREEN_CONTEXT_WAIT_MS);
            ScreenContextEdge::Enabled
        }
        Err(_) => {
            *state.config_screen_context = false;
            state.ui_flag.store(false, Ordering::Relaxed);
            *state.screen_ocr = None;
            set_wait_ms(0);
            ScreenContextEdge::RevertedSpawnFailed
        }
    }
}
