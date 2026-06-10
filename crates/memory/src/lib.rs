//! Encrypted local memory of accepted completions (design spec §6 / §16).
//!
//! Privacy model: text is **redacted** (`redaction`) before it is **encrypted**
//! (AES-256-GCM, random nonce per record) and only the ciphertext is written to
//! the SQLite database — plaintext never touches disk. The 32-byte key comes
//! from a [`KeyProvider`]; production supplies it from the OS keystore (Keychain
//! on macOS — A3 live integration), tests supply a fixed key.
//!
//! Storage is **opt-in**: with [`StorageMode::Off`] (the default) nothing is
//! recorded. `AcceptedOnly` stores accepted completions; `AllMonitored` is the
//! broader opt-in. Records are inspectable (`count`/`recent`) and deletable
//! (`delete_all`, `delete_app`).

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rusqlite::{params, Connection};

const NONCE_LEN: usize = 12;

/// Where the 32-byte AES-256 key comes from. Production binds this to the OS
/// keystore (Keychain); tests use [`StaticKey`].
pub trait KeyProvider {
    fn key(&self) -> [u8; 32];
}

/// A fixed key — for tests and headless use. The real Keychain-backed provider
/// is an A3 live integration.
pub struct StaticKey(pub [u8; 32]);

impl KeyProvider for StaticKey {
    fn key(&self) -> [u8; 32] {
        self.0
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
    Crypto,
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryError::Db(msg) => write!(f, "memory db error: {msg}"),
            MemoryError::Crypto => write!(f, "memory crypto error"),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<rusqlite::Error> for MemoryError {
    fn from(err: rusqlite::Error) -> Self {
        MemoryError::Db(err.to_string())
    }
}

type Result<T> = std::result::Result<T, MemoryError>;

/// Encrypted store of accepted completions.
pub struct MemoryStore {
    conn: Connection,
    cipher: Aes256Gcm,
    mode: StorageMode,
}

impl MemoryStore {
    /// Open (creating if needed) a file-backed store.
    pub fn open(path: &Path, key: &impl KeyProvider, mode: StorageMode) -> Result<Self> {
        Self::from_connection(Connection::open(path)?, key, mode)
    }

    /// Open an in-memory store (tests / ephemeral use).
    pub fn open_in_memory(key: &impl KeyProvider, mode: StorageMode) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, key, mode)
    }

    fn from_connection(
        conn: Connection,
        key: &impl KeyProvider,
        mode: StorageMode,
    ) -> Result<Self> {
        // secure_delete zeroes freed content so delete_all/delete_app actually
        // erase ciphertext from disk (freelist pages), not just unlink rows.
        conn.pragma_update(None, "secure_delete", true)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS memories (
                 id   INTEGER PRIMARY KEY AUTOINCREMENT,
                 app  TEXT NOT NULL,
                 blob BLOB NOT NULL
             )",
            [],
        )?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.key()));
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
        let redacted = redaction::redact(text);
        let blob = self.encrypt(&redacted, app.as_bytes())?;
        self.conn.execute(
            "INSERT INTO memories (app, blob) VALUES (?1, ?2)",
            params![app, blob],
        )?;
        Ok(())
    }

    /// Total number of stored records.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        Ok(n.max(0) as usize)
    }

    /// The most recent `limit` decryptable records for `app`, newest first.
    /// Records that fail to decrypt (e.g. a different key) are skipped.
    pub fn recent(&self, app: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT blob FROM memories WHERE app = ?1 ORDER BY id DESC LIMIT ?2")?;
        let blobs = stmt.query_map(params![app, limit as i64], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for blob in blobs {
            // Best-effort read: a row that fails to decrypt (wrong key, or app
            // column tampered so the AAD no longer matches) is treated as absent.
            if let Some(text) = self.decrypt(&blob?, app.as_bytes()) {
                out.push(text);
            }
        }
        Ok(out)
    }

    /// Per-app record counts, most-used first (the App-Settings pane list).
    /// Counts come straight from the plaintext `app` column — no decryption;
    /// ties break alphabetically so the order is deterministic.
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
            out.push((app, n.max(0) as u64));
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
    fn remembers_and_retrieves_recent_newest_first() {
        let store = MemoryStore::open_in_memory(&key(2), StorageMode::AcceptedOnly).unwrap();
        store.remember("app", "first").unwrap();
        store.remember("app", "second").unwrap();
        assert_eq!(store.recent("app", 10).unwrap(), vec!["second", "first"]);
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
}
