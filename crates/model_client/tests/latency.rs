use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    grammar_fix_prompt, terse_continuation_prompt, LlamaModel, LocalModel, LocalModelErrorKind,
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

fn corpus_path() -> PathBuf {
    // COMPME_QUALITY_CORPUS overrides the in-repo corpus; relative paths
    // resolve against the repo root, mirroring tests/quality.rs.
    if let Ok(raw) = std::env::var("COMPME_QUALITY_CORPUS") {
        if !raw.trim().is_empty() {
            let path = PathBuf::from(raw.trim());
            if path.is_absolute() {
                return path;
            }
            return PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/release/quality-corpus.jsonl")
}

/// Decode the string value of `"key": "..."` in a flat JSON line fragment.
/// Deliberately small: the corpus is authored in-repo, so the common escapes
/// suffice and an unterminated or unsupported value simply yields no case.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\": \"");
    let after = &text[text.find(&pattern)? + pattern.len()..];
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                _ => return None,
            },
            _ => out.push(ch),
        }
    }
    None
}

/// The probe's grammar typo battery as `(typo, want)` pairs: corpus cases on
/// the grammar path whose `single_word_vetted` expect carries a target value.
/// The corpus JSONL is the canonical case list; this line-splitting loader
/// keeps the probe free of a duplicate inline table without pulling the
/// strict corpus parser out of tests/quality.rs (a separate test target).
fn grammar_typo_cases(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|line| line.contains("\"path\": \"grammar\""))
        .filter(|line| line.contains("\"type\": \"single_word_vetted\""))
        .filter_map(|line| {
            let word = json_string_field(line, "word")?;
            let expect = line.split_once("\"expect\": {")?.1;
            let value = json_string_field(expect, "value")?;
            Some((word, value))
        })
        .collect()
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

// The binary never calls `complete` for completions: `crates/app/src/inference.rs`
// goes through `complete_n` with `DEFAULT_CANDIDATES = 1`. Mirror the budget above
// on that path so the 500ms gate measures what ships.
#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
fn warm_complete_n_under_500ms() {
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
    model.warm_up().expect("warm up");
    let started = Instant::now();
    let candidates = model
        .complete_n(&prompt, 12, 1)
        .expect("measured complete_n");
    let elapsed_ms = started.elapsed().as_millis();

    println!("warm complete_n: {elapsed_ms}ms -> {candidates:?}");
    assert_eq!(candidates.len(), 1);
    assert!(
        elapsed_ms < 500,
        "warm complete_n {elapsed_ms}ms exceeded 500ms"
    );

    Box::new(model).shutdown();
}

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates run CPU and a macOS model gate must also run Metal; run with --ignored"]
fn long_generation_observes_shutdown_within_250ms() {
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
    let model = Arc::new(model);
    model.warm_up().expect("warm up");
    let cancellation = model
        .shutdown_cancellation()
        .expect("llama model exposes shutdown cancellation");
    let native_polls_before = cancellation.native_decode_poll_count();
    let worker_model = Arc::clone(&model);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = worker_model.complete(
            &terse_continuation_prompt("Write a detailed multi-paragraph account of"),
            1024,
        );
        let _ = done_tx.send(result);
    });

    let enter_deadline = Instant::now() + Duration::from_secs(2);
    while cancellation.native_decode_poll_count() == native_polls_before {
        assert!(
            Instant::now() < enter_deadline,
            "real generation never entered decode"
        );
        std::thread::yield_now();
    }
    let started = Instant::now();
    cancellation.request();
    let error = done_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("real generation ignored shutdown for more than 250 ms")
        .expect_err("long generation finished instead of observing shutdown");
    assert_eq!(error.kind(), LocalModelErrorKind::ShutdownRequested);
    assert!(started.elapsed() < Duration::from_millis(250));
    worker.join().unwrap();

    let model =
        Arc::try_unwrap(model).unwrap_or_else(|_| panic!("test owns the only model reference"));
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

    const CHILD_MARKER: &str = "COMPME_LONG_PROMPT_TEST_COMPLETED";
    if std::env::var_os("COMPME_LONG_PROMPT_TEST_CHILD").is_some() {
        let reused = load_model_or_skip(&path).expect("required long-prompt model context");
        reused.warm_up().expect("warm up long-prompt model");

        // The same 3,000-repeat prompt aborts unchunked llama.cpp at its native
        // 2,048-token batch assertion, even after warm-up/prefix reuse. With the
        // forced 4,096-token context it therefore proves both a full first batch
        // and a non-empty partial final chunk. Repeating the completion then takes
        // the identical-prefix KV reuse path.
        let long_prompt = " x".repeat(3_000);
        let long_first = reused.complete(&long_prompt, 1).expect("long prompt");
        let long_reused = reused
            .complete(&long_prompt, 1)
            .expect("long prompt with prefix reuse");
        assert_eq!(
            long_first, long_reused,
            "chunked prompt decode changed deterministic prefix-reuse output"
        );
        Box::new(reused).shutdown();
        println!("{CHILD_MARKER}");
        return;
    }

    // llama.cpp aborts the process when a decode exceeds n_batch, so exercise
    // the long-prompt path in a subprocess. The parent can then report a normal
    // test failure instead of taking down the entire model-gate test binary.
    {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary path"))
            .args([
                "--ignored",
                "--exact",
                "prefix_reuse_matches_fresh_context_output",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("COMPME_LONG_PROMPT_TEST_CHILD", "1")
            .env("COMPME_REQUIRE_MODEL_TESTS", "1")
            .env("COMPME_REQUIRE_MODEL_CONTEXT", "1")
            .env("COMPME_MODEL_CONTEXT_TOKENS", "4096")
            .output()
            .expect("run long-prompt model subprocess");
        assert!(
            output.status.success(),
            "long-prompt model subprocess failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(CHILD_MARKER),
            "long-prompt subprocess exited without completion marker\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
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

// Guards prefix-KV reuse on the *production* completion path. The app always calls
// `complete_n` (`crates/app/src/inference.rs`, `DEFAULT_CANDIDATES = 1`), and
// `complete_candidates_on_worker` used to clear `prev_tokens` before candidate 0 too,
// so every debounce re-decoded the whole prompt. Measured on the real model with a
// ~300-token prompt: `complete` 14413ms then 731ms, but `complete_n` 14300ms then
// 14046ms — no reuse at all. Candidate 0 must take the same reuse path `complete`
// does and must still return byte-identical greedy output.
//
// The reuse observable is `last_prompt_tokens_decoded`, not wall-clock time. Two
// earlier assertion shapes both proved host-dependent:
//
// 1. A cold/warm timing ratio. On the paravirtualised macOS CI runner a generated
//    token costs roughly as much as a prompt batch, so even a *working* cache
//    measures only ~1.2x there (the same run on Linux measures ~22x). A ratio
//    assertion fails on the mac lane with the fix in place.
// 2. Greedy-output equality on a degenerate prompt ("the quick brown fox jumps "
//    x60). The greedy next token there is a near-tie, and the argmax flips
//    between the two decode *shapes*: a batched full-prompt decode yields
//    "1. The quick…" while the single-token reuse decode yields " the quick
//    brown…" — deterministically on every run, for `complete` and `complete_n`
//    alike. That is backend numerics on tied logits, not a cache-trim bug: on
//    near-natural prose fresh, reuse, and `complete_n` outputs are identical,
//    which is why the equality assertions below use a natural paragraph.
#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
fn complete_n_reuses_prompt_prefix_across_requests() {
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

    // Natural, non-repetitive prose (~300 tokens over three paragraphs) so the
    // greedy continuation is far from a logit tie and the equality assertions
    // below — which compare a batched full-prompt decode against a one-token
    // reuse decode — hold. See the shape/numerics comment above.
    let paragraph = "The committee met on Tuesday to review the quarterly report. \
Revenue grew across the third quarter on strong renewals and a few new \
enterprise accounts, while operating costs held close to the budget the board \
approved in the spring. Several members asked whether the hiring pause would \
extend into the next fiscal year, and the chair promised a detailed forecast \
before the next meeting. ";
    let prompt = terse_continuation_prompt(&paragraph.repeat(3));

    let started = Instant::now();
    let cold = model.complete_n(&prompt, 8, 1).expect("cold complete_n");
    let cold_ms = started.elapsed().as_millis();
    let cold_decoded = model.last_prompt_tokens_decoded();

    let started = Instant::now();
    let warm = model.complete_n(&prompt, 8, 1).expect("warm complete_n");
    let warm_ms = started.elapsed().as_millis();
    let warm_decoded = model.last_prompt_tokens_decoded();

    // Deterministic reuse observable, immune to host timing: only the warm-up
    // tokens precede the first call, so the bulk of the prompt is decoded live.
    assert!(
        cold_decoded > 1,
        "first complete_n decoded only {cold_decoded} prompt tokens; \
         expected the full prompt (nothing reusable yet)"
    );
    // Second identical call: the cache already holds the whole prompt, so
    // exactly the one mandatory fresh-logits token is decoded. The pre-fix code
    // cleared `prev_tokens` before candidate 0 and re-decoded the entire prompt
    // here (~250 tokens), which is the regression this pins.
    assert_eq!(
        warm_decoded, 1,
        "second identical complete_n re-decoded {warm_decoded} prompt tokens; \
         prefix-KV reuse must leave exactly the one mandatory fresh-logits decode"
    );

    // Correctness: candidate 0 is greedy, so a reuse that corrupted the KV
    // diverges here instead of only showing up as latency. Held on natural
    // prose; see the numerics comment above for why degenerate repeated text
    // cannot carry this comparison.
    let single = model.complete(&prompt, 8).expect("complete");
    assert_eq!(
        model.last_prompt_tokens_decoded(),
        1,
        "complete must reuse the cached prefix exactly like complete_n"
    );
    assert_eq!(cold, warm, "prefix reuse changed complete_n output");
    assert_eq!(
        warm[0], single,
        "complete_n candidate 0 diverged from complete on the same prompt"
    );

    // Diagnostic only: the ratio is host-dependent (see the comment above), so
    // it is printed for triage but never asserted.
    println!(
        "complete_n cold {cold_ms}ms / warm {warm_ms}ms = {:.1}x; \
         prompt tokens decoded cold {cold_decoded} / warm {warm_decoded}",
        cold_ms as f64 / warm_ms.max(1) as f64
    );

    Box::new(model).shutdown();
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

    // The typo battery comes from the canonical quality corpus (the same
    // JSONL the corpus quality gate parses), not an inline duplicate.
    let corpus_path = corpus_path();
    let corpus_text = std::fs::read_to_string(&corpus_path)
        .unwrap_or_else(|err| panic!("read corpus {}: {err}", corpus_path.display()));
    let typos = grammar_typo_cases(&corpus_text);
    assert!(
        !typos.is_empty(),
        "corpus {} has no grammar typo cases",
        corpus_path.display()
    );
    let mut fixed = 0;
    for (typo, want) in &typos {
        let t0 = Instant::now();
        let raw = model
            .complete(
                &grammar_fix_prompt(typo, "I wrote"),
                GRAMMAR_GENERATION_TOKENS,
            )
            .expect("grammar completion");
        let vetted = vet_correction(typo, &raw);
        let ok = vetted.as_deref() == Some(want.as_str());
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

// Model-free branch-CI guard for the corpus loader: the shipped corpus must
// keep supplying the grammar typo battery (a malformed or moved corpus fails
// here, not only in the #[ignore]d probe).
#[test]
fn repo_corpus_supplies_grammar_typo_cases() {
    let path = corpus_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read corpus {}: {err}", path.display()));
    let cases = grammar_typo_cases(&text);
    assert!(
        cases.len() >= 8,
        "corpus {} has too few grammar typo cases: {cases:?}",
        path.display()
    );
    assert!(
        cases
            .iter()
            .any(|(typo, want)| typo == "teh" && want == "the"),
        "corpus {} lost the canonical teh -> the case: {cases:?}",
        path.display()
    );
}
