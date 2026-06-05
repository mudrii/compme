//! Candidate shaping and lightweight ranking helpers.

pub fn cap_words(text: &str, max_words: usize) -> String {
    text.split_whitespace()
        .take(max_words)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn next_word(text: &str) -> (String, String) {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        [] => (String::new(), String::new()),
        [only] => ((*only).to_string(), String::new()),
        [first, rest @ ..] => (format!("{first} "), rest.join(" ")),
    }
}

pub fn repetition_penalty(candidate: &str, recent: &str) -> f64 {
    let candidate: Vec<String> = candidate
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if candidate.is_empty() {
        return 1.0;
    }

    let recent: Vec<String> = recent
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();

    // Penalize only when the candidate appears as a contiguous run of whole
    // words in the recent text — substring matches like "cat" in "category"
    // must not count as repetition.
    if recent.windows(candidate.len()).any(|run| run == candidate) {
        0.25
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_words_caps() {
        assert_eq!(cap_words("one two three four", 2), "one two");
    }

    #[test]
    fn cap_words_fewer_returns_all() {
        assert_eq!(cap_words("a b", 5), "a b");
    }

    #[test]
    fn cap_words_zero_is_empty() {
        assert_eq!(cap_words("a b", 0), "");
    }

    #[test]
    fn cap_words_normalizes_whitespace() {
        assert_eq!(cap_words("  one   two \n three ", 2), "one two");
    }

    #[test]
    fn cap_words_handles_unicode_words() {
        assert_eq!(cap_words("hello 世界 again", 2), "hello 世界");
    }

    #[test]
    fn next_word_splits_with_trailing_space() {
        assert_eq!(
            next_word("hello world foo"),
            ("hello ".to_string(), "world foo".to_string())
        );
    }

    #[test]
    fn next_word_single_word_has_empty_remainder() {
        assert_eq!(next_word("hello"), ("hello".to_string(), String::new()));
    }

    #[test]
    fn next_word_skips_leading_whitespace() {
        assert_eq!(
            next_word("  hello world"),
            ("hello ".to_string(), "world".to_string())
        );
    }

    #[test]
    fn next_word_empty_is_empty() {
        assert_eq!(next_word(" \n\t"), (String::new(), String::new()));
    }

    #[test]
    fn repetition_penalty_is_full_for_new_text() {
        assert_eq!(repetition_penalty("new phrase", "old phrase"), 1.0);
    }

    #[test]
    fn repetition_penalty_is_low_for_exact_recent_text() {
        assert!(repetition_penalty("repeat me", "please repeat me now") < 0.5);
    }

    #[test]
    fn repetition_penalty_is_case_insensitive() {
        assert!(repetition_penalty("Repeat Me", "please repeat me now") < 0.5);
    }

    #[test]
    fn repetition_penalty_ignores_empty_candidate() {
        assert_eq!(repetition_penalty("", "anything"), 1.0);
    }

    #[test]
    fn repetition_penalty_does_not_match_inside_a_word() {
        // "cat" must not be treated as a repeat of "category".
        assert_eq!(repetition_penalty("cat", "category list"), 1.0);
    }

    #[test]
    fn repetition_penalty_matches_contiguous_word_run() {
        assert!(repetition_penalty("repeat me", "you repeat me now") < 0.5);
        // Same words, non-contiguous, is not a repeat.
        assert_eq!(repetition_penalty("repeat now", "repeat me now"), 1.0);
    }
}
