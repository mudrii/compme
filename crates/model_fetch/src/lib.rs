//! Model download support (engine-macos §15 D14 tick 2, designed c95).
//!
//! This slice is the pure core: integrity verification (SHA-256) and
//! resume planning (HTTP Range headers). The blocking network loop (ureq,
//! dedicated thread) is the next slice — keeping it out means everything
//! here is unit-testable without sockets.

use sha2::{Digest, Sha256};
use std::io::Read as _;

/// Hex SHA-256 of `bytes` (lowercase, 64 chars) — the digest format the
/// catalog's expected-hash entries will use.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lowercase hex of a digest.
fn hex(digest: &[u8]) -> String {
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

/// Hex SHA-256 of everything `reader` yields, streamed in 64KB chunks —
/// the fetch loop verifies multi-GB model files with this; `sha256_hex`
/// stays the in-memory primitive for small buffers and tests.
pub fn read_sha256_hex(mut reader: impl std::io::Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            // Retriable per the Read contract (std::io::copy does the same)
            // — a transient EINTR must not abort a multi-GB verification.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(hex(&hasher.finalize()))
}

/// Why a download failed. Kept to the variants this slice can produce —
/// HashMismatch arrives with the catalog-hash slice, Cancelled with the
/// progress-cancel slice (banked D14 design trims to YAGNI per tick).
#[derive(Debug)]
pub enum FetchError {
    /// Connect/transport failure (DNS, TLS, timeout, mid-body IO).
    Network(String),
    /// HTTP error status (4xx/5xx).
    Http(u16),
    /// Local filesystem failure (.part create/write/rename).
    Io(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Network(msg) => write!(f, "network error: {msg}"),
            FetchError::Http(code) => write!(f, "http error: status {code}"),
            FetchError::Io(msg) => write!(f, "io error: {msg}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Download `url` to `dest` with resume. Strategy (banked D14 design):
/// partial bytes live in `dest.part`; a non-empty part sends `Range:
/// bytes=N-`. A 206 appends from N; a 200 means the server ignored Range —
/// truncate and restart from zero. Any failure KEEPS the part file for the
/// next resume attempt; success renames part → dest. `progress` receives
/// (bytes_so_far, total_if_known) per chunk.
pub fn download_url(
    url: &str,
    dest: &std::path::Path,
    progress: impl Fn(u64, Option<u64>),
) -> Result<std::path::PathBuf, FetchError> {
    let part = dest.with_extension("part");
    let existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let mut request = ureq::get(url);
    if let Some(range) = resume_range_header(existing) {
        request = request.set("Range", &range);
    }
    let response = request.call().map_err(|e| match e {
        ureq::Error::Status(code, _) => FetchError::Http(code),
        other => FetchError::Network(other.to_string()),
    })?;

    let status = response.status();
    let resumed = status == 206;
    let total = response
        .header("Content-Length")
        .and_then(|h| h.parse::<u64>().ok())
        .map(|body_len| body_len + if resumed { existing } else { 0 });

    // 206 → append to the part; anything else → truncate (fresh or the
    // server-ignored-Range restart).
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(resumed)
        .write(true)
        .truncate(!resumed)
        .open(&part)
        .map_err(|e| FetchError::Io(e.to_string()))?;

    let mut reader = response.into_reader();
    let mut written = if resumed { existing } else { 0 };
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                use std::io::Write as _;
                file.write_all(&buf[..n])
                    .map_err(|e| FetchError::Io(e.to_string()))?;
                written += n as u64;
                progress(written, total);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // Keep the part file: the bytes so far are the next resume base.
            Err(e) => return Err(FetchError::Network(e.to_string())),
        }
    }
    drop(file);
    std::fs::rename(&part, dest).map_err(|e| FetchError::Io(e.to_string()))?;
    Ok(dest.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write as _};
    use std::net::TcpListener;

    /// Minimal one-request-per-connection HTTP server on a random port:
    /// parses an optional `Range: bytes=N-` header; honors it with 206 when
    /// `support_range`, else always replies 200 with the full body (the
    /// server-ignores-Range case the download loop must restart on).
    fn serve(body: &'static [u8], support_range: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut req = [0u8; 2048];
                let n = stream.read(&mut req).unwrap_or(0);
                let req = String::from_utf8_lossy(&req[..n]);
                let start = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|r| r.split('-').next())
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|_| support_range)
                    .filter(|&s| s < body.len());
                let (status, slice) = match start {
                    Some(s) => ("206 Partial Content", &body[s..]),
                    None => ("200 OK", body),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    slice.len()
                );
                let _ = stream.write_all(slice);
            }
        });
        format!("http://{addr}/model.bin")
    }

    fn temp_dest(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cm-fetch-{tag}-{}.bin", std::process::id()))
    }

    #[test]
    fn fresh_download_writes_dest_and_removes_the_part_file() {
        let url = serve(b"hello model bytes", true);
        let dest = temp_dest("fresh");
        let _ = std::fs::remove_file(&dest);
        let got = download_url(&url, &dest, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"hello model bytes");
        assert!(!dest.with_extension("part").exists());
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn resume_appends_from_the_part_file_offset() {
        let url = serve(b"0123456789", true);
        let dest = temp_dest("resume");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"0123").unwrap();
        let got = download_url(&url, &dest, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn server_ignoring_range_restarts_from_zero() {
        let url = serve(b"0123456789", false); // always 200, full body
        let dest = temp_dest("restart");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"GARBAGE").unwrap();
        let got = download_url(&url, &dest, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn read_sha256_hex_streams_and_matches_the_buffer_hash() {
        // The fetch loop verifies 1.7GB model files — hashing must stream,
        // never slurp. Same digest as the in-memory primitive, across
        // multiple read chunks, and IO errors propagate.
        assert_eq!(
            read_sha256_hex(Cursor::new(b"abc")).unwrap(),
            sha256_hex(b"abc")
        );
        let big: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
        assert_eq!(
            read_sha256_hex(Cursor::new(&big)).unwrap(),
            sha256_hex(&big)
        );

        struct InterruptedOnce(u8);
        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0 {
                    0 => {
                        self.0 = 1;
                        Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                    }
                    1 => {
                        self.0 = 2;
                        // Defensive per the Read contract: honor tiny buffers
                        // (review-c116) — this double is copy-paste bait.
                        let n = 3.min(buf.len());
                        buf[..n].copy_from_slice(&b"abc"[..n]);
                        Ok(n)
                    }
                    _ => Ok(0),
                }
            }
        }
        // ErrorKind::Interrupted is retriable per the Read contract
        // (std::io::copy retries it) — a transient EINTR must not abort a
        // multi-GB download's verification.
        assert_eq!(
            read_sha256_hex(InterruptedOnce(0)).unwrap(),
            sha256_hex(b"abc")
        );

        struct FailingReader;
        impl std::io::Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk gone"))
            }
        }
        assert!(read_sha256_hex(FailingReader).is_err());
    }

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
