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
}
