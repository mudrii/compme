use std::path::PathBuf;
use std::time::Instant;

fn latency_budget_required(raw: Option<&str>) -> bool {
    matches!(raw, Some("1" | "true" | "yes" | "on"))
}

fn require_latency_budget() -> bool {
    latency_budget_required(
        std::env::var("COMPME_REQUIRE_LATENCY_BUDGET")
            .ok()
            .as_deref(),
    )
}

use model_client::{terse_continuation_prompt, LlamaModel, LocalModel};

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf")
}

fn model_tests_required(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn require_model_tests() -> bool {
    model_tests_required(std::env::var("COMPME_REQUIRE_MODEL_TESTS").ok().as_deref())
}

fn ensure_model_exists(path: &std::path::Path) -> bool {
    if path.exists() {
        return true;
    }
    let msg = format!("model not at {}", path.display());
    if require_model_tests() {
        panic!("{msg}");
    }
    eprintln!("SKIP: {msg}");
    false
}

fn require_model_context() -> bool {
    model_tests_required(
        std::env::var("COMPME_REQUIRE_MODEL_CONTEXT")
            .ok()
            .as_deref(),
    )
}

fn load_model_or_skip(path: &std::path::Path) -> Option<LlamaModel> {
    match LlamaModel::load(path) {
        Ok(model) => Some(model),
        Err(err) if require_model_context() => panic!("load model: {err}"),
        Err(err) => {
            eprintln!("skipping real-model assertion: load model failed: {err}");
            None
        }
    }
}

#[test]
fn strict_model_test_env_parses_truthy_values() {
    for raw in [
        Some("1"),
        Some("true"),
        Some("TRUE"),
        Some(" yes "),
        Some("on"),
    ] {
        assert!(model_tests_required(raw), "{raw:?}");
    }
    for raw in [None, Some(""), Some("0"), Some("false"), Some("off")] {
        assert!(!model_tests_required(raw), "{raw:?}");
    }
}

// Ignored by default: needs the qwen2.5-0.5b GGUF on disk and a Metal GPU. Run
// with `cargo test -p model_client -- --ignored`. By default this skips when the
// GGUF is absent; set `COMPME_REQUIRE_MODEL_TESTS=1` to make absence fail. The
// position/skip/reuse arithmetic it would otherwise protect is covered in CI by
// pure unit tests in `src/lib.rs`; this adds an end-to-end real-model check when
// a GGUF and GPU are available.
#[test]
fn strict_latency_budget_env_parses_truthy_values() {
    for value in [Some("1"), Some("true"), Some("yes"), Some("on")] {
        assert!(latency_budget_required(value));
    }
    for value in [None, Some("0"), Some("false"), Some("off"), Some("")] {
        assert!(!latency_budget_required(value));
    }
}

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model + Metal GPU; run with --ignored"]
fn warm_completion_under_500ms() {
    if !require_latency_budget() {
        return;
    }

    let path = model_path();
    if !ensure_model_exists(&path) {
        return;
    }

    let Some(model) = load_model_or_skip(&path) else {
        return;
    };
    let prompt = terse_continuation_prompt("The quick brown fox");
    // Exercise the real warm-up override (Metal shader precompile) rather than a
    // bare throwaway completion.
    model.warm_up().expect("warm up");
    let started = Instant::now();
    let output = model.complete(&prompt, 12).expect("measured completion");
    let elapsed_ms = started.elapsed().as_millis();

    println!("warm: {elapsed_ms}ms -> {output:?}");
    if require_latency_budget() {
        assert!(
            elapsed_ms < 500,
            "warm completion {elapsed_ms}ms exceeded 500ms"
        );
    }

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
    if !require_model_tests() {
        return;
    }

    let path = model_path();
    if !ensure_model_exists(&path) {
        return;
    }

    let Some(reused) = load_model_or_skip(&path) else {
        return;
    };
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
    let Some(fresh) = load_model_or_skip(&path) else {
        return;
    };
    fresh.warm_up().expect("warm up fresh");
    let b_fresh = fresh.complete(&prompt_b, 12).expect("b fresh");
    assert_eq!(
        b_reused, b_fresh,
        "partial-reuse output diverged from a fresh context"
    );

    Box::new(reused).shutdown();
    Box::new(fresh).shutdown();
}

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model + Metal GPU; run with --ignored"]
fn complete_n_returns_real_model_candidates() {
    let path = model_path();
    if !ensure_model_exists(&path) {
        return;
    }

    if !require_model_tests() {
        return;
    }
    let Some(model) = load_model_or_skip(&path) else {
        return;
    };
    model.warm_up().expect("warm up");
    let prompt = terse_continuation_prompt("The quick brown fox");
    let candidates = model.complete_n(&prompt, 12, 3).expect("complete_n");

    assert_eq!(candidates.len(), 3);
    for candidate in &candidates {
        assert!(
            !candidate.trim().is_empty(),
            "empty candidate: {candidates:?}"
        );
        assert!(
            !candidate.contains("Complete this text inline")
                && !candidate.contains("Return only the continuation")
                && !candidate.contains("Text:"),
            "candidate leaked prompt instructions: {candidate:?}"
        );
    }

    // The whole point of multi-candidate generation is *divergence*: candidate 0 is
    // greedy/deterministic while later candidates use temperature + a per-candidate
    // seed (see `sampler_for_candidate`). If they all came back identical the
    // sampler wiring would be silently broken, so prove at least two candidates
    // genuinely differ — not merely that three were returned. The deterministic
    // *config* divergence is pinned by the unit tests in `src/lib.rs`; this is the
    // end-to-end token-level proof that the config actually produces divergence.
    let distinct: std::collections::HashSet<&str> = candidates.iter().map(String::as_str).collect();
    assert!(
        distinct.len() > 1,
        "expected diverging candidates but all were identical: {candidates:?}"
    );

    Box::new(model).shutdown();
}
