//! The inference thread.
//!
//! `LlamaModel::complete()` blocks ~50ms, so it must run off the AppKit main
//! thread (overlay/event drain) and off the dispatcher thread (platform events).
//! This module owns a dedicated worker thread: it warms the model once at launch
//! (priming Metal shaders), flips a `ready` flag, then loops
//! `recv → complete → send outcome`. On shutdown it drops the request sender,
//! joins the thread, and the thread frees the model in deterministic order.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use engine::{CompletionRequest, RequestKind};
use model_client::LocalModel;
use personalization::PersonalizationProfile;
use platform::{CorrectionRange, FieldHandle};

use crate::model_select::{shape_prompt, PromptMode};

/// Output budget for grammar-fix requests: the vetted result is a single word,
/// so 8 tokens is ample. Set on the request in the run loop; the worker honors
/// `request.max_tokens` for every request kind.
pub(crate) const GRAMMAR_MAX_TOKENS: usize = model_client::GRAMMAR_GENERATION_TOKENS;

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

    /// Record an accepted completion for `app`, redacting before storage,
    /// evicting the oldest. Consecutive duplicates are ignored so word-by-word
    /// repeats don't flood the ring.
    pub fn record(&self, app: &str, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let text = redaction::redact(&text);
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
    pub(crate) fn recent(&self, app: &str) -> Vec<String> {
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
    pub screen: Arc<Mutex<Option<ScreenContext>>>,
    pub screen_wait_ms: Arc<AtomicU64>,
    pub max_chars: usize,
    pub diag_context: bool,
}

/// Redacted OCR text scoped to the completion request that produced it. The
/// screen worker is asynchronous, so the inference worker must not attach a
/// prior field or prior request's screen text to the next request that happens
/// to arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScreenContext {
    pub field: FieldHandle,
    pub generation: u64,
    pub snapshot: u64,
    pub text: String,
}

impl WorkerContext {
    pub fn screen_wait_cell(duration: Duration) -> Arc<AtomicU64> {
        Arc::new(AtomicU64::new(
            duration.as_millis().min(u64::MAX as u128) as u64
        ))
    }

    fn screen_wait(&self) -> Duration {
        Duration::from_millis(self.screen_wait_ms.load(Ordering::Relaxed))
    }

    fn matching_screen_text_now(&self, request: &CompletionRequest) -> Option<String> {
        self.screen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|ctx| {
                ctx.field == request.field
                    && ctx.generation == request.generation
                    && ctx.snapshot == request.snapshot
            })
            .map(|ctx| ctx.text.clone())
    }

    fn wait_for_screen_or_newer(
        &self,
        mut request: CompletionRequest,
        requests: &Receiver<CompletionRequest>,
    ) -> (CompletionRequest, Option<String>) {
        if matches!(request.kind, RequestKind::GrammarFix { .. }) {
            return (request, None);
        }
        let mut wait = self.screen_wait();
        if wait.is_zero() {
            let screen_text = self.matching_screen_text_now(&request);
            return (request, screen_text);
        }

        let mut deadline = Instant::now() + wait;
        loop {
            while let Ok(newer) = requests.try_recv() {
                request = newer;
                if matches!(request.kind, RequestKind::GrammarFix { .. }) {
                    return (request, None);
                }
                wait = self.screen_wait();
                if wait.is_zero() {
                    let screen_text = self.matching_screen_text_now(&request);
                    return (request, screen_text);
                }
                deadline = Instant::now() + wait;
            }

            if let Some(text) = self.matching_screen_text_now(&request) {
                return (request, Some(text));
            }

            let now = Instant::now();
            if now >= deadline {
                return (request, None);
            }

            match requests.recv_timeout(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(5)),
            ) {
                Ok(newer) => {
                    request = newer;
                    if matches!(request.kind, RequestKind::GrammarFix { .. }) {
                        return (request, None);
                    }
                    wait = self.screen_wait();
                    if wait.is_zero() {
                        let screen_text = self.matching_screen_text_now(&request);
                        return (request, screen_text);
                    }
                    deadline = Instant::now() + wait;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return (request, None),
            }
        }
    }

    fn block_for_with_screen_text(
        &self,
        request: &CompletionRequest,
        screen_text: Option<&str>,
    ) -> String {
        if self.max_chars == 0 {
            return String::new();
        }
        let recent = self.previous_inputs.recent(&request.field.app);
        let recent_refs = recent.iter().map(String::as_str).collect::<Vec<_>>();
        let clip = self
            .clipboard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        context::build_context_block(clip.as_deref(), screen_text, &recent_refs, self.max_chars)
    }

    #[cfg(test)]
    pub(crate) fn block_for(&self, request: &CompletionRequest) -> String {
        let screen_text = self.matching_screen_text_now(request);
        self.block_for_with_screen_text(request, screen_text.as_deref())
    }
}

fn request_with_screen_context(
    requests: &Receiver<CompletionRequest>,
    worker_context: &WorkerContext,
) -> Option<(CompletionRequest, Option<String>)> {
    let request = recv_latest(requests)?;
    Some(worker_context.wait_for_screen_or_newer(request, requests))
}

fn context_diagnostic_line(block: &str) -> Option<String> {
    let mut has_clipboard = false;
    let mut has_screen = false;
    let mut has_recent = false;
    let mut has_unknown = false;
    let mut chars = 0usize;
    let mut clipboard_chars = 0usize;
    let mut screen_chars = 0usize;
    let mut recent_chars = 0usize;
    let mut unknown_chars = 0usize;

    for line in block.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line == "Context (for reference only):" {
            continue;
        }
        if let Some(value) = line.strip_prefix("Clipboard:") {
            has_clipboard = true;
            let count = value.trim_start().chars().count();
            clipboard_chars += count;
            chars += count;
        } else if let Some(value) = line.strip_prefix("On screen:") {
            has_screen = true;
            let count = value.trim_start().chars().count();
            screen_chars += count;
            chars += count;
        } else if let Some(value) = line.strip_prefix("Recent:") {
            has_recent = true;
            let count = value.trim_start().chars().count();
            recent_chars += count;
            chars += count;
        } else {
            has_unknown = true;
            let count = line.chars().count();
            unknown_chars += count;
            chars += count;
        }
    }

    if chars == 0 {
        return None;
    }

    let mut sources = Vec::new();
    if has_clipboard {
        sources.push("clipboard");
    }
    if has_screen {
        sources.push("screen");
    }
    if has_recent {
        sources.push("recent");
    }
    if has_unknown {
        sources.push("unknown");
    }
    let mut line = format!("sources={} chars={chars}", sources.join(","));
    if has_clipboard {
        line.push_str(&format!(" clipboard_chars={clipboard_chars}"));
    }
    if has_screen {
        line.push_str(&format!(" screen_chars={screen_chars}"));
    }
    if has_recent {
        line.push_str(&format!(" recent_chars={recent_chars}"));
    }
    if has_unknown {
        line.push_str(&format!(" unknown_chars={unknown_chars}"));
    }
    Some(line)
}

/// A completed inference, paired with the request that produced it so the engine
/// can match it against the current generation (and discard if stale).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionOutcome {
    pub request: CompletionRequest,
    /// One or more candidate continuations (multi-candidate, A2 §16). At least
    /// one; the engine shows the first and cycles through the rest.
    pub candidates: Vec<String>,
    pub correction: Option<String>,
    pub correction_range: Option<CorrectionRange>,
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
    profile: Arc<Mutex<PersonalizationProfile>>,
    candidates: usize,
    worker_context: WorkerContext,
    requests: Receiver<CompletionRequest>,
    outcomes: Sender<CompletionOutcome>,
    ready: Arc<AtomicBool>,
) {
    eprintln!("compme: state=loading");
    if let Err(err) = model.warm_up() {
        eprintln!("compme: warm-up failed: {err}");
    }
    ready.store(true, Ordering::SeqCst);
    eprintln!("compme: state=ready");

    while let Some((request, screen_text)) = request_with_screen_context(&requests, &worker_context)
    {
        if let RequestKind::GrammarFix {
            word,
            left_ctx,
            correction_range,
        } = request.kind.clone()
        {
            let prompt = model_client::grammar_fix_prompt(&word, &left_ctx);
            match model.complete(&prompt, request.max_tokens) {
                Ok(raw) => {
                    let correction = grammar::vet_correction(&word, &raw);
                    if outcomes
                        .send(CompletionOutcome {
                            request,
                            candidates: Vec::new(),
                            correction,
                            correction_range: Some(correction_range),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(err) => eprintln!("compme: grammar inference error: {err}"),
            }
            continue;
        }

        // Personalization steering for the focused app and, when the request
        // came from a monitored browser field, the resolved website domain.
        // Read the profile per request so live Settings edits (`set_profile`)
        // take effect without respawning the worker. The lock is held only for
        // the (cheap, owned-String) preamble build.
        let preamble = profile
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .build_preamble(Some(&request.field.app), request.domain.as_deref());
        // Opt-in context augmentation (clipboard + previous inputs): prepend a
        // bounded, already-redacted block ahead of the steering preamble.
        let block = worker_context.block_for_with_screen_text(&request, screen_text.as_deref());
        if worker_context.diag_context {
            eprintln!(
                "compme: prompt_context={:?}",
                context_diagnostic_line(&block)
            );
        }
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
                        correction: None,
                        correction_range: None,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(err) => eprintln!("compme: inference error: {err}"),
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
    /// Shared with the worker thread; the worker reads it per request and
    /// `set_profile` writes it, so personalization edits from the Settings
    /// Personalization pane apply live (no respawn).
    profile: Arc<Mutex<PersonalizationProfile>>,
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
        // Wrap the by-value profile in shared state so live Settings edits reach
        // the running worker. spawn's signature is unchanged: callers still pass
        // a profile by value.
        let profile = Arc::new(Mutex::new(profile));
        let profile_for_thread = Arc::clone(&profile);

        let handle = thread::Builder::new()
            .name("compme-inference".into())
            .spawn(move || {
                run(
                    model,
                    prompt_mode,
                    profile_for_thread,
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
            profile,
        })
    }

    /// Construct a permanently-not-ready handle for startup states where no
    /// model can be loaded yet. This keeps tray/settings reachable while
    /// inference submissions fail closed until the user downloads/selects a
    /// model and restarts.
    pub fn unavailable() -> Self {
        let (_outcome_tx, outcome_rx) = channel::<CompletionOutcome>();
        Self {
            request_tx: None,
            outcome_rx,
            ready: Arc::new(AtomicBool::new(false)),
            handle: None,
            profile: Arc::new(Mutex::new(PersonalizationProfile::default())),
        }
    }

    /// Replace the personalization profile the worker steers with, live — as
    /// driven by the Settings personalization pane. Takes effect on the next
    /// request the worker processes; no respawn, no `MemoryStore` churn.
    /// Called by the run loop's Personalization-pane consumer on each knob edit.
    pub fn set_profile(&self, profile: PersonalizationProfile) {
        *self.profile.lock().unwrap_or_else(|e| e.into_inner()) = profile;
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
    struct ConditionalModel {
        failed_prompt_seen: Sender<()>,
    }
    impl LocalModel for ConditionalModel {
        fn complete(&self, prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
            if prompt.contains("bad") {
                let _ = self.failed_prompt_seen.send(());
                Err(LocalModelError::new("infer", "nope"))
            } else {
                Ok(prompt.to_string())
            }
        }
    }

    /// Errors on the grammar prompt (which carries the word) but serves plain
    /// completion prompts, so a test can prove a grammar `complete()` error
    /// emits no outcome yet the worker keeps serving. Signals when it errored.
    struct GrammarErrorThenServeModel {
        grammar_error_seen: Sender<()>,
    }
    impl LocalModel for GrammarErrorThenServeModel {
        fn complete(&self, prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
            if prompt.contains("teh") {
                let _ = self.grammar_error_seen.send(());
                Err(LocalModelError::new("grammar", "nope"))
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
            domain: None,
            snapshot: generation,
            prompt: prompt.into(),
            max_tokens: 8,
            kind: RequestKind::Completion,
        }
    }

    fn grammar_request(
        word: &str,
        left_ctx: &str,
        correction_range: CorrectionRange,
        generation: u64,
    ) -> CompletionRequest {
        CompletionRequest {
            generation,
            field: FieldHandle {
                app: "TextEdit".into(),
                pid: Some(1),
                element_id: "f".into(),
                generation,
            },
            domain: None,
            snapshot: generation,
            prompt: String::new(),
            // Mirrors production: the run loop stamps the grammar budget on the
            // request and the worker honors request.max_tokens (4c2f8d3).
            max_tokens: GRAMMAR_MAX_TOKENS,
            kind: RequestKind::GrammarFix {
                word: word.into(),
                left_ctx: left_ctx.into(),
                correction_range,
            },
        }
    }

    struct GrammarEchoModel {
        output: &'static str,
        seen: Arc<Mutex<Vec<(String, usize)>>>,
    }

    impl LocalModel for GrammarEchoModel {
        fn complete(&self, prompt: &str, max_tokens: usize) -> LocalModelResult<String> {
            self.seen
                .lock()
                .unwrap()
                .push((prompt.to_string(), max_tokens));
            Ok(self.output.into())
        }

        fn complete_n(
            &self,
            _prompt: &str,
            _max_tokens: usize,
            _n: usize,
        ) -> LocalModelResult<Vec<String>> {
            panic!("grammar fix requests must not call complete_n");
        }
    }

    #[test]
    fn grammar_fix_request_bypasses_screen_wait_context_personalization_and_complete_n() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let previous = PreviousInputs::default();
        previous.record("TextEdit", "previous private context".into());
        let screen = Arc::new(Mutex::new(None));
        let inference = InferenceHandle::spawn(
            Box::new(GrammarEchoModel {
                output: "the",
                seen: Arc::clone(&seen),
            }),
            PromptMode::Raw,
            PersonalizationProfile {
                global_instructions: "Never leak this steering text.".into(),
                ..Default::default()
            },
            4,
            WorkerContext {
                previous_inputs: previous,
                clipboard: Arc::new(Mutex::new(Some("clipboard context".into()))),
                screen,
                screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_secs(10)),
                max_chars: 500,
                ..Default::default()
            },
        )
        .unwrap();

        let request = grammar_request(
            "teh",
            "I read teh",
            CorrectionRange { start: 7, end: 10 },
            1,
        );
        let start = Instant::now();
        assert!(inference.submit(request));
        let outcome = inference
            .outcome_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("grammar outcome must not wait for OCR");

        assert!(
            start.elapsed() < Duration::from_secs(1),
            "grammar branch waited for screen context"
        );
        assert_eq!(outcome.correction.as_deref(), Some("the"));
        assert!(outcome.candidates.is_empty());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, GRAMMAR_MAX_TOKENS);
        assert!(seen[0].0.contains("teh"));
        assert!(seen[0].0.contains("I read teh"));
        assert!(!seen[0].0.contains("clipboard context"));
        assert!(!seen[0].0.contains("previous private context"));
        assert!(!seen[0].0.contains("Never leak this steering text."));

        inference.shutdown();
    }

    #[test]
    fn grammar_fix_request_preserves_range_and_vets_model_output() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let inference = InferenceHandle::spawn(
            Box::new(GrammarEchoModel {
                output: "the",
                seen,
            }),
            PromptMode::Terse,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        let range = CorrectionRange { start: 2, end: 5 };

        assert!(inference.submit(grammar_request("Teh", "A Teh", range, 2)));
        let outcome = inference.recv_outcome().expect("outcome");

        assert_eq!(outcome.correction.as_deref(), Some("The"));
        assert_eq!(outcome.correction_range, Some(range));
        assert!(outcome.candidates.is_empty());
        inference.shutdown();
    }

    #[test]
    fn grammar_fix_rejected_output_returns_no_correction() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let inference = InferenceHandle::spawn(
            Box::new(GrammarEchoModel {
                // First token must be far from the original word: vetting now
                // extracts the first token from runaway output ("the cat"
                // would vet to "the").
                output: "kitten cat",
                seen,
            }),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        let range = CorrectionRange { start: 0, end: 3 };

        assert!(inference.submit(grammar_request("teh", "teh", range, 3)));
        let outcome = inference.recv_outcome().expect("outcome");

        assert_eq!(outcome.correction, None);
        assert_eq!(outcome.correction_range, Some(range));
        assert!(outcome.candidates.is_empty());
        inference.shutdown();
    }

    #[test]
    fn grammar_fix_rejected_outputs_emit_no_correction_for_all_vet_classes() {
        for output in ["teh", "", "kitten cat", "alphabet", "thé"] {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let inference = InferenceHandle::spawn(
                Box::new(GrammarEchoModel { output, seen }),
                PromptMode::Raw,
                PersonalizationProfile::default(),
                1,
                WorkerContext::default(),
            )
            .unwrap();
            let range = CorrectionRange { start: 0, end: 3 };

            assert!(inference.submit(grammar_request("teh", "teh", range, 3)));
            let outcome = inference.recv_outcome().expect("outcome");

            assert_eq!(outcome.correction, None, "{output:?} must be rejected");
            assert_eq!(outcome.correction_range, Some(range));
            assert!(outcome.candidates.is_empty());
            inference.shutdown();
        }
    }

    #[test]
    fn screen_wait_switches_to_newer_grammar_request_without_waiting_for_ocr() {
        let old = request("old typing", 1);
        let grammar = grammar_request("teh", "old teh", CorrectionRange { start: 4, end: 7 }, 2);
        let worker_context = WorkerContext {
            screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_secs(10)),
            ..Default::default()
        };
        let (request_tx, request_rx) = channel();
        request_tx.send(grammar).unwrap();

        let start = Instant::now();
        let (selected, screen_text) = worker_context.wait_for_screen_or_newer(old, &request_rx);

        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(selected.generation, 2);
        assert!(matches!(selected.kind, RequestKind::GrammarFix { .. }));
        assert_eq!(screen_text, None);
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
    fn previous_inputs_record_redacts_before_prompt_context() {
        let previous = PreviousInputs::default();
        let token = "sk-abcdEFGH0123456789abcdEFGH0123";
        previous.record("TextEdit", token.into());
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
            outcome.candidates[0].contains("Recent: [redacted-secret]"),
            "redacted previous input should be used as context: {:?}",
            outcome.candidates[0]
        );
        assert!(
            !outcome.candidates[0].contains(token),
            "raw previous input secret leaked into context: {:?}",
            outcome.candidates[0]
        );
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
    fn screen_context_is_prepended_when_set() {
        // The screen-OCR cell (set by the run loop, A2 §16) surfaces as an
        // "On screen:" line in the prompt the model sees.
        let req = request("typing", 1);
        let screen = Arc::new(Mutex::new(Some(ScreenContext {
            field: req.field.clone(),
            generation: req.generation,
            snapshot: req.snapshot,
            text: "visible window text".to_string(),
        })));
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                screen,
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(req);
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("On screen: visible window text"),
            "screen context present: {:?}",
            outcome.candidates[0]
        );
        inference.shutdown();
    }

    #[test]
    fn screen_context_is_scoped_to_the_request_field() {
        let source = request("source field", 1);
        let target = CompletionRequest {
            generation: 2,
            field: FieldHandle {
                app: "TextEdit".into(),
                pid: Some(1),
                element_id: "other-field".into(),
                generation: 2,
            },
            domain: None,
            snapshot: 2,
            prompt: "target field".into(),
            max_tokens: 8,
            kind: RequestKind::Completion,
        };
        let screen = Arc::new(Mutex::new(Some(ScreenContext {
            field: source.field,
            generation: source.generation,
            snapshot: source.snapshot,
            text: "visible source text".to_string(),
        })));
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                screen,
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(target);
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            !outcome.candidates[0].contains("visible source text"),
            "screen context leaked across fields: {:?}",
            outcome.candidates[0]
        );
        assert!(outcome.candidates[0].contains("target field"));
        inference.shutdown();
    }

    #[test]
    fn screen_context_is_not_reused_for_newer_same_field_request() {
        let source = request("source typing", 1);
        let target = CompletionRequest {
            generation: 2,
            snapshot: 2,
            field: source.field.clone(),
            domain: None,
            prompt: "target typing".into(),
            max_tokens: 8,
            kind: RequestKind::Completion,
        };
        let screen = Arc::new(Mutex::new(Some(ScreenContext {
            field: source.field,
            generation: source.generation,
            snapshot: source.snapshot,
            text: "stale visible source text".to_string(),
        })));
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                screen,
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(target);
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            !outcome.candidates[0].contains("stale visible source text"),
            "stale same-field screen context leaked into newer request: {:?}",
            outcome.candidates[0]
        );
        assert!(outcome.candidates[0].contains("target typing"));
        inference.shutdown();
    }

    #[test]
    fn screen_context_waits_for_matching_same_request_ocr() {
        let req = request("typing", 1);
        let screen = Arc::new(Mutex::new(None));
        let delayed_screen = Arc::clone(&screen);
        let screen_field = req.field.clone();
        let (release_tx, release_rx) = channel();
        let writer = thread::spawn(move || {
            release_rx.recv().unwrap();
            *delayed_screen.lock().unwrap() = Some(ScreenContext {
                field: screen_field,
                generation: 1,
                snapshot: 1,
                text: "delayed visible text".to_string(),
            });
        });
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                screen,
                screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_millis(80)),
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(req);
        release_tx.send(()).unwrap();
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("On screen: delayed visible text"),
            "matching delayed screen context should reach prompt: {:?}",
            outcome.candidates[0]
        );
        writer.join().unwrap();
        inference.shutdown();
    }

    #[test]
    fn screen_context_wait_can_be_enabled_after_worker_spawn() {
        let req = request("typing", 1);
        let screen = Arc::new(Mutex::new(None));
        let screen_wait_ms = WorkerContext::screen_wait_cell(Duration::ZERO);
        let delayed_screen = Arc::clone(&screen);
        let screen_field = req.field.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            *delayed_screen.lock().unwrap() = Some(ScreenContext {
                field: screen_field,
                generation: 1,
                snapshot: 1,
                text: "live enabled visible text".to_string(),
            });
        });
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                screen,
                screen_wait_ms: Arc::clone(&screen_wait_ms),
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();

        // Generous bound: the writer publishes at ~20ms, but loaded CI runners
        // (test-threads=1) have blown an 80ms window before — the wait only
        // runs to the bound when the test is already failing.
        screen_wait_ms.store(2000, Ordering::Relaxed);
        inference.submit(req);
        let outcome = inference.recv_outcome().expect("outcome");

        assert!(
            outcome.candidates[0].contains("On screen: live enabled visible text"),
            "live-enabled screen context should reach prompt: {:?}",
            outcome.candidates[0]
        );
        writer.join().unwrap();
        inference.shutdown();
    }

    #[test]
    fn screen_wait_switches_to_newer_request_before_building_context() {
        let old = request("old typing", 1);
        let new = request("new typing", 2);
        let screen = Arc::new(Mutex::new(None));
        let delayed_screen = Arc::clone(&screen);
        let screen_field = new.field.clone();
        let (request_tx, request_rx) = channel();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            *delayed_screen.lock().unwrap() = Some(ScreenContext {
                field: screen_field,
                generation: 2,
                snapshot: 2,
                text: "newer visible text".to_string(),
            });
            request_tx.send(new).unwrap();
        });
        let worker_context = WorkerContext {
            screen,
            // Generous bound for the same CI-load reason as the live-enable
            // test above: the writer lands at ~10ms; the bound is only reached
            // when the test is already failing.
            screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_millis(2000)),
            max_chars: 160,
            ..Default::default()
        };

        let (selected, screen_text) = worker_context.wait_for_screen_or_newer(old, &request_rx);

        assert_eq!(selected.generation, 2);
        assert_eq!(screen_text.as_deref(), Some("newer visible text"));
        writer.join().unwrap();
    }

    #[test]
    fn screen_wait_coalesces_a_prequeued_burst_to_newest() {
        // A burst of newer requests is already queued before the wait begins; the
        // try_recv drain loop must keep only the NEWEST (gen 3) before building
        // context, so the matching screen ctx for gen 3 is the one that applies.
        let old = request("old typing", 1);
        let newest = request("newest typing", 3);
        let screen = Arc::new(Mutex::new(Some(ScreenContext {
            field: newest.field.clone(),
            generation: 3,
            snapshot: 3,
            text: "newest visible text".to_string(),
        })));
        let (request_tx, request_rx) = channel();
        request_tx.send(request("burst typing", 1)).unwrap();
        request_tx.send(request("burst typing", 2)).unwrap();
        request_tx.send(newest).unwrap();
        let worker_context = WorkerContext {
            screen,
            screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_millis(80)),
            max_chars: 160,
            ..Default::default()
        };

        let (selected, screen_text) = worker_context.wait_for_screen_or_newer(old, &request_rx);

        assert_eq!(selected.generation, 3);
        assert_eq!(screen_text.as_deref(), Some("newest visible text"));
    }

    #[test]
    fn screen_wait_returns_none_on_channel_disconnect() {
        // Shutdown while waiting for OCR: with a positive screen_wait and no
        // matching screen ctx, dropping the request sender must disconnect the
        // wait and return the original request with no screen text — never hang.
        let req = request("typing", 1);
        let screen = Arc::new(Mutex::new(None));
        let (request_tx, request_rx) = channel();
        let dropper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            drop(request_tx);
        });
        let worker_context = WorkerContext {
            screen,
            screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_secs(10)),
            max_chars: 160,
            ..Default::default()
        };

        let (selected, screen_text) = worker_context.wait_for_screen_or_newer(req, &request_rx);

        assert_eq!(selected.generation, 1);
        assert_eq!(screen_text, None);
        dropper.join().unwrap();
    }

    #[test]
    fn screen_context_wait_is_bounded_when_matching_ocr_is_late() {
        let req = request("typing", 1);
        let screen = Arc::new(Mutex::new(None));
        let delayed_screen = Arc::clone(&screen);
        let screen_field = req.field.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            *delayed_screen.lock().unwrap() = Some(ScreenContext {
                field: screen_field,
                generation: 1,
                snapshot: 1,
                text: "late visible text".to_string(),
            });
        });
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext {
                screen,
                screen_wait_ms: WorkerContext::screen_wait_cell(Duration::from_millis(5)),
                max_chars: 160,
                ..Default::default()
            },
        )
        .unwrap();
        inference.submit(req);
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            !outcome.candidates[0].contains("late visible text"),
            "late screen context should not hold inference past the bounded wait: {:?}",
            outcome.candidates[0]
        );
        assert!(outcome.candidates[0].contains("typing"));
        writer.join().unwrap();
        inference.shutdown();
    }

    #[test]
    fn diagnostic_context_line_reports_sources_without_context_text() {
        let block = "Clipboard: copied snippet\nOn screen: visible window text\nRecent: accepted\n";
        let diag = context_diagnostic_line(block).expect("diagnostic summary");
        assert_eq!(
            diag,
            "sources=clipboard,screen,recent chars=41 clipboard_chars=14 screen_chars=19 recent_chars=8"
        );
        assert!(
            !diag.contains("copied snippet")
                && !diag.contains("visible window text")
                && !diag.contains("accepted"),
            "diagnostic context leaked raw text: {diag:?}"
        );
        assert_eq!(context_diagnostic_line(" \n\t\n"), None);
    }

    #[test]
    fn diagnostic_context_line_accounts_for_an_unknown_source() {
        // The happy-path test only covers the three recognized prefixes. A
        // non-empty line that matches no known prefix (and is not the header)
        // falls into the unknown branch: it must add "unknown" to the sources
        // list and emit an `unknown_chars=` accounting for the whole trimmed
        // line, counted into the total `chars`.
        let block = "Clipboard: hi\nMystery: leftover data\n";
        let diag = context_diagnostic_line(block).expect("diagnostic summary");
        // "hi" = 2 chars; "Mystery: leftover data" = 22 chars → total 24.
        assert_eq!(
            diag,
            "sources=clipboard,unknown chars=24 clipboard_chars=2 unknown_chars=22"
        );
        // The diagnostic must still not leak the raw unknown text body... but the
        // whole line IS the unknown body here, so just pin the accounting above
        // and confirm the unknown source is named.
        assert!(diag.contains("unknown_chars=22"));
    }

    #[test]
    fn diagnostic_context_line_skips_the_reference_header() {
        // The literal context header must be dropped, not counted as an unknown
        // source. Existing diag tests build header-less blocks, so the header-skip
        // `continue` (inference.rs:230) is otherwise unpinned — removing it would
        // add ~29 unknown chars and an `unknown` source to every real diagnostic.
        let block = "Context (for reference only):\nClipboard: hi\n";
        assert_eq!(
            context_diagnostic_line(block),
            Some("sources=clipboard chars=2 clipboard_chars=2".into())
        );
    }

    #[test]
    fn previous_inputs_record_ignores_whitespace_only_text() {
        // Blank/whitespace accepts must not enter the CAP=5 ring (they would evict
        // real context and emit empty "Recent:" lines). Pins the trim-empty guard.
        let previous = PreviousInputs::default();
        previous.record("TextEdit", "   ".into());
        previous.record("TextEdit", "\n\t".into());
        assert!(previous.recent("TextEdit").is_empty());
        previous.record("TextEdit", "real".into());
        assert_eq!(previous.recent("TextEdit"), vec!["real".to_string()]);
    }

    #[test]
    fn matching_screen_text_requires_snapshot_not_just_generation() {
        // Every other screen-mismatch test also mismatches generation, so the
        // `ctx.snapshot == request.snapshot` conjunct (inference.rs:119) is
        // otherwise unpinned — a stale-snapshot OCR would leak without it.
        let req = request("typing", 1); // field gen=1, snapshot=1
        let stale = Arc::new(Mutex::new(Some(ScreenContext {
            field: req.field.clone(),
            generation: req.generation,
            snapshot: req.snapshot + 1, // same field+gen, newer snapshot
            text: "stale ocr".into(),
        })));
        let ctx = WorkerContext {
            screen: stale,
            ..Default::default()
        };
        assert_eq!(ctx.matching_screen_text_now(&req), None);
        // Control: an exact snapshot match returns the text.
        let fresh = Arc::new(Mutex::new(Some(ScreenContext {
            field: req.field.clone(),
            generation: req.generation,
            snapshot: req.snapshot,
            text: "fresh ocr".into(),
        })));
        let ctx = WorkerContext {
            screen: fresh,
            ..Default::default()
        };
        assert_eq!(ctx.matching_screen_text_now(&req), Some("fresh ocr".into()));
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
    fn zero_configured_candidates_is_clamped_before_calling_the_model() {
        struct CountingModel {
            seen: Arc<Mutex<Vec<usize>>>,
        }
        impl LocalModel for CountingModel {
            fn complete(&self, _p: &str, _n: usize) -> LocalModelResult<String> {
                Ok("fallback".into())
            }
            fn complete_n(&self, _p: &str, _max: usize, n: usize) -> LocalModelResult<Vec<String>> {
                self.seen.lock().unwrap().push(n);
                Ok(vec![format!("cand{n}")])
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let inference = InferenceHandle::spawn(
            Box::new(CountingModel {
                seen: Arc::clone(&seen),
            }),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            0,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("x", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert_eq!(outcome.candidates, vec!["cand1"]);
        assert_eq!(*seen.lock().unwrap(), vec![1]);
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
    fn set_profile_live_reloads_what_the_worker_steers_with() {
        // The Settings personalization pane edits the profile while the worker is
        // running. The profile was moved by value at spawn (frozen); set_profile
        // must replace what the worker reads per request, so a later submission is
        // steered by the new instructions, not the spawn-time ones.
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile {
                global_instructions: "Write in pirate dialect.".into(),
                ..Default::default()
            },
            1,
            WorkerContext::default(),
        )
        .unwrap();

        inference.set_profile(PersonalizationProfile {
            global_instructions: "Write tersely.".into(),
            ..Default::default()
        });

        inference.submit(request("Ahoy", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("Write tersely."),
            "live-reloaded preamble steers the prompt: {:?}",
            outcome.candidates[0]
        );
        assert!(!outcome.candidates[0].contains("pirate dialect."));
        inference.shutdown();
    }

    #[test]
    fn poisoned_profile_lock_still_lets_set_profile_and_the_worker_degrade_not_panic() {
        // A panic anywhere holding the profile lock (e.g. a future panicking
        // `build_preamble`) poisons the Mutex. Both writers (`set_profile`) and
        // readers (the worker, per request) recover via `into_inner` rather than
        // propagating the poison and killing the pane edit / the worker thread.
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile {
                global_instructions: "Write in pirate dialect.".into(),
                ..Default::default()
            },
            1,
            WorkerContext::default(),
        )
        .unwrap();

        // Poison the shared profile lock: a thread panics while holding the guard.
        let profile = Arc::clone(&inference.profile);
        std::thread::spawn(move || {
            let _guard = profile.lock().unwrap();
            panic!("poison the profile lock");
        })
        .join()
        .expect_err("the poisoning thread must have panicked");
        assert!(
            inference.profile.lock().is_err(),
            "the profile lock must actually be poisoned for this test to prove anything"
        );

        // set_profile must not panic despite the poison, and must write through.
        inference.set_profile(PersonalizationProfile {
            global_instructions: "Write tersely.".into(),
            ..Default::default()
        });

        // The worker's per-request read must also recover the poison and steer
        // with the newly-written profile — a panicking read would drop the
        // outcome and hang recv, so a steered outcome proves the read survived.
        inference.submit(request("Ahoy", 1));
        let outcome = inference
            .recv_outcome()
            .expect("worker served despite poison");
        assert!(
            outcome.candidates[0].contains("Write tersely."),
            "worker read recovered the poisoned lock and steered with set_profile: {:?}",
            outcome.candidates[0]
        );
        inference.shutdown();
    }

    #[test]
    fn set_profile_steers_without_triggering_a_request() {
        // A Personalization-pane edit must be a pure state write: it re-steers the
        // NEXT submitted request but must never itself enqueue inference. If a
        // future change made set_profile "refresh" by re-submitting, the worker
        // would burn a completion (and possibly flash a ghost) on every knob edit.
        // Pin that no outcome is produced until the caller actually submits.
        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();

        // Several edits in a row, with no submit between them.
        for tone in ["Write tersely.", "Write formally.", "Write casually."] {
            inference.set_profile(PersonalizationProfile {
                global_instructions: tone.into(),
                ..Default::default()
            });
        }

        // Give a (mis)triggered request time to surface, then assert silence.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            inference.drain_outcomes().is_empty(),
            "set_profile must not enqueue inference — no outcome may appear without a submit"
        );

        // The last write is what the next real submission is steered by.
        inference.submit(request("hi", 1));
        let outcome = inference.recv_outcome().expect("outcome");
        assert!(
            outcome.candidates[0].contains("Write casually."),
            "the surviving (last) profile steers the prompt: {:?}",
            outcome.candidates[0]
        );
        inference.shutdown();
    }

    #[test]
    fn per_app_personalization_uses_request_app() {
        let mut profile = PersonalizationProfile {
            global_instructions: "Use short completions.".into(),
            ..Default::default()
        };
        profile
            .per_app
            .insert("com.apple.TextEdit".into(), "Use a plain-text tone.".into());

        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            profile,
            1,
            WorkerContext::default(),
        )
        .unwrap();
        assert!(inference.submit(CompletionRequest {
            field: FieldHandle {
                app: "com.apple.TextEdit".into(),
                ..request("TextEdit draft", 1).field
            },
            ..request("TextEdit draft", 1)
        }));
        let first = inference.recv_outcome().expect("TextEdit outcome");

        assert!(inference.submit(CompletionRequest {
            field: FieldHandle {
                app: "com.apple.Notes".into(),
                ..request("Notes draft", 2).field
            },
            ..request("Notes draft", 2)
        }));
        let second = inference.recv_outcome().expect("Notes outcome");

        assert!(first.candidates[0].contains("Use short completions."));
        assert!(first.candidates[0].contains("Use a plain-text tone."));
        assert!(first.candidates[0].contains("TextEdit draft"));
        assert!(second.candidates[0].contains("Use short completions."));
        assert!(!second.candidates[0].contains("Use a plain-text tone."));
        assert!(second.candidates[0].contains("Notes draft"));
        inference.shutdown();
    }

    #[test]
    fn per_domain_personalization_uses_request_domain() {
        let mut profile = PersonalizationProfile {
            global_instructions: "Use short completions.".into(),
            ..Default::default()
        };
        profile.per_domain.insert(
            "docs.google.com".into(),
            "Prefer spreadsheet language.".into(),
        );

        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            profile,
            1,
            WorkerContext::default(),
        )
        .unwrap();
        assert!(inference.submit(CompletionRequest {
            domain: Some("docs.google.com".into()),
            ..request("Budget draft", 1)
        }));
        let first = inference.recv_outcome().expect("domain outcome");

        assert!(inference.submit(request("Local draft", 2)));
        let second = inference.recv_outcome().expect("local outcome");

        assert!(first.candidates[0].contains("Use short completions."));
        assert!(first.candidates[0].contains("Prefer spreadsheet language."));
        assert!(first.candidates[0].contains("Budget draft"));
        assert!(second.candidates[0].contains("Use short completions."));
        assert!(!second.candidates[0].contains("Prefer spreadsheet language."));
        assert!(second.candidates[0].contains("Local draft"));
        inference.shutdown();
    }

    #[test]
    fn per_domain_missing_domain_falls_back_to_global() {
        // A request whose domain is PRESENT but absent from `per_domain` must
        // steer with the GLOBAL instructions only — never another domain's text.
        // The existing per-domain test drives the matching domain and a None
        // domain; this pins the third case (a known-but-unconfigured domain) so a
        // resolver that leaked the wrong domain's preamble would be caught.
        let mut profile = PersonalizationProfile {
            global_instructions: "Use short completions.".into(),
            ..Default::default()
        };
        profile.per_domain.insert(
            "docs.google.com".into(),
            "Prefer spreadsheet language.".into(),
        );

        let inference = InferenceHandle::spawn(
            Box::new(EchoModel),
            PromptMode::Raw,
            profile,
            1,
            WorkerContext::default(),
        )
        .unwrap();
        assert!(inference.submit(CompletionRequest {
            domain: Some("news.example.com".into()),
            ..request("Other draft", 1)
        }));
        let outcome = inference.recv_outcome().expect("outcome");

        assert!(
            outcome.candidates[0].contains("Use short completions."),
            "an unconfigured domain must still get the global preamble: {:?}",
            outcome.candidates[0]
        );
        assert!(
            !outcome.candidates[0].contains("Prefer spreadsheet language."),
            "an unconfigured domain must not leak another domain's preamble: {:?}",
            outcome.candidates[0]
        );
        assert!(outcome.candidates[0].contains("Other draft"));
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
    fn submit_returns_false_when_worker_request_channel_is_closed() {
        let (request_tx, request_rx) = channel::<CompletionRequest>();
        drop(request_rx);
        let (_outcome_tx, outcome_rx) = channel::<CompletionOutcome>();
        let inference = InferenceHandle {
            request_tx: Some(request_tx),
            outcome_rx,
            ready: Arc::new(AtomicBool::new(false)),
            handle: None,
            profile: Arc::new(Mutex::new(PersonalizationProfile::default())),
        };

        assert!(!inference.submit(request("lost worker", 1)));
    }

    #[test]
    fn unavailable_inference_is_not_ready_and_rejects_submissions() {
        let inference = InferenceHandle::unavailable();

        assert!(!inference.is_ready());
        assert!(!inference.submit(request("missing model", 1)));
        assert!(inference.drain_outcomes().is_empty());
        inference.shutdown();
    }

    #[test]
    fn unavailable_handle_fails_closed_and_drops_cleanly() {
        // The startup fallback handle (no model loadable) must fail closed on
        // every entry point AND drop without hanging: `unavailable()` holds no
        // worker thread (handle: None) and a dead request sender (None), so
        // submissions are rejected, it is never ready, no outcomes ever arrive,
        // and dropping it joins nothing. Distinct from the shutdown() path —
        // this pins that a plain `drop` is also non-blocking.
        let inference = InferenceHandle::unavailable();
        assert!(!inference.submit(request("missing model", 1)));
        assert!(!inference.is_ready());
        assert!(inference.drain_outcomes().is_empty());
        drop(inference); // must not hang: no worker thread to join
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
        let (failed_prompt_seen, failed_prompt_rx) = channel();
        let inference = InferenceHandle::spawn(
            Box::new(ConditionalModel { failed_prompt_seen }),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(request("bad", 1));
        failed_prompt_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the worker must actually exercise the complete() error path");
        inference.submit(request("good", 2));
        let outcome = inference
            .recv_outcome()
            .expect("worker survives an error and serves later requests");
        assert_eq!(outcome.candidates[0], "good");
        inference.shutdown();
    }

    #[test]
    fn grammar_complete_error_emits_no_outcome_and_worker_keeps_serving() {
        // A grammar request whose complete() errors must be a silent no-op: the
        // grammar branch logs and `continue`s without sending a CompletionOutcome
        // (spec: "trigger with no correction → no banner"). This is a distinct
        // branch from the completion error path — it has its own `continue` and
        // never emits `correction: None`. Proven by submitting the errored
        // grammar request first, then a good completion request: the completion
        // outcome must arrive as the FIRST outcome received, which is only true
        // if the grammar error emitted nothing ahead of it.
        let (grammar_error_seen, error_rx) = channel();
        let inference = InferenceHandle::spawn(
            Box::new(GrammarErrorThenServeModel { grammar_error_seen }),
            PromptMode::Raw,
            PersonalizationProfile::default(),
            1,
            WorkerContext::default(),
        )
        .unwrap();
        inference.submit(grammar_request(
            "teh",
            "I read teh",
            CorrectionRange { start: 7, end: 10 },
            1,
        ));
        error_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must exercise the grammar complete() error path");
        inference.submit(request("good", 2));
        let outcome = inference
            .recv_outcome()
            .expect("worker survives a grammar error and serves later requests");
        assert_eq!(outcome.candidates[0], "good");
        assert_eq!(outcome.correction, None);
        inference.shutdown();
    }

    #[test]
    fn worker_breaks_out_of_serve_loop_when_outcome_receiver_is_dropped() {
        // At shutdown the main loop drops the outcome receiver. The worker must
        // notice the failed `outcomes.send` and break out of its serve loop
        // (returning from `run`) rather than panicking or looping forever — even
        // while the request sender is still open and a request is queued.
        //
        // Driving `run` on the test thread makes this deterministic: if the
        // send-failure break did not fire, `run` would loop back, block on the
        // still-open request channel, and this call would never return.
        let (request_tx, request_rx) = channel::<CompletionRequest>();
        let (outcome_tx, outcome_rx) = channel::<CompletionOutcome>();
        request_tx.send(request("typing", 1)).unwrap();
        drop(outcome_rx); // receiver gone → the first send fails

        run(
            Box::new(StubModel::new("x")),
            PromptMode::Raw,
            Arc::new(Mutex::new(PersonalizationProfile::default())),
            1,
            WorkerContext::default(),
            request_rx,
            outcome_tx,
            Arc::new(AtomicBool::new(false)),
        );

        // Reaching here proves `run` returned: the keep-alive sender below shows
        // the loop did not exit merely because the request channel closed.
        drop(request_tx);
    }

    #[test]
    fn worker_calls_model_shutdown_on_graceful_channel_close() {
        // After the serve loop exits (request channel closed), run() calls
        // model.shutdown() (the ordered ggml-Metal teardown guard). Every other
        // test model uses the no-op default shutdown, so a refactor that early-
        // returned before that call — disabling the guard — would stay green.
        // Pin that the call actually fires.
        struct ShutdownTrackingModel {
            flag: Arc<AtomicBool>,
        }
        impl LocalModel for ShutdownTrackingModel {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                Ok(String::new())
            }
            fn shutdown(self: Box<Self>) {
                self.flag.store(true, Ordering::SeqCst);
            }
        }

        let flag = Arc::new(AtomicBool::new(false));
        let (request_tx, request_rx) = channel::<CompletionRequest>();
        let (outcome_tx, _outcome_rx) = channel::<CompletionOutcome>();
        drop(request_tx); // request channel closed → serve loop exits gracefully

        run(
            Box::new(ShutdownTrackingModel {
                flag: Arc::clone(&flag),
            }),
            PromptMode::Raw,
            Arc::new(Mutex::new(PersonalizationProfile::default())),
            1,
            WorkerContext::default(),
            request_rx,
            outcome_tx,
            Arc::new(AtomicBool::new(false)),
        );

        assert!(
            flag.load(Ordering::SeqCst),
            "worker must call model.shutdown() on graceful channel-close exit"
        );
        // _outcome_rx held alive so the exit is driven by the closed request
        // channel, not a dropped outcome receiver.
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
    fn recv_latest_drains_superseded_requests() {
        // Coalescing must CONSUME the superseded requests, not just peek the
        // newest: after returning gen 3 from a 3-deep channel, gens 1 & 2 must be
        // gone so a later recv blocks (channel empty) rather than re-serving the
        // stale gen 1. The winner-only test does not pin that the drain emptied
        // the queue.
        let (tx, rx) = channel::<CompletionRequest>();
        tx.send(request("a", 1)).unwrap();
        tx.send(request("b", 2)).unwrap();
        tx.send(request("c", 3)).unwrap();
        let latest = recv_latest(&rx).unwrap();
        assert_eq!(latest.generation, 3);
        // The superseded gens were consumed by the drain — nothing left queued.
        assert!(
            rx.try_recv().is_err(),
            "superseded requests must be drained, not left queued behind the winner"
        );
    }

    #[test]
    fn recv_latest_returns_none_when_sender_dropped() {
        let (tx, rx) = channel::<CompletionRequest>();
        drop(tx);
        assert!(recv_latest(&rx).is_none());
    }
}
