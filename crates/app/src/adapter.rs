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

/// A cheaply-cloneable handle to a single shared `MacosPlatformAdapter`.
#[derive(Clone)]
pub struct SharedAdapter(Arc<MacosPlatformAdapter>);

impl SharedAdapter {
    pub fn new(inner: Arc<MacosPlatformAdapter>) -> Self {
        Self(inner)
    }
}

impl PlatformAdapter for SharedAdapter {
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
