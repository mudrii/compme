//! Spike logic, unit-tested behind seams. Real FFI lives in src/bin/*.

pub mod geometry {
    /// A rectangle in screen coordinates.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct ScreenRect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }

    /// True if a caret rect is plausibly a caret (not empty, not the whole container).
    pub fn usable_caret_rect(w: f64, h: f64) -> bool {
        h > 0.0 && h < 200.0 && w < 2000.0
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
    fn usable_rejects_zero_height() { assert!(!usable_caret_rect(10.0, 0.0)); }

    #[test]
    fn usable_rejects_container_width() { assert!(!usable_caret_rect(2500.0, 18.0)); }

    #[test]
    fn usable_rejects_tall_rect() { assert!(!usable_caret_rect(10.0, 250.0)); }

    #[test]
    fn usable_accepts_normal_caret() { assert!(usable_caret_rect(2.0, 18.0)); }

    #[test]
    fn ax_to_cocoa_flips_y_from_top_left() {
        let r = ScreenRect { x: 50.0, y: 100.0, w: 2.0, h: 20.0 };
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
    pub enum CaretTier { Exact, Derived, None }

    /// Native caret-rect ladder: tier1 zero-length at caret; tier3 prev-char right edge.
    pub fn resolve_caret(src: &dyn BoundsSource, caret: isize) -> (CaretTier, Option<ScreenRect>) {
        if let Some(r) = src.bounds(caret, 0) {
            if usable_caret_rect(r.w, r.h) { return (CaretTier::Exact, Some(r)); }
        }
        if caret > 0 {
            if let Some(r) = src.bounds(caret - 1, 1) {
                if usable_caret_rect(r.w, r.h) {
                    return (CaretTier::Derived, Some(ScreenRect { x: r.x + r.w, y: r.y, w: 1.0, h: r.h }));
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

    struct Fake { zero: Option<ScreenRect>, prev: Option<ScreenRect> }
    impl BoundsSource for Fake {
        fn bounds(&self, _loc: isize, length: isize) -> Option<ScreenRect> {
            if length == 0 { self.zero } else { self.prev }
        }
    }

    #[test]
    fn exact_when_zero_length_usable() {
        let f = Fake { zero: Some(ScreenRect { x: 10.0, y: 20.0, w: 2.0, h: 18.0 }), prev: None };
        let (tier, r) = resolve_caret(&f, 5);
        assert_eq!(tier, CaretTier::Exact);
        assert_eq!(r.unwrap().x, 10.0);
    }

    #[test]
    fn derived_uses_prev_char_right_edge() {
        let f = Fake { zero: None, prev: Some(ScreenRect { x: 10.0, y: 20.0, w: 8.0, h: 18.0 }) };
        let (tier, r) = resolve_caret(&f, 5);
        assert_eq!(tier, CaretTier::Derived);
        assert_eq!(r.unwrap().x, 18.0); // 10 + 8
    }

    #[test]
    fn falls_to_derived_when_zero_is_container() {
        let f = Fake {
            zero: Some(ScreenRect { x: 0.0, y: 0.0, w: 2500.0, h: 18.0 }), // container -> rejected
            prev: Some(ScreenRect { x: 10.0, y: 20.0, w: 8.0, h: 18.0 }),
        };
        assert_eq!(resolve_caret(&f, 5).0, CaretTier::Derived);
    }

    #[test]
    fn none_when_nothing_usable() {
        let f = Fake { zero: None, prev: None };
        assert_eq!(resolve_caret(&f, 5).0, CaretTier::None);
    }

    #[test]
    fn no_prev_lookup_at_caret_zero() {
        let f = Fake { zero: None, prev: Some(ScreenRect { x: 1.0, y: 1.0, w: 8.0, h: 18.0 }) };
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

    #[test] fn left_context_truncates_at_caret() { assert_eq!(left_context("hello world", 5), "hello"); }
    #[test] fn left_context_caret_past_end_returns_all() { assert_eq!(left_context("hi", 99), "hi"); }
    #[test] fn left_context_is_char_safe() { assert_eq!(left_context("héllo", 2), "hé"); }
    #[test] fn left_tail_returns_last_n() { assert_eq!(left_tail("abcdefgh", 8, 3), "fgh"); }
    #[test] fn left_tail_fewer_than_n_returns_all() { assert_eq!(left_tail("ab", 2, 5), "ab"); }
}

pub mod completion {
    use crate::context::left_context;

    /// Seam: the model (real impl = llama.cpp).
    pub trait Completer { fn complete(&self, prompt: &str) -> String; }

    /// Trailing whitespace wrecks small models (spec §5) — strip before prompting.
    pub fn trim_prefix(s: &str) -> &str { s.trim_end() }

    /// Cap a completion to `max_words` (spec: maxCompletionLength in words).
    pub fn cap_words(text: &str, max_words: usize) -> String {
        text.split_whitespace().take(max_words).collect::<Vec<_>>().join(" ")
    }

    /// Full pipeline: left context -> trim -> complete -> cap.
    pub fn suggest(value: &str, caret: usize, c: &dyn Completer, max_words: usize) -> String {
        let prompt = trim_prefix(&left_context(value, caret)).to_string();
        cap_words(&c.complete(&prompt), max_words)
    }
}

#[cfg(test)]
mod completion_tests {
    use super::completion::*;

    struct ReturnsPrompt; // echoes its input so we can assert the pipeline trimmed it
    impl Completer for ReturnsPrompt { fn complete(&self, p: &str) -> String { p.to_string() } }
    struct Fixed(&'static str);
    impl Completer for Fixed { fn complete(&self, _p: &str) -> String { self.0.to_string() } }

    #[test] fn trim_prefix_removes_trailing_ws() { assert_eq!(trim_prefix("hi  "), "hi"); }
    #[test] fn cap_words_caps() { assert_eq!(cap_words("one two three four", 2), "one two"); }
    #[test] fn cap_words_returns_all_when_fewer() { assert_eq!(cap_words("a b", 5), "a b"); }

    #[test]
    fn suggest_trims_trailing_ws_before_completing() {
        // value "hello " caret 6 -> left "hello " -> trimmed "hello" -> echoed -> capped
        assert_eq!(suggest("hello ", 6, &ReturnsPrompt, 4), "hello");
    }

    #[test]
    fn suggest_caps_completion_words() {
        assert_eq!(suggest("x", 1, &Fixed("one two three four five"), 3), "one two three");
    }
}
