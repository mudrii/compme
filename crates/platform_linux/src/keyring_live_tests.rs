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
        // The path that matters most on a headless host: no usable key store means
        // an error, never a fabricated key and never an on-disk plaintext fallback.
        //
        // "Absent" is about the *outcome*, not one specific failure: D-Bus
        // activates `org.freedesktop.secrets` from a system-wide service file, so a
        // session with no keyring configured typically answers `OpenSession` and
        // then refuses at the locked default keyring. On the harness bus that is
        // exactly what happens, which is why the layer list below is a set.
        let result = read_memory_key_on(&connection);
        let Err(PlatformError::CannotComplete { reason }) = result else {
            panic!("a bus with no keyring daemon must fail closed, got {result:?}");
        };
        // Every error this module produces is prefixed "secret-service", so that
        // substring alone proves nothing about *which* failure happened. Require a
        // named layer instead.
        //
        // More than one layer is legitimate here, and which one you get is a
        // property of the bus, not of compme: D-Bus *activates*
        // `org.freedesktop.secrets` from a system-wide service file, so a session
        // that has no keyring configured usually answers `OpenSession` fine and
        // then fails on the default collection. "Absent" therefore means "no
        // usable key store", which is reachable at any of these layers — and all
        // of them must land on the same fail-closed outcome.
        const LAYERS: [&str; 5] = [
            "session",
            "OpenSession",
            "SearchItems",
            "locked",
            "default keyring",
        ];
        assert!(
            LAYERS.iter().any(|layer| reason.contains(layer)),
            "the error must identify the failing layer, not just the subsystem: {reason}"
        );

        // Stronger than any string match: whatever the layer, nothing was minted.
        // A load that failed *after* creating a key would leave a store the next
        // run cannot read, so assert the second attempt fails the same way rather
        // than suddenly succeeding with a key that appeared from nowhere.
        let second = read_memory_key_on(&connection);
        assert!(
            matches!(second, Err(PlatformError::CannotComplete { .. })),
            "a failed read must not have created anything: {second:?}"
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

    // A second *create* must be refused outright. `replace = false` only means
    // "do not replace": before this guard, CreateItem added a second item and the
    // search returned the newer one first, so the next load silently returned a
    // different key and every row encrypted under the first became unreadable.
    let duplicate = crate::keyring::write_memory_key_on(&connection, &[7u8; KEY_LEN]);
    let Err(PlatformError::CannotComplete { reason }) = duplicate else {
        panic!("writing over an existing key must be refused, got {duplicate:?}");
    };
    assert!(
        reason.contains("already exists"),
        "the refusal must say the item exists: {reason}"
    );
    // ...and the refusal must have changed nothing.
    assert_eq!(
        store
            .load_or_create_memory_key()
            .expect("the key must still load after a refused duplicate write"),
        created,
        "a refused duplicate write must leave the stored key untouched"
    );

    // And the store's fail-closed length check is wired to this transport: a
    // foreign 16-byte secret is refused rather than overwritten.
    let refuses_short = MemoryKeyStore::with_seams(
        Arc::new(|| Ok(Some(vec![0u8; 16]))),
        Arc::new(|_| panic!("must not write over a foreign secret")),
        Arc::new(crate::memory_key::random_key),
    );
    assert!(refuses_short.load_or_create_memory_key().is_err());
}
