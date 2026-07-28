//! Memory-store encryption-key handling for Linux (ROADMAP Phase 2.6).
//!
//! Two halves, split on purpose:
//! - **This module** owns the load-or-create *contract* and the CSPRNG. It is
//!   host-independent (seams, no D-Bus), so the rules that actually protect user
//!   data are unit-tested on the macOS and Windows lanes that also build this
//!   crate.
//! - `crate::keyring` owns the transport: the Secret Service D-Bus
//!   conversation. Linux-only, and provable only against a real Secret Service.
//!
//! Fail-closed contract, identical to the macOS Keychain store
//! (`platform_macos::keychain`) so the two platforms behave the same:
//! - a stored secret of the wrong length is an ERROR — never overwritten, since
//!   it may be a foreign or corrupt entry;
//! - a freshly generated key is returned ONLY after it was persisted — a key
//!   that never reached the key store would encrypt a database that becomes
//!   undecryptable on the next launch;
//! - no key store at all (headless server, minimal session, locked keyring that
//!   cannot be unlocked) is an ERROR. There is no on-disk plaintext fallback and
//!   no fabricated key: `app` then runs with memory disabled, which is the
//!   posture the design requires.

use std::sync::Arc;

use platform::PlatformError;
use zeroize::Zeroize;

/// AES-256: the `memory` crate's `StaticKey` is exactly 32 bytes.
pub const KEY_LEN: usize = 32;

/// Identifies the secret. `service`/`account` mirror the macOS Keychain entry's
/// naming (`platform_macos::keychain::MEMORY_KEY_SERVICE`/`_ACCOUNT`) so both
/// platforms describe the same secret with the same words.
pub const MEMORY_KEY_SERVICE: &str = "com.compme.memory";
pub const MEMORY_KEY_ACCOUNT: &str = "aes-256-gcm-key";
/// `xdg:schema` is the attribute libsecret-based tools read to know which
/// application's schema an item follows; the value is an application-chosen
/// dotted name, so this is compme's. Purely cosmetic for lookup (searches match
/// on a subset of attributes), but it is what makes the item legible in
/// seahorse instead of anonymous.
pub const MEMORY_KEY_SCHEMA: &str = "com.compme.MemoryKey";
/// Item label — the human-readable string a keyring UI lists.
pub const MEMORY_KEY_LABEL: &str = "compme memory-store key";

/// The attribute set written on create and searched on load. Sorted-by-key order
/// is irrelevant on the wire (D-Bus `a{ss}`), but the array keeps the set in one
/// place so the write and the search cannot drift apart.
pub const KEY_ATTRIBUTES: [(&str, &str); 3] = [
    ("service", MEMORY_KEY_SERVICE),
    ("account", MEMORY_KEY_ACCOUNT),
    ("xdg:schema", MEMORY_KEY_SCHEMA),
];

/// Reads the stored secret. `Ok(None)` means "no such item" (first use) and is
/// the ONLY signal that authorizes creating one.
pub type SecretReader = dyn Fn() -> Result<Option<Vec<u8>>, PlatformError> + Send + Sync + 'static;
/// Persists the secret. Must not return `Ok` unless the store accepted it.
pub type SecretWriter = dyn Fn(&[u8]) -> Result<(), PlatformError> + Send + Sync + 'static;
pub type KeyGenerator = dyn Fn() -> Result<[u8; KEY_LEN], PlatformError> + Send + Sync + 'static;

/// Loads (or creates on first use) the memory-store key. Construct with
/// `MemoryKeyStore::secret_service` for the real Secret Service; tests inject
/// fakes with [`MemoryKeyStore::with_seams`].
pub struct MemoryKeyStore {
    read_secret: Arc<SecretReader>,
    write_secret: Arc<SecretWriter>,
    generate_key: Arc<KeyGenerator>,
}

impl MemoryKeyStore {
    /// The real store: Secret Service over the session bus, keyed with
    /// `/dev/urandom`.
    #[cfg(target_os = "linux")]
    pub fn secret_service() -> Self {
        Self::with_seams(
            Arc::new(crate::keyring::read_memory_key),
            Arc::new(crate::keyring::write_memory_key),
            Arc::new(random_key),
        )
    }

    pub fn with_seams(
        read_secret: Arc<SecretReader>,
        write_secret: Arc<SecretWriter>,
        generate_key: Arc<KeyGenerator>,
    ) -> Self {
        Self {
            read_secret,
            write_secret,
            generate_key,
        }
    }

    /// The stored 32-byte key, or a generated-and-persisted one on first use.
    /// See the module docs for the fail-closed contract.
    pub fn load_or_create_memory_key(&self) -> Result<[u8; KEY_LEN], PlatformError> {
        match (self.read_secret)()? {
            Some(mut secret) => {
                // Copy into the fixed array, then scrub the transport buffer so
                // the AES key does not linger in a heap Vec after this returns
                // (matching the macOS store and the `memory` crate).
                let result = <[u8; KEY_LEN]>::try_from(secret.as_slice()).map_err(|_| {
                    PlatformError::CannotComplete {
                        reason: format!(
                            "secret-service item {MEMORY_KEY_SERVICE}/{MEMORY_KEY_ACCOUNT} holds \
                             {} bytes, expected {KEY_LEN} — refusing to overwrite a foreign or \
                             corrupt secret",
                            secret.len()
                        ),
                    }
                });
                secret.zeroize();
                result
            }
            None => {
                let key = (self.generate_key)()?;
                (self.write_secret)(&key)?;
                Ok(key)
            }
        }
    }
}

/// What a Secret Service search means for the load path, decided here rather
/// than inline in the D-Bus code so the one rule that can silently destroy user
/// data — treating a *locked* match as "absent" and creating a second key — is
/// tested on every host.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyLookup<T> {
    /// A readable item: use it.
    Unlocked(T),
    /// Matching items exist but are locked. Unlocking must be attempted; if it
    /// fails, the caller reports it. Creating a new key here would re-key the
    /// user's memory store.
    LockedOnly(Vec<T>),
    /// No item anywhere: genuine first use.
    Absent,
}

/// Classify `SearchItems`'s `(unlocked, locked)` reply. The first unlocked item
/// wins; several matches can only come from a duplicate create, and either is
/// equally valid to read.
pub fn classify_lookup<T>(unlocked: Vec<T>, locked: Vec<T>) -> KeyLookup<T> {
    match unlocked.into_iter().next() {
        Some(item) => KeyLookup::Unlocked(item),
        None if locked.is_empty() => KeyLookup::Absent,
        None => KeyLookup::LockedOnly(locked),
    }
}

/// 32 CSPRNG bytes from `/dev/urandom`.
///
/// Why the device and not `getrandom(2)`: this crate has no `libc` dependency,
/// and adding one for 32 bytes at startup buys nothing. On Linux `/dev/urandom`
/// is the kernel CSPRNG and never blocks; compme reads it long after boot (a
/// desktop session is running), so the early-boot seeding caveat does not apply.
/// A short read is an error, never a partially random key.
pub fn random_key() -> Result<[u8; KEY_LEN], PlatformError> {
    use std::io::Read;

    let mut key = [0u8; KEY_LEN];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut key))
        .map_err(|err| PlatformError::CannotComplete {
            reason: format!("/dev/urandom: cannot generate a memory key: {err}"),
        })?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn store(
        read_secret: Arc<SecretReader>,
        write_secret: Arc<SecretWriter>,
        generate_key: Arc<KeyGenerator>,
    ) -> MemoryKeyStore {
        MemoryKeyStore::with_seams(read_secret, write_secret, generate_key)
    }

    #[test]
    fn attributes_name_the_same_secret_as_the_macos_keychain_entry() {
        // The macOS entry is com.compme.memory / aes-256-gcm-key; drifting here
        // would make the two platforms disagree about what the secret is called.
        assert_eq!(
            KEY_ATTRIBUTES,
            [
                ("service", "com.compme.memory"),
                ("account", "aes-256-gcm-key"),
                ("xdg:schema", "com.compme.MemoryKey"),
            ]
        );
        assert_eq!(KEY_LEN, 32, "memory::StaticKey is AES-256");
    }

    #[test]
    fn an_unlocked_match_wins_and_a_locked_match_is_never_absent() {
        assert_eq!(
            classify_lookup(vec!["/item/1", "/item/2"], vec!["/item/3"]),
            KeyLookup::Unlocked("/item/1")
        );
        // The dangerous case: a locked match must NOT read as first use, or the
        // create path would mint a second key and re-key the memory store.
        assert_eq!(
            classify_lookup(Vec::<&str>::new(), vec!["/item/3"]),
            KeyLookup::LockedOnly(vec!["/item/3"])
        );
        assert_eq!(
            classify_lookup(Vec::<&str>::new(), Vec::new()),
            KeyLookup::Absent
        );
    }

    #[test]
    fn load_returns_the_existing_key_without_writing() {
        let stored: [u8; KEY_LEN] = std::array::from_fn(|i| i as u8);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&writes);
        let s = store(
            Arc::new(move || Ok(Some(stored.to_vec()))),
            Arc::new(move |secret: &[u8]| {
                seen.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| Ok([0u8; KEY_LEN])),
        );

        // A per-position pattern pins that the copy out of the transport buffer
        // is faithful (no truncation/offset) and that scrubbing the source does
        // not corrupt the already-copied result.
        assert_eq!(s.load_or_create_memory_key(), Ok(stored));
        assert!(
            writes.lock().unwrap().is_empty(),
            "an existing key must be returned as-is, never rewritten"
        );
    }

    #[test]
    fn first_use_generates_persists_and_returns_the_same_key() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&writes);
        let s = store(
            Arc::new(|| Ok(None)),
            Arc::new(move |secret: &[u8]| {
                seen.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| Ok([9u8; KEY_LEN])),
        );

        assert_eq!(s.load_or_create_memory_key(), Ok([9u8; KEY_LEN]));
        assert_eq!(
            *writes.lock().unwrap(),
            vec![vec![9u8; KEY_LEN]],
            "the generated key must be persisted exactly once, byte-identical"
        );
    }

    #[test]
    fn a_key_that_failed_to_persist_is_never_returned() {
        let s = store(
            Arc::new(|| Ok(None)),
            Arc::new(|_| {
                Err(PlatformError::CannotComplete {
                    reason: "collection is locked".into(),
                })
            }),
            Arc::new(|| Ok([9u8; KEY_LEN])),
        );

        assert_eq!(
            s.load_or_create_memory_key(),
            Err(PlatformError::CannotComplete {
                reason: "collection is locked".into(),
            }),
            "an unpersisted key would encrypt a database that is lost on restart"
        );
    }

    #[test]
    fn a_wrong_length_secret_errors_and_is_never_overwritten() {
        for bad in [vec![1u8; 16], vec![2u8; 64], Vec::new()] {
            let writes = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&writes);
            let len = bad.len();
            let s = store(
                Arc::new(move || Ok(Some(bad.clone()))),
                Arc::new(move |secret: &[u8]| {
                    seen.lock().unwrap().push(secret.to_vec());
                    Ok(())
                }),
                Arc::new(|| Ok([9u8; KEY_LEN])),
            );

            let result = s.load_or_create_memory_key();
            let Err(PlatformError::CannotComplete { reason }) = result else {
                panic!("expected a fail-closed error for {len} bytes, got {result:?}");
            };
            assert!(
                reason.contains(&format!("{len} bytes")) && reason.contains("refusing"),
                "error must name the bad length and the refusal: {reason}"
            );
            assert!(
                writes.lock().unwrap().is_empty(),
                "a foreign/corrupt entry must never be overwritten"
            );
        }
    }

    #[test]
    fn a_generator_failure_propagates_without_writing() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&writes);
        let s = store(
            Arc::new(|| Ok(None)),
            Arc::new(move |secret: &[u8]| {
                seen.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| {
                Err(PlatformError::CannotComplete {
                    reason: "/dev/urandom: cannot generate a memory key".into(),
                })
            }),
        );

        assert!(s.load_or_create_memory_key().is_err());
        assert!(
            writes.lock().unwrap().is_empty(),
            "a failed key generation must never reach the key store"
        );
    }

    #[test]
    fn a_read_error_propagates_without_generating_or_writing() {
        // This is the headless case: no Secret Service on the bus. It must never
        // fall through to "create a key", because a store that cannot be read
        // also cannot persist one.
        let touched = Arc::new(Mutex::new(Vec::new()));
        let in_write = Arc::clone(&touched);
        let in_generate = Arc::clone(&touched);
        let s = store(
            Arc::new(|| {
                Err(PlatformError::CannotComplete {
                    reason: "no org.freedesktop.secrets on the session bus".into(),
                })
            }),
            Arc::new(move |_| {
                in_write.lock().unwrap().push("write");
                Ok(())
            }),
            Arc::new(move || {
                in_generate.lock().unwrap().push("generate");
                Ok([9u8; KEY_LEN])
            }),
        );

        assert_eq!(
            s.load_or_create_memory_key(),
            Err(PlatformError::CannotComplete {
                reason: "no org.freedesktop.secrets on the session bus".into(),
            })
        );
        assert!(
            touched.lock().unwrap().is_empty(),
            "an unreadable key store must not be written to (the key may exist)"
        );
    }

    /// `/dev/urandom` exists on Linux and macOS but not on the Windows lane.
    #[cfg(unix)]
    #[test]
    fn random_key_returns_32_fresh_bytes() {
        let a = random_key().expect("/dev/urandom must be readable");
        let b = random_key().expect("/dev/urandom must be readable");
        assert_eq!(a.len(), KEY_LEN);
        assert_ne!(a, b, "two CSPRNG reads must not return the same key");
        assert_ne!(
            a, [0u8; KEY_LEN],
            "an all-zero key means the read was a no-op"
        );
    }
}
