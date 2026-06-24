//! Candidate shaping and lightweight ranking helpers.

/// Trim a raw completion at the first hard stop boundary.
///
/// Inline completion is a single visual line: a model that runs on into a new
/// paragraph or list must be cut at the first line break before any word
/// capping happens. This is the "aggressive stop sequence" lever called out in
/// the MVP spec (newline/sentence boundary). This stops at the first `\n`/`\r`;
/// sentence-boundary shaping is handled separately by `truncate_at_sentence_end`.
pub fn trim_to_stop_boundary(text: &str) -> &str {
    match text.find(['\n', '\r']) {
        Some(index) => &text[..index],
        None => text,
    }
}

/// Keep at most `max_words` whitespace-separated words, re-joined with single
/// spaces. Whitespace runs are normalized as a side effect, so the output is
/// not a substring of the input — callers must not use it for offset math
/// against the original text. `max_words == 0` yields the empty string.
pub fn cap_words(text: &str, max_words: usize) -> String {
    text.split_whitespace()
        .take(max_words)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split a completion into the next word to insert and the remainder. The word
/// carries one trailing space iff a remainder follows, so word-by-word accepts
/// concatenate back to the (whitespace-normalized) whole — the engine's caret
/// advance counts on that exact spacing. Empty/whitespace-only input yields
/// two empty strings.
pub fn next_word(text: &str) -> (String, String) {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        [] => (String::new(), String::new()),
        [only] => ((*only).to_string(), String::new()),
        [first, rest @ ..] => (format!("{first} "), rest.join(" ")),
    }
}

/// Truncate a completion at the end of its first sentence.
///
/// Inline completion offers a short continuation, not a paragraph. When a small
/// model runs past a sentence terminator (`.`/`!`/`?`) we cut there. A terminator
/// only counts when followed by whitespace or end-of-text, so `3.14` and `e.g.`
/// are not mistaken for sentence ends.
pub fn truncate_at_sentence_end(text: &str) -> &str {
    let bytes = text.as_bytes();
    for (index, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            let next = bytes.get(index + ch.len_utf8());
            if next.is_none_or(|b| b.is_ascii_whitespace()) {
                return &text[..index + ch.len_utf8()];
            }
        }
    }
    text
}

/// Drop a trailing run of `candidate` words that the user already has to the
/// right of the caret.
///
/// Small models regurgitate text after the caret: with caret in `the quick| fox`
/// the model may return `quick brown fox`, which would insert a duplicate `fox`.
/// We compare words case- and punctuation-insensitively and truncate the
/// candidate where its tail re-states the start of the right context.
pub fn strip_suffix_overlap(candidate: &str, right: &str) -> String {
    fn normalize(word: &str) -> String {
        word.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    let cand: Vec<&str> = candidate.split_whitespace().collect();
    let cand_norm: Vec<String> = cand.iter().map(|w| normalize(w)).collect();
    let right_norm: Vec<String> = right.split_whitespace().map(normalize).collect();
    let max_overlap = cand.len().min(right_norm.len());
    for k in (1..=max_overlap).rev() {
        let tail = &cand_norm[cand_norm.len() - k..];
        let head = &right_norm[..k];
        // Require a real word overlap. A pure-punctuation token normalizes to ""
        // and would otherwise match another "" spuriously, dropping punctuation
        // the model legitimately produced.
        if tail.iter().chain(head).any(String::is_empty) {
            continue;
        }
        if tail == head {
            return cand[..cand.len() - k].join(" ");
        }
    }
    candidate.to_string()
}

/// Report whether a completion is a single word or phrase repeated three or more
/// times (`the the the`, `go home go home go home`) — a classic small-model
/// degenerate loop that should be dropped rather than shown.
pub fn is_degenerate_repetition(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    let len = words.len();
    if len < 3 {
        return false;
    }
    for phrase_len in 1..=len / 3 {
        // A non-dividing `phrase_len` leaves a short final chunk that can never
        // equal the full-length phrase, so `all` returns false on its own — no
        // separate divisibility guard (and no `is_multiple_of`/MSRV bump) needed.
        let phrase = &words[..phrase_len];
        if words.chunks(phrase_len).all(|chunk| chunk == phrase) {
            return true;
        }
    }
    false
}

/// Score multiplier for a candidate against recently typed text: 0.25 when the
/// candidate appears as a contiguous, case-insensitive whole-word run inside
/// `recent`, else 1.0. Word-level only — substring hits ("cat" in "category")
/// never count — and an empty candidate is never penalized. Callers drop
/// candidates whose multiplier falls below their floor; the two return values
/// are the entire contract (this is a gate, not a smooth score).
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
    fn trim_to_stop_boundary_cuts_at_first_newline() {
        assert_eq!(
            trim_to_stop_boundary("inline part\nsecond paragraph"),
            "inline part"
        );
    }

    #[test]
    fn trim_to_stop_boundary_cuts_at_carriage_return() {
        assert_eq!(trim_to_stop_boundary("first\r\nsecond"), "first");
    }

    #[test]
    fn trim_to_stop_boundary_cuts_at_bare_carriage_return() {
        assert_eq!(trim_to_stop_boundary("a\rb"), "a");
    }

    #[test]
    fn trim_to_stop_boundary_keeps_single_line_intact() {
        assert_eq!(
            trim_to_stop_boundary("one clean inline line"),
            "one clean inline line"
        );
    }

    #[test]
    fn trim_to_stop_boundary_empty_when_leading_newline() {
        assert_eq!(trim_to_stop_boundary("\nrest"), "");
    }

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
    fn next_word_pieces_concatenate_to_the_normalized_whole() {
        // next_word's contract (lib.rs L28-32): the emitted word carries one
        // trailing space iff a remainder follows, so successive word-by-word
        // accepts concatenate back to the whitespace-normalized whole (the
        // engine's caret advance depends on that exact spacing). Drive the full
        // word-by-word loop over irregular whitespace and assert the accumulation
        // equals the normalized full completion (single-spaced words).
        let raw = "  the\tquick   brown\nfox ";
        let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");

        let mut accumulated = String::new();
        let mut remainder = raw.to_string();
        loop {
            let (word, rest) = next_word(&remainder);
            if word.is_empty() {
                break; // exhausted: empty/whitespace-only remainder
            }
            accumulated.push_str(&word);
            if rest.is_empty() {
                break; // last word carries no trailing space
            }
            remainder = rest;
        }

        assert_eq!(accumulated, normalized);
        assert_eq!(accumulated, "the quick brown fox");
    }

    #[test]
    fn repetition_penalty_is_full_for_new_text() {
        assert_eq!(repetition_penalty("new phrase", "old phrase"), 1.0);
    }

    #[test]
    fn repetition_penalty_is_low_for_exact_recent_text() {
        assert_eq!(
            repetition_penalty("repeat me", "please repeat me now"),
            0.25
        );
    }

    #[test]
    fn repetition_penalty_is_case_insensitive() {
        assert_eq!(
            repetition_penalty("Repeat Me", "please repeat me now"),
            0.25
        );
    }

    #[test]
    fn repetition_penalty_is_case_insensitive_on_recent_context() {
        // The sibling case-insensitivity test folds the CANDIDATE; this pins
        // that the RECENT context is lowercased too (the `recent.to_lowercase()`
        // at the top of the fn). An uppercase recent context still penalizes a
        // lowercase candidate echoing it, exactly as the all-lowercase recent
        // does — so casing of the surrounding text never leaks a degenerate echo.
        let upper_recent = repetition_penalty("repeat me", "PLEASE REPEAT ME NOW");
        let lower_recent = repetition_penalty("repeat me", "please repeat me now");
        assert_eq!(upper_recent, lower_recent);
        assert_eq!(upper_recent, 0.25); // a match, not the 1.0 no-match floor
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
        assert_eq!(repetition_penalty("repeat me", "you repeat me now"), 0.25);
        // Same words, non-contiguous, is not a repeat.
        assert_eq!(repetition_penalty("repeat now", "repeat me now"), 1.0);
    }

    #[test]
    fn repetition_penalty_candidate_longer_than_recent_is_full() {
        assert_eq!(repetition_penalty("a b c d", "a b"), 1.0);
    }

    #[test]
    fn repetition_penalty_candidate_equals_recent_is_low() {
        assert_eq!(repetition_penalty("hello there", "hello there"), 0.25);
    }

    #[test]
    fn repetition_penalty_empty_recent_is_full() {
        assert_eq!(repetition_penalty("hello", ""), 1.0);
    }

    #[test]
    fn repetition_penalty_single_word_whole_match_is_low() {
        assert_eq!(repetition_penalty("world", "hello world today"), 0.25);
    }

    #[test]
    fn repetition_penalty_full_for_substring_not_word() {
        // "cat" is a substring of "category" but not a whole-word run, so the
        // word-level matcher must NOT penalize it — the full (no-match) 1.0.
        assert_eq!(repetition_penalty("cat", "category list"), 1.0);
    }

    #[test]
    fn repetition_penalty_full_for_reordered_words() {
        // The same words in a different (non-contiguous) order are not a
        // contiguous run, so no penalty: "repeat now" never appears as an
        // adjacent pair inside "repeat me now". Full (no-match) 1.0.
        assert_eq!(repetition_penalty("repeat now", "repeat me now"), 1.0);
    }

    #[test]
    fn repetition_penalty_full_when_candidate_longer_than_recent() {
        // A candidate with MORE words than the recent context can never be a
        // contiguous run inside it — `recent.windows(candidate.len())` yields no
        // windows when the candidate is longer (windows(n) on a shorter slice is
        // empty; windows(0) would panic, but an empty candidate is short-circuited
        // earlier). Pins the operand order: full (no-match) 1.0, never a panic.
        assert_eq!(repetition_penalty("a b c d", "a b"), 1.0);
    }

    #[test]
    fn cap_words_empty_string_is_empty() {
        assert_eq!(cap_words("", 4), "");
    }

    #[test]
    fn cap_words_whitespace_only_is_empty() {
        assert_eq!(cap_words("   \n\t", 4), "");
    }

    #[test]
    fn next_word_two_words_splits_cleanly() {
        assert_eq!(
            next_word("hello world"),
            ("hello ".to_string(), "world".to_string())
        );
    }

    #[test]
    fn truncate_at_sentence_end_cuts_after_first_terminator() {
        assert_eq!(
            truncate_at_sentence_end("Hello there. More text here"),
            "Hello there."
        );
    }

    #[test]
    fn truncate_at_sentence_end_keeps_decimal() {
        // A period embedded in a decimal ("3.14") is followed by a digit, not
        // whitespace, so it is not a sentence terminator — the text is kept whole.
        assert_eq!(truncate_at_sentence_end("pi is 3.14 ish"), "pi is 3.14 ish");
    }

    #[test]
    fn truncate_at_sentence_end_handles_question_and_exclaim() {
        assert_eq!(truncate_at_sentence_end("Really? yes"), "Really?");
        assert_eq!(truncate_at_sentence_end("Stop! now"), "Stop!");
    }

    #[test]
    fn truncate_at_sentence_end_keeps_decimals() {
        // A period not followed by whitespace is not a sentence end, so numeric
        // decimals survive — the critical false-positive to avoid.
        assert_eq!(truncate_at_sentence_end("3.14 is pi"), "3.14 is pi");
        assert_eq!(
            truncate_at_sentence_end("version 1.2 ships"),
            "version 1.2 ships"
        );
    }

    #[test]
    fn truncate_at_sentence_end_cuts_abbreviation_period_known_limitation() {
        // A period+space is treated as a sentence end, so abbreviations like
        // "e.g." are cut. Acceptable for a dictionary-free heuristic; the model
        // rarely opens an inline completion with one.
        assert_eq!(truncate_at_sentence_end("e.g. this"), "e.g.");
    }

    #[test]
    fn truncate_at_sentence_end_keeps_unterminated_text() {
        assert_eq!(
            truncate_at_sentence_end("just a continuation"),
            "just a continuation"
        );
    }

    #[test]
    fn truncate_at_sentence_end_includes_trailing_terminator_only() {
        assert_eq!(truncate_at_sentence_end("Done."), "Done.");
    }

    #[test]
    fn truncate_at_sentence_end_keeps_terminator_followed_by_multibyte_char() {
        // The `next` byte after the terminator is the first byte of a multibyte
        // scalar (`世`, 0xE4), which is not ASCII whitespace, so the terminator
        // is NOT a sentence end and the text is kept whole. Pins the non-ASCII
        // branch of the `is_ascii_whitespace` check (only ASCII space + decimals
        // were pinned before).
        assert_eq!(truncate_at_sentence_end("Done.世界"), "Done.世界");
    }

    #[test]
    fn strip_suffix_overlap_removes_words_already_after_caret() {
        // caret in "the quick| fox"; right context is "fox"; model returned
        // "quick brown fox" — the trailing "fox" must be dropped.
        assert_eq!(
            strip_suffix_overlap("quick brown fox", "fox"),
            "quick brown"
        );
    }

    #[test]
    fn strip_suffix_overlap_removes_multi_word_overlap() {
        assert_eq!(
            strip_suffix_overlap("see you later today", "later today maybe"),
            "see you"
        );
    }

    #[test]
    fn strip_suffix_overlap_is_punctuation_and_case_insensitive() {
        assert_eq!(strip_suffix_overlap("hello World", "world!"), "hello");
    }

    #[test]
    fn strip_suffix_overlap_keeps_candidate_without_overlap() {
        assert_eq!(
            strip_suffix_overlap("hello world", "xyz abc"),
            "hello world"
        );
    }

    #[test]
    fn strip_suffix_overlap_ignores_punctuation_only_overlap() {
        // "!" normalizes to "" — an empty normalized token must NOT count as a
        // real word overlap, or punctuation the model emitted gets dropped.
        assert_eq!(strip_suffix_overlap("see !", "! ok"), "see !");
        assert_eq!(strip_suffix_overlap("wait ...", "... more"), "wait ...");
    }

    #[test]
    fn strip_suffix_overlap_empties_when_all_words_overlap() {
        // Model echoed the entire right context; the whole candidate is overlap.
        assert_eq!(strip_suffix_overlap("brown fox", "brown fox more"), "");
    }

    #[test]
    fn strip_suffix_overlap_strips_multi_word_tail_when_candidate_is_longer() {
        // candidate has more words than right; only the overlapping tail goes.
        assert_eq!(
            strip_suffix_overlap("one two three four", "three four five"),
            "one two"
        );
    }

    #[test]
    fn strip_suffix_overlap_misses_overlap_straddling_punctuation_by_design() {
        // "you ..." straddles a punctuation-only token (`...` → ""); the
        // empty-skip guard deliberately leaves it rather than risk dropping
        // legitimately-produced punctuation. A redundant suggestion is safer
        // than mangled inserted text.
        assert_eq!(
            strip_suffix_overlap("see you ...", "you ... more"),
            "see you ..."
        );
    }

    #[test]
    fn strip_suffix_overlap_empty_right_keeps_candidate() {
        assert_eq!(strip_suffix_overlap("hello world", ""), "hello world");
        // The mirror side: an empty candidate against a non-empty right yields
        // "" (max_overlap is 0, the loop never runs). Distinguishes the
        // empty-side guard from a broken overlap loop that might fabricate text.
        assert_eq!(strip_suffix_overlap("", "x y"), "");
    }

    #[test]
    fn strip_suffix_overlap_prefers_longest_overlap() {
        // The overlap search runs longest-first (`(1..=max_overlap).rev()`), so
        // when both a 2-word ("go go") and a 1-word ("go") tail match the right
        // context's head, the longer one wins and more of the candidate is
        // stripped. candidate "go go go" vs right "go go now": the 2-word tail
        // "go go" equals the right head "go go", leaving just "go". A
        // shortest-first search would match the 1-word "go" first and wrongly
        // leave "go go", so this pins the `.rev()` ordering.
        assert_eq!(strip_suffix_overlap("go go go", "go go now"), "go");
    }

    #[test]
    fn is_degenerate_repetition_flags_single_word_loop() {
        assert!(is_degenerate_repetition("the the the"));
    }

    #[test]
    fn is_degenerate_repetition_flags_through_irregular_whitespace() {
        // split_whitespace normalizes runs of spaces/tabs/newlines, so a loop
        // with irregular spacing is still detected — pins the normalize-then-
        // detect contract a future tokenizer change must preserve.
        assert!(is_degenerate_repetition("the\t the  the"));
        assert!(is_degenerate_repetition("go go\n go"));
    }

    #[test]
    fn is_degenerate_repetition_flags_repeated_phrase() {
        assert!(is_degenerate_repetition("go home go home go home"));
    }

    #[test]
    fn is_degenerate_repetition_ignores_normal_text() {
        assert!(!is_degenerate_repetition("the quick brown fox"));
    }

    #[test]
    fn is_degenerate_repetition_ignores_short_text() {
        assert!(!is_degenerate_repetition("hello"));
        assert!(!is_degenerate_repetition("hello world"));
        // Three distinct words DO enter the detection loop (range 1..=len/3 is
        // 1..=1), so this pins the short-text guard threshold: the loop runs but
        // finds no repeat, rather than the len<3 early return short-circuiting.
        assert!(!is_degenerate_repetition("hello there friend"));
    }

    #[test]
    fn is_degenerate_repetition_flags_three_word_phrase_at_loop_boundary() {
        // phrase_len 3 sits at the `1..=len/3` upper bound for len 9 — pins the
        // boundary so an off-by-one (`<len/3`) regression would be caught.
        assert!(is_degenerate_repetition("a b c a b c a b c"));
    }

    #[test]
    fn is_degenerate_repetition_ignores_divisible_but_non_repeating() {
        // len 6 divides by 1/2/3, but no phrase tiles it — must not be flagged.
        assert!(!is_degenerate_repetition("a b a b c d"));
        assert!(!is_degenerate_repetition("one two three four five six"));
    }

    #[test]
    fn is_degenerate_repetition_ignores_two_repeats() {
        // Two repeats is a real phrase ("bye bye"), not a degenerate loop.
        assert!(!is_degenerate_repetition("bye bye"));
    }

    #[test]
    fn is_degenerate_repetition_ignores_phrase_repeated_only_twice() {
        // A multi-word phrase tiled exactly twice ("a b a b", "one two one two")
        // is NOT degenerate: the `1..=len/3` divisor demands 3+ tilings, so for
        // len 4 the loop runs only phrase_len 1 and finds no single-word loop. A
        // `/2` mutant would let phrase_len reach 2 and wrongly flag these.
        assert!(!is_degenerate_repetition("a b a b"));
        assert!(!is_degenerate_repetition("one two one two"));
    }
}
