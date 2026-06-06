//! Model selection: a deterministic `StubModel` for gates, or the real
//! `LlamaModel` for production.
//!
//! The product binary is identical in both cases — only the `Box<dyn LocalModel>`
//! differs. The E2E live gate sets `COMPLETE_ME_STUB_COMPLETION` so the whole
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
        ModelSource::Llama(path) => LlamaModel::load(&path)
            .map(|model| Box::new(model) as Box<dyn LocalModel>)
            .map_err(|err| format!("load model {}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_fixed_completion_ignoring_prompt() {
        let stub = StubModel::new(" - the rest");
        assert_eq!(stub.complete("anything", 4).unwrap(), " - the rest");
        assert_eq!(stub.complete("different prompt", 99).unwrap(), " - the rest");
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
}
