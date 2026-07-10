use std::path::PathBuf;
use std::time::Instant;

/// True for an explicit truthy env value (trimmed, case-insensitive). Shared by
/// every `COMPME_REQUIRE_*` gate below so they parse identically.
fn env_flag_truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn require_latency_budget() -> bool {
    env_flag_truthy(
        std::env::var("COMPME_REQUIRE_LATENCY_BUDGET")
            .ok()
            .as_deref(),
    )
}

use grammar::vet_correction;
use model_client::{
    grammar_fix_prompt, terse_continuation_prompt, LlamaModel, LocalModel,
    GRAMMAR_GENERATION_TOKENS,
};

fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf")
}

fn require_model_tests() -> bool {
    env_flag_truthy(std::env::var("COMPME_REQUIRE_MODEL_TESTS").ok().as_deref())
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
    env_flag_truthy(
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

// Pure env-parsing guard (no GGUF/GPU needed). The latency-budget gate must arm
// only on an explicit truthy COMPME_REQUIRE_LATENCY_BUDGET and stay OFF for
// absent/empty/falsy values, so a normal `cargo test` run never enforces the
// 500ms budget. (The real end-to-end 500ms check lives in
// `warm_completion_under_500ms`, which is #[ignore]'d and needs a GGUF. Release
// gates force the root model-client test through CPU with COMPME_MODEL_GPU_LAYERS=0.)
#[test]
fn strict_latency_budget_env_parses_truthy_values() {
    for value in [
        Some("1"),
        Some("true"),
        Some("TRUE"),
        Some(" yes "),
        Some("on"),
    ] {
        assert!(env_flag_truthy(value));
    }
    for value in [
        None,
        Some("0"),
        Some("false"),
        Some("FALSE"),
        Some("no"),
        Some(" No "),
        Some("off"),
        Some(" off "),
        Some("maybe"),
        Some(""),
    ] {
        assert!(!env_flag_truthy(value));
    }
}

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
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
// would diverge here. Ignored by default — needs the GGUF. Release gates force
// the root model-client test through CPU with COMPME_MODEL_GPU_LAYERS=0.
#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
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
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
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

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
fn grammar_fix_real_model_output_is_vetted() {
    if !require_model_tests() {
        return;
    }

    let path = model_path();
    if !ensure_model_exists(&path) {
        return;
    }

    let Some(model) = load_model_or_skip(&path) else {
        return;
    };
    model.warm_up().expect("warm up");
    let prompt = grammar_fix_prompt("teh", "Please fix");
    let raw = model
        .complete(&prompt, GRAMMAR_GENERATION_TOKENS)
        .expect("grammar fix");
    let vetted = vet_correction("teh", &raw);
    // Diagnostic for live-quality triage (2026-07-07 assisted-UI session found
    // corrections never surviving vetting with the default model): show what
    // the model actually said.
    eprintln!("grammar raw={raw:?} vetted={vetted:?}");
    assert!(
        !raw.trim().is_empty(),
        "real model grammar prompt produced no output"
    );
    let correction =
        vetted.expect("real model grammar prompt must produce a usable vetted correction");
    assert_eq!(
        correction, "the",
        "expected the default release model to correct the public typo"
    );
    assert!(
        correction.is_ascii() && !correction.contains(char::is_whitespace),
        "accepted correction must be a single ASCII token: {correction:?}"
    );

    Box::new(model).shutdown();
}

#[test]
#[ignore = "diagnostic quality probe; needs a local GGUF — run with --ignored --nocapture and COMPME_QUALITY_MODEL_PATH"]
fn model_quality_probe() {
    // Per-model quality battery over the PRODUCT prompt/vet paths, for
    // comparing catalog models. Prints a grid; asserts only that the model
    // loads and speaks. Point COMPME_QUALITY_MODEL_PATH at any GGUF.
    let path = std::env::var("COMPME_QUALITY_MODEL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| model_path());
    if !ensure_model_exists(&path) {
        return;
    }
    let Some(model) = load_model_or_skip(&path) else {
        return;
    };
    model.warm_up().expect("warm up");
    eprintln!("== model: {} ==", path.display());

    let typos = [
        ("teh", "the"),
        ("recieve", "receive"),
        ("adress", "address"),
        ("definately", "definitely"),
        ("wierd", "weird"),
        ("occured", "occurred"),
        ("seperate", "separate"),
        ("beleive", "believe"),
    ];
    let mut fixed = 0;
    for (typo, want) in typos {
        let t0 = Instant::now();
        let raw = model
            .complete(
                &grammar_fix_prompt(typo, "I wrote"),
                GRAMMAR_GENERATION_TOKENS,
            )
            .expect("grammar completion");
        let vetted = vet_correction(typo, &raw);
        let ok = vetted.as_deref() == Some(want);
        fixed += ok as u32;
        eprintln!(
            "grammar {typo:>11} -> want {want:<11} got {:<11} raw {raw:?} ({} ms) {}",
            vetted.as_deref().unwrap_or("-"),
            t0.elapsed().as_millis(),
            if ok { "PASS" } else { "MISS" }
        );
    }
    let mut false_fixes = 0;
    for word in ["the", "receive", "weather", "morning"] {
        let raw = model
            .complete(
                &grammar_fix_prompt(word, "I wrote"),
                GRAMMAR_GENERATION_TOKENS,
            )
            .expect("grammar completion");
        let vetted = vet_correction(word, &raw);
        if let Some(bad) = &vetted {
            false_fixes += 1;
            eprintln!("grammar {word:>11} -> FALSE-FIX {bad:?} raw {raw:?}");
        }
    }
    eprintln!(
        "grammar score: {fixed}/{} fixed, {false_fixes}/4 false-fixes",
        typos.len()
    );
    if require_model_tests() {
        assert!(
            fixed >= 7,
            "strict quality probe expected at least 7/{} typo fixes, got {fixed}",
            typos.len()
        );
        assert_eq!(
            false_fixes, 0,
            "strict quality probe must not alter already-correct words"
        );
    }

    let prompts = [
        "Dear team, I wanted to",
        "The meeting is scheduled for",
        "Thanks for your email. I will",
        "The quarterly results show that",
    ];
    for prefix in prompts {
        let t0 = Instant::now();
        let raw = model
            .complete(&terse_continuation_prompt(prefix), 24)
            .expect("terse completion");
        let terse_ms = t0.elapsed().as_millis();
        let t1 = Instant::now();
        let raw_prefix = model.complete(prefix, 24).expect("raw completion");
        eprintln!(
            "completion {prefix:?}\n  terse ({terse_ms} ms): {raw:?}\n  raw   ({} ms): {raw_prefix:?}",
            t1.elapsed().as_millis()
        );
        assert!(
            !raw.trim().is_empty() || !raw_prefix.trim().is_empty(),
            "model produced no completion for {prefix:?}"
        );
    }

    Box::new(model).shutdown();
}
