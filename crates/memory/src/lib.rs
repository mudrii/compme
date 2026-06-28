//! Encrypted local memory for accepted completions or monitored typing (design spec §6 / §16).
//!
//! Privacy model: text is **redacted** (`redaction`) before it is **encrypted**
//! (AES-256-GCM, random nonce per record) and only text ciphertext is written to
//! the SQLite database — text plaintext never touches disk. The app identifier is
//! kept as plaintext metadata for per-app counts and deletion, and is also bound
//! into the AEAD as AAD so rows cannot be relabeled and decrypted under another
//! app. The 32-byte key is a [`StaticKey`]; production fills it from the OS
//! keystore (Keychain on macOS — A3 live integration), tests supply a fixed key.
//!
//! Storage is **opt-in**: with [`StorageMode::Off`] (the default) nothing is
//! recorded. `AcceptedOnly` stores accepted completions; `AllMonitored` is the
//! broader opt-in. Records are inspectable (`count`/`recent`) and deletable
//! (`delete_all`, `delete_app`).

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rusqlite::{params, Connection};
use zeroize::Zeroize;

const NONCE_LEN: usize = 12;

/// Upper bound on stored records. After each insert the store trims oldest-first
/// (lowest id) back down to this cap, so an `AllMonitored` session cannot grow
/// the database without bound (disk-exhaustion / unbounded retention).
// ponytail: a single global `MAX_RECORDS` cap. The roadmap specifies no
// retention policy, so this is a generous fixed bound; per-app and time-based
// (age) retention are the upgrade path if a roadmap item demands finer control.
const MAX_RECORDS: i64 = 50_000;

/// The 32-byte AES-256 key. Production fills it from the OS keystore (Keychain);
/// tests/headless use a fixed key. (A `KeyProvider` trait was inlined here once it
/// had a single implementation — reintroduce a trait if a second key source lands.)
pub struct StaticKey(pub [u8; 32]);

impl Drop for StaticKey {
    // Scrub the long-lived key copy on drop so the raw AES-256 key does not
    // linger in process memory for a core dump / swap / live inspection to
    // recover. The cipher's internal key schedule is separately zeroized via
    // aes-gcm's `zeroize` feature; the construction temporary is scrubbed in
    // `from_connection`.
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// What gets recorded. `Off` (default) records nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StorageMode {
    #[default]
    Off,
    AcceptedOnly,
    AllMonitored,
}

#[derive(Debug)]
pub enum MemoryError {
    Db(String),
    Io(String),
    Crypto,
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryError::Db(msg) => write!(f, "memory db error: {msg}"),
            MemoryError::Io(msg) => write!(f, "memory io error: {msg}"),
            MemoryError::Crypto => write!(f, "memory crypto error"),
        }
    }
}

impl From<std::io::Error> for MemoryError {
    fn from(err: std::io::Error) -> Self {
        MemoryError::Io(err.to_string())
    }
}

impl std::error::Error for MemoryError {}

impl From<rusqlite::Error> for MemoryError {
    fn from(err: rusqlite::Error) -> Self {
        MemoryError::Db(err.to_string())
    }
}

type Result<T> = std::result::Result<T, MemoryError>;

/// Encrypted store for accepted completions and, in `AllMonitored`, monitored
/// typing.
pub struct MemoryStore {
    conn: Connection,
    cipher: Aes256Gcm,
    mode: StorageMode,
}

impl MemoryStore {
    /// Open (creating if needed) a file-backed store.
    ///
    /// The parent directory is created (0700 on unix) if missing, and the
    /// database file is created/restricted to owner-only (0600 on unix) so its
    /// plaintext `app` metadata column is not world/group-readable.
    pub fn open(path: &Path, key: &StaticKey, mode: StorageMode) -> Result<Self> {
        // Ensure the parent directory exists, or SQLite fails to create the file
        // and the store silently never initializes.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
                }
            }
        }

        // Pre-create the db file at 0600 before SQLite opens it, to shrink the
        // window in which it could exist at the default world-readable mode.
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::fs::OpenOptionsExt;
            if !path.exists() {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(path)?;
            }
        }

        let store = Self::from_connection(Connection::open(path)?, key, mode)?;

        // Belt-and-suspenders: enforce 0600 on the main db and any journal/wal/shm
        // sidecar that exists alongside it (sidecars are NOT covered by
        // secure_delete and would otherwise inherit default perms).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let restrict = |p: &Path| {
                if p.exists() {
                    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
                }
            };
            restrict(path);
            for suffix in ["-journal", "-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_owned();
                sidecar.push(suffix);
                restrict(Path::new(&sidecar));
            }
        }

        Ok(store)
    }

    /// Open an in-memory store (tests / ephemeral use).
    pub fn open_in_memory(key: &StaticKey, mode: StorageMode) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, key, mode)
    }

    fn from_connection(conn: Connection, key: &StaticKey, mode: StorageMode) -> Result<Self> {
        // secure_delete zeroes freed content so delete_all/delete_app actually
        // erase ciphertext from disk (freelist pages), not just unlink rows.
        conn.pragma_update(None, "secure_delete", true)?;
        // Pin journal_mode = DELETE so there is no persistent WAL/-shm sidecar
        // leaking ciphertext/metadata (sidecars are not covered by secure_delete);
        // the rollback journal is deleted on commit. query_value, not _update:
        // journal_mode reports the resulting mode back.
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS memories (
                 id   INTEGER PRIMARY KEY AUTOINCREMENT,
                 app  TEXT NOT NULL,
                 blob BLOB NOT NULL
             )",
            [],
        )?;
        // Scrub the key copy returned by the provider once the cipher has
        // absorbed it; the cipher itself zeroizes its key schedule on drop
        // (aes-gcm `zeroize` feature).
        let mut key_bytes = key.0;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
        key_bytes.zeroize();
        Ok(Self { conn, cipher, mode })
    }

    /// Record an **accepted** completion for `app`. No-op when storage is `Off`.
    /// The text is redacted, then encrypted (with `app` bound as AEAD AAD); only
    /// ciphertext is persisted.
    pub fn remember(&self, app: &str, text: &str) -> Result<()> {
        if self.mode == StorageMode::Off {
            return Ok(());
        }
        self.store(app, text)
    }

    /// Record **monitored-but-not-accepted** text for `app`. Only stored in
    /// `AllMonitored` mode; a no-op in `AcceptedOnly`/`Off` (§16: accepted-only
    /// mode must not persist non-accepted text).
    pub fn monitor(&self, app: &str, text: &str) -> Result<()> {
        if self.mode != StorageMode::AllMonitored {
            return Ok(());
        }
        self.store(app, text)
    }

    fn store(&self, app: &str, text: &str) -> Result<()> {
        // Hold the redacted plaintext in a zeroizing buffer so it is scrubbed from
        // the heap after encryption — matching the key-zeroization rigor under the
        // same core-dump/swap/live-inspection threat model the module guards. The
        // redacted text can still carry private (non-PII) prose. Best-effort: the
        // caller's `text` is its own and is not scrubbed here.
        let redacted = zeroize::Zeroizing::new(redaction::redact(text));
        let blob = self.encrypt(redacted.as_str(), app.as_bytes())?;
        self.conn.execute(
            "INSERT INTO memories (app, blob) VALUES (?1, ?2)",
            params![app, blob],
        )?;
        Self::trim_to_cap(&self.conn, MAX_RECORDS)?;
        Ok(())
    }

    /// Trim the table oldest-first (lowest id) down to at most `cap` rows. A
    /// no-op once the row count is at or below `cap`. Extracted from `store` so
    /// the eviction bound can be unit-tested with a small `cap`. secure_delete
    /// (set in `from_connection`) means the evicted ciphertext is zeroed too.
    fn trim_to_cap(conn: &Connection, cap: i64) -> Result<()> {
        conn.execute(
            "DELETE FROM memories WHERE id NOT IN \
             (SELECT id FROM memories ORDER BY id DESC LIMIT ?1)",
            params![cap],
        )?;
        Ok(())
    }

    /// Total number of stored records.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        // Explicit clamp instead of `as usize`: SQLite COUNT is i64; on a 32-bit
        // target a raw cast would silently truncate. try_from makes the (only on
        // an implausibly huge table) saturation explicit.
        Ok(usize::try_from(n.max(0)).unwrap_or(usize::MAX))
    }

    /// The most recent `limit` decryptable records for `app`, newest first.
    /// Records that fail to decrypt (e.g. a different key) are skipped.
    pub fn recent(&self, app: &str, limit: usize) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT blob FROM memories WHERE app = ?1 ORDER BY id DESC LIMIT ?2 OFFSET ?3",
        )?;
        // Fetch (and decrypt) a page at a time instead of every row up front, so
        // the common all-decryptable case touches only `limit` ciphertexts. We
        // cannot use a single `LIMIT limit`: skipped rows (wrong key after a key
        // change, or a tampered blob) would make us return fewer than `limit`
        // valid records even when more decryptable ones exist further back — a
        // contract this function and its tests rely on. So we page on until the
        // page itself comes up short, meaning the app's rows are exhausted.
        // Match count()/count_by_app(): saturate rather than wrap. A `limit`
        // above i64::MAX would wrap negative, and SQLite reads a negative LIMIT
        // as "no limit".
        let page = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut out = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let blobs =
                stmt.query_map(params![app, page, offset], |row| row.get::<_, Vec<u8>>(0))?;
            let mut fetched = 0usize;
            for blob in blobs {
                fetched += 1;
                // Best-effort read: a row that fails to decrypt (wrong key, or
                // app column tampered so the AAD no longer matches) is absent.
                if let Some(text) = self.decrypt(&blob?, app.as_bytes()) {
                    out.push(text);
                    if out.len() == limit {
                        return Ok(out);
                    }
                }
            }
            // A short page means no older rows remain for this app.
            if fetched < limit {
                return Ok(out);
            }
            offset += page;
        }
    }

    /// Per-app record counts, most-used first (the App-Settings pane list).
    /// Counts come straight from the plaintext `app` column — no decryption;
    /// ties break alphabetically so the order is deterministic. Like
    /// [`Self::count`], this counts STORED rows: after a key change it can
    /// exceed what [`Self::recent`] (which skips undecryptable rows) returns.
    pub fn count_by_app(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT app, COUNT(*) AS cnt FROM memories GROUP BY app ORDER BY cnt DESC, app ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (app, n) = row?;
            // Explicit clamp over `as u64` — COUNT is i64; try_from keeps the
            // conversion lossless and saturates only on an implausibly huge count.
            out.push((app, u64::try_from(n.max(0)).unwrap_or(u64::MAX)));
        }
        Ok(out)
    }

    /// Delete every record (the "disable and erase" control).
    pub fn delete_all(&self) -> Result<()> {
        self.conn.execute("DELETE FROM memories", [])?;
        Ok(())
    }

    /// Delete records for one app; returns how many were removed.
    pub fn delete_app(&self, app: &str) -> Result<usize> {
        let removed = self
            .conn
            .execute("DELETE FROM memories WHERE app = ?1", params![app])?;
        Ok(removed)
    }

    /// Encrypt `plaintext`, binding `aad` (the app id) so a record cannot be
    /// authenticated under a different app. Random 12-byte nonce per record
    /// (the store's record count is far below the GCM birthday bound); a
    /// misuse-resistant nonce (XChaCha20) is a future hardening option.
    fn encrypt(&self, plaintext: &str, aad: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        // Fail closed: if the RNG is unavailable, error rather than store a record
        // with a weak/missing nonce.
        getrandom::getrandom(&mut nonce_bytes).map_err(|_| MemoryError::Crypto)?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad,
                },
            )
            .map_err(|_| MemoryError::Crypto)?;
        // Prepend the nonce so decryption can recover it.
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(blob)
    }

    fn decrypt(&self, blob: &[u8], aad: &[u8]) -> Option<String> {
        if blob.len() < NONCE_LEN {
            return None;
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = self
            .cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .ok()?;
        String::from_utf8(plaintext).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn key(byte: u8) -> StaticKey {
        StaticKey([byte; 32])
    }

    fn temp_db_path() -> PathBuf {
        let mut suffix = [0u8; 8];
        getrandom::getrandom(&mut suffix).unwrap();
        let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        std::env::temp_dir().join(format!("cm-memory-test-{hex}.db"))
    }

    #[test]
    fn off_mode_records_nothing() {
        let store = MemoryStore::open_in_memory(&key(1), StorageMode::Off).unwrap();
        store.remember("com.apple.TextEdit", "hello world").unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn count_by_app_aggregates_sorted_by_count_descending() {
        // App-Settings pane backing: per-app usage counts straight from the
        // plaintext app column (no decryption needed for counting).
        let store = MemoryStore::open_in_memory(&key(9), StorageMode::AcceptedOnly).unwrap();
        store.remember("com.apple.TextEdit", "a").unwrap();
        store.remember("com.apple.TextEdit", "b").unwrap();
        store.remember("com.google.Chrome", "c").unwrap();
        assert_eq!(
            store.count_by_app().unwrap(),
            vec![
                ("com.apple.TextEdit".to_string(), 2),
                ("com.google.Chrome".to_string(), 1),
            ]
        );
        assert!(
            MemoryStore::open_in_memory(&key(9), StorageMode::AcceptedOnly)
                .unwrap()
                .count_by_app()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn count_by_app_breaks_count_ties_alphabetically() {
        // Equal counts must render in a stable order in the pane.
        let store = MemoryStore::open_in_memory(&key(10), StorageMode::AcceptedOnly).unwrap();
        store.remember("org.zed.Zed", "a").unwrap();
        store.remember("com.apple.Notes", "b").unwrap();
        assert_eq!(
            store.count_by_app().unwrap(),
            vec![
                ("com.apple.Notes".to_string(), 1),
                ("org.zed.Zed".to_string(), 1),
            ]
        );
    }

    #[test]
    fn decrypt_rejects_a_blob_that_is_exactly_nonce_len_with_no_tag() {
        // A blob of exactly NONCE_LEN (12) bytes passes the `< NONCE_LEN` length
        // guard, so split_at yields a 12-byte nonce and an EMPTY ciphertext. AES-
        // GCM still has no authentication tag to verify, so decrypt() returns None
        // and recent() treats the row as absent — count() still sees the raw row.
        // Pins the best-effort read path: a malformed/tag-less row must be skipped,
        // never panic.
        assert_eq!(NONCE_LEN, 12, "test assumes a 12-byte GCM nonce");
        let store = MemoryStore::open_in_memory(&key(33), StorageMode::AcceptedOnly).unwrap();
        let bogus = vec![0u8; NONCE_LEN]; // nonce-length, zero ciphertext + no tag
        store
            .conn
            .execute(
                "INSERT INTO memories (app, blob) VALUES (?1, ?2)",
                params!["app", bogus],
            )
            .unwrap();
        assert_eq!(store.count().unwrap(), 1, "the raw row is present");
        assert!(
            store.recent("app", 10).unwrap().is_empty(),
            "GCM rejects the tag-less ciphertext, so recent() yields nothing"
        );
    }

    #[test]
    fn remembers_and_retrieves_recent_newest_first() {
        let store = MemoryStore::open_in_memory(&key(2), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "first").unwrap();
        store.remember("app", "second").unwrap();
        assert_eq!(store.recent("app", 10).unwrap(), vec!["second", "first"]);
    }

    #[test]
    fn recent_isolates_records_across_apps() {
        // The `WHERE app = ?1` filter in recent() (paired with the per-app AAD
        // binding) is the cross-app privacy boundary: one app's stored memory
        // must never surface in another app's history. Both apps hold a live,
        // decryptable row here so the filter is load-bearing — dropping the
        // WHERE clause would leak across apps yet pass the single-app recent()
        // tests, which never hold two decryptable apps at once.
        let store = MemoryStore::open_in_memory(&key(40), StorageMode::AcceptedOnly).unwrap();
        store.remember("app.a", "alpha secret").unwrap();
        store.remember("app.b", "beta secret").unwrap();
        store.remember("app.a", "alpha two").unwrap();
        assert_eq!(
            store.recent("app.a", 10).unwrap(),
            vec!["alpha two", "alpha secret"]
        );
        assert_eq!(store.recent("app.b", 10).unwrap(), vec!["beta secret"]);
        let a = store.recent("app.a", 10).unwrap();
        assert!(
            !a.iter().any(|t| t.contains("beta")),
            "another app's record must not leak into recent(): {a:?}"
        );
    }

    #[test]
    fn file_backed_store_reopens_with_recent_records_newest_first() {
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(22), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "first").unwrap();
            store.remember("app", "second ada@example.com").unwrap();
        }

        let reopened = MemoryStore::open(&path, &key(22), StorageMode::AcceptedOnly).unwrap();
        assert_eq!(reopened.count().unwrap(), 2);
        assert_eq!(
            reopened.recent("app", 10).unwrap(),
            vec!["second [redacted-email]", "first"]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn redacts_before_storing() {
        let store = MemoryStore::open_in_memory(&key(3), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "mail ada@example.com now").unwrap();
        let recent = store.recent("app", 1).unwrap();
        assert_eq!(recent, vec!["mail [redacted-email] now"]);
    }

    #[test]
    fn plaintext_never_written_to_disk() {
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(4), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "supersecretphrase").unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        let needle = b"supersecretphrase";
        let found = raw.windows(needle.len()).any(|w| w == needle);
        let _ = std::fs::remove_file(&path);
        assert!(!found, "plaintext must not appear in the on-disk database");
    }

    #[test]
    fn delete_all_erases_ciphertext_from_disk_not_just_rows() {
        // The secure_delete pragma is the load-bearing half of "disable and
        // erase": without it, DELETE only unlinks rows and the ciphertext
        // lives on in freelist pages. Scan the raw file for the blob bytes
        // after delete_all to pin that the pragma stays effective.
        let path = temp_db_path();
        let blob: Vec<u8> = {
            let store = MemoryStore::open(&path, &key(21), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "erase me entirely").unwrap();
            let conn = Connection::open(&path).unwrap();
            conn.query_row("SELECT blob FROM memories LIMIT 1", [], |row| row.get(0))
                .unwrap()
        };
        assert!(blob.len() >= 16, "sanity: a real ciphertext blob");
        {
            let store = MemoryStore::open(&path, &key(21), StorageMode::AcceptedOnly).unwrap();
            store.delete_all().unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        // Scan for the ciphertext body PAST the nonce (the secret-bearing bytes),
        // not just a 16-byte prefix, so this pins that the actual ciphertext is
        // gone — a zeroed prefix alone would not prove the secret was erased.
        let body = &blob[NONCE_LEN..];
        let found = raw.windows(body.len()).any(|w| w == body);
        let _ = std::fs::remove_file(&path);
        assert!(
            !found,
            "deleted ciphertext must be zeroed on disk (secure_delete)"
        );
    }

    #[test]
    fn delete_all_clears_every_record() {
        let store = MemoryStore::open_in_memory(&key(5), StorageMode::AllMonitored).unwrap();
        store.remember("a", "one").unwrap();
        store.remember("b", "two").unwrap();
        store.delete_all().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn delete_app_removes_only_that_app() {
        let store = MemoryStore::open_in_memory(&key(6), StorageMode::AcceptedOnly).unwrap();
        store.remember("keep", "stay").unwrap();
        store.remember("drop", "gone").unwrap();
        assert_eq!(store.delete_app("drop").unwrap(), 1);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.recent("keep", 10).unwrap(), vec!["stay"]);
        assert!(store.recent("drop", 10).unwrap().is_empty());
    }

    #[test]
    fn delete_app_erases_that_apps_ciphertext_from_disk() {
        let path = temp_db_path();
        let drop_blob: Vec<u8> = {
            let store = MemoryStore::open(&path, &key(23), StorageMode::AcceptedOnly).unwrap();
            store.remember("keep", "stay encrypted").unwrap();
            store.remember("drop", "erase this app").unwrap();
            let conn = Connection::open(&path).unwrap();
            conn.query_row(
                "SELECT blob FROM memories WHERE app = 'drop' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(drop_blob.len() >= 16, "sanity: a real ciphertext blob");

        {
            let store = MemoryStore::open(&path, &key(23), StorageMode::AcceptedOnly).unwrap();
            assert_eq!(store.delete_app("drop").unwrap(), 1);
            assert_eq!(store.count().unwrap(), 1);
            assert_eq!(store.recent("keep", 10).unwrap(), vec!["stay encrypted"]);
        }

        let raw = std::fs::read(&path).unwrap();
        // Scan for the ciphertext body PAST the nonce (the secret-bearing bytes),
        // not just a 16-byte prefix, so this pins that the actual ciphertext is
        // gone — a zeroed prefix alone would not prove the secret was erased.
        let body = &drop_blob[NONCE_LEN..];
        let found = raw.windows(body.len()).any(|w| w == body);
        let _ = std::fs::remove_file(&path);
        assert!(
            !found,
            "per-app deleted ciphertext must be zeroed on disk (secure_delete)"
        );
    }

    #[test]
    fn accepted_only_mode_stores_accepted_but_not_monitored() {
        let store = MemoryStore::open_in_memory(&key(9), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "accepted").unwrap();
        store.monitor("app", "just monitored").unwrap();
        // Only the accepted completion is stored in AcceptedOnly mode (§16 gate).
        assert_eq!(store.recent("app", 10).unwrap(), vec!["accepted"]);
    }

    #[test]
    fn all_monitored_mode_stores_both() {
        let store = MemoryStore::open_in_memory(&key(10), StorageMode::AllMonitored).unwrap();
        store.remember("app", "accepted").unwrap();
        store.monitor("app", "monitored").unwrap();
        assert_eq!(store.count().unwrap(), 2);
        // Both paths persist retrievable plaintext, not just bump the row count.
        // Neither string carries a secret, so redaction leaves them verbatim;
        // `recent` is newest-first (ORDER BY id DESC), so the later `monitor`
        // insert comes before the earlier `remember`.
        assert_eq!(
            store.recent("app", 10).unwrap(),
            vec!["monitored".to_string(), "accepted".to_string()],
        );
    }

    #[test]
    fn monitor_redacts_before_storing() {
        // The monitored-typing path (AllMonitored mode) must scrub secrets before
        // encrypting, exactly like the accepted path (`redacts_before_storing`):
        // broad background capture must never persist a raw token.
        let store = MemoryStore::open_in_memory(&key(11), StorageMode::AllMonitored).unwrap();
        store
            .monitor("app", "type sk-abcdEFGH0123456789abcdEFGH0123")
            .unwrap();
        let recent = store.recent("app", 1).unwrap();
        assert_eq!(recent.len(), 1, "the monitored row is stored");
        assert!(
            recent[0].contains("[redacted-secret]"),
            "monitored secret must be redacted: {:?}",
            recent[0]
        );
        assert!(
            !recent[0].contains("sk-abcd"),
            "raw token must not be stored: {:?}",
            recent[0]
        );
    }

    #[test]
    fn all_monitored_rows_count_and_delete_by_app() {
        let store = MemoryStore::open_in_memory(&key(13), StorageMode::AllMonitored).unwrap();
        store.remember("com.apple.TextEdit", "accepted").unwrap();
        store.monitor("com.apple.TextEdit", "typed").unwrap();
        store.monitor("com.apple.Notes", "note").unwrap();

        assert_eq!(
            store.count_by_app().unwrap(),
            vec![
                ("com.apple.TextEdit".into(), 2),
                ("com.apple.Notes".into(), 1),
            ]
        );

        assert_eq!(store.delete_app("com.apple.TextEdit").unwrap(), 2);
        assert_eq!(
            store.count_by_app().unwrap(),
            vec![("com.apple.Notes".into(), 1)]
        );
        assert_eq!(store.recent("com.apple.Notes", 10).unwrap(), vec!["note"]);
    }

    #[test]
    fn off_mode_ignores_monitor_too() {
        let store = MemoryStore::open_in_memory(&key(11), StorageMode::Off).unwrap();
        store.monitor("app", "x").unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn moving_a_blob_to_another_app_row_breaks_decryption() {
        // The app is bound as AEAD AAD, so tampering with the app column makes the
        // record fail authentication rather than decrypt under the wrong app.
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(12), StorageMode::AcceptedOnly).unwrap();
            store.remember("real.app", "private note").unwrap();
        }
        // Tamper: relabel the row's app via a raw connection.
        {
            let raw = rusqlite::Connection::open(&path).unwrap();
            raw.execute("UPDATE memories SET app = 'attacker.app'", [])
                .unwrap();
        }
        let store = MemoryStore::open(&path, &key(12), StorageMode::AcceptedOnly).unwrap();
        // The row is present but cannot be read under the forged app.
        assert_eq!(store.count().unwrap(), 1);
        assert!(store.recent("attacker.app", 10).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn same_plaintext_gets_a_fresh_nonce_each_time() {
        // GCM security rests on a unique nonce per encryption under a given key.
        // Recording the SAME (app, text) twice must produce blobs whose leading
        // NONCE_LEN nonce bytes differ — a guard against a future fixed/zero-nonce
        // regression, which would be catastrophic for AES-GCM.
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(13), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "identical text").unwrap();
            store.remember("app", "identical text").unwrap();
        }
        // Read the raw stored ciphertext blobs back via a plain connection.
        let raw = rusqlite::Connection::open(&path).unwrap();
        let mut stmt = raw
            .prepare("SELECT blob FROM memories WHERE app = 'app' ORDER BY id")
            .unwrap();
        let blobs: Vec<Vec<u8>> = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|b| b.unwrap())
            .collect();
        drop(stmt);
        drop(raw);
        let _ = std::fs::remove_file(&path);

        assert_eq!(blobs.len(), 2, "both records were stored");
        let nonce_a = &blobs[0][..NONCE_LEN];
        let nonce_b = &blobs[1][..NONCE_LEN];
        assert_ne!(
            nonce_a, nonce_b,
            "per-record nonce must be fresh, not reused for identical plaintext"
        );
        // And the full blobs differ too (distinct nonce → distinct ciphertext).
        assert_ne!(blobs[0], blobs[1]);
    }

    #[test]
    fn recent_with_zero_limit_returns_empty() {
        let store = MemoryStore::open_in_memory(&key(14), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "stored").unwrap();
        assert!(store.recent("app", 0).unwrap().is_empty());
    }

    #[test]
    fn records_are_not_decryptable_with_a_different_key() {
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(7), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "classified").unwrap();
        }
        // Reopen with the wrong key: the record exists but does not decrypt.
        let other = MemoryStore::open(&path, &key(8), StorageMode::AcceptedOnly).unwrap();
        assert_eq!(other.count().unwrap(), 1);
        assert!(other.recent("app", 10).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn count_by_app_counts_undecryptable_rows_after_key_change() {
        // count()/count_by_app() count STORED rows straight from the plaintext app
        // column (no decryption), whereas recent() skips rows that fail to decrypt.
        // After a key change every row becomes undecryptable, so recent() goes
        // empty for each app while the counts still report the stored rows — the
        // counts can EXCEED what recent() returns. Spans two apps to pin the
        // per-app aggregate too.
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(31), StorageMode::AcceptedOnly).unwrap();
            store.remember("app.one", "first one").unwrap();
            store.remember("app.one", "second one").unwrap();
            store.remember("app.two", "only two").unwrap();
        }

        // Reopen with a DIFFERENT key: rows persist but none decrypt.
        let reopened = MemoryStore::open(&path, &key(32), StorageMode::AcceptedOnly).unwrap();
        assert!(
            reopened.recent("app.one", 10).unwrap().is_empty(),
            "recent() skips undecryptable rows under the new key"
        );
        assert!(reopened.recent("app.two", 10).unwrap().is_empty());

        // But the stored-row counts are unchanged — they exceed recent()'s output.
        assert_eq!(reopened.count().unwrap(), 3);
        assert_eq!(
            reopened.count_by_app().unwrap(),
            vec![("app.one".to_string(), 2), ("app.two".to_string(), 1)]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tampered_ciphertext_byte_is_skipped_not_returned_or_panicked() {
        // Distinct from the wrong-key case: here the CORRECT key and app are
        // used, but one ciphertext byte (past the nonce) is flipped. The GCM
        // auth tag must reject the corrupted blob — `recent()` skips it as
        // best-effort (no panic, not returned), while `count()` still sees the
        // stored row.
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(15), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "intact secret note").unwrap();
        }
        // Tamper: flip one byte INSIDE the ciphertext body (after NONCE_LEN, so
        // the nonce is preserved and the blob length stays valid) via a raw
        // connection, keeping the row's app label intact.
        {
            let raw = rusqlite::Connection::open(&path).unwrap();
            let mut blob: Vec<u8> = raw
                .query_row("SELECT blob FROM memories LIMIT 1", [], |row| row.get(0))
                .unwrap();
            assert!(
                blob.len() > NONCE_LEN + 1,
                "sanity: blob has a ciphertext body to corrupt"
            );
            blob[NONCE_LEN] ^= 0x01;
            raw.execute("UPDATE memories SET blob = ?1", params![blob])
                .unwrap();
        }
        // Reopen with the SAME correct key: row present, but the corrupted
        // ciphertext fails GCM authentication and is dropped from recent().
        let store = MemoryStore::open(&path, &key(15), StorageMode::AcceptedOnly).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert!(store.recent("app", 10).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recent_limit_counts_decryptable_rows_after_skipping_corrupt_newer_rows() {
        let store = MemoryStore::open_in_memory(&key(16), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "older").unwrap();
        store.remember("app", "newer").unwrap();

        let mut blob: Vec<u8> = store
            .conn
            .query_row(
                "SELECT blob FROM memories WHERE app = 'app' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            blob.len() > NONCE_LEN + 1,
            "sanity: blob has a ciphertext body to corrupt"
        );
        blob[NONCE_LEN] ^= 0x01;
        store
            .conn
            .execute(
                "UPDATE memories SET blob = ?1 WHERE id = (SELECT MAX(id) FROM memories WHERE app = 'app')",
                params![blob],
            )
            .unwrap();

        assert_eq!(
            store.recent("app", 1).unwrap(),
            vec!["older"],
            "a corrupt newest row must not consume the caller's decryptable limit"
        );
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn decrypt_treats_blob_shorter_than_nonce_as_absent() {
        // A truncated/corrupt row whose blob is shorter than NONCE_LEN (12) bytes
        // cannot carry a nonce. decrypt() must reject it (return None) without
        // panicking on the split_at, so recent() skips it as best-effort while
        // count() still sees the stored row.
        let store = MemoryStore::open_in_memory(&key(18), StorageMode::AcceptedOnly).unwrap();
        // Insert a deliberately too-short blob (< NONCE_LEN) via the store's own
        // connection, bypassing encryption.
        let short: Vec<u8> = vec![0u8; NONCE_LEN - 1];
        store
            .conn
            .execute(
                "INSERT INTO memories (app, blob) VALUES (?1, ?2)",
                params!["app", short],
            )
            .unwrap();
        // count() counts the stored row; recent() skips the undecryptable short
        // blob, all without panicking.
        assert_eq!(store.count().unwrap(), 1);
        assert!(store.recent("app", 10).unwrap().is_empty());
    }

    #[test]
    fn memory_error_display_is_stable() {
        // The documented diagnostic text for each variant, plus the
        // From<rusqlite::Error> conversion mapping into Db(..).
        assert_eq!(
            MemoryError::Db("boom".to_string()).to_string(),
            "memory db error: boom"
        );
        assert_eq!(MemoryError::Crypto.to_string(), "memory crypto error");
        assert_eq!(
            MemoryError::Io("boom".to_string()).to_string(),
            "memory io error: boom"
        );

        // A rusqlite error converts into the Db variant carrying its message.
        let sqlite_err = rusqlite::Error::QueryReturnedNoRows;
        let mapped: MemoryError = sqlite_err.into();
        match mapped {
            MemoryError::Db(msg) => assert_eq!(
                msg,
                rusqlite::Error::QueryReturnedNoRows.to_string(),
                "From<rusqlite::Error> preserves the underlying message"
            ),
            MemoryError::Io(_) | MemoryError::Crypto => {
                panic!("rusqlite errors must map to Db(..)")
            }
        }

        // An io::Error converts into the Io variant carrying its message.
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let mapped_io: MemoryError = io_err.into();
        match mapped_io {
            MemoryError::Io(msg) => assert!(msg.contains("missing")),
            MemoryError::Db(_) | MemoryError::Crypto => {
                panic!("io errors must map to Io(..)")
            }
        }
    }

    #[test]
    fn delete_app_returns_zero_when_no_rows_match() {
        // delete_app reports how many rows were removed; deleting an app with no
        // stored rows removes nothing and returns Ok(0).
        let store = MemoryStore::open_in_memory(&key(19), StorageMode::AcceptedOnly).unwrap();
        assert_eq!(store.delete_app("absent.app").unwrap(), 0);
    }

    #[test]
    fn recent_truncates_to_limit_newest_first() {
        // With MORE decryptable rows than `limit`, recent() returns exactly the
        // newest `limit` of them, newest first — the per-app history-window cap.
        let store = MemoryStore::open_in_memory(&key(17), StorageMode::AcceptedOnly).unwrap();
        for i in 0..5 {
            store.remember("app", &format!("row {i}")).unwrap();
        }
        let recent = store.recent("app", 3).unwrap();
        assert_eq!(
            recent,
            vec![
                "row 4".to_string(),
                "row 3".to_string(),
                "row 2".to_string()
            ],
            "the newest 3 rows, newest first"
        );
    }

    #[test]
    fn recent_pages_past_full_corrupt_pages_to_fill_the_limit() {
        // Exercises the LIMIT/OFFSET paging loop across MORE than one iteration.
        // recent() fetches `limit` rows at a time; when an entire page (>= limit
        // rows) is fetched but enough fail to decrypt, it must advance
        // `offset += page` and keep reading older pages until it has `limit`
        // decryptable records or the rows run out. Here the newest THREE rows are
        // corrupt with limit=2, so the first full page (rows 5,4) is all corrupt,
        // and the corruption straddles the page boundary (row 3 is corrupt but
        // sits at the head of page two). recent() must page three times
        // (offset 0 -> 2 -> 4) to gather the two newest decryptable rows. A naive
        // single `LIMIT 2` (no paging) would return only "row 2" — or nothing —
        // instead of the two newest decryptable rows.
        let store = MemoryStore::open_in_memory(&key(20), StorageMode::AcceptedOnly).unwrap();
        for i in 0..6 {
            store.remember("app", &format!("row {i}")).unwrap();
        }
        // Corrupt the newest three rows (the three highest ids = "row 5/4/3") so
        // they fail GCM auth and are skipped, forcing recent() to page back to the
        // older "row 2" and "row 1".
        let ids: Vec<i64> = {
            let mut stmt = store
                .conn
                .prepare("SELECT id FROM memories WHERE app = 'app' ORDER BY id DESC LIMIT 3")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(ids.len(), 3, "sanity: corrupting the three newest rows");
        for id in ids {
            let mut blob: Vec<u8> = store
                .conn
                .query_row(
                    "SELECT blob FROM memories WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .unwrap();
            blob[NONCE_LEN] ^= 0x01;
            store
                .conn
                .execute(
                    "UPDATE memories SET blob = ?1 WHERE id = ?2",
                    params![blob, id],
                )
                .unwrap();
        }

        assert_eq!(
            store.recent("app", 2).unwrap(),
            vec!["row 2".to_string(), "row 1".to_string()],
            "paging skips the corrupt newer pages and returns the two newest \
             decryptable rows, newest-first"
        );
        // count() still sees every stored row regardless of decryptability.
        assert_eq!(store.count().unwrap(), 6);
    }

    #[test]
    fn recent_pages_return_correct_newest_first_order_across_page_boundaries() {
        // All rows decryptable, but limit is smaller than the row count so the
        // paging loop still runs more than once internally (page after page) when
        // limit < total. With limit == total there is exactly one short page; with
        // limit < total recent() caps at the first `limit`. This pins that the
        // newest-first ordering is stable right at the limit cutoff (row N-1 down
        // to row N-limit), independent of how the page math lands.
        let store = MemoryStore::open_in_memory(&key(21), StorageMode::AcceptedOnly).unwrap();
        for i in 0..7 {
            store.remember("app", &format!("row {i}")).unwrap();
        }
        assert_eq!(
            store.recent("app", 4).unwrap(),
            vec![
                "row 6".to_string(),
                "row 5".to_string(),
                "row 4".to_string(),
                "row 3".to_string(),
            ],
            "the newest 4 rows, newest-first"
        );
    }

    #[test]
    fn journal_mode_is_delete_after_open() {
        // DELETE journal mode is a security claim: no persistent -wal/-shm
        // sidecar leaking ciphertext/metadata alongside the db (sidecars are not
        // covered by secure_delete). Pin both that the pragma resolves to "delete"
        // and that recording a row leaves no -wal file on disk.
        let path = temp_db_path();
        let store = MemoryStore::open(&path, &key(54), StorageMode::AcceptedOnly).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete", "journal_mode must be DELETE, not WAL");

        store.remember("app", "no wal sidecar please").unwrap();

        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let wal_exists = Path::new(&wal).exists();
        let mut shm = path.as_os_str().to_owned();
        shm.push("-shm");
        let shm_exists = Path::new(&shm).exists();
        drop(store);
        let _ = std::fs::remove_file(&path);
        assert!(!wal_exists, "DELETE mode must not leave a -wal sidecar");
        assert!(!shm_exists, "DELETE mode must not leave a -shm sidecar");
    }

    #[cfg(unix)]
    #[test]
    fn open_restricts_a_preexisting_sidecar_to_0600() {
        // The open() restrict loop is belt-and-suspenders: it chmods any
        // -journal/-wal/-shm sidecar that exists alongside the db down to 0600 so
        // a sidecar can never expose its contents at the default world/group-
        // readable mode. Under the pinned DELETE journal mode the -journal sidecar
        // is *transient* — SQLite deletes a stale journal during open (hot-journal
        // handling) and removes the live one on commit — so a -journal file
        // pre-created before a fresh open is gone by the time the loop runs and
        // cannot be caught persistently. (Verified empirically: the file is
        // NotFound after open.) Rather than assert an unreachable state, this
        // exercises the SAME restrict path on a sidecar SQLite does NOT manage in
        // DELETE mode: pre-create a `<path>-shm` at 0644 alongside an
        // already-existing db, reopen, and assert open() tightened it to 0600.
        use std::os::unix::fs::PermissionsExt;
        let path = temp_db_path();
        // First open creates a real, consistent db file on disk.
        {
            let store = MemoryStore::open(&path, &key(55), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "row").unwrap();
        }
        assert!(path.exists(), "sanity: db file persisted after first open");

        // Pre-create an -shm sidecar world/group-readable (0644). DELETE mode does
        // not use -shm, so SQLite leaves this file alone — only the restrict loop
        // touches it.
        let mut shm = path.as_os_str().to_owned();
        shm.push("-shm");
        let shm_path = std::path::PathBuf::from(&shm);
        std::fs::write(&shm_path, b"stale sidecar bytes").unwrap();
        std::fs::set_permissions(&shm_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            std::fs::metadata(&shm_path).unwrap().permissions().mode() & 0o777,
            0o644,
            "sanity: pre-created the sidecar at 0644"
        );

        // Reopen: the restrict loop must tighten the existing sidecar to 0600.
        let store = MemoryStore::open(&path, &key(55), StorageMode::AcceptedOnly).unwrap();
        let mode = std::fs::metadata(&shm_path).unwrap().permissions().mode() & 0o777;
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&shm_path);
        assert_eq!(
            mode, 0o600,
            "open() must restrict an existing sidecar to 0600, got {mode:o}"
        );
    }

    #[test]
    fn delete_all_erases_ciphertext_without_reopening() {
        // Real "disable and erase" usage: delete_all() is called on a LIVE store
        // and the erasure must be visible on disk immediately, without dropping or
        // reopening the connection. Scan the raw file via a SEPARATE read-only
        // handle while the original store is still alive to pin that secure_delete
        // zeroes the freed ciphertext in place (not only after a checkpoint on
        // close).
        let path = temp_db_path();
        let store = MemoryStore::open(&path, &key(56), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "erase me while alive").unwrap();
        let blob: Vec<u8> = store
            .conn
            .query_row("SELECT blob FROM memories LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert!(blob.len() >= 16, "sanity: a real ciphertext blob");

        store.delete_all().unwrap();

        // Separate read-only handle: read the file bytes WITHOUT touching the
        // still-alive `store` connection or reopening it.
        let raw = std::fs::read(&path).unwrap();
        let body = &blob[NONCE_LEN..];
        let found = raw.windows(body.len()).any(|w| w == body);
        drop(store);
        let _ = std::fs::remove_file(&path);
        assert!(
            !found,
            "delete_all on a live store must zero the ciphertext on disk (secure_delete)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_backed_db_is_mode_0600_after_open() {
        // The db file carries the plaintext `app` metadata column, so it must be
        // owner-only (0600), not the default world/group-readable 0644.
        use std::os::unix::fs::PermissionsExt;
        let path = temp_db_path();
        {
            let store = MemoryStore::open(&path, &key(50), StorageMode::AcceptedOnly).unwrap();
            store.remember("app", "secret").unwrap();
        }
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600, "db file must be owner-only, got {mode:o}");
    }

    #[test]
    fn open_creates_missing_parent_directory() {
        // open() must create the parent dir; otherwise SQLite errors and the
        // store silently fails to init.
        let mut suffix = [0u8; 8];
        getrandom::getrandom(&mut suffix).unwrap();
        let hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("cm-memory-test-dir-{hex}"));
        let path = dir.join("nested").join("memory.db");
        assert!(!dir.exists(), "sanity: parent dir does not pre-exist");

        let store = MemoryStore::open(&path, &key(51), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "hello").unwrap();
        assert_eq!(store.recent("app", 1).unwrap(), vec!["hello"]);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trim_to_cap_holds_row_count_at_the_cap_oldest_first() {
        // Drive the eviction bound with a tiny cap (the production MAX_RECORDS is
        // large, so we test the trim SQL directly rather than insert 50k rows).
        // Inserting past the cap must hold the count at the cap and keep the
        // NEWEST rows, dropping the oldest.
        let store = MemoryStore::open_in_memory(&key(52), StorageMode::AcceptedOnly).unwrap();
        for i in 0..5 {
            store.remember("app", &format!("row {i}")).unwrap();
            MemoryStore::trim_to_cap(&store.conn, 3).unwrap();
            assert!(
                store.count().unwrap() <= 3,
                "count must never exceed the cap"
            );
        }
        assert_eq!(store.count().unwrap(), 3, "held at the cap");
        assert_eq!(
            store.recent("app", 10).unwrap(),
            vec![
                "row 4".to_string(),
                "row 3".to_string(),
                "row 2".to_string()
            ],
            "trim drops the oldest rows, keeping the newest cap rows"
        );
    }

    #[test]
    fn store_enforces_max_records_bound() {
        // The production insert path trims to MAX_RECORDS. MAX_RECORDS is large,
        // so this checks an under-cap store is left untouched by the implicit
        // trim in store().
        const { assert!(MAX_RECORDS > 0, "the cap must be a positive bound") };
        let store = MemoryStore::open_in_memory(&key(53), StorageMode::AcceptedOnly).unwrap();
        for i in 0..10 {
            store.remember("app", &format!("row {i}")).unwrap();
        }
        assert_eq!(
            store.count().unwrap(),
            10,
            "well under MAX_RECORDS: nothing evicted"
        );
    }
}
