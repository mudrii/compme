//! The inference thread.
//!
//! `LlamaModel::complete()` blocks ~50ms, so it must run off the AppKit main
//! thread (overlay/event drain) and off the dispatcher thread (platform events).
//! This module owns a dedicated worker thread: it warms the model once at launch
//! (priming Metal shaders), flips a `ready` flag, then loops
//! `recv → complete → send outcome`. On shutdown it drops the request sender,
//! joins the thread, and the thread frees the model in deterministic order.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use engine::CompletionRequest;
use model_client::LocalModel;
use personalization::PersonalizationProfile;

use crate::model_select::{shape_prompt, PromptMode};

/// Per-app bounded rings of recent accepted completions (redacted), shared
/// between the run loop (which records on accept) and the inference worker (which
/// reads them as previous-input context). A2 §16.
///
/// Scoping is **per app**: text accepted in one app only surfaces as context in
/// that same app — cross-app previous inputs are a separate opt-in Cotypist
/// ships behind `featureCrossAppPreviousInputs` (not cloned here), so the default
/// must not leak prose across application boundaries.
#[derive(Clone, Default)]
pub struct PreviousInputs {
    inner: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
}

impl PreviousInputs {
    const CAPACITY: usize = 5;

    /// Record an already-redacted accepted completion for `app`, evicting the
    /// oldest. Consecutive duplicates are ignored so word-by-word repeats don't
    /// flood the ring.
    pub fn record(&self, app: &str, text: String) {
        if text.trim().is_empty() {
            return;
        }
        // Recover the guard on poisoning rather than silently dropping forever.
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let buf = map.entry(app.to_string()).or_default();
        if buf.back() == Some(&text) {
            return;
        }
        if buf.len() == Self::CAPACITY {
            buf.pop_front();
        }
        buf.push_back(text);
    }

    /// The recent inputs for `app`, newest first.
    fn recent(&self, app: &str) -> Vec<String> {
        let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.get(app)
            .map(|buf| buf.iter().rev().cloned().collect())
            .unwrap_or_default()
    }
}

/// The context-augmentation sources the inference worker reads per request
/// (A2 §16): per-app previous inputs, optional clipboard text (redacted, set by
/// the run loop when clipboard context is enabled), and the per-source char
/// bound (`max_chars == 0` disables augmentation entirely).
#[derive(Clone, Default)]
pub struct WorkerContext {
    pub previous_inputs: PreviousInputs,
    pub clipboard: Arc<Mutex<Option<String>>>,
    pub max_chars: usize,
}

impl WorkerContext {
    fn block_for(&self, app: &str) -> String {
        if self.max_chars == 0 {
            return String::new();
        }
        let recent = self.previous_inputs.recent(app);
        let recent_refs: Vec<&str> = recent.iter().map(String::as_str).collect();
        let clip = self
            .clipboard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        context::build_context_block(clip.as_deref(), &recent_refs, self.max_chars)
    }
}

/// A completed inference, paired with the request that produced it so the engine
/// can match it against the current generation (and discard if stale).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionOutcome {
    pub request: CompletionRequest,
    /// One or more candidate continuations (multi-candidate, A2 §16). At least
    /// one; the engine shows the first and cycles through the rest.
    pub candidates: Vec<String>,
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
// Internal plumbing fn: the parameters are the worker's whole context (model,
// prompt config, the two channels, the ready flag); bundling them into a struct
// would not improve clarity here.
#[allow(clippy::too_many_arguments)]
fn run(
    model: Box<dyn LocalModel>,
    prompt_mode: PromptMode,
    profile: PersonalizationProfile,
    candidates: usize,
    worker_context: WorkerContext,
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
        // Personalization steering for the focused app (domain support is a later
        // browser feature; None for now), then shape the engine's raw left-context
        // prefix per the configured strategy (terse continuation by default).
        let preamble = profile.build_preamble(Some(&request.field.app), None);
        // Opt-in context augmentation (clipboard + previous inputs): prepend a
        // bounded, already-redacted block ahead of the steering preamble.
        let block = worker_context.block_for(&request.field.app);
        let full_preamble = if block.is_empty() {
            preamble
        } else {
            format!("{block}{preamble}")
        };
        let prompt = shape_prompt(prompt_mode, &full_preamble, &request.prompt);
        match model.complete_n(&prompt, request.max_tokens, candidates) {
            Ok(candidates) => {
                // A dropped receiver means the main loop is shutting down.
                if outcomes
                    .send(CompletionOutcome {
                        request,
                        candidates,
                    })
                    .is_err()
                {
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
    pub fn spawn(
        model: Box<dyn LocalModel>,
        prompt_mode: PromptMode,
        profile: PersonalizationProfile,
        candidates: usize,
        worker_context: WorkerContext,
    ) -> Result<Self, String> {
        let (request_tx, request_rx) = channel::<CompletionRequest>();
        let (outcome_tx, outcome_rx) = channel::<CompletionOutcome>();
        let ready = Arc::new(AtomicBool::new(false));
        let ready_for_thread = Arc::clone(&ready);

        let handle = thread::Builder::new()
            .name("complete-me-inference".into())
            .spawn(move || {
                run(
                    model,
                    prompt_mode,
                    profile,
                    candidates.max(1),
                    worker_context,
                    request_rx,
                    outcome_tx,
                    ready_for_thread,
                )
            })
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
        let inference = InferenceHandle::spawn(
            Box::new(StubModel::new(" world")),
            PromptMode::Terse,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        assert!(inference.submit(request("hello", 1)));

        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.candidates[0], " world");
        assert_eq!(outcome.request.generation, 1);

        inference.shutdown();
    }

    #[test]
    fn terse_mode_wraps_the_prompt_before_the_model_sees_it() {
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Terse,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("Dear team", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        // Terse mode wraps the prefix before the model sees it: the echoed text
        // contains the prefix and differs from it. The exact template prose is
        // pinned in `model_client`, not coupled here.
        assert!(outcome.candidates[0].contains("Dear team"));
        assert_ne!(outcome.candidates[0], "Dear team");
        inference.shutdown();
    }

    #[test]
    fn previous_input_context_is_prepended_when_enabled() {
        // Recorded inputs surface as a context block ahead of the prompt the
        // model sees (A2 §16 context augmentation).
        let previous = PreviousInputs::default();
        // `request(..)` focuses app "TextEdit", so record under the same app.
        previous.record("TextEdit", "earlier sentence".into());
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                previous_inputs: previous,
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(request("now typing", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("Recent: earlier sentence"),
            "context block present: {:?}",
            outcome.candidates[0]
        );
        assert!(outcome.candidates[0].contains("now typing"));
        inference.shutdown();
    }

    #[test]
    fn clipboard_context_is_prepended_when_set() {
        // The clipboard cell (set by the run loop, A2 §16) surfaces as a
        // "Clipboard:" line in the prompt the model sees.
        let clipboard = Arc::new(Mutex::new(Some("copied snippet".to_string())));
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                clipboard,
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(request("typing", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("Clipboard: copied snippet"),
            "clipboard context present: {:?}",
            outcome.candidates[0]
        );
        inference.shutdown();
    }

    #[test]
    fn context_disabled_prepends_nothing() {
        let previous = PreviousInputs::default();
        previous.record("TextEdit", "earlier".into());
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                previous_inputs: previous,
                max_chars: 0,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(request("now", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.candidates[0], "now");
        inference.shutdown();
    }

    #[test]
    fn previous_inputs_ring_is_bounded_newest_first() {
        let previous = PreviousInputs::default();
        for i in 0..8 {
            previous.record("app", format!("input{i}"));
        }
        let recent = previous.recent("app");
        assert_eq!(recent.len(), PreviousInputs::CAPACITY);
        assert_eq!(recent[0], "input7"); // newest first
        assert!(!recent.contains(&"input0".to_string())); // oldest evicted
    }

    #[test]
    fn previous_inputs_are_scoped_per_app() {
        // Text accepted in one app must not surface as context in another
        // (review #3: no cross-app leak by default).
        let previous = PreviousInputs::default();
        previous.record("app.a", "secret from A".into());
        assert_eq!(previous.recent("app.a"), vec!["secret from A"]);
        assert!(previous.recent("app.b").is_empty());
    }

    #[test]
    fn consecutive_duplicate_inputs_are_ignored() {
        let previous = PreviousInputs::default();
        previous.record("app", "same".into());
        previous.record("app", "same".into());
        previous.record("app", "other".into());
        assert_eq!(previous.recent("app"), vec!["other", "same"]);
    }

    #[test]
    fn requested_candidate_count_flows_to_the_model() {
        // A model that yields N candidates surfaces all N in the outcome
        // (multi-candidate, A2 §16).
        struct MultiModel;
        impl LocalModel for MultiModel {
            fn complete(&self, _p: &str, _n: usize) -> LocalModelResult<String> {
                Ok("one".into())
            }
            fn complete_n(&self, _p: &str, _max: usize, n: usize) -> LocalModelResult<Vec<String>> {
                Ok((0..n).map(|i| format!("cand{i}")).collect())
            }
        }
        let inference = InferenceHandle::spawn(
            Box::new(MultiModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            3,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("x", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.candidates, vec!["cand0", "cand1", "cand2"]);
        inference.shutdown();
    }

    #[test]
    fn raw_mode_passes_the_prompt_through_unchanged() {
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("Dear team", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.candidates[0], "Dear team");
        inference.shutdown();
    }

    #[test]
    fn personalization_preamble_is_prepended_before_the_model_sees_the_prompt() {
        // A profile with instructions steers the prompt: the model sees the
        // preamble ahead of the raw prefix (A2 §6 "suggestions steered by
        // custom instructions").
        let profile = PersonalizationProfile {
            global_instructions: "Write in pirate dialect.".into(),
            ..Default::default()
        };
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            profile,
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("Ahoy", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("Write in pirate dialect."),
            "steering preamble present: {:?}",
            outcome.candidates[0]
        );
        assert!(outcome.candidates[0].trim_end().ends_with("Ahoy"));
        inference.shutdown();
    }

    #[test]
    fn ready_flips_after_warm_up() {
        let inference = InferenceHandle::spawn(
            Box::new(StubModel::new("x")),
            PromptMode::Terse,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        // Submit + receive guarantees the worker has passed warm-up.
        inference.submit(request("p", 1));
        let _ = inference.recv_outcome();
        assert!(inference.is_ready());
        inference.shutdown();
    }

    #[test]
    fn warm_up_failure_is_non_fatal() {
        // A failing warm-up must not block readiness or completions.
        let inference = InferenceHandle::spawn(
            Box::new(WarmUpFailModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("p", 1));
        let outcome = inference
            .recv_outcome()
            .expect("outcome despite warm-up failure");
        assert_eq!(outcome.candidates[0], "served");
        assert!(inference.is_ready());
        inference.shutdown();
    }

    #[test]
    fn complete_error_is_non_fatal_worker_keeps_serving() {
        // The worker hits a complete() error on the "bad" request, then must
        // still serve the later "good" request.
        let inference = InferenceHandle::spawn(
            Box::new(ConditionalModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("bad", 1));
        inference.submit(request("good", 2));
        let outcome = inference
            .recv_outcome()
            .expect("worker survives an error and serves later requests");
        assert_eq!(outcome.candidates[0], "good");
        inference.shutdown();
    }

    #[test]
    fn shutdown_without_work_joins_cleanly() {
        let inference = InferenceHandle::spawn(
            Box::new(StubModel::new("x")),
            PromptMode::Terse,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
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
