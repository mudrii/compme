//! Redaction of sensitive text before any persistence or diagnostics (design
//! spec §6/§7: "Redaction before any persistence — emails, card-like numbers
//! (Luhn), tokens/secrets"). Pure: text in, redacted text out.
//!
//! This is a best-effort scrubber, not a guarantee — it removes the obvious,
//! high-risk PII classes so accepted-completion memory and diagnostics never
//! store raw secrets. Passes run email → secret → card so a long email local
//! part is redacted whole rather than fragmented by the secret pass.
//!
//! When in doubt it OVER-redacts (privacy over fidelity): a Luhn-valid 13–19
//! digit run is scrubbed even if it is not actually a card, and a 32+ char
//! mixed-entropy token is scrubbed even if benign. False positives lose a bit
//! of stored context; false negatives would leak a secret — so the bias is
//! deliberate and one-directional.

use std::sync::OnceLock;

use regex::Regex;

/// Known credential prefixes that are always redacted when matched, regardless
/// of length/entropy. AWS (long-term + STS), Google, Slack, GitHub, GitLab,
/// SendGrid, Stripe-style.
const KEY_PREFIXES: &[&str] = &[
    "AKIA", "ASIA", "AIza", "xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-", "whsec_", "glpat-", "SG.",
    "sk-", "sk_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "pk-", "pk_", "rk-", "rk_",
];

/// Matches API-key / secret-like tokens: vendor-prefixed keys and long
/// high-entropy tokens (base64/base64url incl. padding and JWT dots).
fn secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
              (?:AKIA|ASIA)[0-9A-Z]{16}
            | AIza[0-9A-Za-z_\-]{16,}
            | xox[baprs]-[A-Za-z0-9-]{10,}
            | SG\.[A-Za-z0-9._-]{10,}
            | (?:whsec_|glpat-)[A-Za-z0-9_-]{16,}
            | (?:sk|ghp|gho|ghu|ghs|ghr|pk|rk)[-_][A-Za-z0-9_-]{16,}
            | [A-Za-z0-9+/=._-]{32,}
            ",
        )
        .expect("secret regex")
    })
}

/// Whether a generic long token looks high-entropy enough to be a secret rather
/// than a long word: it has a digit, mixed case, or base64 punctuation. (An
/// all-one-case all-letter 32+ run — rare for a secret — is left alone.)
fn looks_high_entropy(token: &str) -> bool {
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_b64_punct = token.contains(['+', '/', '=']);
    has_digit || (has_upper && has_lower) || has_b64_punct
}

/// Matches a *maximal* run of ASCII digits optionally interleaved with the card
/// separators (whitespace, dash, no-break space). The 13–19-digit Luhn windowing
/// happens inside the run (`redact_card_run`) so two cards separated only by a
/// separator are each detected, rather than a greedy span straddling the card
/// boundary and failing Luhn over the merged digits (which leaked both PANs).
fn card_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d(?:[\s\u{00a0}-]*\d)*").expect("card run regex"))
}

/// Redact every Luhn-valid 13–19-digit window inside one digit/separator run by
/// sliding a longest-first window across the run's digits. This catches both
/// cards separated only by a separator (the greedy span used to straddle the
/// boundary and fail Luhn over the merged digits) AND a PAN abutted by extra
/// digits with no separator (e.g. PAN+CVV or PAN glued to an order id), where a
/// single boundary-aligned span would exceed 19 digits and miss the embedded
/// card. Longest window first, then skip past it, keeps the over-redaction bias
/// (privacy > fidelity) without shredding a number into overlapping fragments.
/// A run with no embedded Luhn window (e.g. a non-card 16-digit order id)
/// survives untouched.
fn redact_card_run(run: &str) -> String {
    // Byte offset of each ASCII digit (digits are 1 byte; separators may be
    // multi-byte, e.g. NBSP — but every redaction boundary lands on a digit
    // offset, so the span slices are always valid UTF-8 boundaries).
    let digit_pos: Vec<usize> = run
        .char_indices()
        .filter(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i)
        .collect();
    if digit_pos.len() < 13 {
        return run.to_string();
    }
    let digits: Vec<u8> = digit_pos.iter().map(|&i| run.as_bytes()[i]).collect();

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < digits.len() {
        let max_k = (digits.len() - i).min(19);
        let mut hit = None;
        for k in (13..=max_k).rev() {
            let window: String = digits[i..i + k].iter().map(|&b| b as char).collect();
            if luhn_valid(&window) {
                hit = Some(k);
                break;
            }
        }
        if let Some(k) = hit {
            spans.push((digit_pos[i], digit_pos[i + k - 1] + 1));
            i += k;
        } else {
            i += 1;
        }
    }
    if spans.is_empty() {
        return run.to_string();
    }
    let mut out = String::with_capacity(run.len());
    let mut cursor = 0;
    for (start, end) in spans {
        out.push_str(&run[cursor..start]);
        out.push_str("[redacted-card]");
        cursor = end;
    }
    out.push_str(&run[cursor..]);
    out
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email regex")
    })
}

/// Replace emails, Luhn-valid card numbers, and API-key/secret-like tokens with
/// stable placeholders. Idempotent on already-redacted text.
///
/// Emails are matched first (so a long local part is redacted whole rather than
/// fragmented by the secret pass), then secrets, then Luhn-checked cards.
pub fn redact(input: &str) -> String {
    // 1. Emails.
    let stage1 = email_re().replace_all(input, "[redacted-email]");

    // 2. Secrets. Vendor-prefixed keys always redact; the generic long-token
    //    branch redacts only high-entropy runs so long prose words survive.
    let stage2 = secret_re().replace_all(&stage1, |caps: &regex::Captures| {
        let m = &caps[0];
        let is_keyed = KEY_PREFIXES.iter().any(|prefix| m.starts_with(prefix));
        if is_keyed || looks_high_entropy(m) {
            "[redacted-secret]".to_string()
        } else {
            m.to_string()
        }
    });

    // 3. Card numbers (Luhn-validated). Each maximal digit/separator run is
    //    windowed internally so adjacent cards are each caught.
    card_run_re()
        .replace_all(&stage2, |caps: &regex::Captures| redact_card_run(&caps[0]))
        .into_owned()
}

/// Whether `digits` (ASCII digits only) satisfies the Luhn checksum.
pub fn luhn_valid(digits: &str) -> bool {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0u32;
    let mut double = false;
    for byte in digits.bytes().rev() {
        let mut d = u32::from(byte - b'0');
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email_addresses() {
        assert_eq!(
            redact("ping ada@example.com please"),
            "ping [redacted-email] please"
        );
    }

    #[test]
    fn redacts_luhn_valid_card_numbers() {
        // 4242 4242 4242 4242 is a canonical Luhn-valid test PAN.
        let out = redact("card 4242 4242 4242 4242 end");
        assert!(out.contains("[redacted-card]"), "got {out:?}");
        assert!(!out.contains("4242"), "digits scrubbed: {out:?}");
    }

    #[test]
    fn leaves_non_luhn_digit_runs_alone() {
        // A 16-digit run that fails Luhn is not a card — keep it (e.g. an order id).
        let out = redact("order 1234567812345671 shipped");
        // 1234567812345671 fails the Luhn checksum; must survive.
        assert!(!luhn_valid("1234567812345671"));
        assert!(out.contains("1234567812345671"), "got {out:?}");
    }

    #[test]
    fn redacts_api_key_like_secrets() {
        let out = redact("token sk-abcdEFGH0123456789abcdEFGH0123 done");
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains("sk-abcd"), "secret scrubbed: {out:?}");
    }

    #[test]
    fn redacts_aws_access_key_id() {
        let out = redact("key AKIAIOSFODNN7EXAMPLE here");
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains("AKIA"), "got {out:?}");
    }

    #[test]
    fn preserves_ordinary_prose() {
        let text = "Let's meet at 3pm to discuss the Q3 roadmap.";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn long_lowercase_prose_tokens_survive_redaction() {
        // The entropy-NEGATIVE branch (audit c121): a 32+ char all-lowercase
        // word matches the generic secret regex's charset but must be judged
        // low-entropy and survive — a privacy filter that eats ordinary
        // prose is broken in the other direction.
        let text = "the pneumonoultramicroscopicsilicovolcanoconiosis diagnosis stands";
        assert_eq!(redact(text), text);
        // Hyphenated lowercase runs too (also inside the regex charset).
        let slug = "a very-long-kebab-case-identifier-name-for-something here";
        assert_eq!(redact(slug), slug);
    }

    #[test]
    fn redacts_long_lowercase_token_with_digits() {
        // The `has_digit`-ALONE entropy arm (all-lowercase letters + digits, no
        // uppercase, no base64 punct) — the most common real secret shape
        // (lowercase hex / id tokens). A regression dropping `has_digit` from the
        // OR would leak exactly this class while every other entropy test passes.
        let out = redact("tok abc123def456abc123def456abc123def456 done");
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains("abc123def456"), "got {out:?}");
        // Control: the SAME run with the digits removed (all-lowercase letters)
        // is low-entropy and must survive — proving it was the digits that
        // tripped the gate, not the length.
        let prose = redact("tok abcdefabcdefabcdefabcdefabcdefabcdef done");
        assert!(
            prose.contains("abcdefabcdefabcdef"),
            "all-letter run survives: {prose:?}"
        );
    }

    #[test]
    fn card_redaction_covers_the_length_band_and_spares_short_luhn_runs() {
        // The card regex matches 13–19 digit runs; both band edges and the
        // below-floor direction need pins (all values are Luhn-valid, so the
        // ONLY thing separating them is length).
        assert!(
            redact("amex 378282246310005 ok").contains("[redacted-card]"),
            "15-digit Amex PAN inside the band"
        );
        assert!(
            redact("visa 4222222222222 ok").contains("[redacted-card]"),
            "13-digit PAN at the regex floor"
        );
        assert!(
            redact("pan 6212345678901232 ok").contains("[redacted-card]"),
            "16-digit non-Visa scheme"
        );
        let short = redact("order id 124000001 here");
        assert!(
            short.contains("124000001"),
            "a Luhn-valid 9-digit run is below the floor and must survive: {short}"
        );
    }

    #[test]
    fn long_uppercase_letter_runs_survive_redaction() {
        // The documented entropy contract says an all-ONE-case all-letter
        // 32+ run is left alone; only the lowercase direction was pinned.
        let text = "HEADING ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEF END";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact("mail ada@example.com");
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn redacts_email_secret_and_card_together_in_one_pass() {
        // Existing tests isolate one PII class each; this pins the staged
        // email→secret→card interaction when all three are present in a single
        // input. A regression where one stage's replacement text fragments a
        // later stage's match (or an early-return short-circuit) would leak one
        // class while the others still scrub.
        let out = redact(
            "mail ada@example.com key sk-abcdEFGH0123456789abcdEFGH0123 card 4242 4242 4242 4242 end",
        );
        assert!(out.contains("[redacted-email]"), "email scrubbed: {out:?}");
        assert!(
            out.contains("[redacted-secret]"),
            "secret scrubbed: {out:?}"
        );
        assert!(out.contains("[redacted-card]"), "card scrubbed: {out:?}");
        // None of the original sensitive substrings survive.
        assert!(!out.contains("ada@example.com"), "got {out:?}");
        assert!(!out.contains("sk-abcd"), "got {out:?}");
        assert!(!out.contains("4242"), "got {out:?}");
        // The non-sensitive framing words are untouched.
        assert!(out.starts_with("mail "), "got {out:?}");
        assert!(out.ends_with(" end"), "got {out:?}");
    }

    #[test]
    fn redacts_all_letter_mixed_case_secret() {
        // Base64/base64url secrets are often all letters (no digit); the
        // letter+digit heuristic must not let them through (review finding 1).
        let out = redact("key abcdefghABCDEFGHabcdefghABCDEFGHxyz done");
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains("abcdefghABCDEFGH"), "got {out:?}");
    }

    #[test]
    fn redacts_all_lowercase_token_with_only_base64_punct() {
        // looks_high_entropy's base64-punct arm (+,/,=) ALONE marks a long token
        // as a secret even with no digit and no uppercase — base64/base64url
        // payloads are often all-lowercase. A regression dropping has_b64_punct
        // would leak exactly this class while the digit/mixed-case arms still pass.
        let token = "abcdefghijklmnopqrstuvwxyzabcd+/="; // 33 chars: lowercase + b64 punct, no digit/upper
        let out = redact(&format!("blob {token} end"));
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains("abcdefghij"), "secret scrubbed: {out:?}");
        // Control: same shape with NO base64 punct (pure lowercase letters) is
        // low-entropy and must survive — proving it was the punct that tripped it.
        let plain = "abcdefghijklmnopqrstuvwxyzabcdefg"; // 33 lowercase letters
        let kept = redact(&format!("blob {plain} end"));
        assert!(
            kept.contains(plain),
            "all-letter low-entropy run survives: {kept:?}"
        );
    }

    #[test]
    fn redacts_jwt_including_payload() {
        // JWT segments are dot-separated; the payload must not leak (review 2).
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1";
        let out = redact(&format!("auth {jwt} ok"));
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains("eyJzdWIi"), "payload scrubbed: {out:?}");
    }

    #[test]
    fn redacts_base64_padded_secret() {
        let out = redact("s=c2VjcmV0c2VjcmV0c2VjcmV0c2VjcmV0c2VjcmV0PT0=");
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
    }

    #[test]
    fn redacts_vendor_key_prefixes() {
        for token in [
            "AKIAIOSFODNN7EXAMPLE",
            "ASIAIOSFODNN7EXAMPLE",
            "AIzaSyA1234567890abcdEFGHijkl",
            "xoxb-123456789012-abcdefghijkl",
            "xoxp-123456789012-abcdefghijkl",
            "xoxa-123456789012-abcdefghijkl",
            "xoxr-123456789012-abcdefghijkl",
            "xoxs-123456789012-abcdefghijkl",
            "whsec_abcdefghijklmnop123456",
            "glpat-abcdefghij1234567890",
            "SG.abcdefghijklmnop1234567890abcdef",
            "sk-abcdefghijklmnop123456",
            "sk_abcdefghijklmnop123456",
            "ghp_abcdefghijklmnop123456",
            "gho_abcdefghijklmnop123456",
            "ghu_abcdefghijklmnop123456",
            "ghs_abcdefghijklmnop123456",
            "ghr_abcdefghijklmnop123456",
            "pk-abcdefghijklmnop123456",
            "pk_abcdefghijklmnop123456",
            "rk-abcdefghijklmnop123456",
            "rk_abcdefghijklmnop123456",
        ] {
            let out = redact(&format!("k {token} done"));
            assert!(out.contains("[redacted-secret]"), "{token} -> {out:?}");
            assert!(!out.contains(token), "{token} leaked -> {out:?}");
        }
    }

    #[test]
    fn redacts_sendgrid_prefix_without_generic_length_entropy() {
        // SG. is a documented always-redacted vendor prefix. Keep it covered
        // below the generic 32+ char token branch so the prefix contract is
        // what protects it.
        let token = "SG.shortKey123";
        let out = redact(&format!("sendgrid {token} done"));
        assert!(out.contains("[redacted-secret]"), "got {out:?}");
        assert!(!out.contains(token), "SG-prefixed token leaked -> {out:?}");
    }

    #[test]
    fn redacts_dash_separated_and_nineteen_digit_cards() {
        let dashed = redact("pan 4242-4242-4242-4242 end");
        assert!(dashed.contains("[redacted-card]"), "got {dashed:?}");
        assert!(!dashed.contains("4242"), "got {dashed:?}");

        let long = redact("pan 4000000000000000006 end");
        assert!(long.contains("[redacted-card]"), "got {long:?}");
        assert!(!long.contains("400000"), "got {long:?}");
    }

    #[test]
    fn redacts_nbsp_separated_card() {
        let out = redact("pan 4242\u{00a0}4242\u{00a0}4242\u{00a0}4242 end");
        assert!(out.contains("[redacted-card]"), "got {out:?}");
        assert!(!out.contains("4242"), "got {out:?}");
    }

    #[test]
    fn redacts_cards_with_tabs_newlines_and_repeated_spaces() {
        let out = redact("pan 4242\t4242\n4242   4242 end");
        assert!(out.contains("[redacted-card]"), "got {out:?}");
        assert!(!out.contains("4242"), "got {out:?}");

        let non_luhn = redact("order 1234\t5678\n1234   5671 end");
        assert!(
            non_luhn.contains("1234\t5678\n1234   5671"),
            "non-Luhn digit runs must survive: {non_luhn:?}"
        );
    }

    #[test]
    fn long_email_local_part_is_fully_redacted() {
        // Email pass runs first so a 32+ char local part is not fragmented by
        // the secret pass into a partial-leak tail (review finding 6).
        let out = redact("verylonglocalpartoverthirtytwochars@example.com");
        assert_eq!(out, "[redacted-email]");
    }

    #[test]
    fn redacts_two_adjacent_cards_separated_only_by_a_separator() {
        // Review finding (privacy MEDIUM): a greedy 13–19 digit span used to
        // straddle the boundary between two back-to-back PANs, fail Luhn over the
        // merged digits, and leak BOTH. Each card (4242… and 4000…0002 are both
        // Luhn-valid) must now be redacted independently.
        // Whitespace/NBSP separators don't merge into one token, so they reach
        // the card stage and yield one [redacted-card] per PAN.
        for sep in [" ", "\u{00a0}", "\t"] {
            let input = format!("pay 4242424242424242{sep}4000000000000002 now");
            let out = redact(&input);
            assert!(
                !out.contains("4242"),
                "first card leaked ({sep:?}): {out:?}"
            );
            assert!(
                !out.contains("4000"),
                "second card leaked ({sep:?}): {out:?}"
            );
            assert_eq!(
                out.matches("[redacted-card]").count(),
                2,
                "both cards redacted ({sep:?}): {out:?}"
            );
        }
        // A dash joins the two PANs into one 33-char run that the *secret* pass
        // catches first — different placeholder, same privacy outcome: no leak.
        let dashed = redact("pay 4242424242424242-4000000000000002 now");
        assert!(
            !dashed.contains("4242"),
            "dash: first card leaked: {dashed:?}"
        );
        assert!(
            !dashed.contains("4000"),
            "dash: second card leaked: {dashed:?}"
        );
        // Grouped-then-grouped form leaks the same way without the fix.
        let grouped = redact("pay 4242 4242 4242 4242 4000 0000 0000 0002 now");
        assert!(!grouped.contains("4242"), "got {grouped:?}");
        assert!(!grouped.contains("4000"), "got {grouped:?}");
    }

    #[test]
    fn redacts_card_abutted_by_extra_digits_with_no_separator() {
        // Review round-2 finding: a Luhn-valid PAN glued to extra digits with NO
        // separator forms a solid >19 (or exactly-19) digit block whose only
        // boundary-aligned span overshoots 19 / fails Luhn — the embedded PAN
        // leaked. The sliding window now catches the embedded 16-digit card.
        // 20-digit block: PAN + 4 trailing digits.
        let glued = redact("ref 42424242424242421234 end");
        assert!(glued.contains("[redacted-card]"), "got {glued:?}");
        assert!(
            !glued.contains("4242424242424242"),
            "embedded PAN leaked: {glued:?}"
        );
        // 19-digit block: PAN + 3-digit CVV, no separator.
        let cvv = redact("x 4242424242424242123 y");
        assert!(!cvv.contains("4242424242424242"), "PAN+CVV leaked: {cvv:?}");
    }

    #[test]
    fn redacts_card_followed_immediately_by_trailing_digits() {
        // A PAN trailed by a CVV-like run (separated by a separator) used to make
        // the greedy span grab 19 digits, fail Luhn, and leak the card. The card
        // is now scrubbed; the short (<13-digit) tail is harmless and survives.
        let out = redact("card 4242 4242 4242 4242 123 ok");
        assert!(out.contains("[redacted-card]"), "got {out:?}");
        assert!(!out.contains("4242"), "card leaked: {out:?}");
        assert!(out.contains("123"), "short tail survives: {out:?}");
    }

    #[test]
    fn luhn_validates_known_values() {
        assert!(luhn_valid("4242424242424242"));
        assert!(!luhn_valid("4242424242424241"));
        assert!(!luhn_valid(""));
    }

    #[test]
    fn luhn_valid_accepts_ascii_digits_only() {
        assert!(!luhn_valid("4242 4242 4242 4242"));
        assert!(!luhn_valid("4242-4242-4242-4242"));
        assert!(!luhn_valid("4242424242424242\n"));
        assert!(!luhn_valid(
            "\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}\u{ff14}\u{ff12}"
        ));
    }
}
