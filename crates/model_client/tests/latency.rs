use std::path::PathBuf;
use std::time::Instant;

use model_client::{terse_continuation_prompt, LlamaModel, LocalModel};

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf")
}

// Ignored by default: needs the qwen2.5-0.5b GGUF on disk and a Metal GPU. Run
// with `cargo test -p model_client -- --ignored`. NOTE: even under `--ignored`
// this SKIPs (and passes) when the GGUF is absent, so it is NOT a CI guard. The
// position/skip/reuse arithmetic it would otherwise protect is covered in CI by
// the pure `plan_decode`/`reusable_prefix_len`/`prompt_tokens_to_skip` unit tests
// in `src/lib.rs`; this test adds an end-to-end real-model check when a GGUF and
// GPU are available.
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

// Guards G3 prefix-KV-cache reuse: the persistent context must produce the
// *same* deterministic (greedy) output as a fresh context, across a sequence of
// completions that share prefixes. A corrupt reuse (wrong seq_rm / position math)
// would diverge here. Ignored by default — needs the GGUF + Metal.
#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model + Metal GPU; run with --ignored"]
fn prefix_reuse_matches_fresh_context_output() {
    let path = model_path();
    if !path.exists() {
        eprintln!("SKIP: model not at {}", path.display());
        return;
    }

    let reused = LlamaModel::load(&path).expect("load model");
    reused.warm_up().expect("warm up");

    let prompt_a = terse_continuation_prompt("The quick brown fox");
    let prompt_b = terse_continuation_prompt("The quick brown fox jumps");

    // Same prompt twice exercises the identical-prompt reuse path; the second
    // result must equal the first (greedy → deterministic).
    let a1 = reused.complete(&prompt_a, 12).expect("a1");
    let a2 = reused.complete(&prompt_a, 12).expect("a2 (reuse)");
    assert_eq!(a1, a2, "identical-prompt reuse changed the output");

    // A shared-prefix prompt exercises partial reuse + a divergent tail.
    let b_reused = reused.complete(&prompt_b, 12).expect("b reuse");
    // Back to A exercises shrinking the cached prefix again.
    let a3 = reused.complete(&prompt_a, 12).expect("a3 (reuse after b)");
    assert_eq!(
        a1, a3,
        "reuse after a divergent prompt corrupted the output"
    );

    // Compare the partial-reuse result against a fresh, never-reused context.
    let fresh = LlamaModel::load(&path).expect("load fresh model");
    fresh.warm_up().expect("warm up fresh");
    let b_fresh = fresh.complete(&prompt_b, 12).expect("b fresh");
    assert_eq!(
        b_reused, b_fresh,
        "partial-reuse output diverged from a fresh context"
    );

    Box::new(reused).shutdown();
    Box::new(fresh).shutdown();
}
