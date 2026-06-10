//! Off-main-thread screen OCR (A2 §16 screen-aware context).
//!
//! Vision OCR of a full display takes ~200–800 ms. Running it inline on the
//! AppKit run loop (as the submit path previously did) stalls overlay repaint
//! and Carbon accept-hotkey callbacks for that entire time, blowing the
//! perceived-latency floor (design spec §11) and violating the "keep heavy work
//! off the main run loop" rule (§2 run-loop contexts).
//!
//! This worker performs the capture + OCR on its own thread and publishes the
//! redacted result into the shared `screen` cell the inference worker reads.
//! Callers fire-and-forget a caret rect per gated submit; bursts coalesce to the
//! latest rect, and the inference worker reads whatever the cell currently
//! holds. One-submit staleness is acceptable here — the same tradeoff the
//! clipboard path already makes — and is vastly cheaper than freezing the UI.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use platform::ScreenRect;
use platform_macos::screen_context_text;

/// Handle to the background screen-OCR worker. Dropping it closes the channel,
/// which makes the worker exit its loop; the thread is then joined.
pub struct ScreenOcr {
    tx: Option<SyncSender<Option<ScreenRect>>>,
    handle: Option<JoinHandle<()>>,
}

impl ScreenOcr {
    /// Spawn the worker. `screen` is the cell the inference worker reads;
    /// `max_chars` bounds the OCR output; `diag` mirrors `COMPME_DIAG_CONTEXT`
    /// logging so the off-thread path keeps the same diagnostics as the old
    /// inline path.
    ///
    /// Returns `Err` on OS thread-spawn failure (resource exhaustion) rather
    /// than panicking — the caller runs on the AppKit main thread and treats a
    /// failure as non-fatal (screen context disabled for the session), matching
    /// the tray-unavailable fallback.
    pub fn spawn(
        screen: Arc<Mutex<Option<String>>>,
        max_chars: usize,
        diag: bool,
    ) -> std::io::Result<Self> {
        // Depth-1 channel: one request can be in flight while at most one waits.
        // A `try_send` that finds the queue full drops the newest rect rather
        // than blocking the run loop (the queued rect is at most one submit old).
        let (tx, rx) = sync_channel::<Option<ScreenRect>>(1);
        let handle = std::thread::Builder::new()
            .name("compme-screen-ocr".into())
            .spawn(move || run(rx, screen, max_chars, diag))?;
        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
        })
    }

    /// Fire-and-forget an OCR request for the display under `caret_rect`. Never
    /// blocks: if the worker is busy and a request is already queued, the new
    /// rect is dropped (coalescing keeps the newest that fits).
    pub fn request(&self, caret_rect: Option<ScreenRect>) {
        if let Some(tx) = &self.tx {
            // Full ⇒ a request is already queued; Disconnected ⇒ worker gone.
            // Both are non-fatal here, so the result is intentionally ignored.
            match tx.try_send(caret_rect) {
                Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }
}

impl Drop for ScreenOcr {
    fn drop(&mut self) {
        // Drop the sender so the worker's `recv` returns `Err` and the loop
        // exits after its current pass. We **detach** rather than join: a Vision
        // OCR call can be mid-flight (and, on a sleeping/reconfiguring display,
        // could block for a long time), and joining here would hang process
        // teardown on the main thread. The worker holds only the `screen` Arc,
        // which it releases when it returns, so detaching is safe.
        self.tx.take();
        drop(self.handle.take());
    }
}

/// Worker body: block for the next rect, OCR the display under it, redact, and
/// publish into the shared cell. Exits when the channel closes.
fn run(
    rx: Receiver<Option<ScreenRect>>,
    screen: Arc<Mutex<Option<String>>>,
    max_chars: usize,
    diag: bool,
) {
    if max_chars == 0 {
        return;
    }
    while let Some(caret_rect) = recv_latest(&rx) {
        let text = screen_context_text(caret_rect, max_chars).map(|t| redaction::redact(&t));
        if diag {
            eprintln!(
                "compme: screen_context={:?}",
                text.as_ref().map(|s| s.chars().count())
            );
        }
        *screen.lock().unwrap_or_else(|e| e.into_inner()) = text;
    }
}

/// Block for the next rect, then drain any that piled up behind it and keep only
/// the newest. Returns `None` when the sender is gone (shutdown). Mirrors the
/// inference worker's `recv_latest` coalescing.
fn recv_latest(rx: &Receiver<Option<ScreenRect>>) -> Option<Option<ScreenRect>> {
    let mut rect = rx.recv().ok()?;
    while let Ok(newer) = rx.try_recv() {
        rect = newer;
    }
    Some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64) -> Option<ScreenRect> {
        Some(ScreenRect {
            x,
            y: 0.0,
            w: 1.0,
            h: 14.0,
        })
    }

    #[test]
    fn recv_latest_coalesces_a_burst_to_the_newest_rect() {
        let (tx, rx) = sync_channel::<Option<ScreenRect>>(8);
        tx.send(rect(1.0)).unwrap();
        tx.send(rect(2.0)).unwrap();
        tx.send(rect(3.0)).unwrap();
        let latest = recv_latest(&rx).unwrap();
        assert_eq!(latest.unwrap().x, 3.0);
    }

    #[test]
    fn recv_latest_returns_none_when_sender_dropped() {
        let (tx, rx) = sync_channel::<Option<ScreenRect>>(1);
        drop(tx);
        assert!(recv_latest(&rx).is_none());
    }

    #[test]
    fn request_never_blocks_when_queue_is_full() {
        // A depth-1 channel with no worker draining it: the first request fills
        // the queue, the rest must be dropped rather than block the caller.
        let (tx, _rx) = sync_channel::<Option<ScreenRect>>(1);
        let ocr = ScreenOcr {
            tx: Some(tx),
            handle: None,
        };
        // First fills depth-1 queue; subsequent calls hit `Full` and drop.
        for x in 0..100 {
            ocr.request(rect(x as f64));
        }
        // Reaching here without blocking is the assertion; keep `_rx` alive so
        // the sends hit `Full` (queue not drained) rather than `Disconnected`.
        drop(_rx);
    }
}
