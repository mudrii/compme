//! Corpus-driven model-quality regression gate (release tooling, not per-push CI).
//!
//! Runs `tools/release/quality-corpus.jsonl` against the pinned GGUF through the
//! real product paths (`terse_continuation_prompt`, `grammar_fix_prompt` +
//! `grammar::vet_correction`) and fails when the pass rate drops below the fixed
//! threshold. Driven by `tools/release/check-quality.sh`; follows the
//! `tests/latency.rs` conventions (skips cleanly without a local GGUF, activates
//! on `COMPME_REQUIRE_MODEL_TESTS=1`). Determinism comes for free: `complete()`
//! decodes with the greedy seedless sampler (`sampler_for_candidate(0)`), so the
//! corpus can assert concrete outcomes while the 80% threshold absorbs minor
//! cross-backend variation.
//!
//! The corpus parser is a deliberately small strict JSON subset (flat objects,
//! string/non-negative-integer values) so the gate adds no new dependencies.
//! Malformed lines, unknown fields, and unknown expect types are loud errors,
//! never skips. The schema's `regex` expect type is intentionally not
//! implemented — a regex engine needs a dependency this harness refuses to add;
//! use `contains`/`not_contains` instead.

use std::path::{Path, PathBuf};

use grammar::vet_correction;
use model_client::{
    grammar_fix_prompt, terse_continuation_prompt, LlamaModel, LocalModel,
    GRAMMAR_GENERATION_TOKENS,
};

/// Pass threshold from the quality-gate plan: catches catastrophic drift, does
/// not grade nuance. 80% of 21 cases = 17 must pass.
const PASS_THRESHOLD_PERCENT: usize = 80;

/// Completion cases decode this many tokens (mirrors the probe budget in
/// `tests/latency.rs`). Grammar cases use the product's one-token
/// `GRAMMAR_GENERATION_TOKENS` unless a case overrides `max_tokens` (only
/// vetting-rejection probes do — the shipped path is one token).
const COMPLETION_MAX_TOKENS: usize = 24;

/// True for an explicit truthy env value (trimmed, case-insensitive), parsed
/// identically to the `COMPME_REQUIRE_*` gates in `tests/latency.rs`.
fn env_flag_truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn require_model_tests() -> bool {
    env_flag_truthy(std::env::var("COMPME_REQUIRE_MODEL_TESTS").ok().as_deref())
}

fn require_model_context() -> bool {
    env_flag_truthy(
        std::env::var("COMPME_REQUIRE_MODEL_CONTEXT")
            .ok()
            .as_deref(),
    )
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Absolute paths pass through; relative ones resolve against the repo root,
/// mirroring how `run-model-gates.sh` absolutizes the spike model path.
fn absolutize_against_repo_root(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        repo_root().join(path)
    }
}

fn model_path() -> PathBuf {
    // COMPME_MODEL_GATE_PATH is the check-quality.sh / release contract (the
    // script passes the verified download location); fall back to the pinned
    // in-repo model like latency.rs.
    if let Ok(raw) = std::env::var("COMPME_MODEL_GATE_PATH") {
        if !raw.trim().is_empty() {
            return absolutize_against_repo_root(raw.trim());
        }
    }
    repo_root().join("tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf")
}

fn corpus_path() -> PathBuf {
    if let Ok(raw) = std::env::var("COMPME_QUALITY_CORPUS") {
        if !raw.trim().is_empty() {
            return absolutize_against_repo_root(raw.trim());
        }
    }
    repo_root().join("tools/release/quality-corpus.jsonl")
}

fn ensure_model_exists(path: &Path) -> bool {
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

fn load_model_or_skip(path: &Path) -> Option<LlamaModel> {
    match LlamaModel::load(path) {
        Ok(model) => Some(model),
        Err(err) if require_model_context() => panic!("load model: {err}"),
        Err(err) => {
            eprintln!("skipping real-model assertion: load model failed: {err}");
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CasePath {
    Completion,
    Grammar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    /// Completion contains the value (case-insensitive — the threshold, not
    /// letter case, is the signal).
    Contains(String),
    /// Completion must not contain the value (case-insensitive): degeneration
    /// controls — repetition loops, suffix regurgitation of `right` text.
    NotContains(String),
    /// Completion stays within this many whitespace-delimited words (the
    /// sentence-boundary / runaway control).
    MaxWords(usize),
    /// Grammar path after `vet_correction`: `Some(target)` requires exactly
    /// that vetted correction; `None` requires vetting to reject the model
    /// output (no correction offered — false-fix and vetting-guard probes).
    SingleWordVetted(Option<String>),
}

#[derive(Debug, PartialEq)]
struct Case {
    id: String,
    path: CasePath,
    left: String,
    /// Text after the caret. Never shown to the model today; documented on
    /// regurgitation-control cases so a future right-context prompt that
    /// parrots the suffix fails the matching `not_contains`.
    right: Option<String>,
    word: Option<String>,
    max_tokens: Option<usize>,
    expect: Expect,
    /// Optional case-sensitive pin on the pre-vetting raw output: rejection
    /// cases use it to prove the raw held the candidate their named vetting
    /// guard rejects, so they cannot pass on an unrelated guard.
    raw_contains: Option<String>,
    note: Option<String>,
}

/// What one corpus case produced, ready for `evaluate`. Grammar carries the
/// pre-vetting raw output so `raw_contains` pins can be checked; for
/// completions the text is the raw output.
enum Outcome {
    Completion(String),
    Grammar { vetted: Option<String>, raw: String },
}

fn evaluate(case: &Case, outcome: &Outcome) -> Result<(), String> {
    let raw = match outcome {
        Outcome::Completion(text) => text,
        Outcome::Grammar { raw, .. } => raw,
    };
    if let Some(want) = &case.raw_contains {
        if !raw.contains(want.as_str()) {
            return Err(format!("raw output lacks {want:?}"));
        }
    }
    match (&case.expect, outcome) {
        (Expect::Contains(want), Outcome::Completion(text)) => {
            if text.to_lowercase().contains(&want.to_lowercase()) {
                Ok(())
            } else {
                Err(format!("completion lacks {want:?}"))
            }
        }
        (Expect::NotContains(want), Outcome::Completion(text)) => {
            if text.to_lowercase().contains(&want.to_lowercase()) {
                Err(format!("completion contains forbidden {want:?}"))
            } else {
                Ok(())
            }
        }
        (Expect::MaxWords(max), Outcome::Completion(text)) => {
            let words = text.split_whitespace().count();
            if words <= *max {
                Ok(())
            } else {
                Err(format!("completion has {words} words, max {max}"))
            }
        }
        (Expect::SingleWordVetted(want), Outcome::Grammar { vetted, .. }) => match want {
            Some(target) => {
                if vetted.as_deref() == Some(target.as_str()) {
                    Ok(())
                } else {
                    Err(format!("vetted {vetted:?}, want {target:?}"))
                }
            }
            None => {
                if vetted.is_none() {
                    Ok(())
                } else {
                    Err(format!("expected vetting to reject, got {vetted:?}"))
                }
            }
        },
        (expect, _) => Err(format!(
            "expect {expect:?} does not apply to the {:?} path",
            case.path
        )),
    }
}

fn run_case(model: &LlamaModel, case: &Case) -> (Outcome, String) {
    match case.path {
        CasePath::Completion => {
            let raw = model
                .complete(
                    &terse_continuation_prompt(&case.left),
                    COMPLETION_MAX_TOKENS,
                )
                .expect("corpus completion");
            (Outcome::Completion(raw.clone()), raw)
        }
        CasePath::Grammar => {
            let word = case.word.as_deref().expect("grammar case word validated");
            let raw = model
                .complete(
                    &grammar_fix_prompt(word, &case.left),
                    case.max_tokens.unwrap_or(GRAMMAR_GENERATION_TOKENS),
                )
                .expect("corpus grammar completion");
            (
                Outcome::Grammar {
                    vetted: vet_correction(word, &raw),
                    raw: raw.clone(),
                },
                raw,
            )
        }
    }
}

// --- strict JSON-subset corpus parser -------------------------------------

/// Objects with string keys, string values, and non-negative integers. The
/// corpus schema is flat by design; arrays, booleans, null, and nested objects
/// beyond the single `expect` object are authoring errors, not features.
#[derive(Debug, PartialEq)]
enum Json {
    Object(Vec<(String, Json)>),
    Str(String),
    Num(u64),
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl JsonParser<'_> {
    fn new(text: &str) -> JsonParser<'_> {
        JsonParser {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn expect_byte(&mut self, want: u8) -> Result<(), String> {
        if self.peek() == Some(want) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!(
                "expected {:?} at offset {}",
                want as char, self.pos
            ))
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b'0'..=b'9') => self.parse_number(),
            _ => Err(format!(
                "unsupported value at offset {} (only objects, strings, integers)",
                self.pos
            )),
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect_byte(b'{')?;
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect_byte(b':')?;
            let value = self.parse_value()?;
            members.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(members));
                }
                _ => return Err(format!("expected ',' or '}}' at offset {}", self.pos)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_byte(b'"')?;
        let mut out = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err("unterminated string".to_string());
            };
            self.pos += 1;
            match byte {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| "invalid UTF-8 in string".to_string());
                }
                b'\\' => {
                    let Some(esc) = self.peek() else {
                        return Err("unterminated escape".to_string());
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.pos..self.pos + 4)
                                .ok_or("truncated \\u escape")?;
                            let hex = std::str::from_utf8(hex).map_err(|_| "bad \\u escape")?;
                            let code =
                                u32::from_str_radix(hex, 16).map_err(|_| "bad \\u escape")?;
                            let ch =
                                char::from_u32(code).ok_or("unsupported \\u escape (surrogate)")?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            self.pos += 4;
                        }
                        _ => return Err(format!("unsupported escape '\\{}'", esc as char)),
                    }
                }
                0x00..=0x1f => return Err("raw control byte in string".to_string()),
                _ => out.push(byte),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        let digits = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| "bad number")?;
        digits
            .parse::<u64>()
            .map(Json::Num)
            .map_err(|_| format!("bad integer {digits:?}"))
    }
}

fn parse_json(line: &str) -> Result<Json, String> {
    let mut parser = JsonParser::new(line);
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(format!("trailing bytes at offset {}", parser.pos));
    }
    Ok(value)
}

fn required_string(value: Option<Json>, field: &str) -> Result<String, String> {
    match value {
        Some(Json::Str(text)) if !text.is_empty() => Ok(text),
        Some(_) => Err(format!("field {field:?} must be a non-empty string")),
        None => Err(format!("missing field {field:?}")),
    }
}

fn optional_string(value: Option<Json>, field: &str) -> Result<Option<String>, String> {
    match value {
        Some(Json::Str(text)) if !text.is_empty() => Ok(Some(text)),
        Some(_) => Err(format!("field {field:?} must be a non-empty string")),
        None => Ok(None),
    }
}

fn expect_from_json(value: Option<Json>) -> Result<Expect, String> {
    let Some(Json::Object(members)) = value else {
        return Err("missing or invalid field \"expect\"".to_string());
    };
    let mut kind = None;
    let mut expect_value = None;
    for (key, value) in members {
        let slot = match key.as_str() {
            "type" => &mut kind,
            "value" => &mut expect_value,
            other => return Err(format!("unknown expect field {other:?}")),
        };
        if slot.is_some() {
            return Err(format!("duplicate expect field {key:?}"));
        }
        *slot = Some(value);
    }
    let Some(Json::Str(kind)) = kind else {
        return Err("expect.type must be a string".to_string());
    };
    match (kind.as_str(), expect_value) {
        ("contains", Some(Json::Str(want))) if !want.is_empty() => Ok(Expect::Contains(want)),
        ("not_contains", Some(Json::Str(want))) if !want.is_empty() => {
            Ok(Expect::NotContains(want))
        }
        ("max_words", Some(Json::Num(max))) if max > 0 => Ok(Expect::MaxWords(max as usize)),
        ("single_word_vetted", None) => Ok(Expect::SingleWordVetted(None)),
        ("single_word_vetted", Some(Json::Str(want))) if !want.is_empty() => {
            Ok(Expect::SingleWordVetted(Some(want)))
        }
        ("regex", _) => Err("expect type \"regex\" is not supported by the dependency-free harness; use contains/not_contains".to_string()),
        ("contains" | "not_contains" | "single_word_vetted", _) => {
            Err(format!("expect type {kind:?} needs a non-empty string value"))
        }
        ("max_words", _) => Err("expect type \"max_words\" needs a positive integer value".to_string()),
        (other, _) => Err(format!("unknown expect type {other:?}")),
    }
}

fn case_from_json(value: Json) -> Result<Case, String> {
    let Json::Object(members) = value else {
        return Err("case must be a JSON object".to_string());
    };
    let mut id = None;
    let mut path = None;
    let mut left = None;
    let mut right = None;
    let mut word = None;
    let mut max_tokens = None;
    let mut expect = None;
    let mut raw_contains = None;
    let mut note = None;
    for (key, value) in members {
        let slot = match key.as_str() {
            "id" => &mut id,
            "path" => &mut path,
            "left" => &mut left,
            "right" => &mut right,
            "word" => &mut word,
            "max_tokens" => &mut max_tokens,
            "expect" => &mut expect,
            "raw_contains" => &mut raw_contains,
            "note" => &mut note,
            other => return Err(format!("unknown field {other:?}")),
        };
        if slot.is_some() {
            return Err(format!("duplicate field {key:?}"));
        }
        *slot = Some(value);
    }

    let id = required_string(id, "id")?;
    let path = match required_string(path, "path")?.as_str() {
        "completion" => CasePath::Completion,
        "grammar" => CasePath::Grammar,
        other => {
            return Err(format!(
                "field \"path\" must be \"completion\" or \"grammar\", got {other:?}"
            ))
        }
    };
    let left = required_string(left, "left")?;
    let right = optional_string(right, "right")?;
    let word = optional_string(word, "word")?;
    let note = optional_string(note, "note")?;
    let max_tokens = match max_tokens {
        Some(Json::Num(tokens)) if tokens > 0 => Some(tokens as usize),
        Some(_) => return Err("field \"max_tokens\" must be a positive integer".to_string()),
        None => None,
    };
    let expect = expect_from_json(expect)?;
    let raw_contains = optional_string(raw_contains, "raw_contains")?;

    // Cross-field rules keep cases honest: every field must drive the outcome,
    // so a misplaced field is an authoring error, not a silent no-op.
    match path {
        CasePath::Grammar => {
            if word.is_none() {
                return Err("grammar case needs a \"word\"".to_string());
            }
            if right.is_some() {
                return Err("grammar case cannot use \"right\"".to_string());
            }
            if !matches!(expect, Expect::SingleWordVetted(_)) {
                return Err(format!(
                    "grammar case {id:?} needs a single_word_vetted expect"
                ));
            }
        }
        CasePath::Completion => {
            if word.is_some() {
                return Err("completion case cannot use \"word\"".to_string());
            }
            if max_tokens.is_some() {
                return Err("completion case cannot override \"max_tokens\"".to_string());
            }
            if matches!(expect, Expect::SingleWordVetted(_)) {
                return Err(format!(
                    "completion case {id:?} cannot use a single_word_vetted expect"
                ));
            }
        }
    }

    Ok(Case {
        id,
        path,
        left,
        right,
        word,
        max_tokens,
        expect,
        raw_contains,
        note,
    })
}

fn parse_corpus(text: &str) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let case = parse_json(line)
            .and_then(case_from_json)
            .map_err(|err| format!("line {}: {err}", index + 1))?;
        if !ids.insert(case.id.clone()) {
            return Err(format!(
                "line {}: duplicate case id {:?}",
                index + 1,
                case.id
            ));
        }
        cases.push(case);
    }
    Ok(cases)
}

#[test]
#[ignore = "requires the qwen2.5-0.5b GGUF model; release gates force CPU with COMPME_MODEL_GPU_LAYERS=0; run with --ignored"]
fn quality_corpus_passes_threshold() {
    if !require_model_tests() {
        return;
    }

    let path = model_path();
    if !ensure_model_exists(&path) {
        return;
    }

    let corpus_path = corpus_path();
    let corpus_text = std::fs::read_to_string(&corpus_path)
        .unwrap_or_else(|err| panic!("read corpus {}: {err}", corpus_path.display()));
    let cases = parse_corpus(&corpus_text)
        .unwrap_or_else(|err| panic!("corpus {}: {err}", corpus_path.display()));
    assert!(
        !cases.is_empty(),
        "corpus {} is empty",
        corpus_path.display()
    );

    let Some(model) = load_model_or_skip(&path) else {
        return;
    };
    model.warm_up().expect("warm up");

    let mut passed = 0usize;
    for case in &cases {
        let (outcome, raw) = run_case(&model, case);
        let raw_snippet: String = raw.chars().take(60).collect();
        let note = case
            .note
            .as_deref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        match evaluate(case, &outcome) {
            Ok(()) => {
                passed += 1;
                println!("PASS {} {:?}{note} raw={raw_snippet:?}", case.id, case.path);
            }
            Err(reason) => {
                println!(
                    "FAIL {} {:?}{note}: {reason} raw={raw_snippet:?}",
                    case.id, case.path
                );
            }
        }
    }

    let total = cases.len();
    let percent = passed * 100 / total;
    println!(
        "quality corpus: {passed}/{total} passed ({percent}%), threshold {PASS_THRESHOLD_PERCENT}%"
    );
    assert!(
        percent >= PASS_THRESHOLD_PERCENT,
        "model quality regressed: {passed}/{total} passed ({percent}%), below the {PASS_THRESHOLD_PERCENT}% threshold"
    );

    Box::new(model).shutdown();
}

// --- parser / evaluator unit tests (no model needed) -----------------------

fn parse_one(line: &str) -> Result<Case, String> {
    parse_json(line).and_then(case_from_json)
}

#[test]
fn corpus_parser_accepts_a_completion_case() {
    let case = parse_one(
        r#"{"id": "c1", "path": "completion", "left": "Dear team,", "right": " tail", "expect": {"type": "contains", "value": "follow"}, "note": "n"}"#,
    )
    .expect("valid completion case");
    assert_eq!(case.id, "c1");
    assert_eq!(case.path, CasePath::Completion);
    assert_eq!(case.left, "Dear team,");
    assert_eq!(case.right.as_deref(), Some(" tail"));
    assert_eq!(case.word, None);
    assert_eq!(case.max_tokens, None);
    assert_eq!(case.expect, Expect::Contains("follow".to_string()));
    assert_eq!(case.note.as_deref(), Some("n"));
}

#[test]
fn corpus_parser_accepts_a_grammar_case_with_max_tokens() {
    let case = parse_one(
        r#"{"id": "g1", "path": "grammar", "left": "I wrote", "word": "teh", "max_tokens": 8, "expect": {"type": "single_word_vetted"}}"#,
    )
    .expect("valid grammar case");
    assert_eq!(case.path, CasePath::Grammar);
    assert_eq!(case.word.as_deref(), Some("teh"));
    assert_eq!(case.max_tokens, Some(8));
    assert_eq!(case.expect, Expect::SingleWordVetted(None));
}

#[test]
fn corpus_parser_decodes_string_escapes() {
    let case = parse_one(
        r#"{"id": "e\"1", "path": "completion", "left": "a\nbé", "expect": {"type": "not_contains", "value": "x\ty"}}"#,
    )
    .expect("escapes decode");
    assert_eq!(case.id, "e\"1");
    assert_eq!(case.left, "a\nbé");
    assert_eq!(case.expect, Expect::NotContains("x\ty".to_string()));
}

#[test]
fn corpus_parser_rejects_malformed_lines() {
    for line in [
        "not json",
        r#"["array"]"#,
        r#"{"id": "x""#,
        r#"{"id": "x",}"#,
        r#"{"id": 3}"#,
        r#"{"id": "x", "unknown": 1}"#,
        r#"{"id": "x", "id": "y"}"#,
        r#"{"id": "x", "path": "completion", "left": "a", "expect": {"type": "regex", "value": "a.*"}}"#,
        r#"{"id": "x", "path": "sideways", "left": "a", "expect": {"type": "contains", "value": "b"}}"#,
        r#"{"id": "x", "path": "completion", "left": "a", "expect": {"type": "contains"}}"#,
        r#"{"id": "x", "path": "completion", "left": "a", "expect": {"type": "max_words", "value": "8"}}"#,
        r#"{"id": "x", "path": "completion", "left": "a", "expect": {"type": "single_word_vetted"}}"#,
        r#"{"id": "x", "path": "grammar", "left": "a", "expect": {"type": "single_word_vetted"}}"#,
        r#"{"id": "x", "path": "grammar", "left": "a", "word": "teh", "right": "r", "expect": {"type": "single_word_vetted"}}"#,
        r#"{"id": "x", "path": "grammar", "left": "a", "word": "teh", "expect": {"type": "contains", "value": "the"}}"#,
        r#"{"id": "x", "path": "grammar", "left": "a", "word": "teh", "raw_contains": "", "expect": {"type": "single_word_vetted"}}"#,
        r#"{"id": "x", "path": "completion", "left": "a", "max_tokens": 4, "expect": {"type": "contains", "value": "b"}}"#,
    ] {
        assert!(parse_one(line).is_err(), "must reject: {line}");
    }
}

#[test]
fn corpus_parser_reports_line_numbers_and_duplicate_ids() {
    let err = parse_corpus("{\"id\": \"a\", \"path\": \"completion\", \"left\": \"x\", \"expect\": {\"type\": \"contains\", \"value\": \"y\"}}\nbad line")
        .expect_err("second line is malformed");
    assert!(err.starts_with("line 2: "), "got: {err}");

    let err = parse_corpus(
        "{\"id\": \"a\", \"path\": \"completion\", \"left\": \"x\", \"expect\": {\"type\": \"contains\", \"value\": \"y\"}}\n{\"id\": \"a\", \"path\": \"completion\", \"left\": \"x\", \"expect\": {\"type\": \"contains\", \"value\": \"z\"}}",
    )
    .expect_err("duplicate id");
    assert!(err.contains("duplicate case id"), "got: {err}");
}

#[test]
fn evaluate_contains_is_case_insensitive() {
    let case = parse_one(
        r#"{"id": "c", "path": "completion", "left": "a", "expect": {"type": "contains", "value": "Follow Up"}}"#,
    )
    .unwrap();
    let outcome = Outcome::Completion(" follow up tomorrow".to_string());
    assert_eq!(evaluate(&case, &outcome), Ok(()));
    let outcome = Outcome::Completion(" see you".to_string());
    assert!(evaluate(&case, &outcome).is_err());
}

#[test]
fn evaluate_not_contains_and_max_words() {
    let case = parse_one(
        r#"{"id": "n", "path": "completion", "left": "a", "expect": {"type": "not_contains", "value": "the the the"}}"#,
    )
    .unwrap();
    let outcome = Outcome::Completion(" and THE THE THE again".to_string());
    assert!(evaluate(&case, &outcome).is_err());
    let outcome = Outcome::Completion(" and on".to_string());
    assert_eq!(evaluate(&case, &outcome), Ok(()));

    let case = parse_one(
        r#"{"id": "m", "path": "completion", "left": "a", "expect": {"type": "max_words", "value": 3}}"#,
    )
    .unwrap();
    let outcome = Outcome::Completion(" one two three".to_string());
    assert_eq!(evaluate(&case, &outcome), Ok(()));
    let outcome = Outcome::Completion(" one two three four".to_string());
    assert!(evaluate(&case, &outcome).is_err());
}

#[test]
fn evaluate_single_word_vetted_matches_or_rejects() {
    let case = parse_one(
        r#"{"id": "g", "path": "grammar", "left": "I wrote", "word": "teh", "expect": {"type": "single_word_vetted", "value": "the"}}"#,
    )
    .unwrap();
    let outcome = Outcome::Grammar {
        vetted: Some("the".to_string()),
        raw: " the".to_string(),
    };
    assert_eq!(evaluate(&case, &outcome), Ok(()));
    let outcome = Outcome::Grammar {
        vetted: Some("tea".to_string()),
        raw: " tea".to_string(),
    };
    assert!(evaluate(&case, &outcome).is_err());
    let outcome = Outcome::Grammar {
        vetted: None,
        raw: " teh".to_string(),
    };
    assert!(evaluate(&case, &outcome).is_err());

    let case = parse_one(
        r#"{"id": "g2", "path": "grammar", "left": "I wrote", "word": "the", "expect": {"type": "single_word_vetted"}}"#,
    )
    .unwrap();
    let outcome = Outcome::Grammar {
        vetted: None,
        raw: " the".to_string(),
    };
    assert_eq!(evaluate(&case, &outcome), Ok(()));
    let outcome = Outcome::Grammar {
        vetted: Some("tea".to_string()),
        raw: " tea".to_string(),
    };
    assert!(evaluate(&case, &outcome).is_err());
}

#[test]
fn corpus_parser_accepts_raw_contains() {
    let case = parse_one(
        r#"{"id": "g", "path": "grammar", "left": "I wrote", "word": "teh", "raw_contains": "the. I", "expect": {"type": "single_word_vetted"}}"#,
    )
    .expect("valid raw_contains case");
    assert_eq!(case.raw_contains.as_deref(), Some("the. I"));

    let case = parse_one(
        r#"{"id": "g2", "path": "grammar", "left": "I wrote", "word": "teh", "expect": {"type": "single_word_vetted"}}"#,
    )
    .expect("raw_contains is optional");
    assert_eq!(case.raw_contains, None);
}

#[test]
fn evaluate_raw_contains_pins_the_pre_vetting_raw() {
    // The named guard provably fires: vetting rejected the output AND the raw
    // held the non-ASCII candidate the case exists to reject.
    let case = parse_one(
        r#"{"id": "g", "path": "grammar", "left": "Le café sert du vin", "word": "cafe", "raw_contains": "café", "expect": {"type": "single_word_vetted"}}"#,
    )
    .unwrap();
    let outcome = Outcome::Grammar {
        vetted: None,
        raw: " café".to_string(),
    };
    assert_eq!(evaluate(&case, &outcome), Ok(()));
    // Vetting rejected, but the raw never held the pinned candidate: the case
    // no longer exercises its named guard and must fail.
    let outcome = Outcome::Grammar {
        vetted: None,
        raw: " cafe".to_string(),
    };
    assert!(evaluate(&case, &outcome).is_err());

    // On the completion path the pin is exact and case-sensitive, unlike the
    // case-insensitive contains expect it complements.
    let case = parse_one(
        r#"{"id": "c", "path": "completion", "left": "a", "raw_contains": "Exact", "expect": {"type": "contains", "value": "exact"}}"#,
    )
    .unwrap();
    let outcome = Outcome::Completion(" Exact match".to_string());
    assert_eq!(evaluate(&case, &outcome), Ok(()));
    let outcome = Outcome::Completion(" exact lower".to_string());
    assert!(evaluate(&case, &outcome).is_err());
}

#[test]
fn repo_quality_corpus_parses() {
    // Model-free branch-CI guard: a malformed shipped corpus fails here, not
    // just at the release gate.
    let path = repo_root().join("tools/release/quality-corpus.jsonl");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read corpus {}: {err}", path.display()));
    let cases =
        parse_corpus(&text).unwrap_or_else(|err| panic!("corpus {}: {err}", path.display()));
    assert!(!cases.is_empty(), "corpus {} is empty", path.display());
}
