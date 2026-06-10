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
    /// Downloaded bytes hash differently than the catalog expects. The part
    /// file is KEPT for inspection; dest is never created.
    HashMismatch { expected: String, actual: String },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Network(msg) => write!(f, "network error: {msg}"),
            FetchError::Http(code) => write!(f, "http error: status {code}"),
            FetchError::Io(msg) => write!(f, "io error: {msg}"),
            FetchError::HashMismatch { expected, actual } => {
                write!(f, "sha256 mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for FetchError {}

/// Download `url` to `dest` with resume. Redirects (HF resolve URLs hop to
/// a CDN) are followed by ureq; whatever the final host does with our Range
/// header is safe either way — a 206 is only trusted after Content-Range
/// validation and anything else restarts from zero.
///
/// Strategy (banked D14 design):
/// partial bytes live in `dest.part`; a non-empty part sends `Range:
/// bytes=N-`. A 206 appends from N; a 200 means the server ignored Range —
/// truncate and restart from zero. Any failure KEEPS the part file for the
/// next resume attempt; success renames part → dest. `progress` receives
/// (bytes_so_far, total_if_known) per chunk.
pub fn download_url(
    url: &str,
    dest: &std::path::Path,
    expected_sha256: Option<&str>,
    progress: impl Fn(u64, Option<u64>),
) -> Result<std::path::PathBuf, FetchError> {
    // Timeouts are mandatory (review-c118 CRITICAL): without a read timeout
    // a stalled server hangs the download — and the worker thread's
    // shutdown — forever. 30s reads tolerate slow CDNs; connect fails fast.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(30))
        .build();
    download_with_agent(&agent, url, dest, expected_sha256, progress)
}

/// Agent-injectable core — tests drive it with millisecond timeouts.
fn download_with_agent(
    agent: &ureq::Agent,
    url: &str,
    dest: &std::path::Path,
    expected_sha256: Option<&str>,
    progress: impl Fn(u64, Option<u64>),
) -> Result<std::path::PathBuf, FetchError> {
    let part = dest.with_extension("part");
    let existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let mut request = agent.get(url);
    if let Some(range) = resume_range_header(existing) {
        request = request.set("Range", &range);
    }
    let response = request.call().map_err(|e| match e {
        ureq::Error::Status(code, _) => FetchError::Http(code),
        other => FetchError::Network(other.to_string()),
    })?;

    let status = response.status();
    // Trust a 206 only when its Content-Range start matches OUR offset —
    // a lying/buggy server stitched at the wrong offset would corrupt the
    // file silently (review-c118 CRITICAL). Anything else → safe restart.
    let resumed = status == 206
        && response
            .header("Content-Range")
            .is_some_and(|cr| cr.starts_with(&format!("bytes {existing}-")));
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
    // Verify BEFORE the rename: dest must never exist with wrong bytes. A
    // mismatch keeps the part for inspection (resume would re-download from
    // its end and mismatch again — the caller decides whether to delete).
    if let Some(expected) = expected_sha256 {
        let file = std::fs::File::open(&part).map_err(|e| FetchError::Io(e.to_string()))?;
        let actual = read_sha256_hex(std::io::BufReader::new(file))
            .map_err(|e| FetchError::Io(e.to_string()))?;
        if actual != expected.to_ascii_lowercase() {
            return Err(FetchError::HashMismatch {
                expected: expected.to_ascii_lowercase(),
                actual,
            });
        }
    }
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
    enum RangeMode {
        /// Honor Range with a correct 206 + Content-Range.
        Honor,
        /// Always 200 with the full body (server ignores Range).
        Ignore,
        /// LIE: reply 206 but serve the FULL body from offset 0 with a
        /// Content-Range that contradicts the request (corruption trap).
        Lie,
    }

    fn serve(body: &'static [u8], mode: RangeMode) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut req = [0u8; 2048];
                let n = stream.read(&mut req).unwrap_or(0);
                let req = String::from_utf8_lossy(&req[..n]);
                let requested = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|r| r.split('-').next())
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|&s| s < body.len());
                let (status, slice, content_range) = match (&mode, requested) {
                    (RangeMode::Honor, Some(s)) => (
                        "206 Partial Content",
                        &body[s..],
                        Some(format!("bytes {s}-{}/{}", body.len() - 1, body.len())),
                    ),
                    (RangeMode::Lie, Some(_)) => (
                        "206 Partial Content",
                        body,
                        Some(format!("bytes 0-{}/{}", body.len() - 1, body.len())),
                    ),
                    _ => ("200 OK", body, None),
                };
                let _ = write!(stream, "HTTP/1.1 {status}\r\n");
                if let Some(cr) = content_range {
                    let _ = write!(stream, "Content-Range: {cr}\r\n");
                }
                let _ = write!(
                    stream,
                    "Content-Length: {}\r\nConnection: close\r\n\r\n",
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
    fn stalled_server_times_out_instead_of_hanging_forever() {
        // review-c118 CRITICAL: no read timeout = a stalled server hangs the
        // download (and later the worker thread's shutdown) forever. The
        // agent must carry a read timeout; here a short one proves the
        // stall surfaces as a Network error instead of a hang.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n"
                );
                // Headers sent, body never arrives.
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
        let agent = ureq::AgentBuilder::new()
            .timeout_read(std::time::Duration::from_millis(300))
            .build();
        let dest = temp_dest("stall");
        let err = download_with_agent(
            &agent,
            &format!("http://{addr}/model.bin"),
            &dest,
            None,
            |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, FetchError::Network(_)));
        let _ = std::fs::remove_file(dest.with_extension("part"));
    }

    #[test]
    fn sha_mismatch_errors_and_keeps_the_part_file_for_inspection() {
        let url = serve(b"corrupted bytes", RangeMode::Honor);
        let dest = temp_dest("badsha");
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(dest.with_extension("part"));
        let expected = sha256_hex(b"expected bytes");
        let err = download_url(&url, &dest, Some(&expected), |_, _| {}).unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }));
        assert!(
            dest.with_extension("part").exists(),
            "part kept for inspection"
        );
        assert!(!dest.exists(), "dest never appears on mismatch");
        let _ = std::fs::remove_file(dest.with_extension("part"));
    }

    #[test]
    fn matching_sha_verifies_and_renames() {
        let url = serve(b"good bytes", RangeMode::Honor);
        let dest = temp_dest("goodsha");
        let _ = std::fs::remove_file(&dest);
        let expected = sha256_hex(b"good bytes");
        let got = download_url(&url, &dest, Some(&expected), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"good bytes");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn fresh_download_writes_dest_and_removes_the_part_file() {
        let url = serve(b"hello model bytes", RangeMode::Honor);
        let dest = temp_dest("fresh");
        let _ = std::fs::remove_file(&dest);
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"hello model bytes");
        assert!(!dest.with_extension("part").exists());
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn resume_appends_from_the_part_file_offset() {
        let url = serve(b"0123456789", RangeMode::Honor);
        let dest = temp_dest("resume");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"0123").unwrap();
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn lying_206_with_wrong_content_range_restarts_instead_of_corrupting() {
        // review-c118 CRITICAL: a 206 whose Content-Range start does not
        // match our offset must NOT be appended — blind stitching corrupts
        // the file silently. The safe move is a from-zero restart.
        let url = serve(b"0123456789", RangeMode::Lie);
        let dest = temp_dest("lie206");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"0123").unwrap();
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn server_ignoring_range_restarts_from_zero() {
        let url = serve(b"0123456789", RangeMode::Ignore); // always 200, full body
        let dest = temp_dest("restart");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"GARBAGE").unwrap();
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
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
