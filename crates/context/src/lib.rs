//! Pure text-context helpers around a caret.

pub fn left_context(value: &str, caret: usize) -> String {
    value.chars().take(caret).collect()
}

pub fn right_context(value: &str, caret: usize) -> String {
    value.chars().skip(caret).collect()
}

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
    previous_inputs: &[&str],
    max_chars: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(clip) = pasteboard {
        let clip = clip.trim();
        if !clip.is_empty() {
            lines.push(format!("Clipboard: {}", tail_chars(clip, max_chars)));
        }
    }
    for input in previous_inputs {
        let input = input.trim();
        if !input.is_empty() {
            lines.push(format!("Recent: {}", tail_chars(input, max_chars)));
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
        assert_eq!(build_context_block(None, &[], 100), "");
        assert_eq!(build_context_block(Some("   "), &["  "], 100), "");
    }

    #[test]
    fn context_block_includes_clipboard() {
        let block = build_context_block(Some("paste me"), &[], 100);
        assert!(block.contains("Clipboard: paste me"));
        assert!(block.starts_with("Context (for reference only):"));
    }

    #[test]
    fn context_block_includes_previous_inputs() {
        let block = build_context_block(None, &["first note", "second note"], 100);
        assert!(block.contains("Recent: first note"));
        assert!(block.contains("Recent: second note"));
    }

    #[test]
    fn context_block_bounds_each_source_to_max_chars_keeping_the_tail() {
        let long = "0123456789abcdef"; // 16 chars
        let block = build_context_block(Some(long), &[], 4);
        assert!(block.contains("Clipboard: cdef"), "got {block:?}");
        assert!(!block.contains("0123"));
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
