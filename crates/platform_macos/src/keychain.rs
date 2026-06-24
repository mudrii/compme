//! Keychain-backed storage for the encrypted memory store's AES-256 key
//! (design spec §16 "key in OS keystore", A3). The Security-framework FFI is
//! isolated behind injectable seams (the file-wide poster pattern), so the
//! load-or-create contract is unit-testable headless.
//!
//! Fail-closed contract:
//! - an existing secret of the wrong length is an ERROR — never overwritten
//!   (it may be a foreign/corrupt entry; overwriting could destroy data);
//! - a freshly generated key is returned ONLY after it was persisted — a key
//!   that never reached the keychain would encrypt a database that becomes
//!   undecryptable on the next launch.

use std::sync::Arc;

use platform::PlatformError;
use zeroize::Zeroize;

/// Keychain service name for the memory-store key entry.
pub const MEMORY_KEY_SERVICE: &str = "com.compme.memory";
/// Keychain account name for the memory-store key entry.
pub const MEMORY_KEY_ACCOUNT: &str = "aes-256-gcm-key";

type SecretReader =
    dyn Fn(&str, &str) -> Result<Option<Vec<u8>>, PlatformError> + Send + Sync + 'static;
type SecretWriter = dyn Fn(&str, &str, &[u8]) -> Result<(), PlatformError> + Send + Sync + 'static;
type KeyGenerator = dyn Fn() -> Result<[u8; 32], PlatformError> + Send + Sync + 'static;

/// Loads (or creates on first use) the memory-store key from the macOS
/// Keychain. Construct with [`KeychainKeyStore::new`] for the real Keychain;
/// tests inject fake seams.
pub struct KeychainKeyStore {
    service: String,
    account: String,
    read_secret: Arc<SecretReader>,
    write_secret: Arc<SecretWriter>,
    generate_key: Arc<KeyGenerator>,
}

impl KeychainKeyStore {
    pub fn new() -> Self {
        Self {
            service: MEMORY_KEY_SERVICE.to_string(),
            account: MEMORY_KEY_ACCOUNT.to_string(),
            read_secret: Arc::new(keychain_read_secret),
            write_secret: Arc::new(keychain_write_secret),
            generate_key: Arc::new(generate_random_key),
        }
    }

    /// Returns the stored 32-byte key, or generates + persists one on first
    /// use. See the module docs for the fail-closed contract.
    pub fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        match (self.read_secret)(&self.service, &self.account)? {
            Some(mut secret) => {
                // Copy into the fixed array, then scrub the raw keychain bytes
                // so the AES key does not linger in the heap Vec after this
                // returns (matches the `memory` crate's key zeroization).
                let result = <[u8; 32]>::try_from(secret.as_slice()).map_err(|_| {
                    PlatformError::CannotComplete {
                        reason: format!(
                            "keychain entry {}/{} holds {} bytes, expected 32 — refusing to \
                                 overwrite a foreign or corrupt secret",
                            self.service,
                            self.account,
                            secret.len()
                        ),
                    }
                });
                secret.zeroize();
                result
            }
            None => {
                let key = (self.generate_key)()?;
                (self.write_secret)(&self.service, &self.account, &key)?;
                Ok(key)
            }
        }
    }
}

impl Default for KeychainKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

/// `errSecItemNotFound` — the only Security-framework status that means
/// "no such entry" (first use) rather than a real failure.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

extern "C" {
    /// macOS libc CSPRNG (max 256 bytes per call — we need 32).
    fn getentropy(buf: *mut u8, buflen: usize) -> i32;
}

fn keychain_read_secret(service: &str, account: &str) -> Result<Option<Vec<u8>>, PlatformError> {
    match security_framework::passwords::get_generic_password(service, account) {
        Ok(secret) => Ok(Some(secret)),
        Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
        Err(err) => Err(PlatformError::CannotComplete {
            reason: format!("keychain read failed: {err}"),
        }),
    }
}

fn keychain_write_secret(service: &str, account: &str, secret: &[u8]) -> Result<(), PlatformError> {
    security_framework::passwords::set_generic_password(service, account, secret).map_err(|err| {
        PlatformError::CannotComplete {
            reason: format!("keychain write failed: {err}"),
        }
    })
}

fn generate_random_key() -> Result<[u8; 32], PlatformError> {
    let mut key = [0u8; 32];
    // SAFETY: getentropy fills exactly `buflen` bytes of the provided
    // buffer; `key` is a live 32-byte array and 32 <= GETENTROPY_MAX (256).
    if unsafe { getentropy(key.as_mut_ptr(), key.len()) } != 0 {
        return Err(PlatformError::CannotComplete {
            reason: "getentropy failed to generate a memory key".into(),
        });
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn store_with_hooks(
        read_secret: Arc<SecretReader>,
        write_secret: Arc<SecretWriter>,
        generate_key: Arc<KeyGenerator>,
    ) -> KeychainKeyStore {
        KeychainKeyStore {
            service: "test-service".into(),
            account: "test-account".into(),
            read_secret,
            write_secret,
            generate_key,
        }
    }

    #[test]
    fn load_returns_the_existing_32_byte_secret_without_writing() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_in_hook = Arc::clone(&writes);
        let store = store_with_hooks(
            Arc::new(|service, account| {
                assert_eq!((service, account), ("test-service", "test-account"));
                Ok(Some(vec![7u8; 32]))
            }),
            Arc::new(move |_, _, secret: &[u8]| {
                writes_in_hook.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| Ok([0u8; 32])),
        );

        assert_eq!(store.load_or_create_memory_key(), Ok([7u8; 32]));
        assert!(
            writes.lock().unwrap().is_empty(),
            "an existing key must be returned as-is, never rewritten"
        );
    }

    #[test]
    fn load_copies_every_byte_of_the_stored_key_before_zeroizing_the_source() {
        // The load path copies the raw keychain Vec into the fixed [u8; 32]
        // array and then zeroizes the source. A distinctive per-position
        // pattern pins that the copy is faithful (no truncation/offset) AND
        // that scrubbing the source Vec does not corrupt the already-copied
        // result. (The source Vec is owned by the function and freed on return,
        // so reading it post-zeroize would be use-after-free UB; we pin the
        // observable returned key instead, which only the zeroizing branch
        // produces.)
        let stored: [u8; 32] = std::array::from_fn(|i| i as u8);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_in_hook = Arc::clone(&writes);
        let store = store_with_hooks(
            Arc::new(move |_, _| Ok(Some(stored.to_vec()))),
            Arc::new(move |_, _, secret: &[u8]| {
                writes_in_hook.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| Ok([0u8; 32])),
        );

        assert_eq!(
            store.load_or_create_memory_key(),
            Ok(stored),
            "every byte of the stored key must survive the copy + source zeroization"
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "loading an existing key must never write"
        );
    }

    #[test]
    fn first_use_generates_persists_and_returns_the_same_key() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_in_hook = Arc::clone(&writes);
        let store = store_with_hooks(
            Arc::new(|_, _| Ok(None)),
            Arc::new(move |service, account, secret: &[u8]| {
                writes_in_hook.lock().unwrap().push((
                    service.to_string(),
                    account.to_string(),
                    secret.to_vec(),
                ));
                Ok(())
            }),
            Arc::new(|| Ok([9u8; 32])),
        );

        assert_eq!(store.load_or_create_memory_key(), Ok([9u8; 32]));
        assert_eq!(
            *writes.lock().unwrap(),
            vec![(
                "test-service".to_string(),
                "test-account".to_string(),
                vec![9u8; 32]
            )],
            "the generated key must be persisted exactly once, byte-identical"
        );
    }

    #[test]
    fn a_key_that_failed_to_persist_is_never_returned() {
        let store = store_with_hooks(
            Arc::new(|_, _| Ok(None)),
            Arc::new(|_, _, _: &[u8]| {
                Err(PlatformError::CannotComplete {
                    reason: "keychain write denied".into(),
                })
            }),
            Arc::new(|| Ok([9u8; 32])),
        );

        assert_eq!(
            store.load_or_create_memory_key(),
            Err(PlatformError::CannotComplete {
                reason: "keychain write denied".into(),
            }),
            "an unpersisted key would encrypt a database that is lost on restart"
        );
    }

    #[test]
    fn a_wrong_length_secret_errors_and_is_never_overwritten() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_in_hook = Arc::clone(&writes);
        let store = store_with_hooks(
            Arc::new(|_, _| Ok(Some(vec![1u8; 16]))),
            Arc::new(move |_, _, secret: &[u8]| {
                writes_in_hook.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| Ok([9u8; 32])),
        );

        let result = store.load_or_create_memory_key();
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("expected a fail-closed error, got {result:?}");
        };
        assert!(
            reason.contains("16 bytes") && reason.contains("refusing"),
            "error must name the bad length and the refusal: {reason}"
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "a foreign/corrupt entry must never be overwritten"
        );
    }

    #[test]
    fn a_too_long_secret_errors_and_is_never_overwritten() {
        // The short-secret test pins the < 32 case; pin the > 32 case too. Both
        // are `try_from` failures, but a foreign entry that is too LONG is a
        // distinct corruption shape (e.g. a different app's larger blob), and it
        // must fail closed identically — named length, refusal, and no write.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_in_hook = Arc::clone(&writes);
        let store = store_with_hooks(
            Arc::new(|_, _| Ok(Some(vec![2u8; 64]))),
            Arc::new(move |_, _, secret: &[u8]| {
                writes_in_hook.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| Ok([9u8; 32])),
        );

        let result = store.load_or_create_memory_key();
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("expected a fail-closed error, got {result:?}");
        };
        assert!(
            reason.contains("64 bytes") && reason.contains("refusing"),
            "error must name the bad length and the refusal: {reason}"
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "a foreign/corrupt entry must never be overwritten"
        );
    }

    #[test]
    fn a_generator_failure_propagates_without_writing() {
        // First use generates then persists; if the CSPRNG itself fails
        // (getentropy returning non-zero), the error must propagate and NO write
        // may happen — persisting a bad/partial key would be worse than failing.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_in_hook = Arc::clone(&writes);
        let store = store_with_hooks(
            Arc::new(|_, _| Ok(None)),
            Arc::new(move |_, _, secret: &[u8]| {
                writes_in_hook.lock().unwrap().push(secret.to_vec());
                Ok(())
            }),
            Arc::new(|| {
                Err(PlatformError::CannotComplete {
                    reason: "getentropy failed to generate a memory key".into(),
                })
            }),
        );

        assert_eq!(
            store.load_or_create_memory_key(),
            Err(PlatformError::CannotComplete {
                reason: "getentropy failed to generate a memory key".into(),
            })
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "a failed key generation must never reach the keychain"
        );
    }

    #[test]
    fn a_read_error_propagates_without_generating_or_writing() {
        let touched = Arc::new(Mutex::new(Vec::new()));
        let touched_in_write = Arc::clone(&touched);
        let touched_in_generate = Arc::clone(&touched);
        let store = store_with_hooks(
            Arc::new(|_, _| {
                Err(PlatformError::CannotComplete {
                    reason: "keychain locked".into(),
                })
            }),
            Arc::new(move |_, _, _: &[u8]| {
                touched_in_write.lock().unwrap().push("write");
                Ok(())
            }),
            Arc::new(move || {
                touched_in_generate.lock().unwrap().push("generate");
                Ok([9u8; 32])
            }),
        );

        assert_eq!(
            store.load_or_create_memory_key(),
            Err(PlatformError::CannotComplete {
                reason: "keychain locked".into(),
            })
        );
        assert!(
            touched.lock().unwrap().is_empty(),
            "an unreadable keychain must not be written to (the key may exist)"
        );
    }
}
