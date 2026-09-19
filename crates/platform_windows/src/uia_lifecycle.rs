//! Small host-portable lifecycle seam for the Windows UIA COM apartment.
//! Keeping the body in an inner scope guarantees every interface it owns is
//! dropped before the cleanup callback runs, including error paths.

pub(crate) fn with_apartment<E>(
    initialize: impl FnOnce() -> Result<(), E>,
    body: impl FnOnce() -> Result<(), E>,
    cleanup: impl FnOnce(),
) -> Result<(), E> {
    initialize()?;
    let guard = ApartmentGuard(Some(cleanup));
    let result = body();
    drop(guard);
    result
}

struct ApartmentGuard<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for ApartmentGuard<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeFactory<'a>(&'a RefCell<Vec<&'static str>>);

    impl Drop for FakeFactory<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().push("factory drop");
        }
    }

    #[test]
    fn interfaces_drop_before_apartment_cleanup() {
        let events = RefCell::new(Vec::new());
        let result: Result<(), ()> = with_apartment(
            || {
                events.borrow_mut().push("initialize");
                Ok(())
            },
            || {
                let _factory = FakeFactory(&events);
                events.borrow_mut().push("factory use");
                Ok(())
            },
            || events.borrow_mut().push("uninitialize"),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            *events.borrow(),
            ["initialize", "factory use", "factory drop", "uninitialize"]
        );
    }

    #[test]
    fn factory_creation_failure_still_balances_successful_initialization() {
        let events = RefCell::new(Vec::new());
        let result: Result<(), &str> = with_apartment(
            || {
                events.borrow_mut().push("initialize");
                Ok(())
            },
            || {
                events.borrow_mut().push("factory failure");
                Err("create failed")
            },
            || events.borrow_mut().push("uninitialize"),
        );
        assert_eq!(result, Err("create failed"));
        assert_eq!(
            *events.borrow(),
            ["initialize", "factory failure", "uninitialize"]
        );
    }

    #[test]
    fn failed_initialization_does_not_uninitialize() {
        let events = RefCell::new(Vec::new());
        let result: Result<(), &str> = with_apartment(
            || {
                events.borrow_mut().push("initialize failure");
                Err("init failed")
            },
            || Ok(()),
            || events.borrow_mut().push("uninitialize"),
        );
        assert_eq!(result, Err("init failed"));
        assert_eq!(*events.borrow(), ["initialize failure"]);
    }
}
