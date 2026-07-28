//! Secret Service (`org.freedesktop.secrets`) transport for the memory-store key
//! (ROADMAP Phase 2.6) — Linux only.
//!
//! **Why D-Bus and not libsecret.** Linking the C library would make the compme
//! binary refuse to *start* on a host without it, a hard failure where this
//! project requires fail-closed degradation — the same reasoning as the AT-SPI
//! path (see [`crate::atspi_live`]). Speaking the D-Bus API directly means a
//! headless server, a session with no keyring daemon, or a locked keyring is
//! something we *report*, not something that breaks the executable.
//!
//! **Why the `plain` session algorithm.** The Secret Service spec allows a
//! DH-negotiated transport, but the bus is a per-user UNIX socket authenticated by
//! peer credentials: an attacker who can read it can also read this process's
//! memory. Negotiating DH would add a crypto dependency and more code to get
//! wrong, protecting the secret only from the `dbus-daemon` we already trust with
//! every other call.
//!
//! **What is deliberately NOT done: driving unlock prompts.** `Unlock` may return
//! a `Prompt` object that must be shown and then awaited via its `Completed`
//! signal. Awaiting a signal with no timeout inside a synchronous startup path is
//! how an app hangs forever with no window, so a keyring that needs an interactive
//! unlock is reported instead. The user unlocks their keyring the way their
//! desktop already does, and the next launch finds it open.
//!
//! **Never create while a match is merely locked.** [`crate::memory_key`]'s
//! `classify_lookup` keeps that decision pure and tested; here it means a locked
//! item, or a locked default collection, produces an error rather than a second
//! key that would silently re-key the user's memory store.

use std::collections::HashMap;

use platform::PlatformError;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::memory_key::{classify_lookup, KeyLookup, KEY_ATTRIBUTES, MEMORY_KEY_LABEL};

const SECRETS_NAME: &str = "org.freedesktop.secrets";
const SECRETS_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
/// The session's default keyring. An alias path rather than a collection path, so
/// it follows whatever the user configured.
const DEFAULT_COLLECTION_PATH: &str = "/org/freedesktop/secrets/aliases/default";
/// `Prompt`/`Session` object paths use `/` to mean "none required".
const NO_OBJECT: &str = "/";
/// Item content type. The key is 32 raw bytes, not text.
const CONTENT_TYPE: &str = "application/octet-stream";

/// A `(oayays)` Secret: session, parameters, value, content type.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

fn failed(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("secret-service {what}: {err}"),
    }
}

fn refused(reason: String) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("secret-service {reason}"),
    }
}

/// Read the stored key. `Ok(None)` is genuine first use — every other outcome,
/// including "the keyring is locked", is an error, so the caller never mints a
/// second key.
pub fn read_memory_key() -> Result<Option<Vec<u8>>, PlatformError> {
    let connection = session_bus()?;
    read_memory_key_on(&connection)
}

/// Persist `key`. Creating is refused when an item already exists, so a
/// concurrently written key can never be destroyed by this path.
pub fn write_memory_key(key: &[u8]) -> Result<(), PlatformError> {
    let connection = session_bus()?;
    write_memory_key_on(&connection, key)
}

/// The session bus. Absent on a headless host, which is a supported
/// configuration and therefore reported rather than retried.
fn session_bus() -> Result<Connection, PlatformError> {
    Connection::session().map_err(|err| failed("session bus", err))
}

/// The read path, against an explicit connection so the live tests can point it
/// at the throwaway session bus the harness stands up.
pub fn read_memory_key_on(connection: &Connection) -> Result<Option<Vec<u8>>, PlatformError> {
    let session = open_session(connection)?;
    let (unlocked, locked) = search_items(connection)?;
    match classify_lookup(unlocked, locked) {
        KeyLookup::Unlocked(item) => Ok(Some(item_secret(connection, &item, &session)?)),
        KeyLookup::LockedOnly(items) => {
            // Attempt the unlock; only a prompt-free success is usable.
            let (unlocked, prompt) = unlock(connection, &items)?;
            match unlocked.first() {
                Some(item) => Ok(Some(item_secret(connection, item, &session)?)),
                None => Err(refused(format!(
                    "the keyring holding compme's memory key is locked and needs an interactive \
                     unlock (prompt {prompt}); unlock your keyring and restart compme — refusing \
                     to create a second key, which would make the existing memory store \
                     unreadable"
                ))),
            }
        }
        KeyLookup::Absent => {
            // No item anywhere. Before calling that first use, make sure the
            // collection we would write into is actually open: a locked
            // collection can hide its items from a search, and creating a key
            // then would re-key an existing store.
            require_unlocked_default_collection(connection)?;
            Ok(None)
        }
    }
}

/// The create path. See [`write_memory_key`].
pub fn write_memory_key_on(connection: &Connection, key: &[u8]) -> Result<(), PlatformError> {
    let session = open_session(connection)?;
    let attributes: HashMap<&str, &str> = KEY_ATTRIBUTES.iter().copied().collect();
    let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
    properties.insert(
        "org.freedesktop.Secret.Item.Label",
        Value::from(MEMORY_KEY_LABEL),
    );
    properties.insert(
        "org.freedesktop.Secret.Item.Attributes",
        Value::from(attributes),
    );
    let secret = (
        session.clone(),
        Vec::<u8>::new(),
        key.to_vec(),
        CONTENT_TYPE.to_string(),
    );

    // `replace = false`: this path runs only after a read reported no item, so
    // replacing could only ever destroy a key another process just created.
    let reply = connection
        .call_method(
            Some(SECRETS_NAME),
            DEFAULT_COLLECTION_PATH,
            Some(COLLECTION_IFACE),
            "CreateItem",
            &(properties, secret, false),
        )
        .map_err(|err| failed("CreateItem", err))?;
    let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|err| failed("CreateItem reply", err))?;
    if item.as_str() == NO_OBJECT {
        return Err(refused(format!(
            "CreateItem returned no item (prompt {prompt}): the default keyring is locked or \
             refused the write, so the generated key was NOT stored"
        )));
    }
    Ok(())
}

/// `OpenSession("plain", "")` → the session object every secret transfer is
/// scoped to.
fn open_session(connection: &Connection) -> Result<OwnedObjectPath, PlatformError> {
    let reply = connection
        .call_method(
            Some(SECRETS_NAME),
            SECRETS_PATH,
            Some(SERVICE_IFACE),
            "OpenSession",
            &("plain", Value::from("")),
        )
        .map_err(|err| failed("OpenSession (is a keyring daemon running?)", err))?;
    let (_output, session): (OwnedValue, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|err| failed("OpenSession reply", err))?;
    Ok(session)
}

/// `SearchItems(attributes)` → `(unlocked, locked)`. Matching is by attribute
/// subset, so extra attributes on the stored item do not prevent a match.
fn search_items(
    connection: &Connection,
) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), PlatformError> {
    let attributes: HashMap<&str, &str> = KEY_ATTRIBUTES.iter().copied().collect();
    let reply = connection
        .call_method(
            Some(SECRETS_NAME),
            SECRETS_PATH,
            Some(SERVICE_IFACE),
            "SearchItems",
            &(attributes,),
        )
        .map_err(|err| failed("SearchItems", err))?;
    reply
        .body()
        .deserialize()
        .map_err(|err| failed("SearchItems reply", err))
}

/// `Item.GetSecret(session)` → the raw value. The length check lives in
/// [`crate::memory_key`], which also scrubs this buffer.
fn item_secret(
    connection: &Connection,
    item: &OwnedObjectPath,
    session: &OwnedObjectPath,
) -> Result<Vec<u8>, PlatformError> {
    let reply = connection
        .call_method(
            Some(SECRETS_NAME),
            item.as_str(),
            Some(ITEM_IFACE),
            "GetSecret",
            &(session.clone(),),
        )
        .map_err(|err| failed("GetSecret", err))?;
    let (_session, _parameters, value, _content_type): Secret = reply
        .body()
        .deserialize()
        .map_err(|err| failed("GetSecret reply", err))?;
    Ok(value)
}

/// `Service.Unlock(objects)` → `(unlocked, prompt)`. A returned prompt is
/// reported, not driven (see the module docs).
fn unlock(
    connection: &Connection,
    objects: &[OwnedObjectPath],
) -> Result<(Vec<OwnedObjectPath>, OwnedObjectPath), PlatformError> {
    let reply = connection
        .call_method(
            Some(SECRETS_NAME),
            SECRETS_PATH,
            Some(SERVICE_IFACE),
            "Unlock",
            &(objects.to_vec(),),
        )
        .map_err(|err| failed("Unlock", err))?;
    reply
        .body()
        .deserialize()
        .map_err(|err| failed("Unlock reply", err))
}

/// Refuse to treat "no item found" as first use while the default collection is
/// locked: a locked keyring can answer a search with nothing, and creating a key
/// then would leave the real one behind and the existing memory store
/// undecryptable. One unlock attempt is made first, since an auto-unlockable
/// collection needs no prompt.
fn require_unlocked_default_collection(connection: &Connection) -> Result<(), PlatformError> {
    if !collection_locked(connection)? {
        return Ok(());
    }
    let collection = OwnedObjectPath::try_from(DEFAULT_COLLECTION_PATH)
        .map_err(|err| failed("default collection path", err))?;
    let (unlocked, prompt) = unlock(connection, &[collection])?;
    if !unlocked.is_empty() && !collection_locked(connection)? {
        return Ok(());
    }
    Err(refused(format!(
        "the default keyring is locked and needs an interactive unlock (prompt {prompt}); compme \
         will not create a memory key in a locked keyring, because a key it cannot read back is \
         worse than none"
    )))
}

/// The default collection's `Locked` property. A missing default collection is
/// itself a refusal: there is nowhere to store the key.
fn collection_locked(connection: &Connection) -> Result<bool, PlatformError> {
    let proxy = zbus::blocking::Proxy::new(
        connection,
        SECRETS_NAME,
        DEFAULT_COLLECTION_PATH,
        COLLECTION_IFACE,
    )
    .map_err(|err| failed("default collection proxy", err))?;
    proxy.get_property::<bool>("Locked").map_err(|err| {
        failed(
            "default collection Locked (is a default keyring configured?)",
            err,
        )
    })
}

/// Live Secret Service tests. In a sibling file (a `#[path]` module) rather than
/// inline, matching how `run_loop` and `platform_macos` keep their tests — see the
/// repo brief's "Where tests live".
#[cfg(test)]
#[path = "keyring_live_tests.rs"]
mod live_tests;
