//! Local model seam. Real implementation is llama.cpp; provider abstraction is later work.

use std::fmt;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel as LlamaCppModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;

/// Result alias for model-backend operations; the error is always
/// [`LocalModelError`].
pub type LocalModelResult<T> = Result<T, LocalModelError>;

/// A failed model operation, tagged with the pipeline `stage` that failed
/// ("tokenize prompt", "decode prompt", …) plus the backend's message. Stage
/// strings are matched by tests and telemetry — treat them as stable API, not
/// free-form text. The error never contains prompt content (privacy: prompts
/// hold the user's typed text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModelError {
    stage: &'static str,
    message: String,
}

impl LocalModelError {
    pub fn new(stage: &'static str, error: impl fmt::Display) -> Self {
        Self {
            stage,
            message: error.to_string(),
        }
    }

    pub fn stage(&self) -> &'static str {
        self.stage
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for LocalModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.stage, self.message)
    }
}

impl std::error::Error for LocalModelError {}

/// The inference seam. Implementors must be callable from any thread
/// (`Send + Sync`) and must serialize their own backend access — callers may
/// invoke `complete` concurrently. Calls are synchronous and blocking, so
/// hosts run them off the UI/event thread. An `Err` must leave the backend
/// reusable for the next call (no poisoned state).
pub trait LocalModel: Send + Sync {
    /// Generate a continuation of `prompt`, decoding at most `max_tokens`
    /// tokens. Blocks until done; returns only the continuation text, never a
    /// restatement of the prompt.
    fn complete(&self, prompt: &str, max_tokens: usize) -> LocalModelResult<String>;

    /// Generate up to `n` candidate continuations (multi-candidate / cycle, A2
    /// §16). The default returns a single candidate from `complete` — backends
    /// without sampling variation (the stub, fakes) yield one. Real backends
    /// override this with N independent samples (temperature/seed variation).
    fn complete_n(
        &self,
        prompt: &str,
        max_tokens: usize,
        n: usize,
    ) -> LocalModelResult<Vec<String>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        Ok(vec![self.complete(prompt, max_tokens)?])
    }

    /// Warm up the model (e.g. run a dummy inference to prime the KV cache).
    /// Default is a no-op; override in production backends.
    fn warm_up(&self) -> Result<(), LocalModelError> {
        Ok(())
    }

    /// Release model resources. Called on graceful shutdown.
    /// Default is a no-op; override in production backends.
    fn shutdown(self: Box<Self>) {}
}

/// The llama.cpp backend is a process-global singleton: `LlamaBackend::init()`
/// errors with `BackendAlreadyInitialized` on a second call. Init it once and
/// share a `'static` reference, so multiple `LlamaModel`s (and the model+context
/// that borrow it) can coexist. The backend is never freed — it lives for the
/// whole process, which also keeps it alive past every model/context it lent to.
fn shared_backend() -> Result<&'static LlamaBackend, String> {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    static INIT_LOCK: Mutex<()> = Mutex::new(());

    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    // Serialize racing initializers, then re-check under the lock so exactly one
    // thread calls `init()`.
    let _guard = INIT_LOCK
        .lock()
        .map_err(|_| "llama backend init lock poisoned".to_string())?;
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let backend = LlamaBackend::init().map_err(|err| format!("init llama backend: {err}"))?;
    let _ = BACKEND.set(backend);
    Ok(BACKEND.get().expect("backend just set"))
}

/// A unit of work sent to the inference worker thread. Each carries a one-shot
/// reply channel so the calling thread can block for the result.
enum Job {
    Complete {
        prompt: String,
        max_tokens: usize,
        reply: Sender<LocalModelResult<String>>,
    },
    CompleteN {
        prompt: String,
        max_tokens: usize,
        n: usize,
        reply: Sender<LocalModelResult<Vec<String>>>,
    },
    WarmUp {
        reply: Sender<Result<(), LocalModelError>>,
    },
}

/// The sampler for a candidate index: candidate 0 is greedy (the deterministic
/// best continuation); later candidates use temperature + a per-candidate seed so
/// they diverge (multi-candidate generation).
fn sampler_for_candidate(index: usize) -> LlamaSampler {
    if index == 0 {
        LlamaSampler::greedy()
    } else {
        // Truncate the low-probability tail (top_k/top_p) before temperature
        // sampling so divergent candidates stay coherent rather than drawing
        // garbage/control tokens from the full vocabulary.
        LlamaSampler::chain_simple([
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::temp(0.8),
            LlamaSampler::dist(index as u32),
        ])
    }
}

/// A handle to a llama.cpp model running on a dedicated worker thread.
///
/// The worker owns the backend, model, and a **persistent** `LlamaContext` for
/// its whole lifetime. Two reasons drive the thread:
///
/// 1. **Lifetime.** `LlamaContext<'a>` borrows the `LlamaModel`, so a context
///    cannot be stored next to its model in one struct (self-reference). Keeping
///    both on the worker's stack sidesteps that without `unsafe` or extra deps.
/// 2. **Spec §5** requires a warm model + prefix cache + serialized llama calls.
///    The persistent context enables prefix-KV reuse, and the single worker
///    serializes calls. `complete` holds a mutex across the round-trip, so two
///    callers can never interleave.
///
/// Reuse note: this assumes a non-recurrent transformer (our Qwen2.5 default),
/// where removing a KV-cache suffix and re-decoding from a position is sound.
/// A recurrent/hybrid model would need full re-decode each call instead.
pub struct LlamaModel {
    job_tx: Mutex<Option<Sender<Job>>>,
    handle: Option<JoinHandle<()>>,
}

impl LlamaModel {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = path.to_path_buf();
        // Load on the worker so the backend, model, and context all live on the
        // same thread for the model's whole lifetime.
        let (load_tx, load_rx) = channel::<Result<(), String>>();
        let (job_tx, job_rx) = channel::<Job>();

        let handle = std::thread::Builder::new()
            .name("model-client-llama".into())
            .spawn(move || {
                let backend = match shared_backend() {
                    Ok(backend) => backend,
                    Err(message) => {
                        let _ = load_tx.send(Err(message));
                        return;
                    }
                };
                let model = match LlamaCppModel::load_from_file(
                    backend,
                    &path,
                    &LlamaModelParams::default().with_n_gpu_layers(999),
                ) {
                    Ok(model) => model,
                    Err(err) => {
                        let _ = load_tx.send(Err(format!("load model: {err}")));
                        return;
                    }
                };
                let context_params = LlamaContextParams::default()
                    .with_n_ctx(Some(NonZeroU32::new(2048).expect("2048 is non-zero")));
                let mut context = match model.new_context(backend, context_params) {
                    Ok(context) => context,
                    Err(err) => {
                        let _ = load_tx.send(Err(format!("create llama context: {err}")));
                        return;
                    }
                };

                // Load succeeded — release the caller. From here the worker owns
                // the context and serves jobs until the channel closes.
                let _ = load_tx.send(Ok(()));

                let mut prev_tokens: Vec<LlamaToken> = Vec::new();
                while let Ok(job) = job_rx.recv() {
                    match job {
                        Job::Complete {
                            prompt,
                            max_tokens,
                            reply,
                        } => {
                            let result = complete_on_worker(
                                &model,
                                &mut context,
                                &mut prev_tokens,
                                &prompt,
                                max_tokens,
                                &mut sampler_for_candidate(0),
                            );
                            let _ = reply.send(result);
                        }
                        Job::CompleteN {
                            prompt,
                            max_tokens,
                            n,
                            reply,
                        } => {
                            let result = complete_candidates_on_worker(
                                &model,
                                &mut context,
                                &mut prev_tokens,
                                &prompt,
                                max_tokens,
                                n,
                            );
                            let _ = reply.send(result);
                        }
                        Job::WarmUp { reply } => {
                            let result = complete_on_worker(
                                &model,
                                &mut context,
                                &mut prev_tokens,
                                "warm up",
                                1,
                                &mut sampler_for_candidate(0),
                            )
                            .map(|_| ());
                            let _ = reply.send(result);
                        }
                    }
                }

                // Channel closed (shutdown): free the context, then the model,
                // in that order — the ggml-Metal exit-abort guard (spec
                // §"ggml-Metal aborts on exit unless freed in order"). The
                // backend is `'static` and intentionally outlives them both.
                drop(context);
                drop(model);
            })
            .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> {
                format!("spawn model worker: {err}").into()
            })?;

        match load_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                job_tx: Mutex::new(Some(job_tx)),
                handle: Some(handle),
            }),
            Ok(Err(message)) => {
                let _ = handle.join();
                Err(message.into())
            }
            Err(_) => {
                let _ = handle.join();
                Err("model worker exited before signalling load".into())
            }
        }
    }

    /// Send a job to the worker and block for its reply, holding the lock across
    /// the whole round-trip so concurrent callers are fully serialized (not just
    /// ordered by the channel). This cannot deadlock: the reply sender is the
    /// worker thread, which never touches `job_tx`.
    fn dispatch<T>(
        &self,
        stage: &'static str,
        make_job: impl FnOnce(Sender<T>) -> Job,
    ) -> Result<T, LocalModelError> {
        let (reply_tx, reply_rx) = channel::<T>();
        let guard = self
            .job_tx
            .lock()
            .map_err(|_| dispatch_error(stage, DispatchFailure::LockPoisoned))?;
        let tx = guard
            .as_ref()
            .ok_or_else(|| dispatch_error(stage, DispatchFailure::AlreadyShutDown))?;
        tx.send(make_job(reply_tx))
            .map_err(|_| dispatch_error(stage, DispatchFailure::SendFailed))?;
        reply_rx
            .recv()
            .map_err(|_| dispatch_error(stage, DispatchFailure::ReplyDropped))
    }
}

/// Why a [`LlamaModel::dispatch`] round-trip failed before producing a reply.
/// Each variant maps to exactly one failure arm so the (otherwise FFI-coupled)
/// error wording can be unit-tested without a real worker/GGUF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchFailure {
    /// The `job_tx` mutex was poisoned by a panicked holder.
    LockPoisoned,
    /// `shutdown`/`drop` already took the sender (worker gone).
    AlreadyShutDown,
    /// The worker's receiver hung up, so the job could not be sent.
    SendFailed,
    /// The worker dropped the one-shot reply sender before answering.
    ReplyDropped,
}

/// Map a [`DispatchFailure`] to its typed [`LocalModelError`]. Pure: no FFI, no
/// channels — just the `stage` tag and the stable diagnostic message for the
/// arm. Keeping this out of `dispatch` lets the message wording be asserted
/// directly (the live path needs a real GGUF). The messages are matched by
/// callers/telemetry — treat them as stable API.
fn dispatch_error(stage: &'static str, failure: DispatchFailure) -> LocalModelError {
    let message = match failure {
        DispatchFailure::LockPoisoned => "model worker lock poisoned",
        DispatchFailure::AlreadyShutDown => "model worker already shut down",
        DispatchFailure::SendFailed => "model worker is gone",
        DispatchFailure::ReplyDropped => "model worker dropped the reply",
    };
    LocalModelError::new(stage, message)
}

/// Run one completion on the worker thread against the persistent context,
/// reusing the KV cache for the shared prefix and re-decoding only the divergent
/// suffix. On any FFI error the cache is reset so the next call starts clean.
/// Generate `n` candidate continuations for one prompt. Each candidate decodes
/// the prompt fresh (prev cleared) so candidates are independent; candidate 0 is
/// greedy, the rest use temperature+seed sampling. `prev_tokens` is left holding
/// the prompt so the next request can reuse its KV prefix.
///
/// Tradeoff (accepted): this re-decodes the shared prompt prefix once per
/// candidate rather than branching the prompt KV, so N candidates cost ~N prompt
/// decodes. For small N (≤5) and short prompts the simplicity (no subtle KV-branch
/// bug) is worth it; a prompt-prefix-reuse optimization is a future enhancement.
fn complete_candidates_on_worker(
    model: &LlamaCppModel,
    context: &mut LlamaContext<'_>,
    prev_tokens: &mut Vec<LlamaToken>,
    prompt: &str,
    max_tokens: usize,
    n: usize,
) -> LocalModelResult<Vec<String>> {
    let mut candidates = Vec::with_capacity(n);
    for index in 0..n {
        // Force a clean decode per candidate so they don't share generated KV.
        prev_tokens.clear();
        let text = complete_on_worker(
            model,
            context,
            prev_tokens,
            prompt,
            max_tokens,
            &mut sampler_for_candidate(index),
        )?;
        candidates.push(text);
    }
    Ok(candidates)
}

fn complete_on_worker(
    model: &LlamaCppModel,
    context: &mut LlamaContext<'_>,
    prev_tokens: &mut Vec<LlamaToken>,
    prompt: &str,
    max_tokens: usize,
    sampler: &mut LlamaSampler,
) -> LocalModelResult<String> {
    let mut tokens = model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|err| LocalModelError::new("tokenize prompt", err))?;
    if tokens.is_empty() {
        return Ok(String::new());
    }

    // All position arithmetic comes from the pure, unit-tested `plan_decode`:
    // clamp the prompt to the context window (drop leading tokens, keep the
    // caret-adjacent tail) and reuse the shared KV prefix, re-decoding only the
    // divergent suffix from `reuse` onward (which also drops any generated tokens
    // left over from the previous completion).
    let plan = plan_decode(prev_tokens, &tokens, max_tokens, context.n_ctx() as usize);
    if plan.skip > 0 {
        tokens.drain(..plan.skip);
    }
    let reuse = plan.reuse;
    let reset_on_err = |context: &mut LlamaContext<'_>, prev: &mut Vec<LlamaToken>| {
        let _ = context.clear_kv_cache_seq(Some(0), None, None);
        prev.clear();
    };

    if let Err(err) = context.clear_kv_cache_seq(Some(0), Some(reuse as u32), None) {
        reset_on_err(context, prev_tokens);
        return Err(LocalModelError::new("trim kv cache", err));
    }

    let to_decode = &tokens[reuse..];
    let mut batch = LlamaBatch::new(to_decode.len().max(1), 1);
    // saturating_sub mirrors the .max(1) above: to_decode is non-empty in
    // practice (plan_decode reserves >=1 prompt token), but never underflow to
    // usize::MAX if that invariant ever changes — the loop just won't flag a
    // "last" token when empty.
    let last = to_decode.len().saturating_sub(1);
    for (index, token) in to_decode.iter().enumerate() {
        let position = (reuse + index) as i32;
        if let Err(err) = batch.add(*token, position, &[0], index == last) {
            reset_on_err(context, prev_tokens);
            return Err(LocalModelError::new("add prompt token to batch", err));
        }
    }
    if let Err(err) = context.decode(&mut batch) {
        reset_on_err(context, prev_tokens);
        return Err(LocalModelError::new("decode prompt", err));
    }

    let mut output = String::new();
    let mut decoder = encoding_rs::UTF_8.new_decoder();

    // The cache now holds the full prompt; record it so the next call can reuse
    // it. Generated-token KV (added below) is dropped by the next call's trim.
    *prev_tokens = tokens.clone();

    let first_generated_pos = tokens.len() as i32;
    // Clamp the budget into i32 with saturating arithmetic so a pathological
    // max_tokens (> i32::MAX, or one that would overflow the position) can't wrap
    // to a negative end and silently produce an empty generation range. llama.cpp
    // positions are i32; a budget at the ceiling is bounded by n_ctx in practice.
    let last_generated_pos =
        first_generated_pos.saturating_add(i32::try_from(max_tokens).unwrap_or(i32::MAX));
    for position in first_generated_pos..last_generated_pos {
        let token = sampler.sample(context, batch.n_tokens() - 1);
        if model.is_eog_token(token) {
            break;
        }

        sampler.accept(token);
        let piece = match model.token_to_piece(token, &mut decoder, true, None) {
            Ok(piece) => piece,
            Err(err) => {
                reset_on_err(context, prev_tokens);
                return Err(LocalModelError::new("decode token piece", err));
            }
        };
        output.push_str(&piece);

        batch.clear();
        if let Err(err) = batch.add(token, position, &[0], true) {
            reset_on_err(context, prev_tokens);
            return Err(LocalModelError::new("add sampled token to batch", err));
        }
        if let Err(err) = context.decode(&mut batch) {
            reset_on_err(context, prev_tokens);
            return Err(LocalModelError::new("decode sampled token", err));
        }
    }

    Ok(output)
}

impl LocalModel for LlamaModel {
    fn complete(&self, prompt: &str, max_tokens: usize) -> LocalModelResult<String> {
        let prompt = prompt.to_string();
        self.dispatch("complete", move |reply| Job::Complete {
            prompt,
            max_tokens,
            reply,
        })?
    }

    fn complete_n(
        &self,
        prompt: &str,
        max_tokens: usize,
        n: usize,
    ) -> LocalModelResult<Vec<String>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let prompt = prompt.to_string();
        self.dispatch("complete_n", move |reply| Job::CompleteN {
            prompt,
            max_tokens,
            n,
            reply,
        })?
    }

    /// Pre-load Metal shaders with a single throwaway decode on the worker.
    ///
    /// The first decode after load triggers ggml's Metal shader compile, which
    /// costs seconds. Spec §"Warm-up mandatory": pre-load model + dummy decode
    /// at launch so the first real completion is on the warm path.
    fn warm_up(&self) -> Result<(), LocalModelError> {
        self.dispatch("warm up", |reply| Job::WarmUp { reply })?
    }

    /// Close the job channel and join the worker, which frees the context, model,
    /// and backend in order before the thread exits.
    ///
    /// Spec §"ggml-Metal aborts on exit unless model/context freed via explicit
    /// `shutdown()` before teardown (guard double-free)".
    fn shutdown(mut self: Box<Self>) {
        self.close_worker();
    }
}

impl LlamaModel {
    /// Drop the sender so the worker's `recv` returns and it runs its ordered
    /// teardown, then join the thread. Idempotent: a second call (e.g. `Drop`
    /// after an explicit `shutdown`) finds the channel already closed and the
    /// handle already taken, so it is a harmless no-op.
    fn close_worker(&mut self) {
        if let Ok(mut guard) = self.job_tx.lock() {
            guard.take();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LlamaModel {
    fn drop(&mut self) {
        // If `shutdown` was not called (e.g. the model is dropped directly), still
        // close the channel and join so the worker frees resources in order.
        self.close_worker();
    }
}

/// Wrap the caret-left `prefix` in the fixed inline-completion instruction.
/// Callers must pass already-trimmed text (`context::trim_trailing`) — no
/// trimming happens here — and `prefix` is user text: it must already be
/// redaction/secure-field gated before reaching this function. Downstream
/// shaping assumes the model returns only the continuation.
pub fn terse_continuation_prompt(prefix: &str) -> String {
    format!("Complete this text inline. Return only the continuation.\nText: {prefix}")
}

/// How many leading tokens of `next` can reuse the KV cache already holding
/// `prev`'s tokens. This is the length of the shared prefix, except we always
/// leave at least one token of `next` to re-decode so the context produces fresh
/// logits for the first generated token (a fully-cached prompt still needs one
/// live decode). Returns 0 when nothing is reusable or `next` is empty.
pub fn reusable_prefix_len<T: PartialEq>(prev: &[T], next: &[T]) -> usize {
    if next.is_empty() {
        return 0;
    }
    let shared = prev
        .iter()
        .zip(next.iter())
        .take_while(|(a, b)| a == b)
        .count();
    // Always leave at least one token of `next` to re-decode for fresh logits.
    shared.min(next.len() - 1)
}

/// How many leading prompt tokens to drop so the prompt plus the generation
/// budget fit in the context window. The completion needs `max_tokens` of room,
/// so the prompt may use at most `n_ctx - max_tokens` tokens (at least 1). When
/// the prompt is longer we drop from the *front*, keeping the caret-adjacent tail
/// (the most relevant context). Without this, an over-long prompt makes every
/// `decode` fail → reset → no completion at all for large-context fields.
pub fn prompt_tokens_to_skip(prompt_len: usize, max_tokens: usize, n_ctx: usize) -> usize {
    // Reserve room for the generated tokens; always leave the prompt at least one
    // token so a tiny/zero window still decodes the caret-adjacent token.
    let budget = n_ctx.saturating_sub(max_tokens).max(1);
    prompt_len.saturating_sub(budget)
}

/// The arithmetic for one decode, derived purely from the previous (clamped)
/// prompt tokens, the new prompt tokens, the generation budget, and the context
/// window. Separated from the FFI so the position math — the part a "wrong
/// `seq_rm` / position" bug would corrupt — is unit-testable without a model.
///
/// Given a plan, `complete_on_worker` must: drop the first `skip` tokens, keep
/// KV positions `[0, reuse)` and clear `[reuse, ∞)`, decode the clamped tokens
/// `[reuse, prompt_len)` at those same positions, then generate starting at
/// position `prompt_len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodePlan {
    /// Leading prompt tokens to drop so prompt + generation fit the window.
    pub skip: usize,
    /// KV-cache prefix (of the clamped prompt) to reuse; the suffix is re-decoded.
    pub reuse: usize,
    /// Clamped prompt length; generation begins at this position.
    pub prompt_len: usize,
}

/// Compute the [`DecodePlan`] for `current` prompt tokens against the `prev`
/// (clamped) tokens still in the KV cache.
pub fn plan_decode<T: PartialEq>(
    prev: &[T],
    current: &[T],
    max_tokens: usize,
    n_ctx: usize,
) -> DecodePlan {
    let skip = prompt_tokens_to_skip(current.len(), max_tokens, n_ctx);
    let clamped = &current[skip..];
    let reuse = reusable_prefix_len(prev, clamped);
    DecodePlan {
        skip,
        reuse,
        prompt_len: clamped.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str);

    impl LocalModel for Fixed {
        fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
            Ok(self.0.into())
        }
    }

    #[test]
    fn trait_object_is_usable() {
        let model: Box<dyn LocalModel> = Box::new(Fixed("ok"));

        assert_eq!(model.complete("x", 8).expect("fixed completion"), "ok");
    }

    #[test]
    fn default_complete_n_returns_a_single_candidate() {
        let model: Box<dyn LocalModel> = Box::new(Fixed("only"));
        assert_eq!(model.complete_n("x", 8, 3).unwrap(), vec!["only"]);
    }

    #[test]
    fn complete_n_zero_is_empty() {
        let model: Box<dyn LocalModel> = Box::new(Fixed("x"));
        assert!(model.complete_n("x", 8, 0).unwrap().is_empty());
    }

    #[test]
    fn default_complete_n_propagates_complete_errors() {
        // The default complete_n is `Ok(vec![self.complete(...)?])`: a backend
        // that fails the single underlying complete() must surface that typed
        // error (same stage/message), not swallow it into an empty/partial vec.
        // Guards the `?` in the default impl — a regression dropping it would
        // turn a hard failure into a silent success.
        struct Failing;
        impl LocalModel for Failing {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                Err(LocalModelError::new("decode prompt", "backend unavailable"))
            }
        }
        let model: Box<dyn LocalModel> = Box::new(Failing);
        let err = model
            .complete_n("x", 8, 3)
            .expect_err("default complete_n must surface complete()'s error");
        assert_eq!(err.stage(), "decode prompt");
        assert_eq!(err.message(), "backend unavailable");
    }

    #[test]
    fn complete_n_override_can_return_multiple() {
        struct Multi;
        impl LocalModel for Multi {
            fn complete(&self, _p: &str, _n: usize) -> LocalModelResult<String> {
                Ok("a".into())
            }
            fn complete_n(&self, _p: &str, _max: usize, n: usize) -> LocalModelResult<Vec<String>> {
                Ok((0..n).map(|i| format!("cand{i}")).collect())
            }
        }
        let model: Box<dyn LocalModel> = Box::new(Multi);
        assert_eq!(model.complete_n("x", 8, 2).unwrap(), vec!["cand0", "cand1"]);
    }

    // Pins the multi-candidate *divergence contract* without a model: candidate 0
    // is greedy/deterministic (no random seed), while candidates 1..n each carry a
    // distinct per-candidate seed via `dist(index)`. `LlamaSampler::get_seed`
    // returns 0xFFFFFFFF for seedless samplers (e.g. greedy) and, for a chain, the
    // first non-default seed found in reverse order — here the `dist(index)` seed.
    // End-to-end token divergence is proven separately by the real-model
    // `complete_n_returns_real_model_candidates` test (#[ignore]'d, needs a GGUF).
    #[test]
    fn sampler_for_candidate_zero_is_greedy_seedless() {
        // Greedy/deterministic: no random seed (sentinel 0xFFFF_FFFF).
        assert_eq!(
            sampler_for_candidate(0).get_seed(),
            0xFFFF_FFFF,
            "candidate 0 must be greedy (seedless/deterministic)"
        );
    }

    #[test]
    fn sampler_for_candidate_nonzero_seeds_diverge() {
        // The divergence CONTRACT (not the exact seed formula): each later
        // candidate is non-greedy (carries an actual random seed) and the seeds
        // are pairwise distinct, so no two divergent candidates share an
        // identical sampler configuration. We deliberately do NOT pin
        // `seed == index` — that the seed happens to equal `dist(index)` is an
        // implementation detail of how distinctness is achieved, and the
        // contract holds for any distinct non-greedy seeding scheme.
        let seeds: Vec<u32> = (1..=4)
            .map(|i| sampler_for_candidate(i).get_seed())
            .collect();

        // Every divergent candidate carries an actual random seed (not the greedy
        // sentinel) — i.e. it is NOT the deterministic candidate 0.
        for (offset, seed) in seeds.iter().enumerate() {
            let index = offset + 1;
            assert_ne!(
                *seed, 0xFFFF_FFFF,
                "candidate {index} must NOT be seedless/greedy"
            );
        }

        // And the seeds are pairwise distinct, so no two divergent candidates
        // share an identical sampler configuration.
        let mut unique = seeds.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            seeds.len(),
            "candidate seeds must be distinct: {seeds:?}"
        );
    }

    // Task 1: the dispatch error-mapping is a pure helper so the post-shutdown /
    // worker-gone paths can be asserted without a real GGUF. Each arm must carry
    // stage=="complete" (or whatever the caller passes) and the documented,
    // stable diagnostic substring — never panic/hang. The live `dispatch` routes
    // every arm through this same helper, so these pin its observable contract.
    #[test]
    fn dispatch_error_lock_poisoned_is_typed() {
        let err = dispatch_error("complete", DispatchFailure::LockPoisoned);
        assert_eq!(err.stage(), "complete");
        assert!(
            err.message().contains("lock poisoned"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn dispatch_error_after_shutdown_says_already_shut_down() {
        // The arm hit when `complete`/`complete_n`/`warm_up` run AFTER shutdown()
        // took the sender: a typed error, not a panic/hang.
        let err = dispatch_error("complete", DispatchFailure::AlreadyShutDown);
        assert_eq!(err.stage(), "complete");
        assert!(
            err.message().contains("already shut down"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn dispatch_error_send_failed_says_worker_is_gone() {
        let err = dispatch_error("complete", DispatchFailure::SendFailed);
        assert_eq!(err.stage(), "complete");
        assert!(
            err.message().contains("worker is gone"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn dispatch_error_reply_dropped_says_dropped_the_reply() {
        let err = dispatch_error("complete", DispatchFailure::ReplyDropped);
        assert_eq!(err.stage(), "complete");
        assert!(
            err.message().contains("dropped the reply"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn dispatch_error_carries_the_callers_stage() {
        // The stage is the caller's pipeline label, not hard-coded — warm_up and
        // complete_n use their own. Pin that the helper forwards it verbatim AND
        // that the failure-specific message is independent of the stage (the
        // stage label must not leak into / overwrite the diagnostic wording).
        for stage in ["complete", "complete_n", "warm up"] {
            let err = dispatch_error(stage, DispatchFailure::AlreadyShutDown);
            assert_eq!(err.stage(), stage);
            assert!(
                err.message().contains("already shut down"),
                "stage {stage} got message: {}",
                err.message()
            );
        }
    }

    #[test]
    fn dispatch_failures_have_distinct_messages() {
        // Each arm must be diagnosable on its own — no two failures collapse to
        // the same wording (telemetry/callers disambiguate on the message).
        let messages: Vec<String> = [
            DispatchFailure::LockPoisoned,
            DispatchFailure::AlreadyShutDown,
            DispatchFailure::SendFailed,
            DispatchFailure::ReplyDropped,
        ]
        .into_iter()
        .map(|f| dispatch_error("complete", f).message().to_string())
        .collect();
        let mut unique = messages.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), messages.len(), "messages: {messages:?}");
    }

    // Task 2: complete_n short-circuits n==0 to Ok(vec![]) WITHOUT dispatching to
    // the worker. Testable on the trait DEFAULT impl with a counting fake — no
    // GGUF needed. The LlamaModel-specific short-circuit (lib.rs ~L476) is the
    // same `if n == 0 { return Ok(Vec::new()) }` guard placed BEFORE `dispatch`,
    // but exercising it needs a real worker (a GGUF), so it stays GGUF-gated; the
    // default-impl test pins the observable "no completion happens" contract.
    #[test]
    fn complete_n_zero_returns_empty_without_dispatch() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counting(AtomicUsize);
        impl LocalModel for Counting {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("x".into())
            }
        }

        let model = Counting(AtomicUsize::new(0));
        let out = model.complete_n("prompt", 8, 0).expect("n==0 is Ok");
        assert!(out.is_empty(), "n==0 must return an empty vec");
        assert_eq!(
            model.0.load(Ordering::SeqCst),
            0,
            "n==0 must short-circuit BEFORE any complete()/dispatch call"
        );
    }

    #[test]
    fn trait_object_can_report_completion_errors() {
        struct Failing;

        impl LocalModel for Failing {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                Err(LocalModelError::new("decode prompt", "backend unavailable"))
            }
        }

        let model: Box<dyn LocalModel> = Box::new(Failing);
        let err = model
            .complete("x", 8)
            .expect_err("failing completion should surface an error");

        assert_eq!(err.stage(), "decode prompt");
        assert_eq!(err.message(), "backend unavailable");
        assert_eq!(err.to_string(), "decode prompt failed: backend unavailable");
    }

    #[test]
    fn default_warm_up_is_ok_noop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A counting fake proves the default warm_up is a true NO-OP: it returns
        // Ok without invoking complete() (no dummy inference). A default that
        // routed through complete() would bump the counter.
        struct Counting(AtomicUsize);
        impl LocalModel for Counting {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("x".into())
            }
        }

        let model = Counting(AtomicUsize::new(0));
        assert_eq!(model.warm_up(), Ok(()));
        assert_eq!(
            model.0.load(Ordering::SeqCst),
            0,
            "default warm_up must not invoke complete()"
        );
    }

    #[test]
    fn warm_up_surfaces_backend_errors() {
        struct WarmFails;

        impl LocalModel for WarmFails {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                Err(LocalModelError::new(
                    "decode prompt",
                    "metal compile failed",
                ))
            }

            fn warm_up(&self) -> Result<(), LocalModelError> {
                self.complete("warm up", 1).map(|_| ())
            }
        }

        let model: Box<dyn LocalModel> = Box::new(WarmFails);
        let err = model
            .warm_up()
            .expect_err("warm-up should surface decode errors");

        assert_eq!(err.stage(), "decode prompt");
    }

    #[test]
    fn shutdown_consumes_the_boxed_model_dropping_it_exactly_once() {
        // shutdown takes `self: Box<Self>`, so a graceful teardown must drop the
        // model EXACTLY once — resources released once, no double-free, no leak.
        // A Drop counter makes the "exactly once" claim in the name verifiable
        // (the old test only proved the call didn't panic).
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        impl LocalModel for Counted {
            fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
                Ok(String::new())
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let model: Box<dyn LocalModel> = Box::new(Counted(Arc::clone(&drops)));
        model.shutdown();
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "shutdown must drop the model exactly once"
        );
    }

    #[test]
    fn plan_decode_fresh_prompt_no_reuse_no_skip() {
        // No prev tokens, prompt fits: decode the whole prompt from position 0.
        let plan = plan_decode::<i32>(&[], &[1, 2, 3, 4], 24, 2048);
        assert_eq!(
            plan,
            DecodePlan {
                skip: 0,
                reuse: 0,
                prompt_len: 4
            }
        );
    }

    #[test]
    fn plan_decode_reuses_shared_prefix() {
        // prev=[1,2,3], current=[1,2,9]: keep [1,2], re-decode the divergent tail.
        let plan = plan_decode(&[1, 2, 3], &[1, 2, 9], 24, 2048);
        assert_eq!(
            plan,
            DecodePlan {
                skip: 0,
                reuse: 2,
                prompt_len: 3
            }
        );
    }

    #[test]
    fn plan_decode_clamps_then_computes_reuse_on_clamped_tokens() {
        // n_ctx=6, max_tokens=2 → budget 4. current len 6 → skip 2, clamped is the
        // last 4 tokens. reuse is computed against prev using the CLAMPED tokens.
        let prev = vec![3, 4, 5, 6]; // matches the clamped tail [3,4,5,6]
        let current = vec![1, 2, 3, 4, 5, 6];
        let plan = plan_decode(&prev, &current, 2, 6);
        assert_eq!(plan.skip, 2);
        assert_eq!(plan.prompt_len, 4);
        // clamped == prev → reuse leaves one to re-decode → 3.
        assert_eq!(plan.reuse, 3);
    }

    #[test]
    fn plan_decode_skip_with_divergent_prev_reuses_nothing() {
        // Over-long prompt (skip>0) whose CLAMPED tail shares no prefix with the
        // stale cache: reuse must be 0, forcing a full re-decode of the clamped
        // window. Pins that reuse is computed against the CLAMPED tokens, not the
        // raw `current` — a bug measuring reuse on raw tokens could match the
        // dropped front and wrongly skip re-decoding caret-adjacent context.
        let prev = vec![1, 2, 3, 4]; // matches the DROPPED front, not the tail
        let current = vec![1, 2, 3, 4, 5, 6];
        let plan = plan_decode(&prev, &current, 2, 6);
        assert_eq!(plan.skip, 2);
        assert_eq!(plan.prompt_len, 4);
        assert_eq!(plan.reuse, 0, "stale prefix on dropped front must not reuse");
    }

    #[test]
    fn plan_decode_reuse_never_reaches_prompt_len() {
        // Identical prompt: must leave at least one token to re-decode.
        let plan = plan_decode(&[1, 2, 3], &[1, 2, 3], 24, 2048);
        assert_eq!(plan.prompt_len, 3);
        assert!(
            plan.reuse < plan.prompt_len,
            "reuse must leave >=1 to decode"
        );
        assert_eq!(plan.reuse, 2);
    }

    #[test]
    fn prompt_skip_zero_when_prompt_fits() {
        // 100-token prompt + 24 budget = 124 <= 2048 → nothing dropped.
        assert_eq!(prompt_tokens_to_skip(100, 24, 2048), 0);
    }

    #[test]
    fn prompt_skip_drops_front_when_over_budget() {
        // budget = 2048 - 24 = 2024; prompt 2100 → drop 76 from the front.
        assert_eq!(prompt_tokens_to_skip(2100, 24, 2048), 76);
    }

    #[test]
    fn prompt_skip_at_exact_budget_keeps_all() {
        // prompt == budget (2024) → nothing dropped.
        assert_eq!(prompt_tokens_to_skip(2024, 24, 2048), 0);
    }

    #[test]
    fn prompt_skip_reserves_at_least_one_prompt_token() {
        // max_tokens >= n_ctx would leave a zero budget; clamp budget to >=1, so
        // the worst case keeps exactly the last prompt token.
        assert_eq!(prompt_tokens_to_skip(50, 2048, 2048), 49);
        assert_eq!(prompt_tokens_to_skip(50, 9999, 2048), 49);
    }

    #[test]
    fn reusable_prefix_len_empty_prev_is_zero() {
        assert_eq!(reusable_prefix_len::<i32>(&[], &[1, 2, 3]), 0);
    }

    #[test]
    fn reusable_prefix_len_empty_next_is_zero() {
        assert_eq!(reusable_prefix_len::<i32>(&[1, 2, 3], &[]), 0);
    }

    #[test]
    fn reusable_prefix_len_no_common_prefix_is_zero() {
        assert_eq!(reusable_prefix_len(&[9, 8, 7], &[1, 2, 3]), 0);
    }

    #[test]
    fn reusable_prefix_len_partial_common_prefix() {
        // prev=[a,b,c], next=[a,b,x] -> keep [a,b], re-decode x.
        assert_eq!(reusable_prefix_len(&[1, 2, 3], &[1, 2, 9]), 2);
    }

    #[test]
    fn reusable_prefix_len_next_extends_prev_keeps_all_prev() {
        // prev fully matches a prefix of next -> keep all prev, decode the rest.
        assert_eq!(reusable_prefix_len(&[1, 2], &[1, 2, 3, 4]), 2);
    }

    #[test]
    fn reusable_prefix_len_identical_leaves_one_to_decode() {
        // Identical prompts: a cached prompt still needs one live decode for
        // fresh logits, so we cannot reuse the final token.
        assert_eq!(reusable_prefix_len(&[1, 2, 3], &[1, 2, 3]), 2);
    }

    #[test]
    fn reusable_prefix_len_next_is_prefix_of_prev_leaves_one() {
        // next=[a,b], prev=[a,b,c,d]: shared=2 == next.len() -> leave one -> 1.
        assert_eq!(reusable_prefix_len(&[1, 2, 3, 4], &[1, 2]), 1);
    }

    #[test]
    fn reusable_prefix_len_single_matching_token_leaves_zero() {
        // next=[a], shared=1 == next.len() -> must leave one -> 0.
        assert_eq!(reusable_prefix_len(&[1, 2], &[1]), 0);
    }

    #[test]
    fn terse_prompt_keeps_prefix_configurable() {
        assert_eq!(
            terse_continuation_prompt("Dear team"),
            "Complete this text inline. Return only the continuation.\nText: Dear team"
        );
    }
}
