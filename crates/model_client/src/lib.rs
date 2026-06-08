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

pub type LocalModelResult<T> = Result<T, LocalModelError>;

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

pub trait LocalModel: Send + Sync {
    fn complete(&self, prompt: &str, max_tokens: usize) -> LocalModelResult<String>;

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
    WarmUp {
        reply: Sender<Result<(), LocalModelError>>,
    },
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
                let context_params = LlamaContextParams::default().with_n_ctx(Some(
                    // SAFETY: 2048 is non-zero
                    unsafe { NonZeroU32::new_unchecked(2048) },
                ));
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
            .map_err(|_| LocalModelError::new(stage, "model worker lock poisoned"))?;
        let tx = guard
            .as_ref()
            .ok_or_else(|| LocalModelError::new(stage, "model worker already shut down"))?;
        tx.send(make_job(reply_tx))
            .map_err(|_| LocalModelError::new(stage, "model worker is gone"))?;
        reply_rx
            .recv()
            .map_err(|_| LocalModelError::new(stage, "model worker dropped the reply"))
    }
}

/// Run one completion on the worker thread against the persistent context,
/// reusing the KV cache for the shared prefix and re-decoding only the divergent
/// suffix. On any FFI error the cache is reset so the next call starts clean.
fn complete_on_worker(
    model: &LlamaCppModel,
    context: &mut LlamaContext<'_>,
    prev_tokens: &mut Vec<LlamaToken>,
    prompt: &str,
    max_tokens: usize,
) -> LocalModelResult<String> {
    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|err| LocalModelError::new("tokenize prompt", err))?;
    if tokens.is_empty() {
        return Ok(String::new());
    }

    // Keep the shared prefix in the KV cache; drop everything from `reuse`
    // onward — that removes both the divergent prompt tail and any generated
    // tokens left over from the previous completion.
    let reuse = reusable_prefix_len(prev_tokens, &tokens);
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
    let last = to_decode.len() - 1;
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

    let mut sampler = LlamaSampler::greedy();
    let mut output = String::new();
    let mut decoder = encoding_rs::UTF_8.new_decoder();

    // The cache now holds the full prompt; record it so the next call can reuse
    // it. Generated-token KV (added below) is dropped by the next call's trim.
    *prev_tokens = tokens.clone();

    let first_generated_pos = tokens.len() as i32;
    for position in first_generated_pos..first_generated_pos + max_tokens as i32 {
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
        // Drop the sender so the worker's `recv` returns and it runs its ordered
        // teardown.
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
        if let Ok(mut guard) = self.job_tx.lock() {
            guard.take();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

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
        let model: Box<dyn LocalModel> = Box::new(Fixed("ok"));

        assert_eq!(model.warm_up(), Ok(()));
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
    fn shutdown_consumes_the_boxed_model() {
        // shutdown takes `self: Box<Self>`, so a graceful teardown both runs the
        // override and guarantees the resources are released exactly once.
        let model: Box<dyn LocalModel> = Box::new(Fixed("ok"));

        model.shutdown();
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
