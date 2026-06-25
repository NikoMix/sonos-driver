//! OS build-number probe backing the process-loopback availability gate.
//!
//! `RtlGetVersion` is used rather than `GetVersionExW` because the latter lies
//! (it caps at 6.2 for applications without a compatibility manifest), which
//! would wrongly reject a capable Windows 11 / Server 2022 host.

use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

/// The current Windows build number, or `None` if it cannot be determined.
pub(super) fn current_os_build() -> Option<u32> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(core::mem::size_of::<OSVERSIONINFOW>()).ok()?,
        ..Default::default()
    };
    // SAFETY: `RtlGetVersion` fills our valid, correctly-sized struct and never
    // retains the pointer; it returns `STATUS_SUCCESS` (0) on success.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    if status.0 == 0 {
        Some(info.dwBuildNumber)
    } else {
        None
    }
}
