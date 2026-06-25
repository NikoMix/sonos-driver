//! Toolhelp process enumeration feeding the pure PID resolver in
//! [`crate::process`].
//!
//! This is the only FFI here; all matching/selection policy lives in the
//! FFI-free [`crate::process`] module so it can be unit-tested without hardware.

use audio_core::{Error, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::process::ProcInfo;

/// RAII guard that closes a toolhelp snapshot handle on drop.
struct SnapshotHandle(HANDLE);

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle came from `CreateToolhelp32Snapshot` and is
            // closed exactly once here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Enumerate every running process into FFI-free [`ProcInfo`] records.
///
/// # Errors
///
/// Returns [`Error::Capture`] if the snapshot cannot be taken or the first
/// process cannot be read.
pub(super) fn enumerate_processes() -> Result<Vec<ProcInfo>> {
    // SAFETY: requests a process-list snapshot; returns a valid handle or an error.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| super::win_err("CreateToolhelp32Snapshot", &e))?;
    let guard = SnapshotHandle(snapshot);

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(core::mem::size_of::<PROCESSENTRY32W>())
            .map_err(|_| Error::capture("PROCESSENTRY32W size does not fit in u32"))?,
        ..Default::default()
    };

    // SAFETY: `entry` is a valid, `dwSize`-initialised record and `guard.0` is a
    // live snapshot handle.
    unsafe { Process32FirstW(guard.0, &raw mut entry) }
        .map_err(|e| super::win_err("Process32FirstW", &e))?;

    let mut out = Vec::new();
    loop {
        out.push(ProcInfo {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            name: wide_to_string(&entry.szExeFile),
        });
        // SAFETY: same live handle and valid record; an `Err` (e.g.
        // `ERROR_NO_MORE_FILES`) marks the natural end of the list.
        if unsafe { Process32NextW(guard.0, &raw mut entry) }.is_err() {
            break;
        }
    }
    drop(guard);
    Ok(out)
}

/// Convert a NUL-terminated UTF-16 image name into a `String` (lossy).
fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_to_string_stops_at_nul() {
        let mut buf = [0u16; 8];
        for (slot, ch) in buf.iter_mut().zip("abc".encode_utf16()) {
            *slot = ch;
        }
        assert_eq!(wide_to_string(&buf), "abc");
    }

    #[test]
    fn wide_to_string_handles_no_nul() {
        let buf: Vec<u16> = "xy".encode_utf16().collect();
        assert_eq!(wide_to_string(&buf), "xy");
    }

    #[test]
    fn enumerate_includes_this_process() {
        // The current process must appear in its own snapshot.
        let procs = enumerate_processes().expect("snapshot");
        let me = std::process::id();
        assert!(procs.iter().any(|p| p.pid == me));
    }
}
