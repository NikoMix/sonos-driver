//! RAII wrappers for COM lifetime, COM-allocated memory and Win32 event handles.
//!
//! Each type pairs an acquire call with its release in `Drop`, so the FFI in
//! sibling modules can stay linear and exception-safe without manual cleanup on
//! every error path.

use core::ffi::c_void;

use audio_core::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::CreateEventW;

/// Initialises COM (multithreaded apartment) for the current thread and
/// uninitialises it on drop. One per worker thread.
pub(super) struct ComGuard {
    /// Whether this guard owns a successful init that must be balanced.
    owns_init: bool,
}

impl ComGuard {
    /// Initialise COM for the calling thread.
    ///
    /// `S_FALSE` (already initialised, same apartment) still counts as a
    /// successful init that we must balance. `RPC_E_CHANGED_MODE` means another
    /// component initialised COM with a different apartment model on this thread;
    /// we reuse it but must **not** call `CoUninitialize`.
    pub(super) fn new() -> Result<Self> {
        // SAFETY: `CoInitializeEx` is the documented COM bootstrap; the reserved
        // pointer is null and we request MTA. Every owned success is balanced by
        // exactly one `CoUninitialize` in `Drop`.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self { owns_init: false });
        }
        hr.ok().map_err(|e| super::win_err("CoInitializeEx", &e))?;
        Ok(Self { owns_init: true })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owns_init {
            // SAFETY: balances the single successful `CoInitializeEx` from `new`
            // on this same thread; called exactly once.
            unsafe { CoUninitialize() };
        }
    }
}

/// Owns a block allocated by COM (e.g. via `GetMixFormat`/`GetId`) and frees it
/// with `CoTaskMemFree` on drop.
pub(super) struct TaskMem(*const c_void);

impl TaskMem {
    /// Take ownership of a COM-allocated pointer.
    pub(super) fn new(ptr: *const c_void) -> Self {
        Self(ptr)
    }
}

impl Drop for TaskMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer was returned by a COM allocator and is freed
            // exactly once here; no other code retains it past this scope.
            unsafe { CoTaskMemFree(Some(self.0)) };
        }
    }
}

/// Owns a Win32 auto-reset event handle and closes it on drop.
pub(super) struct EventHandle(HANDLE);

impl EventHandle {
    /// The raw handle, for passing to `SetEventHandle` / `WaitForSingleObject`.
    pub(super) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: `self.0` came from `CreateEventW`; closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Create an auto-reset, initially-unsignalled, unnamed event.
pub(super) fn create_event() -> Result<EventHandle> {
    // SAFETY: all-default attributes, manual-reset = false, initial = false,
    // unnamed. Returns a valid handle or an error HRESULT.
    let handle = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .map_err(|e| super::win_err("CreateEventW", &e))?;
    Ok(EventHandle(handle))
}
