//! P2: real `spike::completion::Completer` backed by llama.cpp + Metal.
//!
//! Proves the lib's `suggest()` pipeline (left-context -> trim -> complete -> cap)
//! runs end-to-end against the real Qwen2.5-0.5B GGUF, and reports warm latency.
//! The decode loop mirrors `tests/model_integration.rs` (the latency acceptance gate).
use std::num::NonZeroU32;
use std::time::Instant;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

use spike::completion::{suggest, Completer};

const MODEL: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";
/// Greedy-decode budget per completion (lib `cap_words` trims this to the visible suggestion).
const N_TOKENS: usize = 8;

/// Owns the backend + model; each `complete` spins a fresh context and greedy-decodes.
struct LlamaCompleter {
    backend: LlamaBackend,
    model: LlamaModel,
    ctx_params: LlamaContextParams,
}

impl LlamaCompleter {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let backend = LlamaBackend::init()?;
        let model = LlamaModel::load_from_file(
            &backend,
            MODEL,
            &LlamaModelParams::default().with_n_gpu_layers(999),
        )?;
        let ctx_params =
            LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(2048).unwrap()));
        Ok(Self { backend, model, ctx_params })
    }
}

impl Completer for LlamaCompleter {
    /// Greedy decode of up to `N_TOKENS` tokens, accumulating the generated text.
    fn complete(&self, prompt: &str) -> String {
        let mut ctx = self.model.new_context(&self.backend, self.ctx_params.clone()).unwrap();
        let toks = self.model.str_to_token(prompt, AddBos::Always).unwrap();
        let mut b = LlamaBatch::new(512, 1);
        let last = toks.len() - 1;
        for (i, t) in toks.iter().enumerate() {
            b.add(*t, i as i32, &[0], i == last).unwrap();
        }
        ctx.decode(&mut b).unwrap();
        let mut s = LlamaSampler::greedy();
        let mut cur = b.n_tokens();
        let mut out = String::new();
        for _ in 0..N_TOKENS {
            let tok = s.sample(&ctx, b.n_tokens() - 1);
            if self.model.is_eog_token(tok) {
                break;
            }
            if let Ok(piece) = self.model.token_to_str(tok, Special::Tokenize) {
                out.push_str(&piece);
            }
            b.clear();
            b.add(tok, cur, &[0], true).unwrap();
            cur += 1;
            ctx.decode(&mut b).unwrap();
        }
        out
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("loading {MODEL}");
    let completer = LlamaCompleter::load()?;

    // Warm-up: one throwaway decode so Metal shader compile is out of the timing path.
    let _ = completer.complete("warm up");

    // value="Dear team, I wanted to " (23 chars), capped to 4 words via the tested pipeline.
    let value = "Dear team, I wanted to ";
    let t0 = Instant::now();
    let completion = suggest(value, 23, &completer, 4);
    let warm_ms = t0.elapsed().as_millis();

    println!("prompt:     {value:?}");
    println!("completion: {completion:?}");
    println!("warm suggest() latency: {warm_ms}ms");
    Ok(())
}
