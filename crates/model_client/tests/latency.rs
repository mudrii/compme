use std::path::PathBuf;
use std::time::Instant;

use model_client::{terse_continuation_prompt, LlamaModel, LocalModel};

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf")
}

#[test]
fn warm_completion_under_500ms() {
    let path = model_path();
    if !path.exists() {
        eprintln!("SKIP: model not at {}", path.display());
        return;
    }

    let model = LlamaModel::load(&path).expect("load model");
    let prompt = terse_continuation_prompt("The quick brown fox");
    let _ = model.complete(&prompt, 12).expect("warm completion");

    let started = Instant::now();
    let output = model.complete(&prompt, 12).expect("measured completion");
    let elapsed_ms = started.elapsed().as_millis();

    println!("warm: {elapsed_ms}ms -> {output:?}");
    assert!(
        elapsed_ms < 500,
        "warm completion {elapsed_ms}ms exceeded 500ms"
    );
}
