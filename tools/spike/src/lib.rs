//! Spike logic, unit-tested behind seams. Real FFI lives in src/bin/*.

pub mod geometry {
    /// A rectangle in screen coordinates.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct ScreenRect {
        pub x: f64,
        pub y: f64,
        pub w: f64,
        pub h: f64,
    }

    /// True if a caret rect is plausibly a caret (not empty, not the whole container).
    pub fn usable_caret_rect(w: f64, h: f64) -> bool {
        w > 0.0 && w < 2000.0 && h > 0.0 && h < 200.0
    }

    /// Convert an AX rect (top-left origin, y grows downward) to a Cocoa window
    /// origin (bottom-left origin, y grows upward) given the primary screen height.
    pub fn ax_to_cocoa_origin(screen_h: f64, r: ScreenRect) -> (f64, f64) {
        (r.x, screen_h - r.y - r.h)
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::geometry::*;

    #[test]
    fn usable_rejects_zero_height() {
        assert!(!usable_caret_rect(10.0, 0.0));
    }

    #[test]
    fn usable_rejects_container_width() {
        assert!(!usable_caret_rect(2500.0, 18.0));
    }

    #[test]
    fn usable_rejects_tall_rect() {
        assert!(!usable_caret_rect(10.0, 250.0));
    }

    #[test]
    fn usable_accepts_normal_caret() {
        assert!(usable_caret_rect(2.0, 18.0));
    }

    #[test]
    fn usable_rejects_negative_width() {
        assert!(!usable_caret_rect(-5.0, 18.0));
    }

    #[test]
    fn ax_to_cocoa_flips_y_from_top_left() {
        let r = ScreenRect {
            x: 50.0,
            y: 100.0,
            w: 2.0,
            h: 20.0,
        };
        let (x, y) = ax_to_cocoa_origin(1000.0, r);
        assert_eq!(x, 50.0);
        assert_eq!(y, 880.0); // 1000 - 100 - 20
    }
}

pub mod caret {
    use crate::geometry::{usable_caret_rect, ScreenRect};

    /// Seam: source of text-range bounds (real impl = AX; tests use a fake).
    pub trait BoundsSource {
        fn bounds(&self, location: isize, length: isize) -> Option<ScreenRect>;
    }

    #[derive(Debug, PartialEq, Clone, Copy)]
    pub enum CaretTier {
        Exact,
        Derived,
        None,
    }

    /// Native caret-rect ladder: tier1 zero-length at caret; tier3 prev-char right edge.
    pub fn resolve_caret(src: &dyn BoundsSource, caret: isize) -> (CaretTier, Option<ScreenRect>) {
        if let Some(r) = src.bounds(caret, 0) {
            if usable_caret_rect(r.w, r.h) {
                return (CaretTier::Exact, Some(r));
            }
        }
        if caret > 0 {
            if let Some(r) = src.bounds(caret - 1, 1) {
                if usable_caret_rect(r.w, r.h) {
                    return (
                        CaretTier::Derived,
                        Some(ScreenRect {
                            x: r.x + r.w,
                            y: r.y,
                            w: 1.0,
                            h: r.h,
                        }),
                    );
                }
            }
        }
        (CaretTier::None, None)
    }
}

#[cfg(test)]
mod caret_tests {
    use super::caret::*;
    use super::geometry::ScreenRect;

    struct Fake {
        zero: Option<ScreenRect>,
        prev: Option<ScreenRect>,
    }
    impl BoundsSource for Fake {
        fn bounds(&self, _loc: isize, length: isize) -> Option<ScreenRect> {
            if length == 0 {
                self.zero
            } else {
                self.prev
            }
        }
    }

    #[test]
    fn exact_when_zero_length_usable() {
        let f = Fake {
            zero: Some(ScreenRect {
                x: 10.0,
                y: 20.0,
                w: 2.0,
                h: 18.0,
            }),
            prev: None,
        };
        let (tier, r) = resolve_caret(&f, 5);
        assert_eq!(tier, CaretTier::Exact);
        assert_eq!(r.unwrap().x, 10.0);
    }

    #[test]
    fn derived_uses_prev_char_right_edge() {
        let f = Fake {
            zero: None,
            prev: Some(ScreenRect {
                x: 10.0,
                y: 20.0,
                w: 8.0,
                h: 18.0,
            }),
        };
        let (tier, r) = resolve_caret(&f, 5);
        assert_eq!(tier, CaretTier::Derived);
        assert_eq!(r.unwrap().x, 18.0); // 10 + 8
    }

    #[test]
    fn falls_to_derived_when_zero_is_container() {
        let f = Fake {
            zero: Some(ScreenRect {
                x: 0.0,
                y: 0.0,
                w: 2500.0,
                h: 18.0,
            }), // container -> rejected
            prev: Some(ScreenRect {
                x: 10.0,
                y: 20.0,
                w: 8.0,
                h: 18.0,
            }),
        };
        assert_eq!(resolve_caret(&f, 5).0, CaretTier::Derived);
    }

    #[test]
    fn none_when_nothing_usable() {
        let f = Fake {
            zero: None,
            prev: None,
        };
        assert_eq!(resolve_caret(&f, 5).0, CaretTier::None);
    }

    #[test]
    fn no_prev_lookup_at_caret_zero() {
        let f = Fake {
            zero: None,
            prev: Some(ScreenRect {
                x: 1.0,
                y: 1.0,
                w: 8.0,
                h: 18.0,
            }),
        };
        assert_eq!(resolve_caret(&f, 0).0, CaretTier::None);
    }
}

pub mod context {
    /// Seam: focused editable field (real impl = AX).
    pub trait FieldSource {
        fn value(&self) -> Option<String>;
        fn caret(&self) -> usize;
    }

    /// Text left of the caret (char-safe).
    pub fn left_context(value: &str, caret: usize) -> String {
        value.chars().take(caret).collect()
    }

    /// Last `n` chars left of the caret (for compact display/logging).
    pub fn left_tail(value: &str, caret: usize, n: usize) -> String {
        let left: Vec<char> = value.chars().take(caret).collect();
        let start = left.len().saturating_sub(n);
        left[start..].iter().collect()
    }
}

#[cfg(test)]
mod context_tests {
    use super::context::*;

    #[test]
    fn left_context_truncates_at_caret() {
        assert_eq!(left_context("hello world", 5), "hello");
    }
    #[test]
    fn left_context_caret_past_end_returns_all() {
        assert_eq!(left_context("hi", 99), "hi");
    }
    #[test]
    fn left_context_is_char_safe() {
        assert_eq!(left_context("héllo", 2), "hé");
    }
    #[test]
    fn left_tail_returns_last_n() {
        assert_eq!(left_tail("abcdefgh", 8, 3), "fgh");
    }
    #[test]
    fn left_tail_fewer_than_n_returns_all() {
        assert_eq!(left_tail("ab", 2, 5), "ab");
    }
    #[test]
    fn left_tail_zero_n_returns_empty() {
        assert_eq!(left_tail("abc", 3, 0), "");
    }
    #[test]
    fn left_context_caret_zero_returns_empty() {
        assert_eq!(left_context("hi", 0), "");
    }
}

pub mod completion {
    use crate::context::left_context;

    /// Seam: the model (real impl = llama.cpp).
    pub trait Completer {
        fn complete(&self, prompt: &str) -> String;
    }

    /// Trailing whitespace wrecks small models (spec §5) — strip before prompting.
    pub fn trim_prefix(s: &str) -> &str {
        s.trim_end()
    }

    /// Cap a completion to `max_words` (spec: maxCompletionLength in words).
    pub fn cap_words(text: &str, max_words: usize) -> String {
        text.split_whitespace()
            .take(max_words)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Full pipeline: left context -> trim -> complete -> cap.
    pub fn suggest(value: &str, caret: usize, c: &dyn Completer, max_words: usize) -> String {
        let prompt = trim_prefix(&left_context(value, caret)).to_string();
        cap_words(&c.complete(&prompt), max_words)
    }

    pub fn quality_flags(raw: &str, capped: &str, prefix: &str) -> String {
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
}

pub mod model_compare {
    use crate::completion::{cap_words, quality_flags, trim_prefix};
    use crate::context::left_context;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PromptCase {
        pub name: &'static str,
        pub value: &'static str,
        pub max_words: usize,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum PromptMode {
        Raw,
        FimEmptySuffix,
        TerseInstruction,
    }

    impl PromptMode {
        pub fn name(self) -> &'static str {
            match self {
                Self::Raw => "raw",
                Self::FimEmptySuffix => "fim_empty_suffix",
                Self::TerseInstruction => "terse_instruction",
            }
        }

        pub fn build(self, prefix: &str) -> String {
            match self {
                Self::Raw => prefix.to_string(),
                Self::FimEmptySuffix => {
                    format!("<|fim_prefix|>{prefix}<|fim_suffix|><|fim_middle|>")
                }
                Self::TerseInstruction => {
                    format!(
                        "Complete this text inline. Return only the continuation.\nText: {prefix}"
                    )
                }
            }
        }
    }

    pub const MODES: &[PromptMode] = &[
        PromptMode::Raw,
        PromptMode::FimEmptySuffix,
        PromptMode::TerseInstruction,
    ];

    pub const CASES: &[PromptCase] = &[
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

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ReportTiming {
        pub context_init_ms: u128,
        pub prompt_eval_ms: u128,
        pub ttft_ms: u128,
        pub decode_ms: u128,
        pub total_ms: u128,
        pub emitted_tokens: usize,
    }

    pub fn prompt_for_case(mode: PromptMode, case: PromptCase) -> (String, String) {
        let caret = case.value.chars().count();
        let prefix = trim_prefix(&left_context(case.value, caret)).to_string();
        let prompt = mode.build(&prefix);
        (prefix, prompt)
    }

    pub fn escaped(s: &str) -> String {
        s.replace('\n', "\\n").replace('\t', "\\t")
    }

    pub fn build_report_row(
        mode: PromptMode,
        case: PromptCase,
        timing: &ReportTiming,
        prefix: &str,
        raw: &str,
    ) -> String {
        let capped = cap_words(raw, case.max_words);
        let flags = quality_flags(raw, &capped, prefix);
        format!(
            "mode={} case={} context_init_ms={} prompt_eval_ms={} ttft_ms={} decode_ms={} total_ms={} emitted_tokens={} quality_flags={} prefix={:?} raw={:?} capped={:?}",
            mode.name(),
            case.name,
            timing.context_init_ms,
            timing.prompt_eval_ms,
            timing.ttft_ms,
            timing.decode_ms,
            timing.total_ms,
            timing.emitted_tokens,
            flags,
            escaped(prefix),
            escaped(raw),
            escaped(&capped)
        )
    }
}

#[cfg(test)]
mod model_compare_tests {
    use super::model_compare::*;

    #[test]
    fn prompt_modes_have_stable_names() {
        assert_eq!(
            MODES.iter().map(|mode| mode.name()).collect::<Vec<_>>(),
            vec!["raw", "fim_empty_suffix", "terse_instruction"]
        );
    }

    #[test]
    fn prompt_modes_build_expected_prompts() {
        assert_eq!(PromptMode::Raw.build("hello"), "hello");
        assert_eq!(
            PromptMode::FimEmptySuffix.build("hello"),
            "<|fim_prefix|>hello<|fim_suffix|><|fim_middle|>"
        );
        assert_eq!(
            PromptMode::TerseInstruction.build("hello"),
            "Complete this text inline. Return only the continuation.\nText: hello"
        );
    }

    #[test]
    fn cases_include_unicode_probe() {
        assert!(CASES
            .iter()
            .any(|case| case.name == "unicode_note" && case.value.contains("résumé")));
    }

    #[test]
    fn prompt_for_case_trims_trailing_prefix_whitespace() {
        let case = PromptCase {
            name: "tmp",
            value: "hello ",
            max_words: 1,
        };
        let (prefix, prompt) = prompt_for_case(PromptMode::Raw, case);
        assert_eq!(prefix, "hello");
        assert_eq!(prompt, "hello");
    }

    #[test]
    fn escaped_protects_row_schema_from_tabs_and_newlines() {
        assert_eq!(escaped("a\tb\nc"), "a\\tb\\nc");
    }

    #[test]
    fn report_row_has_stable_columns_and_caps_output() {
        let case = PromptCase {
            name: "tmp",
            value: "The next ",
            max_words: 2,
        };
        let timing = ReportTiming {
            context_init_ms: 1,
            prompt_eval_ms: 2,
            ttft_ms: 3,
            decode_ms: 4,
            total_ms: 5,
            emitted_tokens: 6,
        };
        let row = build_report_row(
            PromptMode::Raw,
            case,
            &timing,
            "The next",
            " milestone ships\nsoon",
        );
        assert_eq!(
            row,
            "mode=raw case=tmp context_init_ms=1 prompt_eval_ms=2 ttft_ms=3 decode_ms=4 total_ms=5 emitted_tokens=6 quality_flags=chat_or_markdown prefix=\"The next\" raw=\" milestone ships\\\\nsoon\" capped=\"milestone ships\""
        );
    }
}

#[cfg(test)]
mod completion_tests {
    use super::completion::*;

    struct ReturnsPrompt; // echoes its input so we can assert the pipeline trimmed it
    impl Completer for ReturnsPrompt {
        fn complete(&self, p: &str) -> String {
            p.to_string()
        }
    }
    struct Fixed(&'static str);
    impl Completer for Fixed {
        fn complete(&self, _p: &str) -> String {
            self.0.to_string()
        }
    }

    #[test]
    fn trim_prefix_removes_trailing_ws() {
        assert_eq!(trim_prefix("hi  "), "hi");
    }
    #[test]
    fn cap_words_caps() {
        assert_eq!(cap_words("one two three four", 2), "one two");
    }
    #[test]
    fn cap_words_returns_all_when_fewer() {
        assert_eq!(cap_words("a b", 5), "a b");
    }
    #[test]
    fn cap_words_zero_max_returns_empty() {
        assert_eq!(cap_words("one two", 0), "");
    }
    #[test]
    fn cap_words_normalizes_whitespace() {
        assert_eq!(cap_words("  one   two  ", 2), "one two");
    }

    #[test]
    fn suggest_trims_trailing_ws_before_completing() {
        // value "hello " caret 6 -> left "hello " -> trimmed "hello" -> echoed -> capped
        assert_eq!(suggest("hello ", 6, &ReturnsPrompt, 4), "hello");
    }

    #[test]
    fn suggest_caps_completion_words() {
        assert_eq!(
            suggest("x", 1, &Fixed("one two three four five"), 3),
            "one two three"
        );
    }

    #[test]
    fn quality_flags_empty_output() {
        assert_eq!(quality_flags("  ", "", "The next"), "empty");
    }

    #[test]
    fn quality_flags_prompt_leaks() {
        assert_eq!(
            quality_flags("<|fim_prefix|>The next", "fim", "The next"),
            "prompt_leak|chat_or_markdown"
        );
        assert_eq!(
            quality_flags("Complete this text inline. Text: hi", "hi", "The next"),
            "prompt_leak"
        );
    }

    #[test]
    fn quality_flags_garbage_unicode() {
        assert_eq!(
            quality_flags("bad \u{fffd}", "bad", "The next"),
            "garbage_unicode"
        );
        assert_eq!(quality_flags("继续", "继续", "The next"), "garbage_unicode");
    }

    #[test]
    fn quality_flags_chat_or_markdown() {
        assert_eq!(
            quality_flags("Sure:\n- item", "Sure:", "The next"),
            "chat_or_markdown"
        );
        assert_eq!(
            quality_flags("> quoted", "quoted", "The next"),
            "chat_or_markdown"
        );
    }

    #[test]
    fn quality_flags_prefix_repetition() {
        assert_eq!(
            quality_flags(
                "should happen",
                "should happen",
                "The next milestone should"
            ),
            "prefix_repetition"
        );
    }

    #[test]
    fn quality_flags_clean_output_is_ok() {
        assert_eq!(
            quality_flags(
                " be shipped soon",
                "be shipped soon",
                "The next milestone should"
            ),
            "ok"
        );
    }
}

pub mod keys {
    pub const KEYCODE_TAB: i64 = 48;

    /// Two-tap rule (spec §4): only swallow the accept key while a suggestion is visible,
    /// so plain Tab still navigates when nothing is shown.
    pub fn should_swallow(keycode: i64, suggestion_visible: bool) -> bool {
        keycode == KEYCODE_TAB && suggestion_visible
    }
}

#[cfg(test)]
mod keys_tests {
    use super::keys::*;
    #[test]
    fn swallow_tab_when_suggestion_visible() {
        assert!(should_swallow(KEYCODE_TAB, true));
    }
    #[test]
    fn pass_tab_when_no_suggestion() {
        assert!(!should_swallow(KEYCODE_TAB, false));
    }
    #[test]
    fn pass_other_keys_even_when_visible() {
        assert!(!should_swallow(0, true));
    }
}
