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
//! of stored context: a URL or filesystem path whose 32+ char run carries a
//! digit or mixed case (`/Users/Alice/…`, `…/v2/app.js`) is one such loss,
//! pinned by test. The deliberate false-negative boundary is all-one-case
//! all-letter prose and all-lowercase paths/URLs: those runs survive unless a
//! credential key/prefix or other entropy signal identifies them as secrets.

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
///
/// The vendor-prefix branch captures its left separator as group 1 because the
/// `regex` crate has no lookbehind: without a boundary that branch fired on the
/// `sk-` inside `risk-`, the `sk_` inside `task_` and the `rk-` inside
/// `network-`, shredding ordinary compound words. The caller re-emits the
/// separator and judges only the key that follows it. Every other branch leaves
/// group 1 unmatched, so the key is the whole match there.
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
            | (^|[^A-Za-z0-9])(?:sk|ghp|gho|ghu|ghs|ghr|pk|rk)[-_][A-Za-z0-9_-]{16,}
            | [A-Za-z0-9+/=._-]{32,}
            ",
        )
        .expect("secret regex")
    })
}

/// Whether a generic long token looks high-entropy enough to be a secret rather
/// than a long word or a path: it has a digit, mixed case, or base64 `+`/`=`.
/// A `/` on its own is NOT a signal — it is the separator of every URL path and
/// filesystem path, and a real base64 secret that is all one case with no digit
/// and no `+`/`=` is vanishingly unlikely (`(26/64)^32`). An all-one-case
/// all-letter 32+ run is left alone unless another credential signal catches it.
fn looks_high_entropy(token: &str) -> bool {
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_b64_punct = token.contains(['+', '=']);
    has_digit || (has_upper && has_lower) || has_b64_punct
}

/// Matches a *maximal* run of decimal digits optionally interleaved with the
/// card separators (whitespace, dash, dot, comma, no-break space). `\d` is
/// Unicode `\p{Nd}` in the `regex` crate, so the fullwidth digits U+FF10..U+FF19
/// that a CJK input method emits are in scope too and [`digit_value`] maps them.
/// The 13–19-digit Luhn windowing happens inside the run (`redact_card_run`) so
/// two cards separated only by a separator are each detected, rather than a
/// greedy span straddling the card boundary and failing Luhn over the merged
/// digits (which leaked both PANs).
fn card_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d(?:[\s\u{00a0}.,-]*\d)*").expect("card run regex"))
}

/// Decimal value of an ASCII or fullwidth (U+FF10..U+FF19) digit; `None` for any
/// other char. `char::to_digit(10)` is ASCII-only, so the fullwidth block — which
/// [`card_run_re`]'s `\d` does match — has to be mapped explicitly, or a
/// fullwidth PAN reaches the card stage and is returned untouched.
fn digit_value(c: char) -> Option<u8> {
    if c.is_ascii_digit() {
        return Some(c as u8 - b'0');
    }
    let fullwidth = (c as u32).wrapping_sub('\u{ff10}' as u32);
    (fullwidth < 10).then_some(fullwidth as u8)
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
    // Byte offset, UTF-8 length and decimal value of each digit. Digits AND
    // separators may be multi-byte (a fullwidth digit is 3 bytes, NBSP is 2), so
    // a span ends at `offset + len_utf8`, not `offset + 1`. Every redaction
    // boundary still lands on a digit edge, so the span slices are always valid
    // UTF-8 boundaries.
    let digits: Vec<(usize, usize, u8)> = run
        .char_indices()
        .filter_map(|(i, c)| digit_value(c).map(|value| (i, c.len_utf8(), value)))
        .collect();
    if digits.len() < 13 {
        return run.to_string();
    }
    // `luhn_valid_bytes` keeps its ASCII-digit-byte contract (it is shared with
    // the `luhn_valid` test wrapper): map each value back to its ASCII byte here
    // rather than widening it.
    let values: Vec<u8> = digits.iter().map(|&(_, _, value)| b'0' + value).collect();

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < values.len() {
        let max_k = (values.len() - i).min(19);
        let mut hit = None;
        for k in (13..=max_k).rev() {
            // luhn over the byte slice directly — no per-window String allocation
            // (the card stage runs on every stored/diagnostic string).
            if luhn_valid_bytes(&values[i..i + k]) {
                hit = Some(k);
                break;
            }
        }
        if let Some(k) = hit {
            let (last_offset, last_len, _) = digits[i + k - 1];
            spans.push((digits[i].0, last_offset + last_len));
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

/// Matches `code=<value>`, the OAuth authorization-code assignment.
///
/// Kept separate from [`credential_re`] because `code` is the one credential key
/// that must NOT accept `_` as a left separator: `_` is a word character, so
/// widening the boundary to catch `DB_PASSWORD=` would also start matching
/// `error_code=500`, `postal_code=12345` and `status_code=404`, which are
/// ordinary data. This pattern runs first so a swallowed `_code=` span cannot
/// mask a real credential sitting inside its value.
fn code_credential_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(code\b["'“”‘’«»]?\s*[:=]\s*(?:bearer\s+)?)("[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+)"#,
        )
        .expect("code credential regex")
    })
}

/// Matches `<key>[:=] <value>` credential assignments.
///
/// The leading `(^|[^A-Za-z0-9])` replaces the previous `\b`: `_` is a word
/// character in the `regex` crate, so `\b` never fired for the compound
/// environment-variable keys that carry most real secrets (`DB_PASSWORD=`,
/// `POSTGRES_PASSWORD=`, `SLACK_TOKEN=`) and they were stored verbatim. The
/// separator is captured because the crate has no lookbehind, and the caller
/// re-emits it. Alphanumerics are still excluded, so `mypassword=` and
/// `xtoken=` keep surviving mid-word.
fn credential_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
        r#"(?i)(^|[^A-Za-z0-9])((?:password|passwd|secret|access[_-]?token|id[_-]?token|refresh[_-]?token|token|client[_-]?secret|api[_-]?key|authorization)\b["'“”‘’«»]?\s*[:=]\s*(?:bearer\s+)?)("[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+)"#,
        )
        .expect("credential assignment regex")
    })
}

// Unlike the colon/equals form above, the whitespace form deliberately omits
// the `code` key: assignment-shaped `code=abc123` is an OAuth credential, but
// space-separated "code 404" / "area code 212" / "status code 500" is everyday
// prose and would be corrupted by the digit-bearing-value heuristic below.
// The leading `(^|[^A-Za-z0-9])` is shared by both branches (hence the `(?:…)`
// wrapper) for the same `_`-compound-key reason as `credential_re`.
fn whitespace_credential_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)(^|[^A-Za-z0-9])(?:((?:password|passwd|secret|access[_-]?token|id[_-]?token|refresh[_-]?token|token|client[_-]?secret|api[_-]?key)\b["'“”‘’«»]?\s+)("[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+)|(authorization\b["'“”‘’«»]?\s+bearer\s+)("[^"]*"|'[^']*'|“[^”]*”|‘[^’]*’|«[^»]*»|"[^\n;&]*|'[^\n;&]*|“[^\n;&]*|‘[^\n;&]*|[^\s,;&]+))"#,
        )
        .expect("whitespace credential regex")
    })
}

/// Whether a captured credential value is exactly an existing placeholder,
/// optionally followed by JSON/paren closers (the shape a prior redact() pass
/// leaves behind, e.g. `[redacted-secret]}`). Decided here rather than in the
/// regex because leftmost-first alternation cannot express "placeholder only
/// when nothing secret follows" — any stopper set in the pattern becomes the
/// bypass (mask a real secret behind `[redacted-x]}`).
fn already_redacted(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("[redacted-") else {
        return false;
    };
    let Some(end) = rest.find(']') else {
        return false;
    };
    !rest[..end].is_empty()
        && rest[..end].bytes().all(|b| b.is_ascii_lowercase())
        && rest[end + 1..]
            .chars()
            .all(|c| matches!(c, '}' | ']' | ')'))
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
    //    Already-redacted values pass through untouched (idempotency) — but
    //    ONLY when the value is exactly placeholder+closers; anything glued on
    //    re-redacts so a placeholder-shaped prefix can't mask a real secret.
    let stage2_code = code_credential_re().replace_all(&stage1, |caps: &regex::Captures| {
        let (prefix, value) = (&caps[1], &caps[2]);
        if already_redacted(value) {
            caps[0].to_string()
        } else {
            format!("{prefix}[redacted-secret]")
        }
    });
    let stage2 = credential_re().replace_all(&stage2_code, |caps: &regex::Captures| {
        let (left, prefix, value) = (&caps[1], &caps[2], &caps[3]);
        if already_redacted(value) {
            caps[0].to_string()
        } else {
            format!("{left}{prefix}[redacted-secret]")
        }
    });
    let stage2b = whitespace_credential_re().replace_all(&stage2, |caps: &regex::Captures| {
        let left = &caps[1];
        let (prefix, value) = match (caps.get(2), caps.get(3), caps.get(4), caps.get(5)) {
            (Some(prefix), Some(value), _, _) => (prefix.as_str(), value.as_str()),
            (_, _, Some(prefix), Some(value)) => (prefix.as_str(), value.as_str()),
            _ => return caps[0].to_string(),
        };
        if already_redacted(value) {
            caps[0].to_string()
        } else if prefix.to_ascii_lowercase().contains("authorization")
            || should_redact_whitespace_credential(prefix, value)
        {
            format!("{left}{prefix}[redacted-secret]")
        } else {
            caps[0].to_string()
        }
    });
    let stage3 = secret_re().replace_all(&stage2b, |caps: &regex::Captures| {
        let m = &caps[0];
        // Group 1 is the vendor branch's captured left separator (empty for the
        // `^` case, absent for every other branch). Judge the KEY that follows
        // it, and re-emit the separator — it may be multi-byte, so slice by byte
        // length, not char count.
        let boundary = caps.get(1).map_or("", |sep| sep.as_str());
        let key = &m[boundary.len()..];
        let is_keyed = KEY_PREFIXES.iter().any(|prefix| key.starts_with(prefix));
        if is_keyed || looks_high_entropy(key) {
            format!("{boundary}[redacted-secret]")
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
/// Callers holding non-ASCII decimal digits map each to its ASCII byte first
/// (`redact_card_run` does this for the fullwidth block), so this stays ASCII-only.
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
    fn redacts_low_entropy_credentials_by_structure_not_content() {
        // Every other key=value / auth-header test uses a value that ALSO trips
        // `should_redact` (digit/dash/quote), so a refactor that gated these
        // branches on entropy would still pass them. `devkey` is pure lowercase
        // letters, no digit/punct, <16 chars: it can only be redacted by the
        // structural `key=value` (stage 2) and `authorization` (stage 2b) arms.
        assert_eq!(redact("token=devkey"), "token=[redacted-secret]");
        assert!(redact("Authorization Bearer devkey").contains("[redacted-secret]"));
        assert!(!redact("Authorization Bearer devkey").contains("devkey"));
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
        assert!(
            !out.contains("hunter2"),
            "smart-quoted password leaked: {out}"
        );

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
            assert!(
                !out.contains("letmein"),
                "weak password leaked: {input} -> {out}"
            );
            assert!(out.contains("[redacted-secret]"), "no placeholder: {out}");
        }
    }

    #[test]
    fn redacts_whitespace_delimited_secret_key_values() {
        // `secret` participates in the whitespace form like the colon form
        // (parity fix): a digit/separator/long value after "secret " is scrubbed.
        for input in [
            "secret hunter2verysecretvalue",
            "secret abc-def-ghi",
            "SECRET \"quoted value\"",
        ] {
            let out = redact(input);
            assert!(
                out.contains("[redacted-secret]"),
                "whitespace secret leaked: {input} -> {out}"
            );
        }
        // Plain prose after "secret" survives — the value heuristic (digits,
        // separators, quoting, length) still gates the whitespace form.
        assert_eq!(redact("keep it secret garden"), "keep it secret garden");
    }

    #[test]
    fn whitespace_code_prose_survives_by_design() {
        // The whitespace form deliberately omits the `code` key (see
        // whitespace_credential_re): these everyday phrases must never be
        // corrupted, while assignment-shaped `code=` stays redacted.
        assert_eq!(redact("status code 404"), "status code 404");
        assert_eq!(redact("area code 212"), "area code 212");
        assert_eq!(redact("code abc123"), "code abc123");
        assert_eq!(redact("code=abc123"), "code=[redacted-secret]");
    }

    #[test]
    fn redaction_is_idempotent_for_quoted_credential_shapes() {
        for input in [
            r#"{"password": "hunter2", "api_key": "k"}"#,
            "“password”: “hunter2”",
            "«api_key»: «short-dev-key»",
            "password “letmein” trailing",
            "password [redacted-secret]}",
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
            // Stopper-glued variants: a closer between placeholder and secret
            // must not carve the secret out of the match.
            "password=[redacted-secret]}hunter2trailing",
            "password=[redacted-x])hunter2trailing",
            "password=[redacted-]hunter2trailing",
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

        // The placeholder shell itself is strict: a non-lowercase name or an
        // empty name is NOT an existing placeholder and re-redacts whole, so
        // a secret wrapped in a placeholder costume cannot ride through.
        let cased = redact("password=[redacted-Abc123]");
        assert_eq!(cased, "password=[redacted-secret]");
        assert!(!cased.contains("Abc123"));
        assert_eq!(redact("password=[redacted-]"), "password=[redacted-secret]");
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
        // looks_high_entropy's base64-punct arm (+,=) ALONE marks a long token
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
        // looks_high_entropy's base64-punct arm lists ('+','='). A standard
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
    fn redacts_all_lowercase_vendor_keyed_token_via_prefix_not_entropy() {
        // Every token in redacts_vendor_key_prefixes carries a digit or mixed
        // case, so looks_high_entropy would redact it even if the is_keyed
        // prefix check were removed. These tokens are all-lowercase, all-letter
        // (bar the vendor prefix), under 32 chars, and dash/underscore is not a
        // base64-punct signal — so looks_high_entropy returns false and the
        // KEY_PREFIXES is_keyed branch is the ONLY thing that redacts them.
        // Pins that dropping is_keyed would silently leak lowercase vendor keys.
        for token in [
            "sk-abcdefghijklmnopqrst",
            "ghp_abcdefghijklmnopqrst",
            "glpat-abcdefghijklmnopqr",
            "whsec_abcdefghijklmnopqr",
        ] {
            let out = redact(&format!("key {token} done"));
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
    fn fullwidth_digit_card_numbers_are_redacted() {
        // `card_run_re`'s `\d` is Unicode `\p{Nd}`, so a fullwidth-digit run
        // (U+FF10..U+FF19) reaches `redact_card_run` — which counted only
        // `is_ascii_digit()` and so saw zero digits and returned the run
        // untouched. A CJK input method emits exactly these glyphs, so the SAME
        // PAN was scrubbed or leaked depending on the keyboard layout. Probe
        // before the fix:
        //   redact("４１１１１１１１１１１１１１１１") -> unchanged
        //   redact("4111111111111111")            -> "[redacted-card]"
        //
        // GIVEN a Luhn-valid PAN typed in fullwidth digits, WHEN it is redacted,
        // THEN it becomes the card placeholder exactly like its ASCII twin.
        let fullwidth = format!("４{}", "１".repeat(15)); // 4111111111111111
        assert_eq!(redact(&fullwidth), "[redacted-card]");
        assert_eq!(
            redact(&format!("pan {fullwidth} end")),
            "pan [redacted-card] end"
        );
        assert_eq!(redact("4111111111111111"), "[redacted-card]");

        // A run mixing ASCII and fullwidth digits is ONE card: the span covers
        // both, so no half-redacted digit tail survives.
        let mixed = format!("４{}１", "1".repeat(14)); // 4111111111111111
        assert_eq!(redact(&mixed), "[redacted-card]");

        // NEGATIVE control — Luhn still gates it, so the fix cannot amount to
        // "redact any long fullwidth digit run". `1234567812345671` is the run
        // `leaves_non_luhn_digit_runs_alone` pins as a surviving order id: it has
        // no Luhn-valid 13–19 window anywhere inside it, so its fullwidth twin
        // must survive verbatim too.
        let ascii_order_id = "1234567812345671";
        let fullwidth_order_id: String = ascii_order_id
            .chars()
            .map(|c| {
                char::from_u32(u32::from('\u{ff10}') + (c as u32 - '0' as u32)).expect("digit")
            })
            .collect();
        assert_eq!(redact(ascii_order_id), ascii_order_id);
        assert_eq!(redact(&fullwidth_order_id), fullwidth_order_id);
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
    fn redacts_every_weak_password_after_a_password_prefix() {
        // The space-delimited weak-password set (admin/password/qwerty/secret/
        // welcome, plus the letmein/swordfish already pinned above) is the ONLY
        // arm that catches these: each value is all-letters, unquoted, has no
        // digit or punct, and is under 16 chars, so no length/entropy arm fires.
        // Removing any entry from the list would silently leak that credential.
        for weak in ["admin", "password", "qwerty", "secret", "welcome"] {
            assert_eq!(
                redact(&format!("password {weak}")),
                "password [redacted-secret]",
                "weak password {weak} not redacted"
            );
        }
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
    fn lowercase_urls_and_paths_survive_the_generic_secret_branch() {
        // A `/` is the separator of every URL and filesystem path, not an
        // entropy signal on its own: before this pin, any 32+ char lowercase
        // path run was scrubbed to `[redacted-secret]`, silently stripping most
        // URLs from stored memory and diagnostics (2026-09-08 audit, G13).
        let url = "see https://example.com/some/long/path/segment/that/keeps/going";
        assert_eq!(redact(url), url);
        let path = "open /home/alice/documents/projects/compme/crates/redaction/src/lib.rs";
        assert_eq!(redact(path), path);
        // A lowercase run with `+` or `=` is still base64-shaped and still trips.
        let with_plus = "blob abcdefghij/klmnopqrstuvwxyz+abcdefghij end";
        assert_eq!(redact(with_plus), "blob [redacted-secret] end");
    }

    #[test]
    fn mixed_case_or_digit_paths_are_still_over_redacted_known_cost() {
        // ACCEPTED COST of the entropy heuristic: a path run that carries a
        // digit or mixed case is indistinguishable from a base64 token by
        // shape, and privacy wins. Pinned so the boundary cannot move silently
        // in either direction.
        assert_eq!(
            redact("open /Users/Alice/Documents/projects/compme/crates/redaction"),
            "open [redacted-secret]"
        );
        assert_eq!(
            redact("see https://cdn.example.com/assets/v2/application/bundle.js"),
            "see https:[redacted-secret]"
        );
        // Keyed parameters inside a URL are caught by the credential pass
        // regardless of the generic branch (unchanged behaviour).
        assert_eq!(
            redact("https://x.com/cb?code=abc123"),
            "https://x.com/cb?code=[redacted-secret]"
        );
    }

    #[test]
    fn all_lowercase_all_letter_long_token_is_not_redacted_known_gap() {
        // ACCEPTED GAP / entropy-heuristic boundary: a 32+ char token matches
        // the generic secret regex, but `looks_high_entropy` leaves an
        // all-one-case, all-letter run alone (no digit, no mixed case, no b64
        // punctuation) because the public contract preserves long prose words
        // unless a credential key/prefix or other entropy signal is present.
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
    fn underscore_compound_credential_keys_are_redacted() {
        // `_` is a word character in the `regex` crate, so the old leading `\b`
        // never fired for the compound environment-variable keys that carry most
        // real secrets. Probe before the fix:
        //   redact("DB_PASSWORD=hunter2 POSTGRES_PASSWORD=root mypassword=abc")
        //     -> unchanged, nothing redacted.
        // The left separator is captured and re-emitted, so the key name survives
        // verbatim and only the value is scrubbed.
        assert_eq!(
            redact("DB_PASSWORD=hunter2"),
            "DB_PASSWORD=[redacted-secret]"
        );
        assert_eq!(
            redact("POSTGRES_PASSWORD: root"),
            "POSTGRES_PASSWORD: [redacted-secret]"
        );
        assert_eq!(redact("SLACK_TOKEN=abc"), "SLACK_TOKEN=[redacted-secret]");
        assert_eq!(
            redact("DB_PASSWORD=hunter2 POSTGRES_PASSWORD=root"),
            "DB_PASSWORD=[redacted-secret] POSTGRES_PASSWORD=[redacted-secret]"
        );

        // The widened boundary accepts only non-alphanumerics, so a key name
        // glued to a preceding letter still survives mid-word.
        assert_eq!(redact("mypassword=hunter2value"), "mypassword=hunter2value");

        // `code` keeps the strict `\b`: widening it would corrupt ordinary data.
        assert_eq!(redact("error_code=500"), "error_code=500");
    }

    #[test]
    fn token_secret_keys_respect_word_boundary() {
        // The higher-frequency credential keys (`token`, `secret`, …) anchor their
        // key NAME with a captured `(^|[^A-Za-z0-9])` left separator — the `\b`
        // they used to use, widened to also accept `_` (see `credential_re`). Only
        // `code` kept a strict word-boundary pin, in its own pattern; this covers
        // the common keys that otherwise ride on inference. Two directions:
        //
        // NEGATIVE — the left separator must NOT accept an alphanumeric. Here the
        // credential key name is glued to a letter AND sits directly adjacent to
        // `=`, so the `\s*[:=]` adjacency requirement does NOT save us — only the
        // separator does. Widening it (or dropping it) makes `token=`/`password=`
        // match the tail of `xtoken`/`mypassword` and wrongly redact. These must
        // survive verbatim.
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
        // NEGATIVE direction: `code` lives in its own `code_credential_re` pattern
        // anchored `\bcode\b`, deliberately NOT behind the `_`-tolerant left
        // separator the other credential keys use. Compound keys whose suffix is
        // `code` (preceded by a word char like `_` or a letter) are legitimate
        // data and must pass through verbatim — value preserved unchanged.
        // Folding `code` back into `credential_re`'s alternation would silently
        // start redacting these and corrupt the data.
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

    #[test]
    fn ordinary_compound_words_are_not_treated_as_vendor_prefixed_keys() {
        // The vendor-prefix branch `(?:sk|ghp|gho|ghu|ghs|ghr|pk|rk)[-_]…{16,}`
        // had no left boundary, so it fired on the `sk-` inside `risk-`, the
        // `sk_` inside `task_` and the `rk-` inside `network-`. Probe before the
        // fix:
        //   redact("risk-assessment-framework task_management_service network-security-policies")
        //     -> "ri[redacted-secret] ta[redacted-secret] netwo[redacted-secret]"
        //
        // GIVEN ordinary hyphen/snake compound words that merely CONTAIN a vendor
        // prefix mid-word, WHEN they are redacted, THEN each survives verbatim.
        for word in [
            "risk-assessment-framework",
            "task_management_service",
            "network-security-policies",
            "desk-organisation-checklist",
            "disk_cache_directory_structure",
        ] {
            let text = format!("see {word} here");
            assert_eq!(redact(&text), text, "compound word scrubbed: {word}");
        }
        let prose = "risk-assessment-framework task_management_service network-security-policies";
        assert_eq!(redact(prose), prose);

        // POSITIVE — a real vendor key still redacts behind every left boundary
        // the pattern accepts: start of string, after a space, and after `(`.
        for key in ["sk-abcdefghijklmnop123456", "ghp_abcdefghijklmnop123456"] {
            for input in [
                key.to_string(),
                format!("key {key} done"),
                format!("({key})"),
            ] {
                let out = redact(&input);
                assert!(
                    out.contains("[redacted-secret]"),
                    "{input} not redacted -> {out}"
                );
                assert!(!out.contains(key), "{input} leaked -> {out}");
            }
        }

        // The boundary character is re-emitted, not swallowed — including a
        // multi-byte one, which a char-count slice would corrupt.
        assert_eq!(redact("(sk-abcdefghijklmnop123456)"), "([redacted-secret])");
        assert_eq!(redact("«sk-abcdefghijklmnop123456»"), "«[redacted-secret]»");
    }
}
