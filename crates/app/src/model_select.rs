//! Model selection: a deterministic `StubModel` for gates, or the real
//! `LlamaModel` for production.
//!
//! The product binary is identical in both cases — only the `Box<dyn LocalModel>`
//! differs. The E2E live gate sets `COMPME_STUB_COMPLETION` so the whole
//! focus→read→ghost→accept→insert pipeline is asserted with a fixed completion,
//! while a real run loads the GGUF.

use std::path::PathBuf;

use model_client::{LlamaModel, LocalModel, LocalModelResult};

/// A model that returns a fixed completion regardless of prompt. Used by the
/// deterministic E2E gate so the wiring is provable without model nondeterminism.
pub struct StubModel {
    completion: String,
}

impl StubModel {
    pub fn new(completion: impl Into<String>) -> Self {
        Self {
            completion: completion.into(),
        }
    }
}

impl LocalModel for StubModel {
    fn complete(&self, _prompt: &str, _max_tokens: usize) -> LocalModelResult<String> {
        Ok(self.completion.clone())
    }
}

/// Which backend to construct. Resolved purely from config so it is unit-testable
/// without touching the filesystem or loading a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSource {
    Stub(String),
    Llama(PathBuf),
}

/// How the engine's raw left-context prompt is shaped before it reaches the
/// model. `Terse` is the documented A1a development default (wraps the prefix in
/// the continuation instruction); `Raw` passes the prefix through unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptMode {
    Terse,
    Raw,
}

/// Resolve the prompt strategy from config. Default is `Terse` (the A1a default);
/// `COMPME_PROMPT_MODE=raw` opts out. Keeping this configurable satisfies the
/// contract requirement that prompt strategy stay configurable, not hardcoded.
pub fn resolve_prompt_mode(raw: Option<String>) -> PromptMode {
    match raw.as_deref() {
        Some("raw") => PromptMode::Raw,
        _ => PromptMode::Terse,
    }
}

/// Apply the prompt mode to the engine's raw left-context prefix, prepending the
/// personalization steering `preamble` (empty when personalization is off or has
/// nothing to steer with — see `personalization::PersonalizationProfile`).
pub fn shape_prompt(mode: PromptMode, preamble: &str, prefix: &str) -> String {
    let body = match mode {
        PromptMode::Terse => model_client::terse_continuation_prompt(prefix),
        PromptMode::Raw => prefix.to_string(),
    };
    if preamble.is_empty() {
        body
    } else {
        format!("{preamble}{body}")
    }
}

/// Stub completion (when set) always wins so gates stay deterministic; otherwise
/// load the real model from `model_path`.
pub fn resolve_source(stub_completion: Option<String>, model_path: PathBuf) -> ModelSource {
    match stub_completion {
        Some(text) => ModelSource::Stub(text),
        None => ModelSource::Llama(model_path),
    }
}

/// Construct the boxed model for a resolved source. The Llama path performs real
/// I/O and is exercised by the manual real-model gate, not unit tests.
pub fn load_model(source: ModelSource) -> Result<Box<dyn LocalModel>, String> {
    match source {
        ModelSource::Stub(text) => Ok(Box::new(StubModel::new(text))),
        ModelSource::Llama(path) => {
            if !path.is_file() {
                return Err(format!("model file not found: {}", path.display()));
            }
            LlamaModel::load(&path)
                .map(|model| Box::new(model) as Box<dyn LocalModel>)
                .map_err(|err| format!("load model {}: {err}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_fixed_completion_ignoring_prompt() {
        let stub = StubModel::new(" - the rest");
        assert_eq!(stub.complete("anything", 4).unwrap(), " - the rest");
        assert_eq!(
            stub.complete("different prompt", 99).unwrap(),
            " - the rest"
        );
    }

    #[test]
    fn stub_warm_up_is_ok() {
        let stub = StubModel::new("x");
        assert!(stub.warm_up().is_ok());
    }

    #[test]
    fn resolve_prefers_stub_when_present() {
        let source = resolve_source(Some("hi".into()), PathBuf::from("/models/m.gguf"));
        assert_eq!(source, ModelSource::Stub("hi".into()));
    }

    #[test]
    fn resolve_uses_llama_path_without_stub() {
        let source = resolve_source(None, PathBuf::from("/models/m.gguf"));
        assert_eq!(source, ModelSource::Llama(PathBuf::from("/models/m.gguf")));
    }

    #[test]
    fn load_model_builds_working_stub() {
        let model = load_model(ModelSource::Stub("done".into())).unwrap();
        assert_eq!(model.complete("p", 4).unwrap(), "done");
    }

    #[test]
    fn load_model_rejects_missing_llama_path_before_backend_spawn() {
        let missing = PathBuf::from("/definitely/not/a/compme/model.gguf");
        let err = match load_model(ModelSource::Llama(missing.clone())) {
            Ok(_) => panic!("missing model path should fail before backend spawn"),
            Err(err) => err,
        };
        assert_eq!(err, format!("model file not found: {}", missing.display()));
    }

    #[test]
    fn prompt_mode_defaults_to_terse() {
        assert_eq!(resolve_prompt_mode(None), PromptMode::Terse);
        assert_eq!(resolve_prompt_mode(Some("terse".into())), PromptMode::Terse);
        assert_eq!(
            resolve_prompt_mode(Some("anything".into())),
            PromptMode::Terse
        );
    }

    #[test]
    fn prompt_mode_raw_opts_out() {
        assert_eq!(resolve_prompt_mode(Some("raw".into())), PromptMode::Raw);
    }

    #[test]
    fn terse_mode_wraps_prefix_raw_mode_passes_through() {
        let raw = shape_prompt(PromptMode::Raw, "", "Dear team");
        assert_eq!(raw, "Dear team");

        let terse = shape_prompt(PromptMode::Terse, "", "Dear team");
        // Behavioural check: terse wraps the prefix (contains it, differs from
        // raw) without pinning the template's exact prose — that literal lives in
        // its owner, `model_client::terse_continuation_prompt`'s own test.
        assert!(terse.contains("Dear team"));
        assert_ne!(terse, "Dear team");
    }

    #[test]
    fn shape_prompt_prepends_a_nonempty_preamble() {
        let out = shape_prompt(PromptMode::Raw, "STEER\n", "hello");
        assert_eq!(out, "STEER\nhello");
    }

    #[test]
    fn shape_prompt_preamble_precedes_the_terse_body() {
        let out = shape_prompt(PromptMode::Terse, "STEER\n", "hello");
        assert!(out.starts_with("STEER\n"));
        assert!(out.contains("hello"));
    }
}
