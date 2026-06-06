//! Local model seam. Real implementation is llama.cpp; provider abstraction is later work.

use std::fmt;
use std::num::NonZeroU32;
use std::path::Path;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel as LlamaCppModel};
use llama_cpp_2::sampling::LlamaSampler;

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

pub struct LlamaModel {
    backend: LlamaBackend,
    model: LlamaCppModel,
    context_params: LlamaContextParams,
}

impl LlamaModel {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let backend = LlamaBackend::init()?;
        let model = LlamaCppModel::load_from_file(
            &backend,
            path,
            &LlamaModelParams::default().with_n_gpu_layers(999),
        )?;
        let context_params = LlamaContextParams::default().with_n_ctx(Some(
            // SAFETY: 2048 is non-zero
            unsafe { NonZeroU32::new_unchecked(2048) },
        ));

        Ok(Self {
            backend,
            model,
            context_params,
        })
    }
}

impl LocalModel for LlamaModel {
    fn complete(&self, prompt: &str, max_tokens: usize) -> LocalModelResult<String> {
        let mut context = self
            .model
            .new_context(&self.backend, self.context_params.clone())
            .map_err(|err| LocalModelError::new("create llama context", err))?;
        let tokens = self
            .model
            .str_to_token(prompt, AddBos::Always)
            .map_err(|err| LocalModelError::new("tokenize prompt", err))?;
        if tokens.is_empty() {
            return Ok(String::new());
        }

        let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
        let last = tokens.len() - 1;
        for (index, token) in tokens.iter().enumerate() {
            batch
                .add(*token, index as i32, &[0], index == last)
                .map_err(|err| LocalModelError::new("add prompt token to batch", err))?;
        }
        context
            .decode(&mut batch)
            .map_err(|err| LocalModelError::new("decode prompt", err))?;

        let mut sampler = LlamaSampler::greedy();
        let mut output = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        let first_generated_pos = batch.n_tokens();
        for position in first_generated_pos..first_generated_pos + max_tokens as i32 {
            let token = sampler.sample(&context, batch.n_tokens() - 1);
            if self.model.is_eog_token(token) {
                break;
            }

            sampler.accept(token);
            let piece = self
                .model
                .token_to_piece(token, &mut decoder, true, None)
                .map_err(|err| LocalModelError::new("decode token piece", err))?;
            output.push_str(&piece);

            batch.clear();
            batch
                .add(token, position, &[0], true)
                .map_err(|err| LocalModelError::new("add sampled token to batch", err))?;
            context
                .decode(&mut batch)
                .map_err(|err| LocalModelError::new("decode sampled token", err))?;
        }

        Ok(output)
    }

    /// Pre-load Metal shaders with a single throwaway decode.
    ///
    /// The first decode after load triggers ggml's Metal shader compile, which
    /// costs seconds. Spec §"Warm-up mandatory": pre-load model + dummy decode
    /// at launch so the first real completion is on the warm path.
    fn warm_up(&self) -> Result<(), LocalModelError> {
        self.complete("warm up", 1).map(|_| ())
    }

    /// Free the model and backend in a deterministic order before process exit.
    ///
    /// Spec §"ggml-Metal aborts on exit unless model/context freed via explicit
    /// `shutdown()` before teardown (guard double-free)". Completion contexts are
    /// already dropped per call, so here we drop the model before the backend it
    /// borrows from, rather than relying on struct field drop order at exit.
    fn shutdown(self: Box<Self>) {
        let Self { backend, model, .. } = *self;
        drop(model);
        drop(backend);
    }
}

pub fn terse_continuation_prompt(prefix: &str) -> String {
    format!("Complete this text inline. Return only the continuation.\nText: {prefix}")
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
    fn terse_prompt_keeps_prefix_configurable() {
        assert_eq!(
            terse_continuation_prompt("Dear team"),
            "Complete this text inline. Return only the continuation.\nText: Dear team"
        );
    }
}
