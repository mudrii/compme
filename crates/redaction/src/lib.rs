//! Redaction of sensitive text before any persistence or diagnostics (design
//! spec §6/§7: "Redaction before any persistence — emails, card-like numbers
//! (Luhn), tokens/secrets"). Pure: text in, redacted text out.
//!
//! This is a best-effort scrubber, not a guarantee — it removes the obvious,
//! high-risk PII classes so accepted-completion memory and diagnostics never
//! store raw secrets. Order matters: secrets and cards are matched before the
//! generic email pass so a token containing `@` is not mis-handled.

use std::sync::OnceLock;

use regex::Regex;

/// Matches API-key / secret-like tokens: AWS access-key ids, common prefixed
/// keys (`sk-`, `ghp_`-style), and long mixed alphanumeric tokens.
fn secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
              AKIA[0-9A-Z]{16}
            | (?:sk|ghp|gho|ghu|ghs|ghr|pk|rk)[-_][A-Za-z0-9_-]{16,}
            | [A-Za-z0-9+/_-]{32,}
            ",
        )
        .expect("secret regex")
    })
}

/// Matches a run of 13–19 digits, optionally separated by single spaces or
/// dashes (a candidate card number; Luhn-checked before redacting).
fn card_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d(?:[ -]?\d){12,18}").expect("card regex"))
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email regex")
    })
}

fn has_letter_and_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic()) && s.chars().any(|c| c.is_ascii_digit())
}

/// Replace emails, Luhn-valid card numbers, and API-key/secret-like tokens with
/// stable placeholders. Idempotent on already-redacted text.
///
/// Secrets and cards are matched before emails so a token containing `@` is not
/// mishandled, and card candidates are Luhn-checked so ordinary long ids survive.
pub fn redact(input: &str) -> String {
    // 1. Secrets. The generic long-token branch only redacts mixed alphanumerics
    //    so long all-letter words in prose are preserved.
    let stage1 = secret_re().replace_all(input, |caps: &regex::Captures| {
        let m = &caps[0];
        let is_keyed = m.starts_with("AKIA")
            || matches!(
                m.split_once(['-', '_']),
                Some((prefix, _)) if matches!(prefix, "sk" | "ghp" | "gho" | "ghu" | "ghs" | "ghr" | "pk" | "rk")
            );
        if is_keyed || (m.len() >= 32 && has_letter_and_digit(m)) {
            "[redacted-secret]".to_string()
        } else {
            m.to_string()
        }
    });

    // 2. Card numbers (Luhn-validated).
    let stage2 = card_re().replace_all(&stage1, |caps: &regex::Captures| {
        let m = &caps[0];
        let digits: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
        if luhn_valid(&digits) {
            "[redacted-card]".to_string()
        } else {
            m.to_string()
        }
    });

    // 3. Emails.
    email_re()
        .replace_all(&stage2, "[redacted-email]")
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
    fn redaction_is_idempotent() {
        let once = redact("mail ada@example.com");
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn luhn_validates_known_values() {
        assert!(luhn_valid("4242424242424242"));
        assert!(!luhn_valid("4242424242424241"));
        assert!(!luhn_valid(""));
    }
}
