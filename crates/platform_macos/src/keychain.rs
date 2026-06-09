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

/// Keychain service name for the memory-store key entry.
pub const MEMORY_KEY_SERVICE: &str = "com.complete-me.memory";
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
            Some(secret) => {
                <[u8; 32]>::try_from(secret.as_slice()).map_err(|_| PlatformError::CannotComplete {
                    reason: format!(
                        "keychain entry {}/{} holds {} bytes, expected 32 — refusing to \
                             overwrite a foreign or corrupt secret",
                        self.service,
                        self.account,
                        secret.len()
                    ),
                })
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
