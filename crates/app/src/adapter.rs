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
    fn insert(
        &self,
        field: &FieldHandle,
        text: &str,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        self.0.insert(field, text, strategy)
    }
}
