//! The inference thread.
//!
//! `LlamaModel::complete()` blocks ~50ms, so it must run off the AppKit main
//! thread (overlay/event drain) and off the dispatcher thread (platform events).
//! This module owns a dedicated worker thread: it warms the model once at launch
//! (priming Metal shaders), flips a `ready` flag, then loops
//! `recv → complete → send outcome`. On shutdown it drops the request sender,
//! joins the thread, and the thread frees the model in deterministic order.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use engine::CompletionRequest;
use model_client::LocalModel;

use crate::model_select::{shape_prompt, PromptMode};

/// A completed inference, paired with the request that produced it so the engine
/// can match it against the current generation (and discard if stale).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionOutcome {
    pub request: CompletionRequest,
    pub text: String,
}

/// Block for the next request, then drain any that piled up behind it and keep
/// only the newest. Returns `None` when the sender is gone (shutdown).
fn recv_latest(requests: &Receiver<CompletionRequest>) -> Option<CompletionRequest> {
    let mut request = requests.recv().ok()?;
    while let Ok(newer) = requests.try_recv() {
        request = newer;
    }
    Some(request)
}

/// The worker body. Warms the model, signals readiness, then serves requests
/// until the channel closes; finally releases the model.
fn run(
    model: Box<dyn LocalModel>,
    prompt_mode: PromptMode,
    requests: Receiver<CompletionRequest>,
    outcomes: Sender<CompletionOutcome>,
    ready: Arc<AtomicBool>,
) {
    eprintln!("complete-me: state=loading");
    if let Err(err) = model.warm_up() {
        eprintln!("complete-me: warm-up failed: {err}");
    }
    ready.store(true, Ordering::SeqCst);
    eprintln!("complete-me: state=ready");

    while let Some(request) = recv_latest(&requests) {
        // Shape the engine's raw left-context prefix per the configured strategy
        // (terse continuation prompt by default — the A1a development default).
        let prompt = shape_prompt(prompt_mode, &request.prompt);
        match model.complete(&prompt, request.max_tokens) {
            Ok(text) => {
                // A dropped receiver means the main loop is shutting down.
                if outcomes.send(CompletionOutcome { request, text }).is_err() {
                    break;
                }
            }
            Err(err) => eprintln!("complete-me: inference error: {err}"),
        }
    }

    model.shutdown();
}

/// Owns the inference worker thread and the channels to it.
pub struct InferenceHandle {
    request_tx: Option<Sender<CompletionRequest>>,
    outcome_rx: Receiver<CompletionOutcome>,
    ready: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl InferenceHandle {
    /// Spawn the worker, moving the model onto it. Warm-up begins immediately.
    ///
    /// Returns `Err` if the OS refuses the thread (resource limits); the caller
    /// propagates it rather than panicking, matching the crate's no-panic-in-
    /// runtime-paths convention.
    pub fn spawn(model: Box<dyn LocalModel>, prompt_mode: PromptMode) -> Result<Self, String> {
        let (request_tx, request_rx) = channel::<CompletionRequest>();
        let (outcome_tx, outcome_rx) = channel::<CompletionOutcome>();
        let ready = Arc::new(AtomicBool::new(false));
        let ready_for_thread = Arc::clone(&ready);

        let handle = thread::Builder::new()
            .name("complete-me-inference".into())
            .spawn(move || run(model, prompt_mode, request_rx, outcome_tx, ready_for_thread))
            .map_err(|err| format!("spawn inference thread: {err}"))?;

        Ok(Self {
            request_tx: Some(request_tx),
            outcome_rx,
            ready,
            handle: Some(handle),
        })
    }

    /// True once warm-up has finished. The run loop withholds suggestions until
    /// then (the P0 "loading" state, surfaced via logs — no tray yet).
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Submit a request for inference. Returns false if the worker is gone.
    pub fn submit(&self, request: CompletionRequest) -> bool {
        match &self.request_tx {
            Some(tx) => tx.send(request).is_ok(),
            None => false,
        }
    }

    /// Drain all completed outcomes without blocking.
    pub fn drain_outcomes(&self) -> Vec<CompletionOutcome> {
        self.outcome_rx.try_iter().collect()
    }

    /// Drop the request sender and join the worker, freeing the model in order.
    pub fn shutdown(mut self) {
        self.request_tx = None; // closes the channel → worker exits its loop
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    #[cfg(test)]
    fn recv_outcome(&self) -> Option<CompletionOutcome> {
        self.outcome_rx.recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_select::StubModel;
    use model_client::{LocalModelError, LocalModelResult};
    use platform::FieldHandle;

    /// Echoes the exact prompt string it receives, so a test can assert what the
    /// worker actually fed the model after prompt shaping.
    struct EchoModel;
    impl LocalModel for EchoModel {
        fn complete(&self, prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
            Ok(prompt.to_string())
        }
    }

    /// Fails warm-up but completes normally — proves warm-up failure is non-fatal.
    struct WarmUpFailModel;
    impl LocalModel for WarmUpFailModel {
        fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
            Ok("served".into())
        }
        fn warm_up(&self) -> Result<(), LocalModelError> {
            Err(LocalModelError::new("warm-up", "boom"))
        }
    }

    /// Errors only on prompts containing "bad" — lets a test exercise the
    /// complete()-error branch and confirm the worker keeps serving afterwards.
    struct ConditionalModel;
    impl LocalModel for ConditionalModel {
        fn complete(&self, prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
            if prompt.contains("bad") {
                Err(LocalModelError::new("infer", "nope"))
            } else {
                Ok(prompt.to_string())
            }
        }
    }

    fn request(prompt: &str, generation: u64) -> CompletionRequest {
        CompletionRequest {
            generation,
            field: FieldHandle {
                app: "TextEdit".into(),
                pid: Some(1),
                element_id: "f".into(),
                generation,
            },
            snapshot: generation,
            prompt: prompt.into(),
            max_tokens: 8,
        }
    }

    #[test]
    fn completes_a_request_with_the_model() {
        let inference =
            InferenceHandle::spawn(Box::new(StubModel::new(" world")), PromptMode::Terse).unwrap();
        assert!(inference.submit(request("hello", 1)));

        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.text, " world");
        assert_eq!(outcome.request.generation, 1);

        inference.shutdown();
    }

    #[test]
    fn terse_mode_wraps_the_prompt_before_the_model_sees_it() {
        let inference = InferenceHandle::spawn(Box::new(EchoModel), PromptMode::Terse).unwrap();
        inference.submit(request("Dear team", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(outcome.text.contains("Dear team"));
        assert!(outcome.text.starts_with("Complete this text inline"));
        inference.shutdown();
    }

    #[test]
    fn raw_mode_passes_the_prompt_through_unchanged() {
        let inference = InferenceHandle::spawn(Box::new(EchoModel), PromptMode::Raw).unwrap();
        inference.submit(request("Dear team", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.text, "Dear team");
        inference.shutdown();
    }

    #[test]
    fn ready_flips_after_warm_up() {
        let inference =
            InferenceHandle::spawn(Box::new(StubModel::new("x")), PromptMode::Terse).unwrap();
        // Submit + receive guarantees the worker has passed warm-up.
        inference.submit(request("p", 1));
        let _ = inference.recv_outcome();
        assert!(inference.is_ready());
        inference.shutdown();
    }

    #[test]
    fn warm_up_failure_is_non_fatal() {
        // A failing warm-up must not block readiness or completions.
        let inference = InferenceHandle::spawn(Box::new(WarmUpFailModel), PromptMode::Raw).unwrap();
        inference.submit(request("p", 1));
        let outcome = inference
            .recv_outcome()
            .expect("outcome despite warm-up failure");
        assert_eq!(outcome.text, "served");
        assert!(inference.is_ready());
        inference.shutdown();
    }

    #[test]
    fn complete_error_is_non_fatal_worker_keeps_serving() {
        // The worker hits a complete() error on the "bad" request, then must
        // still serve the later "good" request.
        let inference =
            InferenceHandle::spawn(Box::new(ConditionalModel), PromptMode::Raw).unwrap();
        inference.submit(request("bad", 1));
        inference.submit(request("good", 2));
        let outcome = inference
            .recv_outcome()
            .expect("worker survives an error and serves later requests");
        assert_eq!(outcome.text, "good");
        inference.shutdown();
    }

    #[test]
    fn shutdown_without_work_joins_cleanly() {
        let inference =
            InferenceHandle::spawn(Box::new(StubModel::new("x")), PromptMode::Terse).unwrap();
        inference.shutdown(); // must not hang
    }

    #[test]
    fn recv_latest_coalesces_a_burst() {
        let (tx, rx) = channel::<CompletionRequest>();
        tx.send(request("a", 1)).unwrap();
        tx.send(request("b", 2)).unwrap();
        tx.send(request("c", 3)).unwrap();
        let latest = recv_latest(&rx).unwrap();
        assert_eq!(latest.generation, 3);
        assert_eq!(latest.prompt, "c");
    }

    #[test]
    fn recv_latest_returns_none_when_sender_dropped() {
        let (tx, rx) = channel::<CompletionRequest>();
        drop(tx);
        assert!(recv_latest(&rx).is_none());
    }
}
