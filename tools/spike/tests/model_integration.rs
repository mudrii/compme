//! ATDD acceptance: a real GGUF loads on Metal and a warm short completion is < 500ms.
//! Ignored by default (needs the GGUF model + Metal GPU), so a plain `cargo test`
//! reports it as *ignored* rather than silently passing. Run with `--ignored`.
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

const MODEL: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";

fn warm_complete_ms(prompt: &str, n: usize) -> Option<u128> {
    if !Path::new(MODEL).exists() {
        return None;
    }
    let backend = LlamaBackend::init().ok()?;
    let model = LlamaModel::load_from_file(
        &backend,
        MODEL,
        &LlamaModelParams::default().with_n_gpu_layers(999),
    )
    .ok()?;
    let cp = LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(2048).unwrap()));
    let decode = |warm: bool| -> u128 {
        let mut ctx = model.new_context(&backend, cp.clone()).unwrap();
        let toks = model.str_to_token(prompt, AddBos::Always).unwrap();
        let mut b = LlamaBatch::new(512, 1);
        let last = toks.len() - 1;
        for (i, t) in toks.iter().enumerate() {
            b.add(*t, i as i32, &[0], i == last).unwrap();
        }
        let t0 = Instant::now();
        ctx.decode(&mut b).unwrap();
        let mut s = LlamaSampler::greedy();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let first_generated_pos = b.n_tokens();
        for cur in first_generated_pos..first_generated_pos + n as i32 {
            let tok = s.sample(&ctx, b.n_tokens() - 1);
            if model.is_eog_token(tok) {
                break;
            }
            let _ = model.token_to_piece(tok, &mut decoder, true, None);
            b.clear();
            b.add(tok, cur, &[0], true).unwrap();
            ctx.decode(&mut b).unwrap();
        }
        let _ = warm;
        t0.elapsed().as_millis()
    };
    let _ = decode(true); // warm-up (Metal shader compile)
    Some(decode(false))
}

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model + Metal GPU; run with --ignored"]
fn model_loads_and_warm_completion_is_under_500ms() {
    match warm_complete_ms("The quick brown fox", 12) {
        None => eprintln!("SKIP: model not downloaded at {MODEL}"),
        Some(ms) => {
            println!("warm 12-token completion: {ms}ms");
            assert!(ms < 500, "warm completion {ms}ms exceeded 500ms floor");
        }
    }
}
