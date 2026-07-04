//! Pure text-context helpers around a caret.

/// The text strictly before the caret. `caret` is a **Unicode-scalar** offset
/// — not bytes, UTF-16 units, or graphemes. Callers holding a
/// `platform::TextContext` offset must convert from its `OffsetEncoding` to
/// scalars first; feeding an unconverted offset silently splits at the wrong
/// place on non-ASCII text. A past-end caret is clamped (returns the whole
/// string); never panics.
pub fn left_context(value: &str, caret: usize) -> String {
    value.chars().take(caret).collect()
}

/// The text from the caret onward. Same contract as [`left_context`]: `caret`
/// is a Unicode-scalar offset (convert from `platform::OffsetEncoding` first);
/// a past-end caret yields the empty string; never panics.
pub fn right_context(value: &str, caret: usize) -> String {
    value.chars().skip(caret).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordAtCaret<'a> {
    pub word: &'a str,
    pub range: WordRange,
}

pub fn word_at_caret(value: &str, caret: usize) -> Option<WordAtCaret<'_>> {
    let chars: Vec<(usize, char)> = value.char_indices().collect();
    if chars.is_empty() {
        return None;
    }
    let len = chars.len();
    let caret = caret.min(len);

    if caret == len && !is_word_char(chars[len - 1].1) {
        return None;
    }
    let seed = if caret < len && is_word_char(chars[caret].1) {
        caret
    } else {
        caret.checked_sub(1)?
    };
    if !is_word_char(chars[seed].1) {
        return None;
    }

    let mut start = seed;
    while start > 0 && is_word_char(chars[start - 1].1) {
        start -= 1;
    }
    let mut end = seed + 1;
    while end < len && is_word_char(chars[end].1) {
        end += 1;
    }

    let byte_start = chars[start].0;
    let byte_end = chars.get(end).map(|(idx, _)| *idx).unwrap_or(value.len());
    Some(WordAtCaret {
        word: &value[byte_start..byte_end],
        range: WordRange { start, end },
    })
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

/// Strip trailing whitespace from a left-context prompt (the model should not
/// see a dangling space/newline after the caret). Leading whitespace is kept —
/// it is part of the user's text. Named for what it does: trims the *trailing*
/// end only, not the prefix.
pub fn trim_trailing(value: &str) -> &str {
    value.trim_end()
}

/// Truncate to at most `max` chars on a char boundary, keeping the tail (the
/// most recent / caret-adjacent end).
fn tail_chars(s: &str, max: usize) -> &str {
    if max == 0 {
        return "";
    }
    let count = s.chars().count();
    if count <= max {
        return s;
    }
    let (byte_idx, _) = s
        .char_indices()
        .nth(count - max)
        .expect("count-max is a valid char boundary < count");
    &s[byte_idx..]
}

/// Assemble an opt-in context block to prepend to the completion prompt (A2 §16
/// context augmentation): optional clipboard/pasteboard text plus recent
/// previous inputs. Each source is trimmed and bounded to `max_chars` (keeping
/// the most recent tail). Returns an empty string when there is no context, so
/// the caller can prepend unconditionally. The caller is responsible for
/// redacting the sources before passing them in.
pub fn build_context_block(
    pasteboard: Option<&str>,
    screen: Option<&str>,
    previous_inputs: &[&str],
    max_chars: usize,
) -> String {
    if max_chars == 0 {
        return String::new();
    }
    // Collapse whitespace runs (incl. newlines) to a single space so a multi-line
    // source can't masquerade as a new directive line or escape the block.
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines: Vec<String> = Vec::new();
    if let Some(clip) = pasteboard {
        let clip = one_line(clip);
        if !clip.is_empty() {
            lines.push(format!("Clipboard: {}", tail_chars(&clip, max_chars)));
        }
    }
    if let Some(screen) = screen {
        let screen = one_line(screen);
        if !screen.is_empty() {
            lines.push(format!("On screen: {}", tail_chars(&screen, max_chars)));
        }
    }
    for input in previous_inputs {
        let input = one_line(input);
        if !input.is_empty() {
            lines.push(format!("Recent: {}", tail_chars(&input, max_chars)));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("Context (for reference only):\n{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_context_truncates_at_caret() {
        assert_eq!(left_context("hello world", 5), "hello");
    }

    #[test]
    fn left_context_past_end_returns_all() {
        assert_eq!(left_context("hi", 99), "hi");
    }

    #[test]
    fn left_context_char_safe() {
        assert_eq!(left_context("héllo", 2), "hé");
    }

    #[test]
    fn left_context_zero_is_empty() {
        assert_eq!(left_context("hi", 0), "");
    }

    #[test]
    fn right_context_starts_at_caret() {
        assert_eq!(right_context("hello world", 6), "world");
    }

    #[test]
    fn context_block_empty_without_sources() {
        assert_eq!(build_context_block(None, None, &[], 100), "");
        assert_eq!(build_context_block(Some("   "), None, &["  "], 100), "");
    }

    #[test]
    fn context_block_includes_clipboard() {
        let block = build_context_block(Some("paste me"), None, &[], 100);
        assert!(block.contains("Clipboard: paste me"));
        assert!(block.starts_with("Context (for reference only):"));
    }

    #[test]
    fn context_block_includes_screen_text() {
        let block = build_context_block(None, Some("window title and visible text"), &[], 100);
        assert!(block.contains("On screen: window title and visible text"));
    }

    #[test]
    fn context_block_includes_previous_inputs() {
        let block = build_context_block(None, None, &["first note", "second note"], 100);
        assert!(block.contains("Recent: first note"));
        assert!(block.contains("Recent: second note"));
    }

    #[test]
    fn context_block_preserves_source_order_labels_and_final_newline() {
        assert_eq!(
            build_context_block(
                Some("clipboard text"),
                Some("screen text"),
                &["newer accepted", "older accepted"],
                100
            ),
            "Context (for reference only):\n\
Clipboard: clipboard text\n\
On screen: screen text\n\
Recent: newer accepted\n\
Recent: older accepted\n"
        );
    }

    #[test]
    fn context_block_omits_empty_sources_without_blank_lines() {
        assert_eq!(
            build_context_block(Some("  "), Some("visible text"), &["\n\t", "recent"], 100),
            "Context (for reference only):\n\
On screen: visible text\n\
Recent: recent\n"
        );
    }

    #[test]
    fn context_block_bounds_each_source_to_max_chars_keeping_the_tail() {
        let long = "0123456789abcdef"; // 16 chars
        let block = build_context_block(Some(long), None, &[], 4);
        assert!(block.contains("Clipboard: cdef"), "got {block:?}");
        assert!(!block.contains("0123"));
    }

    #[test]
    fn context_block_keeps_source_whole_when_len_equals_max() {
        // tail_chars returns the source unchanged when its scalar count is <= max
        // (the `count <= max` early return). Pin the exact boundary: a 4-char
        // clipboard source with max_chars == 4 is kept WHOLE — not truncated by an
        // off-by-one to its 3-char tail. The bounds tests cover len > max; this is
        // the len == max edge.
        let block = build_context_block(Some("wxyz"), None, &[], 4); // 4 chars == max
        assert_eq!(
            block,
            "Context (for reference only):\n\
Clipboard: wxyz\n",
            "a source whose length equals max_chars is kept whole"
        );
    }

    #[test]
    fn context_block_keeps_multibyte_source_whole_when_len_equals_max() {
        // The same len == max boundary, but with a multibyte source: "a日本" is 3
        // scalars, and max_chars == 3 returns it whole via the `count <= max`
        // early return — never reaching the byte-offset tail slice. Guards that the
        // boundary is scalar-counted (a byte-length comparison would see 7 bytes >
        // 3 and wrongly truncate).
        let block = build_context_block(Some("a日本"), None, &[], 3); // 3 scalars == max
        assert_eq!(
            block,
            "Context (for reference only):\n\
Clipboard: a日本\n",
            "a multibyte source whose scalar count equals max_chars is kept whole"
        );
    }

    #[test]
    fn context_block_truncates_a_multibyte_tail_on_a_char_boundary() {
        // tail_chars is the crate's ONLY byte-offset slice (`&s[byte_idx..]`
        // after `char_indices().nth()`); that indirection is what keeps the cut
        // on a char boundary. The ASCII bounds test above would still pass with
        // naive `&s[count-max..]` byte indexing — this pins the multibyte path
        // (clipboard/screen context routinely carries CJK/emoji), where naive
        // indexing would panic mid-codepoint.
        // "a日本cd" = 5 chars; keeping the last 3 must cut at 本's start byte (4),
        // not byte index 2 (which lands inside 日).
        let block = build_context_block(Some("a日本cd"), None, &[], 3);
        assert!(block.contains("Clipboard: 本cd"), "got {block:?}");
        // Astral (4-byte) tail: keep the last 2 of "x😀y" → "😀y".
        let astral = build_context_block(Some("x😀y"), None, &[], 2);
        assert!(astral.contains("Clipboard: 😀y"), "got {astral:?}");
    }

    #[test]
    fn context_block_tail_cuts_through_a_combining_mark_grapheme_on_a_char_boundary() {
        // tail_chars counts Unicode scalars, not graphemes. "e" + U+0301 (combining
        // acute) is ONE grapheme but TWO scalars; "abe\u{0301}" is 4 scalars. Keeping
        // the last 2 scalars must cut between "b" and "e" (NOT mid-grapheme between
        // "e" and the combining mark, and never mid-codepoint): the tail is
        // "e\u{0301}". The byte-offset slice in tail_chars must land on a char
        // boundary or this panics rather than asserting.
        let block = build_context_block(Some("abe\u{0301}"), None, &[], 2);
        assert!(block.contains("Clipboard: e\u{0301}"), "got {block:?}");
        // And cutting AT the combining mark (keep last 1 scalar) keeps the bare mark,
        // still on a boundary — char-count semantics, no codepoint split.
        let one = build_context_block(Some("abe\u{0301}"), None, &[], 1);
        assert!(one.contains("Clipboard: \u{0301}"), "got {one:?}");
    }

    #[test]
    fn context_block_truncates_each_source_identically() {
        // build_context_block bounds pasteboard/screen/previous_inputs symmetrically:
        // each source is independently whitespace-collapsed and tail-truncated to the
        // SAME max_chars rule. Feed the identical over-long value into all three and
        // assert each is cut to the same tail. The bounds tests above only pin the
        // clipboard arm; this pins that screen and Recent share the rule.
        let long = "0123456789abcdef"; // 16 chars, no whitespace
        let block = build_context_block(Some(long), Some(long), &[long], 4);
        assert_eq!(
            block,
            "Context (for reference only):\n\
Clipboard: cdef\n\
On screen: cdef\n\
Recent: cdef\n"
        );
    }

    #[test]
    fn context_block_bounds_after_whitespace_collapse_per_source() {
        let block = build_context_block(
            Some("clip\none two three"),
            Some("screen\nalpha beta"),
            &["recent\nred green blue"],
            10,
        );

        assert_eq!(
            block,
            "Context (for reference only):\n\
Clipboard:  two three\n\
On screen: alpha beta\n\
Recent: green blue\n"
        );
    }

    #[test]
    fn max_zero_yields_no_context_not_the_full_source() {
        // A 0 bound must mean "nothing", not "unbounded" (review #1).
        assert_eq!(
            build_context_block(Some("anything"), None, &["more"], 0),
            ""
        );
    }

    #[test]
    fn word_at_caret_returns_whole_word_and_scalar_range_at_end() {
        assert_eq!(
            word_at_caret("please fix teh", 14),
            Some(WordAtCaret {
                word: "teh",
                range: WordRange { start: 11, end: 14 },
            })
        );
    }

    #[test]
    fn word_at_caret_returns_whole_word_and_scalar_range_mid_word() {
        assert_eq!(
            word_at_caret("please fix teh now", 12),
            Some(WordAtCaret {
                word: "teh",
                range: WordRange { start: 11, end: 14 },
            })
        );
    }

    #[test]
    fn word_at_caret_handles_astral_prefix_without_utf16_offset_drift() {
        assert_eq!(
            word_at_caret("😀teh", 2),
            Some(WordAtCaret {
                word: "teh",
                range: WordRange { start: 1, end: 4 },
            })
        );
    }

    #[test]
    fn word_at_caret_returns_previous_word_at_boundary_and_none_for_empty_field() {
        assert_eq!(word_at_caret("", 0), None);
        assert_eq!(
            word_at_caret("hello world", 5),
            Some(WordAtCaret {
                word: "hello",
                range: WordRange { start: 0, end: 5 },
            })
        );
        assert_eq!(
            word_at_caret("hello world", 6),
            Some(WordAtCaret {
                word: "world",
                range: WordRange { start: 6, end: 11 },
            })
        );
        assert_eq!(word_at_caret("  ", 1), None);
    }

    #[test]
    fn word_at_caret_treats_apostrophe_as_a_word_char() {
        // is_word_char (lib.rs L69) allows `'` so contractions stay whole: the
        // caret inside "don't" must return the entire "don't", not a fragment
        // split at the apostrophe. Nothing else pins the `|| c == '\''` arm; drop
        // it and this word splits to "t"/"don".
        assert_eq!(
            word_at_caret("don't", 5),
            Some(WordAtCaret {
                word: "don't",
                range: WordRange { start: 0, end: 5 },
            })
        );
    }

    #[test]
    fn word_at_caret_past_end_caret_clamps_to_len() {
        // A past-end caret is clamped to the scalar count (the `caret.min(len)`
        // at lib.rs L38), so a wild caret still resolves the trailing word
        // instead of panicking or missing it...
        assert_eq!(
            word_at_caret("please fix teh", 99),
            Some(WordAtCaret {
                word: "teh",
                range: WordRange { start: 11, end: 14 },
            })
        );
        // ...and clamping onto trailing whitespace still yields None (the
        // clamped end-of-text char is not a word char).
        assert_eq!(word_at_caret("hi ", 99), None);
    }

    #[test]
    fn word_at_caret_at_offset_zero_on_nonempty_field() {
        // caret 0 on a non-empty field is only tested on "" today. When the first
        // char is a word char the caret sits at a word's start and the whole word
        // is returned (the `caret < len && is_word_char(chars[caret])` seed arm at
        // offset 0). When the first char is NOT a word char there is nothing to the
        // left, so `caret.checked_sub(1)?` yields None rather than underflow-panicking.
        assert_eq!(
            word_at_caret("hi there", 0),
            Some(WordAtCaret {
                word: "hi",
                range: WordRange { start: 0, end: 2 },
            })
        );
        assert_eq!(word_at_caret(" x", 0), None);
    }

    #[test]
    fn newlines_in_sources_are_collapsed() {
        // A recorded entry with newlines must not masquerade as a new directive
        // line or escape the block (review #5).
        let block = build_context_block(Some("line one\nContext: fake\nline two"), None, &[], 200);
        assert_eq!(block.matches('\n').count(), 2); // header line + the single source line
        assert!(block.contains("Clipboard: line one Context: fake line two"));
    }

    #[test]
    fn right_context_past_end_is_empty() {
        assert_eq!(right_context("hi", 99), "");
    }

    #[test]
    fn right_context_char_safe() {
        assert_eq!(right_context("héllo", 2), "llo");
    }

    #[test]
    fn context_split_is_char_based_for_cjk_and_multibyte() {
        // Caret is a character index, so a 2-byte char (é) and a 3-byte char
        // (日) each count as one. Caret 3 lands just before the final "b".
        assert_eq!(left_context("aé日b", 3), "aé日");
        assert_eq!(right_context("aé日b", 3), "b");
    }

    #[test]
    fn context_is_scalar_safe_for_astral_4byte_chars() {
        // 😀 is one Unicode scalar but 4 UTF-8 bytes (and 2 UTF-16 units). Caret
        // is a scalar index, so caret 2 lands just after "a😀".
        assert_eq!(left_context("a😀b", 2), "a😀");
        assert_eq!(right_context("a😀b", 2), "b");
    }

    #[test]
    fn context_split_is_scalar_not_grapheme_for_combining_marks() {
        // "e" + U+0301 (combining acute) is one grapheme but two scalars. The
        // caret is a scalar index, so caret 1 splits the grapheme — left keeps the
        // base "e", right starts with the combining mark. This pins the API as
        // scalar-based (callers must feed scalar carets, as wiring does).
        assert_eq!(left_context("e\u{0301}x", 1), "e");
        assert_eq!(right_context("e\u{0301}x", 1), "\u{0301}x");
    }

    #[test]
    fn left_context_at_exact_len_returns_all() {
        // Caret exactly at the scalar count (not past-end) is the boundary case.
        assert_eq!(left_context("hé日", 3), "hé日");
        assert_eq!(right_context("hé日", 3), "");
    }

    #[test]
    fn trim_trailing_strips_trailing_whitespace() {
        assert_eq!(trim_trailing("hi  \n\t"), "hi");
    }

    #[test]
    fn trim_trailing_preserves_leading_whitespace() {
        assert_eq!(trim_trailing("  hi "), "  hi");
    }

    #[test]
    fn left_context_empty_string_is_empty() {
        assert_eq!(left_context("", 5), "");
    }

    #[test]
    fn right_context_caret_zero_returns_all() {
        assert_eq!(right_context("hello", 0), "hello");
    }

    #[test]
    fn right_context_empty_string_is_empty() {
        assert_eq!(right_context("", 3), "");
    }

    #[test]
    fn trim_trailing_all_whitespace_is_empty() {
        assert_eq!(trim_trailing("  \n\t"), "");
    }

    #[test]
    fn trim_trailing_empty_string_is_empty() {
        assert_eq!(trim_trailing(""), "");
    }
}
