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

use aes_gcm::aead::{Aead, KeyInit};
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

    /// Record `text` for `app`. No-op when storage is `Off`. The text is redacted,
    /// then encrypted; only ciphertext is persisted.
    pub fn remember(&self, app: &str, text: &str) -> Result<()> {
        if self.mode == StorageMode::Off {
            return Ok(());
        }
        let redacted = redaction::redact(text);
        let blob = self.encrypt(&redacted)?;
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
            if let Some(text) = self.decrypt(&blob?) {
                out.push(text);
            }
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

    fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce_bytes).map_err(|_| MemoryError::Crypto)?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| MemoryError::Crypto)?;
        // Prepend the nonce so decryption can recover it.
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(blob)
    }

    fn decrypt(&self, blob: &[u8]) -> Option<String> {
        if blob.len() < NONCE_LEN {
            return None;
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plaintext = self.cipher.decrypt(nonce, ciphertext).ok()?;
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
