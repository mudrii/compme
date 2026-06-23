//! `SharedAdapter` lets the run-loop and the `Engine` share one
//! `MacosPlatformAdapter`.
//!
//! `Engine::new` takes the adapter *by value*, but the binary also needs the
//! adapter to install focus/caret/accept subscriptions and to `read_context`
//! inside the caret handler. `MacosPlatformAdapter` is not `Clone`, so we share
//! it behind an `Arc`. The orphan rule forbids `impl PlatformAdapter for
//! Arc<MacosPlatformAdapter>` directly (both are foreign), so this local newtype
//! carries the impl and forwards every method to the inner adapter.

use std::sync::Arc;

use platform::{
    AcceptCallback, AcceptSubscription, AppId, Capabilities, CaretCallback, Environment,
    FieldHandle, FocusCallback, InsertStrategy, Inserted, PlatformAdapter, PlatformError,
    ScreenRect, Subscription, TextContext,
};
use platform_macos::MacosPlatformAdapter;

/// A cheaply-cloneable handle to a single shared adapter. Generic over the inner
/// adapter (defaulting to `MacosPlatformAdapter`, the only production inner) so
/// the forwarding impl can be exercised against a fake in unit tests — the bare
/// `SharedAdapter` name and `SharedAdapter::new(arc)` call site are unaffected by
/// the default type parameter.
#[derive(Clone)]
pub struct SharedAdapter<A: PlatformAdapter = MacosPlatformAdapter>(Arc<A>);

impl<A: PlatformAdapter> SharedAdapter<A> {
    pub fn new(inner: Arc<A>) -> Self {
        Self(inner)
    }
}

impl<A: PlatformAdapter> PlatformAdapter for SharedAdapter<A> {
    fn environment(&self) -> Environment {
        self.0.environment()
    }
    fn subscribe_focus(&self, cb: FocusCallback) -> Result<Subscription, PlatformError> {
        self.0.subscribe_focus(cb)
    }
    fn subscribe_caret(&self, cb: CaretCallback) -> Result<Subscription, PlatformError> {
        self.0.subscribe_caret(cb)
    }
    fn subscribe_accept(&self, cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        self.0.subscribe_accept(cb)
    }
    fn front_app(&self) -> Option<AppId> {
        self.0.front_app()
    }
    fn capabilities(&self, field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        self.0.capabilities(field)
    }
    fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError> {
        self.0.read_context(field)
    }
    fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        self.0.caret_rect(field)
    }
    fn popup_anchor(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        self.0.popup_anchor(field)
    }
    fn focused_page_url(&self, field: &FieldHandle) -> Result<Option<String>, PlatformError> {
        // Explicit forward — inheriting the trait's Ok(None) default here
        // would silently disable domain detection through the wrapper (the
        // same forwarding-wrapper trap as insert_replacing below, c42).
        self.0.focused_page_url(field)
    }
    fn insert(
        &self,
        field: &FieldHandle,
        text: &str,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        self.0.insert(field, text, strategy)
    }
    fn insert_replacing(
        &self,
        field: &FieldHandle,
        text: &str,
        replace_left: usize,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        // Forward to the macOS adapter's range-replacing insert. This forward
        // was MISSING while the trait method had an append-only default — the
        // live step-6 symptom was `:smile😄` (typed token never deleted). The
        // trait method is now required precisely so this wrapper can never
        // silently downgrade a replacement again.
        self.0.insert_replacing(field, text, replace_left, strategy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::OperatingSystem;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal inner adapter whose two trap-prone overrides are observable:
    /// `focused_page_url` returns a real URL (NOT the trait's `Ok(None)`
    /// default) and `insert_replacing` records the `replace_left` it received.
    /// Every other method is irrelevant to the forwarding contract under test.
    #[derive(Default)]
    struct RecordingInner {
        last_replace_left: AtomicUsize,
    }

    impl PlatformAdapter for RecordingInner {
        fn environment(&self) -> Environment {
            Environment {
                os: OperatingSystem::Macos,
                version: String::new(),
            }
        }
        fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn subscribe_accept(
            &self,
            _cb: AcceptCallback,
        ) -> Result<AcceptSubscription, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn front_app(&self) -> Option<AppId> {
            None
        }
        fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn focused_page_url(&self, _field: &FieldHandle) -> Result<Option<String>, PlatformError> {
            Ok(Some("https://bank.example/login".into()))
        }
        fn insert(
            &self,
            _field: &FieldHandle,
            _text: &str,
            _strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            Err(PlatformError::StaleField)
        }
        fn insert_replacing(
            &self,
            _field: &FieldHandle,
            text: &str,
            replace_left: usize,
            strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            self.last_replace_left.store(replace_left, Ordering::SeqCst);
            Ok(Inserted {
                bytes: text.len(),
                chars: text.chars().count(),
                strategy,
            })
        }
    }

    fn field() -> FieldHandle {
        FieldHandle {
            app: "com.apple.Safari".into(),
            pid: Some(7),
            element_id: "f".into(),
            generation: 1,
        }
    }

    #[test]
    fn shared_adapter_forwards_focused_page_url_instead_of_inheriting_default() {
        // The c42 trap: if this override is deleted, the wrapper inherits the
        // trait's Ok(None) and domain detection silently dies. Pin that the
        // wrapper returns the INNER url, never None.
        let shared = SharedAdapter::new(Arc::new(RecordingInner::default()));
        assert_eq!(
            shared.focused_page_url(&field()).unwrap(),
            Some("https://bank.example/login".into()),
            "SharedAdapter must forward focused_page_url, not return the default None"
        );
    }

    #[test]
    fn shared_adapter_forwards_replace_left_through_insert_replacing() {
        // The original c42 symptom (`:smile😄`): a wrapper that downgraded
        // insert_replacing to an append-only insert dropped replace_left. Pin
        // that the inner adapter receives the exact replace_left.
        let inner = Arc::new(RecordingInner::default());
        let shared = SharedAdapter::new(Arc::clone(&inner));
        shared
            .insert_replacing(&field(), "😄", 5, InsertStrategy::AxSet)
            .expect("forwarded insert_replacing");
        assert_eq!(
            inner.last_replace_left.load(Ordering::SeqCst),
            5,
            "the inner adapter must receive the exact replace_left, not a dropped 0"
        );
    }
}
