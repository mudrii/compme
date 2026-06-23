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

/// The last `n` scalars of the text before the caret. `caret` is a
/// Unicode-scalar offset (see [`left_context`] for the conversion obligation);
/// both `caret` and `n` are clamped to what exists — never panics.
pub fn left_tail(value: &str, caret: usize, n: usize) -> String {
    if n == 0 {
        return String::new();
    }

    let left: Vec<char> = value.chars().take(caret).collect();
    left[left.len().saturating_sub(n)..].iter().collect()
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
    match s.char_indices().nth(count - max) {
        Some((byte_idx, _)) => &s[byte_idx..],
        None => s,
    }
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
        assert_eq!(left_tail("a😀b", 2, 1), "😀");
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
    fn left_tail_caret_zero_is_empty() {
        assert_eq!(left_tail("abc", 0, 2), "");
    }

    #[test]
    fn left_context_at_exact_len_returns_all() {
        // Caret exactly at the scalar count (not past-end) is the boundary case.
        assert_eq!(left_context("hé日", 3), "hé日");
        assert_eq!(right_context("hé日", 3), "");
    }

    #[test]
    fn left_tail_last_n() {
        assert_eq!(left_tail("abcdefgh", 8, 3), "fgh");
    }

    #[test]
    fn left_tail_before_caret() {
        assert_eq!(left_tail("abcdefgh", 5, 3), "cde");
    }

    #[test]
    fn left_tail_zero_is_empty() {
        assert_eq!(left_tail("abc", 3, 0), "");
    }

    #[test]
    fn left_tail_char_safe() {
        assert_eq!(left_tail("aé日b", 3, 2), "é日");
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
    fn left_tail_n_exceeds_available_returns_all() {
        assert_eq!(left_tail("abc", 1, 5), "a");
    }

    #[test]
    fn left_tail_caret_past_end_returns_all_left() {
        assert_eq!(left_tail("abc", 99, 2), "bc");
    }

    #[test]
    fn left_tail_empty_string_is_empty() {
        assert_eq!(left_tail("", 3, 4), "");
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
