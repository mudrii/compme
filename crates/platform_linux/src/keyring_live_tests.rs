//! Live Secret Service tests (ROADMAP Phase 2.6). Linux only, and `#[ignore]`d:
//! they need a real session bus, and the runner must say which kind of session it
//! stood up.
//!
//! `COMPME_KEYRING_EXPECT` declares that: `absent` (a session bus with no keyring
//! daemon — the headless/minimal-session case), `present` (a daemon serving
//! `org.freedesktop.secrets` with an unlocked default collection), or `locked`
//! (the same daemon after the collection holding compme's key was locked). The
//! test refuses to guess: one that decides for itself whether a key store exists
//! reports success when the store is broken, which is exactly the failure this
//! suite exists to catch.
//!
//! **absent** — no Secret Service on the bus, the fail-closed path:
//!
//! ```sh
//! COMPME_KEYRING_EXPECT=absent \
//!   tools/acceptance/run-linux-atspi-session.sh --run-in-session \
//!   cargo test -p platform_linux -- --ignored --test-threads=1 keyring
//! ```
//!
//! **present**, then **locked** — a real keyring. `HOME` must be a throwaway
//! directory: these runs CREATE a key in the keyring they find. Build the test
//! binary *before* switching `HOME`, so cargo still uses the real `CARGO_HOME`:
//!
//! ```sh
//! cargo test -p platform_linux --no-run          # note the binary path it prints
//! BIN=target/debug/deps/platform_linux-<hash>
//! TMPHOME="$(mktemp -d)"
//! env -i PATH="$PATH" HOME="$TMPHOME" XDG_DATA_HOME="$TMPHOME/.local/share" \
//!   dbus-run-session -- sh -c '
//!     (echo -n testpw | gnome-keyring-daemon --foreground --components=secrets --unlock &)
//!     sleep 3
//!     COMPME_KEYRING_EXPECT=present "$BIN" --ignored --test-threads=1 keyring
//!     dbus-send --session --print-reply --dest=org.freedesktop.secrets \
//!       /org/freedesktop/secrets org.freedesktop.Secret.Service.Lock \
//!       array:objpath:/org/freedesktop/secrets/collection/login
//!     COMPME_KEYRING_EXPECT=locked "$BIN" --ignored --test-threads=1 keyring'
//! ```

use std::sync::Arc;

use super::*;
use crate::memory_key::{MemoryKeyStore, KEY_LEN};

/// What the runner set up. Panics with the commands above rather than guessing.
fn declared_expectation() -> String {
    let value = std::env::var("COMPME_KEYRING_EXPECT").unwrap_or_default();
    assert!(
        matches!(value.as_str(), "absent" | "present" | "locked"),
        "set COMPME_KEYRING_EXPECT=absent|present|locked to declare which session this run \
         provides (see this module's docs); got {value:?}"
    );
    value
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn keyring_behaves_as_the_runner_declared() {
    let connection = Connection::session().expect("the runner must provide a session bus");
    let expectation = declared_expectation();

    if expectation == "absent" {
        // The path that matters most on a headless host: no key store means an
        // error, never a fabricated key and never an on-disk plaintext fallback.
        let result = read_memory_key_on(&connection);
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("a bus with no keyring daemon must fail closed, got {result:?}");
        };
        assert!(
            reason.contains("secret-service"),
            "the error must name the subsystem: {reason}"
        );

        // The whole store, not just the transport: no key is returned, and no
        // key is generated-and-lost either.
        let store = MemoryKeyStore::secret_service();
        assert!(
            store.load_or_create_memory_key().is_err(),
            "load_or_create must fail closed without a Secret Service"
        );
        return;
    }

    if expectation == "locked" {
        // The item exists (the `present` run created it) but its collection is
        // locked and nothing can answer an unlock prompt. Both the read and the
        // whole store must refuse: creating a second key here would leave every
        // stored memory row undecryptable.
        let result = read_memory_key_on(&connection);
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("a locked keyring must fail closed, got {result:?}");
        };
        assert!(
            reason.contains("locked"),
            "the error must say the keyring is locked: {reason}"
        );
        assert!(
            MemoryKeyStore::secret_service()
                .load_or_create_memory_key()
                .is_err(),
            "load_or_create must not mint a second key while the keyring is locked"
        );
        return;
    }

    // present: a real keyring. First call creates, second reads back the same
    // bytes — which is the only proof that the key was actually persisted rather
    // than generated and forgotten.
    let store = MemoryKeyStore::secret_service();
    let created = store
        .load_or_create_memory_key()
        .expect("a running Secret Service must store the key");
    assert_eq!(created.len(), KEY_LEN);
    assert_ne!(created, [0u8; KEY_LEN], "the key must be CSPRNG bytes");

    let loaded = store
        .load_or_create_memory_key()
        .expect("the second call must read the stored key");
    assert_eq!(
        loaded, created,
        "a second load must return the SAME key: a changed key silently makes every stored \
         memory row undecryptable"
    );

    // The transport-level read must now see the item as unlocked (not merely
    // present-but-locked), and hand back exactly the stored bytes.
    let secret = read_memory_key_on(&connection)
        .expect("read must succeed against a live keyring")
        .expect("the item exists after the create above");
    assert_eq!(secret, created.to_vec());
    assert_eq!(secret.len(), KEY_LEN);

    // And the store's fail-closed length check is wired to this transport: a
    // foreign 16-byte secret is refused rather than overwritten.
    let refuses_short = MemoryKeyStore::with_seams(
        Arc::new(|| Ok(Some(vec![0u8; 16]))),
        Arc::new(|_| panic!("must not write over a foreign secret")),
        Arc::new(crate::memory_key::random_key),
    );
    assert!(refuses_short.load_or_create_memory_key().is_err());
}
