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
/// separators (whitespace, dash, dot, comma, no-break space). The 13–19-digit Luhn windowing
/// happens inside the run (`redact_card_run`) so two cards separated only by a
/// separator are each detected, rather than a greedy span straddling the card
/// boundary and failing Luhn over the merged digits (which leaked both PANs).
fn card_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d(?:[\s\u{00a0}.,-]*\d)*").expect("card run regex"))
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
            // luhn over the byte slice directly — no per-window String allocation
            // (the card stage runs on every stored/diagnostic string).
            if luhn_valid_bytes(&digits[i..i + k]) {
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

fn credential_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
        r#"(?i)\b((?:password|passwd|secret|access[_-]?token|id[_-]?token|refresh[_-]?token|token|client[_-]?secret|api[_-]?key|authorization|code)\b["'“”‘’«»]?\s*[:=]\s*(?:bearer\s+)?)(\[redacted-[a-z]+\][^\s,;&}\])]*|"[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+)"#,
        )
        .expect("credential assignment regex")
    })
}

fn whitespace_credential_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b((?:password|passwd|access[_-]?token|id[_-]?token|refresh[_-]?token|token|client[_-]?secret|api[_-]?key)\b["'“”‘’«»]?\s+)(\[redacted-[a-z]+\][^\s,;&}\])]*|"[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+)|\b(authorization\b["'“”‘’«»]?\s+bearer\s+)(\[redacted-[a-z]+\][^\s,;&}\])]*|"[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+)"#,
        )
        .expect("whitespace credential regex")
    })
}

fn should_redact_whitespace_credential(prefix: &str, value: &str) -> bool {
    let unquoted = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .or_else(|| value.strip_prefix('“').and_then(|s| s.strip_suffix('”')))
        .or_else(|| value.strip_prefix('‘').and_then(|s| s.strip_suffix('’')))
        .or_else(|| value.strip_prefix('«').and_then(|s| s.strip_suffix('»')))
        .unwrap_or(value);
    let prefix = prefix.trim().to_ascii_lowercase();
    let weak_password = matches!(
        unquoted.to_ascii_lowercase().as_str(),
        "admin" | "letmein" | "password" | "qwerty" | "secret" | "swordfish" | "welcome"
    );
    (matches!(prefix.as_str(), "password" | "passwd") && weak_password)
        || value.starts_with(['"', '\'', '“', '‘', '«'])
        || unquoted.chars().any(|c| c.is_ascii_digit())
        || unquoted.contains(['_', '-', '.', '/', '+', '='])
        || unquoted.len() >= 16
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
    let stage2 = credential_re().replace_all(&stage1, "$1[redacted-secret]");
    let stage2b = whitespace_credential_re().replace_all(&stage2, |caps: &regex::Captures| {
        let (prefix, value) = match (caps.get(1), caps.get(2), caps.get(3), caps.get(4)) {
            (Some(prefix), Some(value), _, _) => (prefix.as_str(), value.as_str()),
            (_, _, Some(prefix), Some(value)) => (prefix.as_str(), value.as_str()),
            _ => return caps[0].to_string(),
        };
        if prefix.to_ascii_lowercase().contains("authorization")
            || should_redact_whitespace_credential(prefix, value)
        {
            format!("{prefix}[redacted-secret]")
        } else {
            caps[0].to_string()
        }
    });
    let stage3 = secret_re().replace_all(&stage2b, |caps: &regex::Captures| {
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
        .replace_all(&stage3, |caps: &regex::Captures| redact_card_run(&caps[0]))
        .into_owned()
}

/// Whether `digits` (ASCII digits only) satisfies the Luhn checksum.
///
/// Test-only `&str` convenience wrapper over [`luhn_valid_bytes`]; the
/// production card-redaction path validates raw bytes directly.
#[cfg(test)]
pub fn luhn_valid(digits: &str) -> bool {
    luhn_valid_bytes(digits.as_bytes())
}

/// Luhn over raw ASCII-digit bytes. Any non-ASCII-digit byte makes it `false`
/// (mirrors the `&str` contract — a multibyte char's UTF-8 bytes aren't digits).
/// Operating on bytes lets the card-run windowing avoid a String alloc per window.
fn luhn_valid_bytes(digits: &[u8]) -> bool {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let mut sum = 0u32;
    let mut double = false;
    for &byte in digits.iter().rev() {
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
    fn redacts_oauth_callback_params_by_name_regardless_of_entropy() {
        // `code` and `access_token` are anchored by key name, so even short,
        // low-entropy values (below the stage-3 {32,} high-entropy floor) are
        // redacted. Without the key-name arm these would leak.
        let out = redact("https://x.com/cb?code=abc123&access_token=xyz");
        assert_eq!(
            out,
            "https://x.com/cb?code=[redacted-secret]&access_token=[redacted-secret]"
        );
        assert!(!out.contains("abc123"), "code value must be gone");
        assert!(!out.contains("xyz"), "access_token value must be gone");
    }

    #[test]
    fn redacts_short_credential_assignments_and_auth_headers() {
        let out = redact(
        "password=hunter2 Authorization: Bearer abc123 token=abc123def456 api_key=short-dev-key password=\"foo bar baz\" api_key='quoted dev key' authorization: Bearer \"space bearer\"",
    );

        assert!(out.contains("password=[redacted-secret]"));
        assert!(out.contains("Authorization: Bearer [redacted-secret]"));
        assert!(out.contains("token=[redacted-secret]"));
        assert!(out.contains("api_key=[redacted-secret]"));
        assert!(out.contains("password=[redacted-secret]"));
        assert!(out.contains("authorization: Bearer [redacted-secret]"));
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("abc123def456"));
        assert!(!out.contains("short-dev-key"));
        assert!(!out.contains("foo bar baz"));
        assert!(!out.contains("quoted dev key"));
        assert!(!out.contains("space bearer"));
    }

    #[test]
    fn redacts_json_quoted_credential_keys() {
        // JSON puts a closing quote between the key and the colon
        // (`"password": "v"`); the key regexes must tolerate it or short
        // secrets in pasted JSON blobs survive redaction verbatim.
        let out = redact(r#"{"password": "hunter2", "api_key": "short-dev-key"}"#);
        assert!(!out.contains("hunter2"), "JSON password leaked: {out}");
        assert!(!out.contains("short-dev-key"), "JSON api_key leaked: {out}");
        assert!(out.contains("[redacted-secret]"));

        let single = redact("'token': 'abc-def'");
        assert!(!single.contains("abc-def"), "quoted token leaked: {single}");
    }

    #[test]
    fn redacts_smart_quoted_credential_keys_and_values() {
        // macOS substitutes smart quotes by default in Notes/Mail/Pages, so
        // pasted credential snippets often arrive curly-quoted; the key and
        // value quote classes must both tolerate the typographic glyphs.
        let out = redact("“password”: “hunter2”");
        assert!(!out.contains("hunter2"), "smart-quoted password leaked: {out}");

        let mixed = redact("password: “hunter2 trailing”");
        assert!(
            !mixed.contains("hunter2") && !mixed.contains("trailing"),
            "curly-quoted value with space leaked: {mixed}"
        );

        let guillemet = redact("«api_key»: «short-dev-key»");
        assert!(
            !guillemet.contains("short-dev-key"),
            "guillemet-quoted api_key leaked: {guillemet}"
        );

        let curly_single = redact("‘token’: ‘abc-def’");
        assert!(
            !curly_single.contains("abc-def"),
            "curly-single-quoted token leaked: {curly_single}"
        );
        let curly_single_pw = redact("‘password’: ‘hunter2’");
        assert!(
            !curly_single_pw.contains("hunter2"),
            "curly-single-quoted password leaked: {curly_single_pw}"
        );
    }

    #[test]
    fn redacts_smart_quoted_whitespace_delimited_weak_passwords() {
        // The space-delimited form routes through the weak-password heuristic,
        // whose quote stripping must cover the same glyphs as the regex.
        for input in [
            "password \"letmein\"",
            "password “letmein”",
            "password «letmein»",
            "passwd ‘letmein’",
        ] {
            let out = redact(input);
            assert!(!out.contains("letmein"), "weak password leaked: {input} -> {out}");
            assert!(out.contains("[redacted-secret]"), "no placeholder: {out}");
        }
    }

    #[test]
    fn redaction_is_idempotent_for_quoted_credential_shapes() {
        for input in [
            r#"{"password": "hunter2", "api_key": "k"}"#,
            "“password”: “hunter2”",
            "«api_key»: «short-dev-key»",
            "password “letmein” trailing",
        ] {
            let once = redact(input);
            let twice = redact(&once);
            assert_eq!(once, twice, "second pass changed text for: {input}");
        }
    }

    #[test]
    fn placeholder_shaped_prefix_cannot_mask_a_trailing_secret() {
        // Leftmost-first alternation would otherwise stop at a literal
        // "[redacted-x]" prefix and leave a glued-on real secret outside the
        // match (adversarial or coincidental paste shape).
        for input in [
            "password=[redacted-secret]hunter2trailing",
            "password [redacted-secret]hunter2trailing",
            r#""token": [redacted-secret]hunter2trailing"#,
        ] {
            let out = redact(input);
            assert!(
                !out.contains("hunter2trailing"),
                "masked secret leaked: {input} -> {out}"
            );
        }
        // ...while a bare placeholder before a JSON closer stays untouched
        // (the idempotency contract this alternative exists for).
        let json = redact(r#"{"password": [redacted-secret]}"#);
        assert_eq!(json, r#"{"password": [redacted-secret]}"#);
    }

    #[test]
    fn quoted_credential_nouns_in_prose_keep_low_entropy_neighbors() {
        // The optional key-quote must not scrub ordinary prose that merely
        // quotes a credential noun without a value attached.
        for input in [
            "the “token” bucket algorithm",
            "he typed “password” quietly",
            "the word \"token\" means a lexeme",
        ] {
            assert_eq!(redact(input), input, "benign prose was scrubbed");
        }
    }

    #[test]
    fn unterminated_quote_credential_value_redacts_tail_to_safe_delimiter() {
        let out = redact("password=\"hunter2 trailing secret");
        assert!(
            out.starts_with("password=[redacted-secret]"),
            "opening-quoted value must be redacted, got: {out}"
        );
        assert!(!out.contains("hunter2"), "secret first word leaked: {out}");
        assert!(
            !out.contains("trailing secret"),
            "secret tail leaked: {out}"
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
    fn email_requires_a_two_char_tld_and_a_dot() {
        // The email regex ends `\.[A-Za-z]{2,}` — a dotted TLD of >=2 letters is
        // mandatory. A bare host with no dot-TLD (`user@localhost`) does not match
        // and survives unredacted, while a minimal real domain (`a@b.io`) becomes
        // the email placeholder. Pins the TLD anchor against a regression that
        // dropped the `\.[A-Za-z]{2,}` tail (which would over-match local hosts).
        assert_eq!(
            redact("login user@localhost now"),
            "login user@localhost now",
            "no dot-TLD => not an email match"
        );
        assert_eq!(redact("mail a@b.io ok"), "mail [redacted-email] ok");
    }

    #[test]
    fn dot_and_comma_are_card_separators() {
        let dotted = redact("card 4242.4242.4242.4242 end");
        assert!(
            dotted.contains("[redacted-card]"),
            "dot-separated PAN must redact: {dotted:?}"
        );
        assert!(!dotted.contains("4242.4242.4242.4242"), "got {dotted:?}");

        let comma = redact("card 4242,4242,4242,4242 end");
        assert!(comma.contains("[redacted-card]"), "got {comma:?}");
        assert!(!comma.contains("4242,4242,4242,4242"), "got {comma:?}");
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
    fn redaction_is_idempotent_for_card_secret_and_mixed() {
        // The docstring promises idempotence on ALREADY-redacted text broadly, but
        // only the email path was pinned. The run loop / diagnostics can re-redact
        // stored or logged strings, so a second pass over [redacted-secret] /
        // [redacted-card] must be a no-op. A future regex change that re-matched or
        // mangled a placeholder (e.g. broadening the token charset to include
        // brackets, or a card re-window over placeholder-adjacent digits) would
        // silently break this and leak on the second pass.
        let mixed = redact(
            "mail ada@example.com key sk-abcdEFGH0123456789abcdEFGH0123 card 4242 4242 4242 4242 end",
        );
        assert_eq!(
            redact(&mixed),
            mixed,
            "second pass over mixed PII is a no-op"
        );
        // The placeholders themselves survive a redact pass unchanged.
        assert_eq!(redact("[redacted-card]"), "[redacted-card]");
        assert_eq!(redact("[redacted-secret]"), "[redacted-secret]");
        assert_eq!(redact("[redacted-email]"), "[redacted-email]");
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
    fn redacts_base64_token_whose_only_punct_is_padding_equals() {
        // looks_high_entropy's base64-punct arm lists ('+','/','='). A standard
        // base64 token whose ONLY special char is '=' padding (no '+'/'/', no
        // digit, no uppercase) relies solely on '=' to be flagged. A regression
        // dropping '=' from the punct set would leak exactly this token while the
        // other arms still pass, so pin it explicitly.
        let out = redact("blob abcdefghijklmnopqrstuvwxyzabcdefg== end");
        assert!(
            out.contains("[redacted-secret]"),
            "=-only base64 padding redacted: {out:?}"
        );
        assert!(
            !out.contains("abcdefghij"),
            "=-padded secret scrubbed: {out:?}"
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

    #[test]
    fn vendor_prefix_too_short_for_keyed_branch_survives() {
        // NEGATIVE for the keyed-secret category. Every vendor-prefix test pins a
        // token that DOES redact; this pins the other direction. The keyed branch
        // requires a `{16,}` suffix (e.g. `(?:sk|ghp|...)[-_][A-Za-z0-9_-]{16,}`),
        // and the generic branch needs 32+ chars. A benign lookalike that merely
        // STARTS like a vendor prefix but is too short to reach either floor is
        // low-risk prose and must survive unchanged. A regression loosening the
        // suffix length (or anchoring on the bare prefix) would over-redact these.
        for benign in ["ghp_short", "sk-tiny", "AKIA123", "glpat-nope"] {
            let text = format!("see {benign} here");
            assert_eq!(
                redact(&text),
                text,
                "short vendor-prefix lookalike must survive: {benign}"
            );
        }
    }

    #[test]
    fn pathological_repetitive_input_scrubs_promptly_and_correctly() {
        // Guards against catastrophic-backtracking regex: a very long, highly
        // repetitive input must return promptly AND with the correct safe outcome
        // (no original sensitive span leaks). The generic secret branch matches a
        // 32+ char `[A-Za-z0-9+/=._-]` run, and `looks_high_entropy` treats a run
        // containing a digit as a secret — so a 100k-digit run is OVER-redacted to
        // the secret placeholder (privacy > fidelity). None of the raw digits may
        // survive.
        let big_digits = "7".repeat(100_000);
        let scrubbed = redact(&big_digits);
        assert_eq!(scrubbed, "[redacted-secret]", "long digit run over-redacts");
        assert!(
            !scrubbed.contains('7'),
            "no raw digits leak from long input"
        );
        // A 100k-char run of a single lowercase letter is low-entropy (no digit,
        // no mixed case, no base64 punct) and must survive UNCHANGED.
        let big_letters = "a".repeat(100_000);
        assert_eq!(
            redact(&big_letters),
            big_letters,
            "repetitive letters survive"
        );
        // And a real PAN embedded in a long benign run is still scrubbed (the long
        // surrounding text does not mask the card or blow up the matcher).
        let padded = format!(
            "{pad} 4242 4242 4242 4242 {pad}",
            pad = "word ".repeat(2_000)
        );
        let out = redact(&padded);
        assert!(out.contains("[redacted-card]"), "embedded PAN scrubbed");
        assert!(!out.contains("4242"), "no card digits leak from long input");
    }

    #[test]
    fn redacts_high_confidence_space_delimited_credentials() {
        assert_eq!(redact("password hunter2"), "password [redacted-secret]");
        assert_eq!(redact("password swordfish"), "password [redacted-secret]");
        assert_eq!(redact("passwd letmein"), "passwd [redacted-secret]");
        assert!(
            !redact("password \"hunter2 trailing secret").contains("trailing secret"),
            "unterminated quoted password tail must be redacted"
        );
        assert_eq!(redact("token abc123secretvalue"), "token [redacted-secret]");
        assert_eq!(
            redact("Authorization Bearer abc123"),
            "Authorization Bearer [redacted-secret]"
        );
        assert_eq!(redact("token \"dev key\""), "token [redacted-secret]");
        assert_eq!(redact("api_key dev-key"), "api_key [redacted-secret]");
        assert_eq!(
            redact("client_secret abc.def"),
            "client_secret [redacted-secret]"
        );
        assert_eq!(
            redact("access_token abcdefghijklmnop"),
            "access_token [redacted-secret]"
        );
        assert_eq!(redact("api_key devkey"), "api_key devkey");
    }

    #[test]
    fn leaves_prose_after_credential_words_alone() {
        assert_eq!(redact("token bucket algorithm"), "token bucket algorithm");
        assert_eq!(
            redact("password requirements include length"),
            "password requirements include length"
        );
        assert_eq!(
            redact("authorization failed for request"),
            "authorization failed for request"
        );
    }

    #[test]
    fn all_lowercase_all_letter_long_token_is_not_redacted_known_gap() {
        // ACCEPTED GAP / entropy-heuristic boundary: a 32+ char token matches
        // the generic secret regex, but `looks_high_entropy` leaves an
        // all-one-case, all-letter run alone (no digit, no mixed case, no b64
        // punctuation) on the theory that it is more likely a long word than a
        // secret. A 40-char all-lowercase-letter token therefore survives.
        let token = "abcdefghijklmnopqrstuvwxyzabcdefghijklmn"; // 40 letters
        assert_eq!(token.len(), 40);
        assert_eq!(redact(token), token);

        // Contrast: flipping a single character to a digit pushes the same run
        // over the entropy boundary (`has_digit`), and it IS redacted. This
        // pins the boundary so neither side can regress silently.
        let with_digit = "abcdefghijklmnopqrstuvwxyzabcdefghijklm1"; // 39 letters + 1 digit
        assert_eq!(with_digit.len(), 40);
        assert_eq!(redact(with_digit), "[redacted-secret]");
    }
    #[test]
    fn redacts_url_query_client_secret_and_refresh_token() {
        let out = redact(
            "open https://example.test/callback?client_secret=abc123def456abc123&refresh_token=ref1234567890xyz&state=ok",
        );

        assert!(out.contains("client_secret=[redacted-secret]"));
        assert!(out.contains("refresh_token=[redacted-secret]"));
        assert!(out.contains("state=ok"));
        assert!(!out.contains("abc123def456abc123"), "{out:?}");
        assert!(!out.contains("ref1234567890xyz"), "{out:?}");
    }

    #[test]
    fn token_secret_keys_respect_word_boundary() {
        // The higher-frequency credential keys (`token`, `secret`, …) anchor their
        // key NAME with `\b`, same as the `code` key pinned separately. Only `code`
        // had a word-boundary pin; this covers the common keys that otherwise ride
        // on inference. Two directions:
        //
        // NEGATIVE — the leading `\b` must NOT fire mid-word. Here the credential
        // key name is glued to a non-boundary prefix AND sits directly adjacent to
        // `=`, so the `\s*[:=]` adjacency requirement does NOT save us — only the
        // leading `\b` does. Dropping it makes `token=`/`password=` match the tail
        // of `xtoken`/`mypassword` and wrongly redact. These must survive verbatim.
        assert_eq!(redact("xtoken=secret123value"), "xtoken=secret123value");
        assert_eq!(redact("mypassword=hunter2value"), "mypassword=hunter2value");

        // POSITIVE control — the SAME bare keys, this time at a word boundary and
        // adjacent to `=`, DO match and redact their value.
        assert_eq!(redact("token=secret123value"), "token=[redacted-secret]");

        // POSITIVE — legitimate `_`-compound credential keys ARE in the
        // alternation (`refresh[_-]?token`, `client[_-]?secret`) and their values
        // must be scrubbed regardless of value entropy.
        assert_eq!(
            redact("refresh_token=abc123secret"),
            "refresh_token=[redacted-secret]"
        );
        assert_eq!(
            redact("client_secret=abc123secret"),
            "client_secret=[redacted-secret]"
        );
    }

    #[test]
    fn code_key_requires_word_boundary_so_compound_keys_survive() {
        // NEGATIVE direction: the `code` key alternation is `\bcode\b`, so the
        // leading `\b` must NOT fire mid-word. Compound keys whose suffix is
        // `code` (preceded by a word char like `_` or a letter) are legitimate
        // data and must pass through verbatim — value preserved unchanged.
        // A regression dropping the `\b` (e.g. `\bcode\b` -> `code`) would
        // silently start redacting these and corrupt the data.
        assert_eq!(redact("error_code=500"), "error_code=500");
        assert_eq!(redact("postal_code=12345"), "postal_code=12345");
        assert_eq!(redact("status_code=404"), "status_code=404");
        assert_eq!(redact("barcode=12345"), "barcode=12345");

        // POSITIVE direction: a bare `code` key IS a credential key (OAuth
        // authorization code) and its value must be redacted regardless of
        // entropy. The secret value must be gone, replaced by the marker.
        let out = redact("code=abc123");
        assert_eq!(out, "code=[redacted-secret]");
        assert!(!out.contains("abc123"), "secret value leaked: {out:?}");
    }
}
