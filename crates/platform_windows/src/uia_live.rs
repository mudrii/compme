//! UIA read path — the Windows-only COM half of the read-only slice
//! (audit plan item 8): `GetFocusedElement` + `TextPattern` feeding
//! `capabilities`/`read_context`. This module is `cfg(windows)`; the pure
//! halves it delegates to (`uia_ids`, `uia_caps`, `uia_text`) are compiled and
//! tested on every host.
//!
//! Threading is the recorded decision of record (`Qfd.md` §23): one dedicated
//! worker thread, `CoInitializeEx(COINIT_MULTITHREADED)`, owning no windows
//! and never pumping a message loop. Every UIA COM object is created on that
//! thread; trait calls cross by bounded request/reply — 10 s deadline, mapping
//! to [`PlatformError::Timeout`] per the contract's no-unbounded-block rule.
//! A COM error that means "nothing has focus right now"
//! (`UIA_E_ELEMENTNOTAVAILABLE`) degrades to "no focused element", never an
//! engine-visible error: focus transitions are normal, not failures.
//!
//! Scope honesty: this slice *reads*. No insert path, keyboard hook, overlay,
//! or event subscription exists yet, so `capabilities` reports the blocked
//! write/intercept floors (see `uia_caps`) and every subscribe method still
//! fails closed in `lib.rs`.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use windows::core::{IUnknown, Interface};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    SAFEARRAY,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
    IUIAutomationTextRange, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    UIA_TextPatternId, UIA_E_ELEMENTNOTAVAILABLE,
};

use crate::uia_caps::UiaFieldFacts;
use crate::uia_ids::encode_element_id;
use platform::PlatformError;

/// Deadline for one UIA round trip. UIA calls are cross-process COM against
/// the focused app's UI thread — slow by design (tens of ms typical, seconds
/// on a busy provider), so the bound exists to keep the run loop contract
/// ("never block unboundedly") rather than to preempt a healthy call.
pub(crate) const UIA_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Same bound for worker startup (COM init + `CUIAutomation` coclass).
const UIA_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum UiaRequest {
    /// The focused element's identity, or `None` when nothing has focus.
    FocusedIdentity {
        reply: Sender<Result<Option<(u32, String)>, PlatformError>>,
    },
    /// Facts about the focused element, refusing unless it still is the field.
    FieldFacts {
        element_id: String,
        reply: Sender<Result<UiaFieldFacts, PlatformError>>,
    },
    /// The focused field's document text plus its selection as UTF-16
    /// endpoints into that text, refusing unless it still is the field.
    ReadDocument {
        element_id: String,
        reply: Sender<Result<(String, i64, i64), PlatformError>>,
    },
}

/// Handle to the live UIA worker. `Drop` lets the worker exit (the request
/// channel closes; the thread drains its COM apartment with
/// `CoUninitialize`), so no join is needed on the run-loop thread.
pub(crate) struct UiaWorker {
    tx: Sender<UiaRequest>,
}

impl std::fmt::Debug for UiaWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiaWorker").finish_non_exhaustive()
    }
}

impl UiaWorker {
    /// Spawn the worker and wait (bounded) for COM init + the
    /// `CUIAutomation` factory. A host where either fails is reported now,
    /// not on first use — startup cost belongs at startup.
    pub fn spawn() -> Result<Self, PlatformError> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("compme-uia".into())
            .spawn(move || run_worker(rx, ready_tx))
            .map_err(|err| PlatformError::CannotComplete {
                reason: format!("failed to spawn UIA worker thread: {err}"),
            })?;
        match ready_rx.recv_timeout(UIA_STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self { tx }),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(PlatformError::Timeout),
        }
    }

    fn round_trip<T>(
        &self,
        build: impl FnOnce(Sender<Result<T, PlatformError>>) -> UiaRequest,
    ) -> Result<T, PlatformError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(build(reply_tx))
            .map_err(|_| PlatformError::CannotComplete {
                reason: "UIA worker is gone (channel closed)".into(),
            })?;
        reply_rx.recv_timeout(UIA_CALL_TIMEOUT)?
    }

    pub fn focused_identity(&self) -> Result<Option<(u32, String)>, PlatformError> {
        self.round_trip(|reply| UiaRequest::FocusedIdentity { reply })
    }

    pub fn field_facts(&self, element_id: &str) -> Result<UiaFieldFacts, PlatformError> {
        let element_id = element_id.to_string();
        self.round_trip(|reply| UiaRequest::FieldFacts { element_id, reply })
    }

    pub fn read_document(&self, element_id: &str) -> Result<(String, i64, i64), PlatformError> {
        let element_id = element_id.to_string();
        self.round_trip(|reply| UiaRequest::ReadDocument { element_id, reply })
    }
}

fn run_worker(rx: Receiver<UiaRequest>, ready: Sender<Result<(), PlatformError>>) {
    // MTA per the decision of record (Qfd §23): no message loop is serviced
    // on this thread, which only an MTA apartment permits.
    // SAFETY: `CoInitializeEx` with no reserved pointer; called once on this
    // dedicated thread. A failure (e.g. the process MTA is already running
    // this thread in another apartment) is reported at startup, and every
    // successful init — `S_OK` or `S_FALSE` alike — is balanced by exactly
    // one `CoUninitialize` on the same thread before it exits.
    let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if init.is_err() {
        let _ = ready.send(Err(PlatformError::CannotComplete {
            reason: format!("CoInitializeEx(MTA) failed: {init:?}"),
        }));
        return;
    }
    // SAFETY: standard coclass creation of the UIA client object, in-process;
    // the returned interface lives on this MTA thread for the worker's life.
    let factory: Result<IUIAutomation> =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) };
    match factory {
        Ok(factory) => {
            let _ = ready.send(Ok(()));
            // recv() returns Err only when the adapter (and every caller) is
            // gone — the orderly shutdown path.
            while let Ok(request) = rx.recv() {
                handle_request(&factory, request);
            }
            // SAFETY: balances the successful `CoInitializeEx` above, on the
            // same thread, after the last COM use.
            unsafe { CoUninitialize() };
        }
        Err(err) => {
            let _ = ready.send(Err(PlatformError::CannotComplete {
                reason: format!("CoCreateInstance(CUIAutomation) failed: {err}"),
            }));
        }
    }
}

fn handle_request(factory: &IUIAutomation, request: UiaRequest) {
    match request {
        UiaRequest::FocusedIdentity { reply } => {
            let _ = reply.send(focused_identity(factory));
        }
        UiaRequest::FieldFacts { element_id, reply } => {
            let _ = reply.send(field_facts(factory, &element_id));
        }
        UiaRequest::ReadDocument { element_id, reply } => {
            let _ = reply.send(read_document(factory, &element_id));
        }
    }
}

/// One COM call failure. The API name is in the reason so a live failure
/// names the exact call instead of a bare HRESULT.
fn com_error(api: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("UIA {api} failed: {err}"),
    }
}

/// The focused element, or `None` when focus is mid-transition
/// (`UIA_E_ELEMENTNOTAVAILABLE`). Any other failure is an error.
fn focused_element(factory: &IUIAutomation) -> Result<Option<IUIAutomationElement>, PlatformError> {
    // SAFETY: interface call on the factory created on this same thread.
    match unsafe { factory.GetFocusedElement() } {
        Ok(element) => Ok(Some(element)),
        Err(ref err) if err.code() == UIA_E_ELEMENTNOTAVAILABLE => Ok(None),
        Err(err) => Err(com_error("GetFocusedElement", err)),
    }
}

/// The focused element *if it still is* `element_id` — the stale-field guard.
/// "Nothing has focus" is stale too: identity cannot match a ghost.
fn focused_element_matching(
    factory: &IUIAutomation,
    element_id: &str,
) -> Result<IUIAutomationElement, PlatformError> {
    let Some(element) = focused_element(factory)? else {
        return Err(PlatformError::StaleField);
    };
    let pid = focused_pid(&element)?;
    let runtime_id = runtime_id(&element)?;
    if encode_element_id(pid, &runtime_id) != element_id {
        return Err(PlatformError::StaleField);
    }
    Ok(element)
}

fn focused_pid(element: &IUIAutomationElement) -> Result<u32, PlatformError> {
    // SAFETY: interface call on an element read on this same thread.
    unsafe { element.CurrentProcessId() }.map_err(|err| com_error("CurrentProcessId", err))
}

fn runtime_id(element: &IUIAutomationElement) -> Result<Vec<i32>, PlatformError> {
    // SAFETY: interface call on an element read on this same thread.
    let safearray =
        unsafe { element.GetRuntimeId() }.map_err(|err| com_error("GetRuntimeId", err))?;
    runtime_id_from_safearray(&safearray)
}

fn runtime_id_from_safearray(safearray: &SAFEARRAY) -> Result<Vec<i32>, PlatformError> {
    // SAFETY: the array is UIA's runtime-id array, documented as VT_I4; the
    // borrow ends before any other call touches it.
    let components =
        unsafe { safearray.as_1d::<i32>() }.map_err(|err| com_error("GetRuntimeId(as_1d)", err))?;
    Ok(components.to_vec())
}

fn bstr_to_string(value: windows::core::BSTR) -> String {
    String::from_utf16_lossy(value.as_wide())
}

fn focused_identity(factory: &IUIAutomation) -> Result<Option<(u32, String)>, PlatformError> {
    let Some(element) = focused_element(factory)? else {
        return Ok(None);
    };
    let pid = focused_pid(&element)?;
    let runtime_id = runtime_id(&element)?;
    Ok(Some((pid, encode_element_id(pid, &runtime_id))))
}

fn field_facts(factory: &IUIAutomation, element_id: &str) -> Result<UiaFieldFacts, PlatformError> {
    let element = focused_element_matching(factory, element_id)?;
    // SAFETY: property reads on the element read on this same thread.
    let (has_text_pattern, has_value_pattern, is_password, framework) = unsafe {
        (
            element.CurrentIsTextPatternAvailable(),
            element.CurrentIsValuePatternAvailable(),
            element.CurrentIsPassword(),
            element.CurrentFrameworkId(),
        )
    };
    Ok(UiaFieldFacts {
        has_text_pattern: has_text_pattern
            .map_err(|err| com_error("CurrentIsTextPatternAvailable", err))?
            .as_bool(),
        has_value_pattern: has_value_pattern
            .map_err(|err| com_error("CurrentIsValuePatternAvailable", err))?
            .as_bool(),
        is_password: is_password
            .map_err(|err| com_error("CurrentIsPassword", err))?
            .as_bool(),
        framework: bstr_to_string(framework.map_err(|err| com_error("CurrentFrameworkId", err))?),
    })
}

fn read_document(
    factory: &IUIAutomation,
    element_id: &str,
) -> Result<(String, i64, i64), PlatformError> {
    let element = focused_element_matching(factory, element_id)?;
    // SAFETY: property read on the element read on this same thread.
    if !unsafe { element.CurrentIsTextPatternAvailable() }
        .map_err(|err| com_error("CurrentIsTextPatternAvailable", err))?
        .as_bool()
    {
        return Err(PlatformError::UnsupportedField {
            reason: "focused element exposes no UIA TextPattern".into(),
        });
    }
    // SAFETY: pattern fetch and range reads on the element read on this same
    // thread; every interface created here stays on the worker.
    unsafe {
        let pattern: IUIAutomationTextPattern = element
            .GetCurrentPatternAs(UIA_TextPatternId)
            .map_err(|err| com_error("GetCurrentPatternAs(Text)", err))?;
        let document = pattern
            .DocumentRange()
            .map_err(|err| com_error("DocumentRange", err))?;
        let text = bstr_to_string(
            document
                .GetText(-1)
                .map_err(|err| com_error("GetText", err))?,
        );
        let selection = pattern
            .GetSelection()
            .map_err(|err| com_error("GetSelection", err))?;
        let ranges = selection
            .as_1d::<IUnknown>()
            .map_err(|err| com_error("GetSelection(as_1d)", err))?;
        let Some(first) = ranges.first() else {
            // A text field with no selection range: treat as collapsed at 0.
            return Ok((text, 0, 0));
        };
        let range = IUIAutomationTextRange::from_unknown(first);
        let start = range
            .CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &document,
                TextPatternRangeEndpoint_Start,
            )
            .map_err(|err| com_error("CompareEndpoints(Start)", err))?;
        let end = range
            .CompareEndpoints(
                TextPatternRangeEndpoint_End,
                &document,
                TextPatternRangeEndpoint_Start,
            )
            .map_err(|err| com_error("CompareEndpoints(End)", err))?;
        Ok((text, start as i64, end as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WindowsAdapter;
    use platform::FieldHandle;

    fn bogus_field() -> FieldHandle {
        FieldHandle {
            app: "ci".into(),
            pid: Some(u32::MAX),
            element_id: "uia:pid=4294967295:rid=1.2.3".into(),
            generation: 0,
        }
    }

    #[test]
    fn worker_spawns_and_refuses_a_field_that_cannot_be_focused() {
        let adapter =
            WindowsAdapter::with_uia().expect("UIA factory must build on a Windows runner");
        // Identity can never match this synthetic id, whatever has focus on
        // the runner — a deterministic read-only round trip through COM.
        assert!(matches!(
            adapter.capabilities(&bogus_field()),
            Err(PlatformError::StaleField)
        ));
        assert!(matches!(
            adapter.read_context(&bogus_field()),
            Err(PlatformError::StaleField)
        ));
    }

    #[test]
    fn focused_identity_round_trips_through_the_worker() {
        let adapter =
            WindowsAdapter::with_uia().expect("UIA factory must build on a Windows runner");
        // Whatever is focused on the runner, the worker must answer — Ok(None)
        // is legal (focus mid-transition), a pid+id pair is the normal case.
        let worker = adapter.uia.as_ref().expect("with_uia built the worker");
        if let Some((pid, id)) = worker
            .focused_identity()
            .expect("focused identity round trip must not error")
        {
            assert_eq!(crate::uia_ids::decode_element_id(&id).unwrap().0, pid);
        }
    }

    /// The plan-item-8 hardware smoke: a human focuses a text field (Notepad
    /// or any editor) on a real desktop, then runs with `--ignored`. Asserts
    /// the read path end-to-end — identity, facts, and a document read whose
    /// selection fits the text it reports.
    #[test]
    #[ignore = "needs an interactive Windows desktop with a text field focused (plan-item-8 hardware pass)"]
    fn focused_text_field_reads_document_and_selection() {
        let adapter = WindowsAdapter::with_uia().expect("UIA available on a desktop session");
        let worker = adapter.uia.as_ref().expect("with_uia built the worker");
        let (pid, element_id) = worker
            .focused_identity()
            .expect("identity round trip")
            .expect("a text field must be focused for this smoke");
        let field = FieldHandle {
            app: "manual".into(),
            pid: Some(pid),
            element_id,
            generation: 0,
        };
        let caps = adapter
            .capabilities(&field)
            .expect("facts for the focused field");
        eprintln!(
            "smoke: framework={:?} readable={} value_pattern_recorded_via_facts",
            caps.toolkit, caps.readable_text
        );
        let (doc, start, end) = adapter.read_context(&field).expect("document read");
        assert!(
            doc.encode_utf16().count() as i64 >= start.max(end),
            "selection endpoints must fit the document they came from"
        );
    }
}
