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
//! Callers fire-and-forget the focused field plus caret rect per gated submit;
//! bursts coalesce to the latest request. The inference worker only consumes a
//! result when its request stamp still matches, so async OCR cannot leak a prior
//! request's visible text into a later prompt.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::inference::ScreenContext;
use platform::FieldHandle;
use platform::ScreenRect;
use platform_macos::screen_context_text;

/// Handle to the background screen-OCR worker. Dropping it closes the channel,
/// which lets the worker exit its loop after any in-flight OCR pass.
pub struct ScreenOcr {
    queue: Option<Arc<LatestRequestQueue>>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug, PartialEq)]
struct ScreenOcrRequest {
    field: FieldHandle,
    generation: u64,
    snapshot: u64,
    caret_rect: Option<ScreenRect>,
}

#[derive(Debug, Default)]
struct LatestRequestQueue {
    state: Mutex<LatestRequestState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct LatestRequestState {
    pending: Option<ScreenOcrRequest>,
    closed: bool,
}

impl LatestRequestQueue {
    fn submit(&self, request: ScreenOcrRequest) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = Some(request);
        self.ready.notify_one();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = None;
        state.closed = true;
        self.ready.notify_one();
    }

    fn recv(&self) -> Option<ScreenOcrRequest> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(request) = state.pending.take() {
                return Some(request);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
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
        screen: Arc<Mutex<Option<ScreenContext>>>,
        max_chars: usize,
        diag: bool,
    ) -> std::io::Result<Self> {
        // Latest-slot queue: request submission only takes a short mutex and
        // overwrites stale pending work, so a slow OCR pass cannot build an
        // unbounded backlog of field/caret metadata.
        let queue = Arc::new(LatestRequestQueue::default());
        let worker_queue = Arc::clone(&queue);
        let handle = std::thread::Builder::new()
            .name("compme-screen-ocr".into())
            .spawn(move || run(worker_queue, screen, max_chars, diag))?;
        Ok(Self {
            queue: Some(queue),
            handle: Some(handle),
        })
    }

    /// Fire-and-forget an OCR request for the display under `caret_rect`. Never
    /// waits for OCR; if the worker is behind, stale pending work is replaced by
    /// the newest request.
    pub fn request(
        &self,
        field: FieldHandle,
        generation: u64,
        snapshot: u64,
        caret_rect: Option<ScreenRect>,
    ) {
        if let Some(queue) = &self.queue {
            queue.submit(ScreenOcrRequest {
                field,
                generation,
                snapshot,
                caret_rect,
            });
        }
    }
}

impl Drop for ScreenOcr {
    fn drop(&mut self) {
        // Close the latest-slot queue so the worker exits after its current
        // pass. We **detach** rather than join: a Vision OCR call can be
        // mid-flight (and, on a sleeping/reconfiguring display, could block for
        // a long time), and joining here would hang process teardown on the main
        // thread. The worker holds only the `screen` Arc, which it releases when
        // it returns, so detaching is safe.
        if let Some(queue) = self.queue.take() {
            queue.close();
        }
        drop(self.handle.take());
    }
}

/// Worker body: block for the next rect, OCR the display under it, redact, and
/// publish into the shared cell. Exits when the channel closes.
fn run(
    queue: Arc<LatestRequestQueue>,
    screen: Arc<Mutex<Option<ScreenContext>>>,
    max_chars: usize,
    diag: bool,
) {
    if max_chars == 0 {
        return;
    }
    while let Some(request) = queue.recv() {
        let raw = screen_context_text(request.caret_rect, max_chars);
        publish_screen_context(&screen, &request, raw, diag);
    }
}

/// Redact `raw` OCR text and publish it into the shared `screen` cell under the
/// request's stamp. This `redaction::redact` call is the SOLE redaction point for
/// captured on-screen content (which routinely shows passwords/tokens/PII), so it
/// is split out as a pure seam and pinned by test — a regression dropping the
/// redact here would leak raw screen secrets into the model prompt.
fn publish_screen_context(
    screen: &Mutex<Option<ScreenContext>>,
    request: &ScreenOcrRequest,
    raw: Option<String>,
    diag: bool,
) {
    let text = raw.map(|t| redaction::redact(&t));
    if diag {
        eprintln!(
            "compme: screen_context={:?}",
            text.as_ref().map(|s| s.chars().count())
        );
    }
    *screen.lock().unwrap_or_else(|e| e.into_inner()) = text.map(|text| ScreenContext {
        field: request.field.clone(),
        generation: request.generation,
        snapshot: request.snapshot,
        text,
    });
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

    fn field(generation: u64) -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(1),
            element_id: "field".into(),
            generation,
        }
    }

    #[test]
    fn publish_screen_context_redacts_before_publishing() {
        // The sole redaction point for captured on-screen content. A regression
        // dropping the redact here would leak raw screen secrets into the prompt.
        let screen: Mutex<Option<ScreenContext>> = Mutex::new(None);
        let req = ScreenOcrRequest {
            field: field(7),
            generation: 7,
            snapshot: 7,
            caret_rect: rect(1.0),
        };
        publish_screen_context(
            &screen,
            &req,
            Some("login sk-abcdEFGH0123456789abcdEFGH0123 now".into()),
            false,
        );
        let guard = screen.lock().unwrap();
        let ctx = guard.as_ref().expect("published a screen context");
        assert!(
            ctx.text.contains("[redacted-secret]"),
            "redacted: {:?}",
            ctx.text
        );
        assert!(
            !ctx.text.contains("sk-abcdEFGH"),
            "raw screen secret must not reach the prompt cell: {:?}",
            ctx.text
        );
        assert_eq!(ctx.generation, 7);
        assert_eq!(ctx.snapshot, 7);
    }

    #[test]
    fn publish_screen_context_with_no_ocr_text_clears_the_cell() {
        // A None OCR result publishes None, clearing any stale prior context
        // rather than leaving a previous request's visible text in the cell.
        let screen: Mutex<Option<ScreenContext>> = Mutex::new(Some(ScreenContext {
            field: field(1),
            generation: 1,
            snapshot: 1,
            text: "stale visible text".into(),
        }));
        let req = ScreenOcrRequest {
            field: field(2),
            generation: 2,
            snapshot: 2,
            caret_rect: rect(2.0),
        };
        publish_screen_context(&screen, &req, None, false);
        assert!(
            screen.lock().unwrap().is_none(),
            "no OCR text clears the cell"
        );
    }

    #[test]
    fn worker_exits_immediately_when_screen_context_disabled() {
        // max_chars == 0 (screen context disabled): run() returns before the loop,
        // touching neither the Vision FFI nor the screen cell.
        let queue = Arc::new(LatestRequestQueue::default());
        queue.submit(ScreenOcrRequest {
            field: field(1),
            generation: 1,
            snapshot: 1,
            caret_rect: rect(1.0),
        });
        let screen = Arc::new(Mutex::new(None));
        run(Arc::clone(&queue), Arc::clone(&screen), 0, false);
        assert!(
            screen.lock().unwrap().is_none(),
            "disabled worker publishes nothing"
        );
    }

    #[test]
    fn latest_queue_coalesces_a_burst_to_the_newest_rect() {
        let queue = LatestRequestQueue::default();
        queue.submit(ScreenOcrRequest {
            field: field(1),
            generation: 1,
            snapshot: 1,
            caret_rect: rect(1.0),
        });
        queue.submit(ScreenOcrRequest {
            field: field(2),
            generation: 2,
            snapshot: 2,
            caret_rect: rect(2.0),
        });
        queue.submit(ScreenOcrRequest {
            field: field(3),
            generation: 3,
            snapshot: 3,
            caret_rect: rect(3.0),
        });
        let latest = queue.recv().unwrap();
        assert_eq!(latest.field.generation, 3);
        assert_eq!(latest.generation, 3);
        assert_eq!(latest.snapshot, 3);
        assert_eq!(latest.caret_rect.unwrap().x, 3.0);
        queue.close();
        assert!(queue.recv().is_none());
    }

    #[test]
    fn latest_queue_returns_none_when_closed() {
        let queue = LatestRequestQueue::default();
        queue.close();
        assert!(queue.recv().is_none());
    }

    #[test]
    fn request_never_waits_for_worker_drain_and_keeps_only_latest() {
        // No worker is draining this queue: requests should only replace the
        // pending slot, and the eventual worker read should get the latest
        // field/rect rather than a backlog.
        let queue = Arc::new(LatestRequestQueue::default());
        let ocr = ScreenOcr {
            queue: Some(Arc::clone(&queue)),
            handle: None,
        };
        for x in 0..100 {
            ocr.request(field(x), x, x, rect(x as f64));
        }
        let latest = queue.recv().unwrap();
        assert_eq!(latest.field.generation, 99);
        assert_eq!(latest.generation, 99);
        assert_eq!(latest.snapshot, 99);
        assert_eq!(latest.caret_rect.unwrap().x, 99.0);
        queue.close();
        assert!(queue.recv().is_none());
    }
}
