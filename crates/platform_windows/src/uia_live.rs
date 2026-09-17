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

use std::os::raw::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use windows::core::Interface;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    SAFEARRAY,
};
use windows::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetElement};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, UIA_TextPatternId,
    UIA_ValuePatternId, UIA_E_ELEMENTNOTAVAILABLE,
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
    /// Test-only until the event slice lands (which will carry identity on
    /// the events themselves); the hardware smoke exercises it.
    #[cfg(test)]
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
        // A dead reply channel is the worker-died case; a timeout is the
        // contract's bounded-wait. Both map to the portable error set.
        reply_rx
            .recv_timeout(UIA_CALL_TIMEOUT)
            .map_err(|_| PlatformError::Timeout)?
    }

    #[cfg(test)]
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

/// Owns a UIA-returned `*mut SAFEARRAY` and destroys it exactly once — the
/// generated UIA methods hand back a raw array pointer with no RAII, so every
/// early return would otherwise leak the provider's array.
struct OwnedSafeArray(*mut SAFEARRAY);

impl OwnedSafeArray {
    fn get(&self) -> &SAFEARRAY {
        // SAFETY: the pointer came from a successful UIA call and stays valid
        // until `SafeArrayDestroy`; `self` owns exactly that lifetime.
        unsafe { &*self.0 }
    }
}

impl Drop for OwnedSafeArray {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was produced by a successful UIA
            // safearray-returning call and destroyed exactly here.
            unsafe {
                let _ = SafeArrayDestroy(self.0);
            }
        }
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
    let factory: windows::core::Result<IUIAutomation> =
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
        #[cfg(test)]
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
        Err(ref err) if err.code() == windows::core::HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32) => {
            Ok(None)
        }
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
    let pid =
        unsafe { element.CurrentProcessId() }.map_err(|err| com_error("CurrentProcessId", err))?;
    // UIA reports the pid as a signed int; a negative value has no meaning as
    // a process id and would corrupt the element-id encoding, so refuse it.
    u32::try_from(pid).map_err(|_| PlatformError::CannotComplete {
        reason: format!("UIA CurrentProcessId returned a negative pid: {pid}"),
    })
}

fn runtime_id(element: &IUIAutomationElement) -> Result<Vec<i32>, PlatformError> {
    // SAFETY: interface call on an element read on this same thread. The
    // generated API hands back a raw `*mut SAFEARRAY` owned by the caller —
    // the guard below destroys it exactly once on every path.
    let array = OwnedSafeArray(
        unsafe { element.GetRuntimeId() }.map_err(|err| com_error("GetRuntimeId", err))?,
    );
    runtime_id_from_safearray(array.get())
}

/// UIA documents the runtime id as a one-dimensional `VT_I4` array. Read it
/// element-by-element through `SafeArrayGetElement`, which copies with the
/// array's own locking — no raw access-data lifetime to manage.
fn runtime_id_from_safearray(safearray: &SAFEARRAY) -> Result<Vec<i32>, PlatformError> {
    let psa = safearray as *const SAFEARRAY as *mut SAFEARRAY;
    let count = safearray.rgsabound[0].cElements;
    let mut components = Vec::with_capacity(count as usize);
    for index in 0..count as i32 {
        let mut value: i32 = 0;
        // SAFETY: `index` addresses the single dimension of a live array;
        // `value` is the out-slot for one VT_I4 element.
        unsafe { SafeArrayGetElement(psa, &index, &mut value as *mut i32 as *mut c_void) }
            .map_err(|err| com_error("GetRuntimeId(element)", err))?;
        components.push(value);
    }
    Ok(components)
}

fn bstr_to_string(value: windows::core::BSTR) -> String {
    // BSTR derefs to its UTF-16 code units.
    String::from_utf16_lossy(&value)
}

/// The focused element's identity, or `None` when focus is mid-transition
/// (`UIA_E_ELEMENTNOTAVAILABLE`). Any other failure is an error.
#[cfg(test)]
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
    // SAFETY: pattern probes on the element read on this same thread.
    // `GetCurrentPattern` answers S_OK with a null interface when the
    // pattern is unavailable, so availability is a null check — the
    // generated element type has no `CurrentIs*PatternAvailable` helpers.
    let text_pattern = unsafe { element.GetCurrentPattern(UIA_TextPatternId) }
        .map_err(|err| com_error("GetCurrentPattern(Text)", err))?;
    let value_pattern = unsafe { element.GetCurrentPattern(UIA_ValuePatternId) }
        .map_err(|err| com_error("GetCurrentPattern(Value)", err))?;
    Ok(UiaFieldFacts {
        has_text_pattern: !text_pattern.as_raw().is_null(),
        has_value_pattern: !value_pattern.as_raw().is_null(),
        is_password: unsafe { element.CurrentIsPassword() }
            .map_err(|err| com_error("CurrentIsPassword", err))?
            .as_bool(),
        framework: bstr_to_string(
            unsafe { element.CurrentFrameworkId() }
                .map_err(|err| com_error("CurrentFrameworkId", err))?,
        ),
    })
}

fn read_document(
    factory: &IUIAutomation,
    element_id: &str,
) -> Result<(String, i64, i64), PlatformError> {
    let element = focused_element_matching(factory, element_id)?;
    // SAFETY: pattern fetch and range reads on the element read on this same
    // thread; every interface created here stays on the worker. The pattern
    // comes back as a null interface when the element has no TextPattern —
    // the generated element type has no `CurrentIsTextPatternAvailable`.
    let pattern: IUIAutomationTextPattern = unsafe {
        element
            .GetCurrentPatternAs(UIA_TextPatternId)
            .map_err(|err| com_error("GetCurrentPatternAs(Text)", err))
    }?;
    if pattern.as_raw().is_null() {
        return Err(PlatformError::UnsupportedField {
            reason: "focused element exposes no UIA TextPattern".into(),
        });
    }
    let document =
        unsafe { pattern.DocumentRange() }.map_err(|err| com_error("DocumentRange", err))?;
    let text =
        bstr_to_string(unsafe { document.GetText(-1) }.map_err(|err| com_error("GetText", err))?);
    let selection =
        unsafe { pattern.GetSelection() }.map_err(|err| com_error("GetSelection", err))?;
    // SAFETY: interface calls on the selection array read on this same
    // thread; `Length` bounds the indexed `GetElement` below.
    let length =
        unsafe { selection.Length() }.map_err(|err| com_error("GetSelection(Length)", err))?;
    if length == 0 {
        // A text field with no selection range: treat as collapsed at 0.
        return Ok((text, 0, 0));
    }
    let range =
        unsafe { selection.GetElement(0) }.map_err(|err| com_error("GetSelection(0)", err))?;
    let start = unsafe {
        range.CompareEndpoints(
            TextPatternRangeEndpoint_Start,
            &document,
            TextPatternRangeEndpoint_Start,
        )
    }
    .map_err(|err| com_error("CompareEndpoints(Start)", err))?;
    let end = unsafe {
        range.CompareEndpoints(
            TextPatternRangeEndpoint_End,
            &document,
            TextPatternRangeEndpoint_Start,
        )
    }
    .map_err(|err| com_error("CompareEndpoints(End)", err))?;
    Ok((text, start as i64, end as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WindowsAdapter;
    use platform::{FieldHandle, PlatformAdapter};

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
        let ctx = adapter.read_context(&field).expect("document read");
        let total: usize = ctx.left.encode_utf16().count()
            + ctx
                .selected_text
                .as_deref()
                .map(|text| text.encode_utf16().count())
                .unwrap_or(0)
            + ctx.right.encode_utf16().count();
        if let Some(selection) = &ctx.selection {
            assert!(
                selection.end <= total,
                "selection endpoints must fit the document they came from"
            );
        }
    }
}
