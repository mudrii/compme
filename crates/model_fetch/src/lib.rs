//! Model download support (engine-macos §15 D14).
//!
//! Two halves, one crate: a PURE core (SHA-256 integrity, resume planning —
//! unit-testable with no IO) and the blocking network half (`download_url`
//! over ureq with resume/restart/verify semantics, plus the
//! `ModelDownloader` worker thread). The seam stays inside this crate so
//! the protocol tests can drive the real network code against a loopback
//! mini-server; nothing here touches AppKit or the engine.

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
    // shutdown — forever. timeout_read is PER READ CALL (socket-level), not
    // whole-body: each 64KB chunk gets a fresh 30s window, so multi-GB
    // downloads on slow links are fine; only a true stall trips it.
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
    let response = match request.call() {
        Ok(response) => response,
        // 416 with a part on disk = the server's file is smaller than our
        // resume offset (it shrank/changed). Keeping the part would 416 on
        // every retry forever (review-c119) — drop it and restart unranged.
        // Recursion is bounded ONLY because removal succeeds: the retry then
        // recomputes existing==0 and sends no Range, so it cannot 416 down
        // this arm again. If the part can't be removed (immutable flag,
        // read-only parent, transient EIO), surface Io rather than recurse —
        // a surviving part re-sends a Range and 416s back into this arm,
        // overflowing the stack instead of failing.
        Err(ureq::Error::Status(416, _)) if existing > 0 => {
            std::fs::remove_file(&part).map_err(|e| FetchError::Io(e.to_string()))?;
            return download_with_agent(agent, url, dest, expected_sha256, progress);
        }
        Err(ureq::Error::Status(code, _)) => return Err(FetchError::Http(code)),
        Err(other) => return Err(FetchError::Network(other.to_string())),
    };

    let status = response.status();
    // Trust a 206 only when its Content-Range start matches OUR offset —
    // a lying/buggy server stitched at the wrong offset would corrupt the
    // file silently (review-c118 CRITICAL). Anything else → safe restart.
    let resumed = status == 206
        && response
            .header("Content-Range")
            .is_some_and(|cr| cr.starts_with(&format!("bytes {existing}-")));
    // A 206 we could NOT validate after a ranged request is doubly suspect:
    // its body may genuinely be the tail slice we asked for (a server that
    // honors Range but omits Content-Range), so consuming it down the
    // truncate path would silently write a head-truncated dest. Mirror the
    // 416 arm: drop the part and re-request unranged — bounded the same way
    // (the retry sends no Range, so this arm cannot recurse).
    if status == 206 && !resumed && existing > 0 {
        // Same bound as the 416 arm: removal must succeed for the retry to
        // recompute existing==0 and drop the Range. Propagate Io on failure
        // rather than recursing into an unbounded re-request.
        std::fs::remove_file(&part).map_err(|e| FetchError::Io(e.to_string()))?;
        return download_with_agent(agent, url, dest, expected_sha256, progress);
    }
    let total = response
        .header("Content-Length")
        .and_then(|h| h.parse::<u64>().ok())
        // saturating: Content-Length is attacker-controlled; a value near u64::MAX
        // plus the resume offset would overflow (a debug-build panic) for a number
        // that only ever feeds the progress bar. The SHA-256 verify-before-rename
        // is the real integrity gate, so a clamped total is harmless.
        .map(|body_len| body_len.saturating_add(if resumed { existing } else { 0 }));

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
    // Flush buffered bytes to disk before the rename so a crash between the
    // write and the rename can't leave `dest` pointing at unpersisted data.
    // On failure the part file is kept as the resume base.
    file.sync_all().map_err(|e| FetchError::Io(e.to_string()))?;
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

/// Where a queued download stands. The run loop polls this (no callbacks
/// into AppKit from the worker thread).
#[derive(Debug, Default)]
pub enum DownloadState {
    #[default]
    Idle,
    Running,
    Done(std::path::PathBuf),
    Failed(String),
}

/// Shared progress block: the worker writes, the run loop reads.
#[derive(Debug, Default)]
pub struct DownloadStatus {
    /// Bytes written so far (resume offset included).
    pub downloaded: std::sync::atomic::AtomicU64,
    /// Total bytes if the server said (0 = no Content-Length, size unknown;
    /// a genuinely zero-byte file would read the same, but no model is).
    pub total: std::sync::atomic::AtomicU64,
    pub state: std::sync::Mutex<DownloadState>,
}

/// One queued download.
pub struct DownloadRequest {
    pub url: String,
    pub dest: std::path::PathBuf,
    pub expected_sha256: Option<String>,
    pub status: std::sync::Arc<DownloadStatus>,
}

/// Fire-and-forget download worker (the ScreenOcr pattern): a depth-1
/// channel coalesces bursts, requests run one at a time on a dedicated
/// thread, and Drop detaches rather than joins — a mid-download shutdown
/// must not block on a slow server (the part file makes it resumable).
///
/// ONE per process (review-c120): the detached thread finishes its current
/// item after Drop, so a drop-and-respawn pattern could put two workers on
/// the same dest.part concurrently. The run loop owns a single instance for
/// the process lifetime.
pub struct ModelDownloader {
    tx: Option<std::sync::mpsc::SyncSender<DownloadRequest>>,
    _handle: std::thread::JoinHandle<()>,
}

impl ModelDownloader {
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<DownloadRequest>(1);
        let handle = std::thread::Builder::new()
            .name("compme-model-fetch".into())
            .spawn(move || {
                while let Ok(req) = rx.recv() {
                    *req.status.state.lock().unwrap_or_else(|e| e.into_inner()) =
                        DownloadState::Running;
                    let status = std::sync::Arc::clone(&req.status);
                    let result = download_url(
                        &req.url,
                        &req.dest,
                        req.expected_sha256.as_deref(),
                        move |written, total| {
                            status
                                .downloaded
                                .store(written, std::sync::atomic::Ordering::Relaxed);
                            // total==0 is the "unknown total" sentinel (server sent
                            // no Content-Length): a polling consumer computing a
                            // percentage must treat 0 as indeterminate, not as a
                            // known-zero/known-small total.
                            status
                                .total
                                .store(total.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
                        },
                    );
                    *req.status.state.lock().unwrap_or_else(|e| e.into_inner()) = match result {
                        Ok(path) => DownloadState::Done(path),
                        Err(err) => DownloadState::Failed(err.to_string()),
                    };
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            _handle: handle,
        })
    }

    /// Queue a download; never blocks. A full queue drops the request and
    /// returns `false` — callers must NOT track a dropped request's status
    /// (its state stays Idle forever, which would wedge an idle-gated
    /// consume edge). `true` = queued.
    // must_use because the bool is load-bearing for that invariant (the
    // repo's first — ignoring it silently reintroduces the c130 wedge).
    #[must_use]
    pub fn request(&self, request: DownloadRequest) -> bool {
        match &self.tx {
            Some(tx) => tx.try_send(request).is_ok(),
            None => false,
        }
    }
}

impl Drop for ModelDownloader {
    fn drop(&mut self) {
        // Close the channel; the worker exits after its current item.
        self.tx.take();
    }
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
    /// One-shot 404 server (no Range logic).
    fn serve_404() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let _ = write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        format!("http://{addr}/missing.bin")
    }

    #[test]
    fn http_404_surfaces_as_a_typed_http_error() {
        let url = serve_404();
        let dest = temp_dest("nf404");
        let err = download_url(&url, &dest, None, |_, _| {}).unwrap_err();
        assert!(matches!(err, FetchError::Http(404)), "got: {err}");
    }

    #[test]
    fn worker_surfaces_a_failed_download_as_failed_state() {
        // The Err→Failed(msg) mapping is what the picker UI renders — pin
        // that a dead download lands in Failed with the status in the text.
        let url = serve_404();
        let dest = temp_dest("wfail");
        let worker = ModelDownloader::spawn().unwrap();
        let status = std::sync::Arc::new(DownloadStatus::default());
        assert!(worker.request(DownloadRequest {
            url,
            dest,
            expected_sha256: None,
            status: std::sync::Arc::clone(&status),
        }));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let state = status.state.lock().unwrap();
                match &*state {
                    DownloadState::Failed(msg) => {
                        assert!(msg.contains("404"), "got: {msg}");
                        break;
                    }
                    DownloadState::Done(path) => panic!("unexpected success: {path:?}"),
                    _ => {}
                }
            }
            assert!(std::time::Instant::now() < deadline, "worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    enum RangeMode {
        /// Honor Range with a correct 206 + Content-Range.
        Honor,
        /// Honor Range (slice the body) but send NO Content-Range header —
        /// a 206 the client cannot validate (corruption trap: the body is
        /// partial, so consuming it as the full file truncates the head).
        HonorNoHeader,
        /// Always 200 with the full body (server ignores Range).
        Ignore,
        /// 200 with the full body but NO Content-Length header — the body is
        /// terminated by closing the connection (HTTP/1.0-style). The download
        /// loop can't know the total, so every progress event carries None.
        NoContentLength,
        /// LIE: reply 206 but serve the FULL body from offset 0 with a
        /// Content-Range that contradicts the request (corruption trap).
        Lie,
        /// LIE: reply 206 with a Content-Range whose start is a DIGIT-PREFIX
        /// of the requested offset (e.g. `bytes 40-` for a `bytes=4-`
        /// request) and the matching wrong-offset body slice. Pins that the
        /// trailing `-` in the offset check defeats a digit-prefix collision
        /// (`bytes 4` is a prefix of `bytes 40`).
        LieWrongOffset,
        /// Reply 416 to ANY ranged request (shrunk-file scenario).
        Unsatisfiable,
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
                let ranged = req.lines().any(|l| l.starts_with("Range:"));
                if matches!(mode, RangeMode::Unsatisfiable) && ranged {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    continue;
                }
                let (status, slice, content_range) = match (&mode, requested) {
                    (RangeMode::Honor, Some(s)) => (
                        "206 Partial Content",
                        &body[s..],
                        Some(format!("bytes {s}-{}/{}", body.len() - 1, body.len())),
                    ),
                    (RangeMode::HonorNoHeader, Some(s)) => {
                        ("206 Partial Content", &body[s..], None)
                    }
                    (RangeMode::Lie, Some(_)) => (
                        "206 Partial Content",
                        body,
                        Some(format!("bytes 0-{}/{}", body.len() - 1, body.len())),
                    ),
                    // Ranged request at offset s, but reply as if it were
                    // offset 40: `bytes 40-{end}/{total}` + body[40..]. When
                    // s==4 the wrong start `40` has `4` as a digit-prefix, so
                    // an offset check missing the trailing `-` would accept it.
                    (RangeMode::LieWrongOffset, Some(_)) => (
                        "206 Partial Content",
                        &body[40..],
                        Some(format!("bytes 40-{}/{}", body.len() - 1, body.len())),
                    ),
                    _ => ("200 OK", body, None),
                };
                let _ = write!(stream, "HTTP/1.1 {status}\r\n");
                if let Some(cr) = content_range {
                    let _ = write!(stream, "Content-Range: {cr}\r\n");
                }
                if matches!(mode, RangeMode::NoContentLength) {
                    // No Content-Length: the body length is signalled by the
                    // connection closing at the end of this loop iteration.
                    let _ = write!(stream, "Connection: close\r\n\r\n");
                } else {
                    let _ = write!(
                        stream,
                        "Content-Length: {}\r\nConnection: close\r\n\r\n",
                        slice.len()
                    );
                }
                let _ = stream.write_all(slice);
            }
        });
        format!("http://{addr}/model.bin")
    }

    fn temp_dest(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cm-fetch-{tag}-{}.bin", std::process::id()))
    }

    #[test]
    fn downloader_worker_runs_a_request_off_thread_and_reports_progress() {
        // 3b.3: fire-and-forget worker (ScreenOcr pattern). The run loop
        // polls DownloadStatus; Done carries the final path.
        let url = serve(b"worker model bytes", RangeMode::Honor);
        let dest = temp_dest("worker");
        let _ = std::fs::remove_file(&dest);
        let worker = ModelDownloader::spawn().unwrap();
        let status = std::sync::Arc::new(DownloadStatus::default());
        assert!(worker.request(DownloadRequest {
            url,
            dest: dest.clone(),
            expected_sha256: None,
            status: std::sync::Arc::clone(&status),
        }));
        // Spin-wait with a hard deadline — never hang the suite.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // Scope the guard: a match-scrutinee guard would live through
            // the sleep arm and deadlock the worker (match temporaries drop
            // at the END of the match).
            {
                let state = status.state.lock().unwrap();
                match &*state {
                    DownloadState::Done(path) => {
                        assert_eq!(std::fs::read(path).unwrap(), b"worker model bytes");
                        break;
                    }
                    DownloadState::Failed(err) => panic!("worker failed: {err}"),
                    _ => {}
                }
            }
            assert!(std::time::Instant::now() < deadline, "worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            status.downloaded.load(std::sync::atomic::Ordering::Relaxed),
            b"worker model bytes".len() as u64
        );
        assert_eq!(
            status.total.load(std::sync::atomic::Ordering::Relaxed),
            b"worker model bytes".len() as u64
        );
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn worker_enforces_expected_sha256_and_reports_failed_state() {
        let url = serve(b"corrupt worker bytes", RangeMode::Honor);
        let dest = temp_dest("worker-badsha");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(&part);
        let worker = ModelDownloader::spawn().unwrap();
        let status = std::sync::Arc::new(DownloadStatus::default());
        assert!(worker.request(DownloadRequest {
            url,
            dest: dest.clone(),
            expected_sha256: Some(sha256_hex(b"expected worker bytes")),
            status: std::sync::Arc::clone(&status),
        }));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let state = status.state.lock().unwrap();
                match &*state {
                    DownloadState::Failed(msg) => {
                        assert!(msg.contains("sha256 mismatch"), "got: {msg}");
                        break;
                    }
                    DownloadState::Done(path) => panic!("unexpected success: {path:?}"),
                    _ => {}
                }
            }
            assert!(std::time::Instant::now() < deadline, "worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!dest.exists(), "dest never appears on mismatch");
        assert!(part.exists(), "part kept for inspection");
        let _ = std::fs::remove_file(&part);
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
    fn mid_body_failure_keeps_the_partial_bytes_as_the_resume_base() {
        // The read-loop's Network-error arm must KEEP the part file: the bytes
        // written before the connection dropped are the next resume base (the
        // "Keep the part file: the bytes so far are the next resume base"
        // contract the whole resume design leans on). The stalled-server test
        // covers the 0-byte hang; this covers a drop AFTER real bytes landed.
        // The server promises 200 bytes (Content-Length) but sends only 10 and
        // closes, so ureq's length-framed body reader errors mid-stream once
        // the 10 bytes are consumed. Pin: typed Network error, dest never
        // appears, and the part holds EXACTLY those 10 bytes (not deleted, not
        // truncated) so a later resume continues from offset 10.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 200\r\nConnection: close\r\n\r\n"
                );
                // Deliver 10 of the promised 200 bytes, then drop the stream:
                // the truncated body makes the client's next read error.
                let _ = stream.write_all(b"0123456789");
            }
        });
        let dest = temp_dest("midbody");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(&part);
        let agent = ureq::AgentBuilder::new()
            .timeout_read(std::time::Duration::from_secs(5))
            .build();
        let err = download_with_agent(
            &agent,
            &format!("http://{addr}/model.bin"),
            &dest,
            None,
            |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, FetchError::Network(_)), "got: {err}");
        assert!(!dest.exists(), "dest never appears on a mid-body failure");
        assert_eq!(
            std::fs::read(&part).unwrap(),
            b"0123456789",
            "the part keeps exactly the bytes received before the drop, as the resume base"
        );
        let _ = std::fs::remove_file(&part);
    }

    #[test]
    fn connect_timeout_aborts_a_stalled_connection_instead_of_hanging() {
        // Sibling to the read-timeout test: timeout_read only fires once bytes
        // start flowing — a connection that never COMPLETES the TCP handshake
        // (a black-holed host, the SYN dropped) would hang the download (and
        // the worker thread's shutdown) forever without timeout_connect.
        // 192.0.2.1 is RFC 5737 TEST-NET-1: guaranteed non-routable, so the
        // connect stalls deterministically and a short timeout_connect must
        // surface it as a typed Network error rather than blocking. The
        // production agent in `download_url` carries a 10s timeout_connect; a
        // millisecond one here proves the abort without a long test.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_millis(200))
            .build();
        let dest = temp_dest("connect-stall");
        let start = std::time::Instant::now();
        let err = download_with_agent(
            &agent,
            "http://192.0.2.1:8080/model.bin",
            &dest,
            None,
            |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, FetchError::Network(_)), "got: {err}");
        // The timeout actually aborted the connect rather than the test
        // hanging on a real network round-trip: well under a second.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "connect should abort on the timeout, not hang: {:?}",
            start.elapsed()
        );
        // A failed connect never opens the part file.
        assert!(!dest.with_extension("part").exists());
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
    fn uppercase_expected_sha_still_verifies() {
        // Hashes get pasted from release notes in either case — an UPPERCASE
        // expected digest must still verify (the compare lowercases `expected`
        // before matching against the lowercase `actual`).
        let url = serve(b"good bytes", RangeMode::Honor);
        let dest = temp_dest("uppersha");
        let _ = std::fs::remove_file(&dest);
        let expected = sha256_hex(b"good bytes").to_uppercase();
        let got = download_url(&url, &dest, Some(&expected), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"good bytes");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn wrong_offset_206_is_rejected_and_restarts() {
        // A 4-byte part sends `Range: bytes=4-`. The server lies with
        // `Content-Range: bytes 40-.../...` — a digit-prefix collision the
        // trailing `-` in the offset check defeats (`bytes 4` is NOT a prefix
        // of `bytes 40-`; the char after `bytes 4` is `0`, not `-`). The
        // wrong-offset 206 is rejected, the part dropped, and the unranged
        // retry returns the FULL correct body.
        let body: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDE"; // len 41
        let url = serve(body, RangeMode::LieWrongOffset);
        let dest = temp_dest("wrongoff");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"0123").unwrap();
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), body);
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
    fn resume_then_sha_verify_stitches_and_renames_or_keeps_part_on_mismatch() {
        // The existing resume test (no hash) and the sha test (fresh download)
        // are disjoint. Joining them: a resumed download whose STITCHED whole
        // file must still pass verify-before-rename. A correct prefix on disk
        // → the appended tail completes the file, the hash matches, dest
        // appears. A WRONG prefix (right length, garbage bytes) → the stitched
        // file hashes differently, so dest must NOT appear and the part stays.
        let body: &[u8] = b"0123456789";
        let full_hash = sha256_hex(body);

        // (a) correct 4-byte prefix → resume, stitch, verify, rename.
        let url = serve(body, RangeMode::Honor);
        let dest = temp_dest("resume-sha-ok");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"0123").unwrap();
        let got = download_url(&url, &dest, Some(&full_hash), |_, _| {}).unwrap();
        assert_eq!(got, dest, "verified resume renames the part to dest");
        assert_eq!(
            std::fs::read(&got).unwrap(),
            body,
            "the stitched whole file is the full body"
        );
        assert!(!part.exists(), "a verified resume removes the part");
        let _ = std::fs::remove_file(&dest);

        // (b) WRONG prefix of the right length → the server appends from
        // offset 4, so the stitched file is b"WRON" + b"456789" which hashes
        // differently from the real body → HashMismatch, no dest, part kept.
        let url = serve(body, RangeMode::Honor);
        let dest = temp_dest("resume-sha-bad");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"WRON").unwrap();
        let err = download_url(&url, &dest, Some(&full_hash), |_, _| {}).unwrap_err();
        match err {
            FetchError::HashMismatch { expected, actual } => {
                assert_eq!(expected, full_hash, "expected side is the catalog hash");
                assert_eq!(
                    actual,
                    sha256_hex(b"WRON456789"),
                    "actual side is the stitched corrupt-prefix file's hash"
                );
            }
            other => panic!("expected HashMismatch, got {other}"),
        }
        assert!(
            !dest.exists(),
            "dest never appears when the resume verify fails"
        );
        assert!(part.exists(), "the part is kept for inspection on mismatch");
        let _ = std::fs::remove_file(&part);
    }

    #[test]
    fn download_url_reports_fresh_and_resumed_progress_totals() {
        let fresh_url = serve(b"fresh model bytes", RangeMode::Honor);
        let fresh_dest = temp_dest("progress-fresh");
        let fresh_part = fresh_dest.with_extension("part");
        let _ = std::fs::remove_file(&fresh_dest);
        let _ = std::fs::remove_file(&fresh_part);
        let fresh_events = std::cell::RefCell::new(Vec::new());
        let got = download_url(&fresh_url, &fresh_dest, None, |written, total| {
            fresh_events.borrow_mut().push((written, total));
        })
        .unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"fresh model bytes");
        assert_eq!(
            fresh_events.borrow().last(),
            Some(&(
                b"fresh model bytes".len() as u64,
                Some(b"fresh model bytes".len() as u64)
            ))
        );
        let _ = std::fs::remove_file(&fresh_dest);

        let resumed_url = serve(b"0123456789", RangeMode::Honor);
        let resumed_dest = temp_dest("progress-resume");
        let resumed_part = resumed_dest.with_extension("part");
        let _ = std::fs::remove_file(&resumed_dest);
        std::fs::write(&resumed_part, b"0123").unwrap();
        let resumed_events = std::cell::RefCell::new(Vec::new());
        let got = download_url(&resumed_url, &resumed_dest, None, |written, total| {
            resumed_events.borrow_mut().push((written, total));
        })
        .unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        assert_eq!(resumed_events.borrow().last(), Some(&(10, Some(10))));
        let _ = std::fs::remove_file(&resumed_dest);
    }

    #[test]
    fn no_content_length_reports_unknown_total() {
        // A server that omits Content-Length (terminating the body by closing
        // the connection) gives the download loop no way to know the total.
        // Content-Length parsing yields None, so every progress event must
        // carry None for the total — and the worker stores total=0.
        let url = serve(b"unsized model bytes", RangeMode::NoContentLength);
        let dest = temp_dest("no-content-length");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(&part);
        let events = std::cell::RefCell::new(Vec::new());
        let got = download_url(&url, &dest, None, |written, total| {
            events.borrow_mut().push((written, total));
        })
        .unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"unsized model bytes");
        // The body still transferred fully, but with an unknown total.
        assert_eq!(
            events.borrow().last(),
            Some(&(b"unsized model bytes".len() as u64, None)),
            "the final progress event reports the bytes written and an unknown total"
        );
        // EVERY event carries None — not just the last — since the total is
        // never learned mid-stream.
        assert!(
            events.borrow().iter().all(|&(_, total)| total.is_none()),
            "no progress event should ever report a known total: {:?}",
            events.borrow()
        );
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn resumed_download_first_progress_event_starts_at_the_resume_offset() {
        // The existing progress test only checks `.last()`. The FIRST event in
        // a resumed download must already include the bytes that were on disk
        // (written starts at `existing`, not 0) — otherwise a resumed download
        // would briefly report a backwards/zeroed progress bar to the UI. With
        // a 4-byte part and a 10-byte body, the server appends 6 bytes one
        // chunk at a time, so the first reported `written` is existing(4)+1.
        let url = serve(b"0123456789", RangeMode::Honor);
        let dest = temp_dest("progress-resume-first");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"0123").unwrap();
        let events = std::cell::RefCell::new(Vec::new());
        let got = download_url(&url, &dest, None, |written, total| {
            events.borrow_mut().push((written, total));
        })
        .unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        let first = *events
            .borrow()
            .first()
            .expect("a resumed download still reports at least one chunk");
        assert!(
            first.0 >= 4,
            "first progress event must start at the resume offset (>= existing 4), got {}",
            first.0
        );
        // Stronger: it must NOT report from zero — the resume offset is baked
        // into the running counter before the first chunk lands.
        assert_ne!(first.0, 0, "a resumed download must never report written=0");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn unvalidated_206_with_partial_body_restarts_instead_of_corrupting() {
        // A 206 with NO Content-Range cannot be validated as a resume — and
        // its body may genuinely be the requested TAIL slice. Consuming that
        // suspect body as the full file truncates the head silently (dest =
        // b"456789"). The safe move mirrors the 416 arm: drop the part and
        // re-request unranged.
        let url = serve(b"0123456789", RangeMode::HonorNoHeader);
        let dest = temp_dest("nohdr206");
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
        // the file silently. The safe move is a from-zero restart. (Scope:
        // this server lies with the FULL body; the partial-body variant of
        // an unvalidatable 206 is pinned by
        // `unvalidated_206_with_partial_body_restarts_instead_of_corrupting`.)
        let url = serve(b"0123456789", RangeMode::Lie);
        let dest = temp_dest("lie206");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"0123").unwrap();
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"0123456789");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn http_416_restarts_from_zero_instead_of_looping_forever() {
        // review-c119 MEDIUM: an oversized part (server file shrank) makes
        // every resume reply 416; keeping the part would 416 forever. The
        // fix deletes the part and restarts unranged exactly once.
        let url = serve(b"shrunk", RangeMode::Unsatisfiable);
        let dest = temp_dest("u416");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"way longer than the new body").unwrap();
        let got = download_url(&url, &dest, None, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"shrunk");
        assert!(!part.exists());
        let _ = std::fs::remove_file(&dest);
    }

    /// 416 to EVERY request, ranged or not (a permanently broken server).
    fn serve_always_416() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let _ = write!(
                    stream,
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        format!("http://{addr}/model.bin")
    }

    #[test]
    fn http_416_on_an_unranged_request_surfaces_after_exactly_one_retry() {
        // The 416 arm's recursion bound (`existing > 0`): the retry is
        // unranged, so a server that 416s EVERYTHING terminates with a typed
        // Http(416) instead of looping — the comment's claim, pinned.
        let url = serve_always_416();
        let dest = temp_dest("always416");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"stale").unwrap();
        let err = download_url(&url, &dest, None, |_, _| {}).unwrap_err();
        assert!(matches!(err, FetchError::Http(416)), "got: {err}");
        assert!(!part.exists(), "the stale part was dropped by the retry");
    }

    #[cfg(unix)]
    #[test]
    fn http_416_with_unremovable_part_surfaces_io_instead_of_recursing() {
        // The 416 restart's recursion bound holds ONLY if remove_file succeeds
        // (the retry then recomputes existing==0 and drops the Range). If the
        // part survives removal it re-sends a Range and 416s back into the same
        // arm — unbounded recursion → stack overflow. Here a read-only parent
        // dir blocks unlink(part); pin that this surfaces a typed Io error
        // instead of looping. ponytail: skipped when run as root (root ignores
        // dir perms so unlink succeeds) — the assertion still holds either way
        // since success just yields the normal Http(416) terminal error.
        use std::os::unix::fs::PermissionsExt;
        let url = serve_always_416();
        let dir = std::env::temp_dir().join(format!("compme_ro_part_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("model.bin");
        let part = dest.with_extension("part");
        std::fs::write(&part, b"stale but unremovable").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = download_url(&url, &dest, None, |_, _| {}).unwrap_err();
        // Restore write so the temp dir and its part can be cleaned up.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(err, FetchError::Io(_) | FetchError::Http(416)),
            "got: {err}"
        );
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn oversized_part_against_honoring_server_restarts_and_verifies() {
        // A range-HONORING server (not the 416 path) still replies 200 with the
        // FULL body when the requested offset is past EOF: the part is longer
        // than the server's file (the file shrank). `Range: bytes=20-` against a
        // 10-byte body is unsatisfiable, so the Honor server falls through to a
        // plain 200 + full body. The download must TRUNCATE-restart (not append
        // the new body onto the stale 20 bytes) so the final file is exactly the
        // 10-byte body and the overlong part is gone — the append-vs-truncate +
        // saturating_add(total) math on the honoring-200 branch, pinned.
        let body = b"shrunk2bk-"; // exactly 10 bytes
        assert_eq!(body.len(), 10);
        let url = serve(body, RangeMode::Honor);
        let dest = temp_dest("oversize200");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"01234567890123456789").unwrap(); // 20 bytes > body
        let expected = sha256_hex(body);
        let got = download_url(&url, &dest, Some(&expected), |_, _| {}).unwrap();
        assert_eq!(
            std::fs::read(&got).unwrap(),
            body,
            "overlong part must be discarded via truncate-restart, not appended"
        );
        assert!(
            !part.exists(),
            "the part is renamed into dest, not left behind"
        );
        let _ = std::fs::remove_file(&dest);
    }

    #[cfg(unix)]
    #[test]
    fn rename_failure_surfaces_io_and_keeps_part() {
        // The verify-then-rename tail: write_all + sync_all succeed, the sha
        // matches, but `rename(part, dest)` fails because the dest's PARENT dir
        // is read-only (creating the new `dest` entry needs write on the dir).
        // The part is pre-created so the truncate-write reopens an EXISTING file
        // (allowed under a 0o555 dir) — the failure lands on the rename arm, not
        // an earlier create. Pin: the error is the crate's Io variant AND the
        // part survives as the resume base. Skipped under root (root ignores dir
        // perms, so rename succeeds and the download just completes normally).
        use std::os::unix::fs::PermissionsExt;
        let body = b"rename-arm-body";
        let url = serve(body, RangeMode::Ignore);
        let dir = std::env::temp_dir().join(format!("compme_ro_rename_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("model.bin");
        let part = dest.with_extension("part");
        // Pre-create the part so opening it for write+truncate works under the
        // read-only dir (creating a NEW file there would fail at the create arm).
        std::fs::write(&part, b"stale").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let expected = sha256_hex(body);
        let res = download_url(&url, &dest, Some(&expected), |_, _| {});
        // Restore write so the temp dir can be cleaned up.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        match res {
            Err(FetchError::Io(_)) => {
                assert!(
                    part.exists(),
                    "part kept as the resume base when rename fails"
                );
                assert!(!dest.exists(), "dest never appears when the rename fails");
            }
            // Root ignores dir perms: rename succeeds, the download completes.
            Ok(_) => assert!(dest.exists()),
            other => panic!("expected Io or success, got: {other:?}"),
        }
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn worker_queue_overflow_drops_the_request_and_leaves_it_idle() {
        // The depth-1 sync_channel contract: with one download in flight and
        // one queued, a third request is DROPPED (try_send), its status
        // stays Idle, and nothing blocks. A regression to an unbounded
        // channel would silently queue stale downloads.
        let stall_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stall_addr = stall_listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in stall_listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n"
                );
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
        let worker = ModelDownloader::spawn().unwrap();
        let statuses: Vec<_> = (0..3)
            .map(|_| std::sync::Arc::new(DownloadStatus::default()))
            .collect();
        let queued: Vec<bool> = statuses
            .iter()
            .map(|status| {
                worker.request(DownloadRequest {
                    url: format!("http://{stall_addr}/model.bin"),
                    dest: temp_dest("overflow"),
                    expected_sha256: None,
                    status: std::sync::Arc::clone(status),
                })
            })
            .collect();
        // The first request always queues; the return value tells callers
        // whether to track the status (a dropped request stays Idle forever).
        assert!(queued[0], "first request queues");
        // Exact overflow capacity: with `sync_channel(1)` (depth-1 buffer) plus
        // at most one in-flight slot once the worker has consumed, the most this
        // burst can accept is TWO — never all three. Whether the worker pulled
        // request 0 into flight before the burst finished is the only race, so
        // the accepted count is exactly 1 or 2, and the dropped count is exactly
        // `3 - accepted` (>=1, the overflow guarantee).
        let accepted = queued.iter().filter(|&&q| q).count();
        assert!(
            (1..=2).contains(&accepted),
            "overflow must accept exactly 1 or 2 of three burst requests \
             (never all three); got {accepted} accepted: {queued:?}"
        );
        // Every request that reported `false` was dropped, so per the
        // must_use contract its status must stay Idle (untracked) — the
        // boolean and the observable state must agree exactly.
        for (i, &q) in queued.iter().enumerate() {
            if !q {
                let state = statuses[i].state.lock().unwrap();
                assert!(
                    matches!(&*state, DownloadState::Idle),
                    "dropped request {i} (returned false) must stay Idle, got {:?}",
                    &*state
                );
            }
        }
        // Request 0 reaches Running (the stalled body holds it there);
        // request 2 must have been dropped: still Idle after the worker has
        // demonstrably started consuming.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let state = statuses[0].state.lock().unwrap();
                if matches!(&*state, DownloadState::Running) {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "worker never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let third = statuses[2].state.lock().unwrap();
        assert!(
            matches!(&*third, DownloadState::Idle),
            "overflow request must be dropped, not queued"
        );
    }

    #[test]
    fn restart_from_zero_still_verifies_the_sha_before_renaming() {
        // The existing restart tests (server-ignores-Range, 416, lying/unvalidated
        // 206) all pass `None` for the hash, so none prove that a RESTART path
        // still runs verify-before-rename. Here a stale part forces a restart
        // (the server ignores Range and replies 200 full body), and we pass
        // Some(hash): the from-zero body must be verified before it can become
        // dest. Correct hash → dest appears; wrong hash → mismatch, no dest,
        // part kept.
        let body: &[u8] = b"0123456789";

        // (a) restart + correct hash → verify passes → rename.
        let url = serve(body, RangeMode::Ignore); // always 200, full body
        let dest = temp_dest("restart-sha-ok");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"STALE GARBAGE").unwrap();
        let got = download_url(&url, &dest, Some(&sha256_hex(body)), |_, _| {}).unwrap();
        assert_eq!(
            std::fs::read(&got).unwrap(),
            body,
            "the restart discarded the stale part and wrote the full fresh body"
        );
        assert!(!part.exists(), "a verified restart removes the part");
        let _ = std::fs::remove_file(&dest);

        // (b) restart + WRONG expected hash → verify fails AFTER the restart →
        // dest never appears, part kept. Proves the verify gate is on the
        // restart path, not just the resume/fresh paths.
        let url = serve(body, RangeMode::Ignore);
        let dest = temp_dest("restart-sha-bad");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&part, b"STALE GARBAGE").unwrap();
        let err = download_url(
            &url,
            &dest,
            Some(&sha256_hex(b"a different body")),
            |_, _| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, FetchError::HashMismatch { .. }),
            "a restart must still verify before rename; got {err}"
        );
        assert!(
            !dest.exists(),
            "dest never appears when the restart verify fails"
        );
        assert!(
            part.exists(),
            "part kept for inspection on a failed restart verify"
        );
        let _ = std::fs::remove_file(&part);
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
    fn resume_range_header_starts_fresh_at_zero_and_resumes_beyond() {
        assert_eq!(resume_range_header(0), None);
        assert_eq!(resume_range_header(1), Some("bytes=1-".to_string()));
        assert_eq!(
            resume_range_header(398_000_000),
            Some("bytes=398000000-".to_string())
        );
    }

    #[test]
    fn worker_stores_zero_total_when_content_length_unknown() {
        // The download_url callback reports total=None when the server omits
        // Content-Length (covered by no_content_length_reports_unknown_total).
        // This pins the WORKER's translation of that None into the shared
        // DownloadStatus: total.unwrap_or(0), so a polling consumer reads 0 as
        // the "unknown total" sentinel. Drive the real worker against a
        // no-Content-Length server and assert status.total == 0 after success.
        let url = serve(b"unsized worker bytes", RangeMode::NoContentLength);
        let dest = temp_dest("worker-no-content-length");
        let part = dest.with_extension("part");
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(&part);
        let worker = ModelDownloader::spawn().unwrap();
        let status = std::sync::Arc::new(DownloadStatus::default());
        assert!(worker.request(DownloadRequest {
            url,
            dest: dest.clone(),
            expected_sha256: None,
            status: std::sync::Arc::clone(&status),
        }));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let state = status.state.lock().unwrap();
                match &*state {
                    DownloadState::Done(path) => {
                        assert_eq!(std::fs::read(path).unwrap(), b"unsized worker bytes");
                        break;
                    }
                    DownloadState::Failed(err) => panic!("worker failed: {err}"),
                    _ => {}
                }
            }
            assert!(std::time::Instant::now() < deadline, "worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // The bytes all transferred, but the total stays the unknown-total
        // sentinel (0) — the worker never learned a Content-Length.
        assert_eq!(
            status.downloaded.load(std::sync::atomic::Ordering::Relaxed),
            b"unsized worker bytes".len() as u64
        );
        assert_eq!(
            status.total.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no Content-Length must leave status.total at the 0 sentinel"
        );
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn fetch_error_display_is_stable_per_variant() {
        // These strings land in telemetry and the picker UI's Failed(msg)
        // text, so they're matched by users/tooling — pin the exact stable
        // form of every variant against accidental rewording.
        assert_eq!(
            FetchError::Network("connection reset".into()).to_string(),
            "network error: connection reset"
        );
        assert_eq!(FetchError::Http(404).to_string(), "http error: status 404");
        assert_eq!(
            FetchError::Io("disk full".into()).to_string(),
            "io error: disk full"
        );
        assert_eq!(
            FetchError::HashMismatch {
                expected: "abc".into(),
                actual: "def".into(),
            }
            .to_string(),
            "sha256 mismatch: expected abc, got def"
        );
    }
}
