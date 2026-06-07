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

pub fn trim_prefix(value: &str) -> &str {
    value.trim_end()
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
    fn trim_prefix_strips_trailing_whitespace() {
        assert_eq!(trim_prefix("hi  \n\t"), "hi");
    }

    #[test]
    fn trim_prefix_preserves_leading_whitespace() {
        assert_eq!(trim_prefix("  hi "), "  hi");
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
    fn trim_prefix_all_whitespace_is_empty() {
        assert_eq!(trim_prefix("  \n\t"), "");
    }

    #[test]
    fn trim_prefix_empty_string_is_empty() {
        assert_eq!(trim_prefix(""), "");
    }
}
