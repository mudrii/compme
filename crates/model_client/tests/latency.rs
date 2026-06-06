use std::path::PathBuf;
use std::time::Instant;

use model_client::{terse_continuation_prompt, LlamaModel, LocalModel};

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf")
}

// Ignored by default: needs the qwen2.5-0.5b GGUF on disk and a Metal GPU, so a
// plain `cargo test` reports it as *ignored* rather than silently passing. Run
// the real ATDD with `cargo test -p model_client -- --ignored`.
#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model + Metal GPU; run with --ignored"]
fn warm_completion_under_500ms() {
    let path = model_path();
    if !path.exists() {
        eprintln!("SKIP: model not at {}", path.display());
        return;
    }

    let model = LlamaModel::load(&path).expect("load model");
    let prompt = terse_continuation_prompt("The quick brown fox");
    // Exercise the real warm-up override (Metal shader precompile) rather than a
    // bare throwaway completion.
    model.warm_up().expect("warm up");

    let started = Instant::now();
    let output = model.complete(&prompt, 12).expect("measured completion");
    let elapsed_ms = started.elapsed().as_millis();

    println!("warm: {elapsed_ms}ms -> {output:?}");
    assert!(
        elapsed_ms < 500,
        "warm completion {elapsed_ms}ms exceeded 500ms"
    );

    // Exercise the real shutdown override (model dropped before backend).
    Box::new(model).shutdown();
}
