//! P2b: compare latency + inline autocomplete quality for base vs instruct GGUFs.
//!
//! This is a decision probe, not production model code. It intentionally feeds the
//! same raw left-context prefix to both models, because the product path is inline
//! continuation, not chat.
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Instant;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use spike::completion::{cap_words, trim_prefix};
use spike::context::left_context;

const N_TOKENS: usize = 12;

struct ModelSpec {
    label: &'static str,
    file: &'static str,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        label: "instruct",
        file: "qwen2.5-0.5b-instruct-q4_k_m.gguf",
    },
    ModelSpec {
        label: "base",
        file: "qwen2.5-0.5b-q4_k_m.gguf",
    },
];

struct PromptCase {
    name: &'static str,
    value: &'static str,
    max_words: usize,
}

struct PromptMode {
    name: &'static str,
    build: fn(&str) -> String,
}

fn raw_prefix(prefix: &str) -> String {
    prefix.to_string()
}

fn fim_empty_suffix(prefix: &str) -> String {
    format!("<|fim_prefix|>{prefix}<|fim_suffix|><|fim_middle|>")
}

fn terse_instruction(prefix: &str) -> String {
    format!("Complete this text inline. Return only the continuation.\nText: {prefix}")
}

const MODES: &[PromptMode] = &[
    PromptMode {
        name: "raw",
        build: raw_prefix,
    },
    PromptMode {
        name: "fim_empty_suffix",
        build: fim_empty_suffix,
    },
    PromptMode {
        name: "terse_instruction",
        build: terse_instruction,
    },
];

const CASES: &[PromptCase] = &[
    PromptCase {
        name: "email_followup",
        value: "Dear team, I wanted to ",
        max_words: 4,
    },
    PromptCase {
        name: "product_plan",
        value: "The next milestone should ",
        max_words: 5,
    },
    PromptCase {
        name: "bug_report",
        value: "When I click the button, ",
        max_words: 5,
    },
    PromptCase {
        name: "meeting_note",
        value: "The meeting starts at ",
        max_words: 4,
    },
    PromptCase {
        name: "code_comment",
        value: "Return early if the ",
        max_words: 5,
    },
    PromptCase {
        name: "unicode_note",
        value: "Please send résumé feedback to ",
        max_words: 5,
    },
];

struct CompletionTiming {
    raw: String,
    emitted_tokens: usize,
    context_init_ms: u128,
    prompt_eval_ms: u128,
    ttft_ms: u128,
    decode_ms: u128,
    total_ms: u128,
}

struct LlamaCompleter {
    backend: LlamaBackend,
    model: LlamaModel,
    ctx_params: LlamaContextParams,
}

impl LlamaCompleter {
    fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let backend = LlamaBackend::init()?;
        let model = LlamaModel::load_from_file(
            &backend,
            path,
            &LlamaModelParams::default().with_n_gpu_layers(999),
        )?;
        let ctx_params =
            LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(2048).unwrap()));
        Ok(Self {
            backend,
            model,
            ctx_params,
        })
    }

    fn complete_timed(&self, prompt: &str) -> CompletionTiming {
        let total_start = Instant::now();
        let context_start = Instant::now();
        let mut ctx = self
            .model
            .new_context(&self.backend, self.ctx_params.clone())
            .unwrap();
        let context_init_ms = context_start.elapsed().as_millis();

        let prompt_start = Instant::now();
        let toks = self.model.str_to_token(prompt, AddBos::Always).unwrap();
        let mut batch = LlamaBatch::new(toks.len().max(1), 1);
        let last = toks.len() - 1;
        for (i, tok) in toks.iter().enumerate() {
            batch.add(*tok, i as i32, &[0], i == last).unwrap();
        }
        ctx.decode(&mut batch).unwrap();
        let prompt_eval_ms = prompt_start.elapsed().as_millis();

        let mut sampler = LlamaSampler::greedy();
        let mut out = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let generation_start = Instant::now();
        let mut ttft_ms = None;
        let mut emitted_tokens = 0;

        let first_generated_pos = batch.n_tokens();
        for pos in first_generated_pos..first_generated_pos + N_TOKENS as i32 {
            let tok = sampler.sample(&ctx, batch.n_tokens() - 1);
            if self.model.is_eog_token(tok) {
                break;
            }
            sampler.accept(tok);
            if ttft_ms.is_none() {
                ttft_ms = Some(generation_start.elapsed().as_millis());
            }
            if let Ok(piece) = self.model.token_to_piece(tok, &mut decoder, true, None) {
                out.push_str(&piece);
            }
            emitted_tokens += 1;
            batch.clear();
            batch.add(tok, pos, &[0], true).unwrap();
            ctx.decode(&mut batch).unwrap();
        }

        CompletionTiming {
            raw: out,
            emitted_tokens,
            context_init_ms,
            prompt_eval_ms,
            ttft_ms: ttft_ms.unwrap_or_else(|| generation_start.elapsed().as_millis()),
            decode_ms: generation_start.elapsed().as_millis(),
            total_ms: total_start.elapsed().as_millis(),
        }
    }
}

fn escaped(s: &str) -> String {
    s.replace('\n', "\\n").replace('\t', "\\t")
}

fn model_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("models")
        .join(file)
}

fn quality_flags(raw: &str, capped: &str, prefix: &str) -> String {
    let mut flags = Vec::new();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        flags.push("empty");
    }
    if raw.contains("<|") || raw.contains("Complete this text") || raw.contains("Text:") {
        flags.push("prompt_leak");
    }
    if raw.contains('\u{fffd}')
        || raw
            .chars()
            .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
    {
        flags.push("garbage_unicode");
    }
    if raw.contains('\n') || raw.contains('>') {
        flags.push("chat_or_markdown");
    }
    let prefix_tail = prefix.split_whitespace().last().unwrap_or("");
    if !prefix_tail.is_empty() && capped.split_whitespace().any(|word| word == prefix_tail) {
        flags.push("prefix_repetition");
    }
    if flags.is_empty() {
        "ok".to_string()
    } else {
        flags.join("|")
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("backend=llama-cpp-2-metal host=Apple Silicon macOS");
    for spec in MODELS {
        let path = model_path(spec.file);
        if !Path::new(&path).exists() {
            return Err(format!("missing model {} at {}", spec.label, path.display()).into());
        }

        println!("=== model={} path={} ===", spec.label, path.display());
        let completer = LlamaCompleter::load(&path)?;
        let _ = completer.complete_timed("warm up");

        for mode in MODES {
            println!("--- mode={} ---", mode.name);
            for case in CASES {
                let caret = case.value.chars().count();
                let prefix = trim_prefix(&left_context(case.value, caret)).to_string();
                let prompt = (mode.build)(&prefix);
                let timing = completer.complete_timed(&prompt);
                let capped = cap_words(&timing.raw, case.max_words);
                let flags = quality_flags(&timing.raw, &capped, &prefix);
                println!(
                    "mode={} case={} context_init_ms={} prompt_eval_ms={} ttft_ms={} decode_ms={} total_ms={} emitted_tokens={} quality_flags={} prefix={:?} raw={:?} capped={:?}",
                    mode.name,
                    case.name,
                    timing.context_init_ms,
                    timing.prompt_eval_ms,
                    timing.ttft_ms,
                    timing.decode_ms,
                    timing.total_ms,
                    timing.emitted_tokens,
                    flags,
                    escaped(&prefix),
                    escaped(&timing.raw),
                    escaped(&capped)
                );
            }
        }
    }

    Ok(())
}
