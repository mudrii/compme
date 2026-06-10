//! Model download support (engine-macos §15 D14 tick 2, designed c95).
//!
//! This slice is the pure core: integrity verification (SHA-256) and
//! resume planning (HTTP Range headers). The blocking network loop (ureq,
//! dedicated thread) is the next slice — keeping it out means everything
//! here is unit-testable without sockets.

use sha2::{Digest, Sha256};

/// Hex SHA-256 of `bytes` (lowercase, 64 chars) — the digest format the
/// catalog's expected-hash entries will use.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `bytes` hash to `expected_hex` (case-insensitive comparison —
/// hashes get pasted from release notes in either case).
pub fn sha256_matches(bytes: &[u8], expected_hex: &str) -> bool {
    sha256_hex(bytes) == expected_hex.to_ascii_lowercase()
}

/// The `Range` header value to resume a partial download of `existing_len`
/// bytes, or `None` to start from scratch (nothing on disk yet).
pub fn resume_range_header(existing_len: u64) -> Option<String> {
    (existing_len > 0).then(|| format!("bytes={existing_len}-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_the_known_empty_and_abc_vectors() {
        // FIPS 180-2 test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_matches_is_case_insensitive_and_rejects_wrong_hashes() {
        assert!(sha256_matches(
            b"abc",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        ));
        assert!(!sha256_matches(b"abc", "deadbeef"));
        assert!(!sha256_matches(b"abcd", &sha256_hex(b"abc")));
    }

    #[test]
    fn resume_range_header_starts_fresh_at_zero_and_resumes_beyond() {
        assert_eq!(resume_range_header(0), None);
        assert_eq!(resume_range_header(1), Some("bytes=1-".to_string()));
        assert_eq!(
            resume_range_header(398_000_000),
            Some("bytes=398000000-".to_string())
        );
    }
}
