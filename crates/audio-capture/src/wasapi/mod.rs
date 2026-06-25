//! WASAPI loopback FFI — the workspace's only `unsafe` island.
//!
//! Everything under this module talks to the Windows Core Audio COM API through
//! the `windows` crate. The safe layers ([`crate::capture`], [`crate::device`],
//! [`crate::format`]) own all policy and pure logic; this module only performs
//! the FFI calls, each `unsafe` block annotated with the invariant that makes it
//! sound. Keeping the FFI here is what lets `cargo geiger` show `unsafe` confined
//! to `audio-capture`.
//!
//! Threads created here each initialise COM for themselves ([`com::ComGuard`])
//! and build their own COM objects from the owned device selector, because COM
//! interface pointers are not `Send` and must never cross threads.

use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::thread;

use audio_core::{Error, Result};

use crate::capture::{CaptureItem, LoopbackCapture, SharedState, CHANNEL_CAPACITY};
use crate::device::{self, EndpointInfo};
use crate::process;

mod com;
mod endpoint;
mod keepalive;
mod osinfo;
mod process_loopback;
mod processes;
mod pump;
mod session;

/// `AUDCLNT_BUFFERFLAGS_SILENT`: the captured/rendered buffer is (to be treated
/// as) silence. Defined locally so the value is independent of which `windows`
/// feature happens to surface the constant.
pub(super) const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;

/// Outcome of a single capture / keep-alive session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// A stop was requested by the owner; the worker should exit.
    Stop,
    /// The device/format was invalidated; the worker should re-resolve and retry.
    Reconnect,
}

/// Wrap a [`windows::core::Error`] as an [`Error::Capture`] with context.
pub(super) fn win_err(context: &str, err: &windows::core::Error) -> Error {
    Error::capture(format!("{context}: {err}"))
}

/// Enumerate active render endpoints (used by the public crate API).
pub(crate) fn enumerate_render_endpoints() -> Result<Vec<EndpointInfo>> {
    let _com = com::ComGuard::new()?;
    let enumerator = endpoint::create_enumerator()?;
    endpoint::enumerate(&enumerator)
}

/// Open a loopback capture session: validate the device, seed the live format,
/// then spawn the capture and keep-alive worker threads.
pub(crate) fn open_capture(selector: Option<&str>) -> Result<LoopbackCapture> {
    let format = {
        // Resolve and probe on the calling thread so device-not-found and
        // unsupported-format failures surface synchronously from `open`.
        let _com = com::ComGuard::new()?;
        let enumerator = endpoint::create_enumerator()?;
        let endpoints = endpoint::enumerate(&enumerator)?;
        let chosen = device::select(&endpoints, selector)?.id.clone();
        let target = endpoint::device_by_id(&enumerator, &chosen)?;
        endpoint::probe_format(&target)?
    };

    let shared = Arc::new(SharedState::new(format));
    let (tx, rx) = sync_channel::<CaptureItem>(CHANNEL_CAPACITY);

    {
        let live = shared.format();
        tracing::info!(
            selector = selector.unwrap_or("<default>"),
            sample_rate = live.sample_rate,
            channels = live.channels,
            "opened WASAPI loopback capture"
        );
    }

    let owned_selector = selector.map(str::to_owned);

    let capture_selector = owned_selector.clone();
    let capture_shared = Arc::clone(&shared);
    let capture_tx = tx.clone();
    let capture_handle = thread::Builder::new()
        .name("wasapi-loopback-capture".to_owned())
        .spawn(move || {
            session::run_capture(capture_selector.as_deref(), &capture_shared, &capture_tx);
        })
        .map_err(|e| Error::capture(format!("failed to spawn capture thread: {e}")))?;

    let keepalive_shared = Arc::clone(&shared);
    let keepalive_handle = thread::Builder::new()
        .name("wasapi-loopback-keepalive".to_owned())
        .spawn(move || {
            keepalive::run_keepalive(owned_selector.as_deref(), &keepalive_shared);
        })
        .map_err(|e| Error::capture(format!("failed to spawn keep-alive thread: {e}")))?;

    // The capture thread holds its own sender clone; drop ours so the receiver
    // observes a disconnect once that thread exits.
    drop(tx);

    Ok(LoopbackCapture::from_parts(
        rx,
        shared,
        vec![capture_handle, keepalive_handle],
    ))
}

/// Open a per-application process-loopback capture session.
///
/// `target` is a process id or image name (resolved against a live toolhelp
/// snapshot); `exclude` flips capture from "only this process tree" to
/// "everything except this process tree". The host OS build is gated up front —
/// process loopback needs Windows 11 / Server 2022 (build 20348+) — returning a
/// clear typed [`Error`] on older systems rather than failing opaquely in the FFI.
///
/// Unlike the default-device path there is no keep-alive renderer (we capture a
/// specific app, and silence while it is idle is expected) and no reconnect loop
/// (a single app has a single session), so only one worker thread is spawned.
pub(crate) fn open_process_capture(target: &str, exclude: bool) -> Result<LoopbackCapture> {
    let build = osinfo::current_os_build().ok_or_else(|| {
        Error::capture("unable to determine the Windows build for the process-loopback gate")
    })?;
    if !process::meets_min_build(build) {
        return Err(Error::capture(format!(
            "per-application loopback requires Windows 11 / Server 2022 (build {min}+); \
             this host reports build {build}",
            min = process::MIN_PROCESS_LOOPBACK_BUILD
        )));
    }

    // Resolve the target on the calling thread so a bad name/pid fails synchronously.
    let selector = process::parse_selector(target)?;
    let snapshot = processes::enumerate_processes()?;
    let pid = process::resolve_pid(&selector, &snapshot)?;
    let mode = process::ProcessLoopbackMode::from_exclude(exclude);

    let format = process_loopback::reported_format()?;
    let shared = Arc::new(SharedState::new(format));
    let (tx, rx) = sync_channel::<CaptureItem>(CHANNEL_CAPACITY);

    {
        let live = shared.format();
        tracing::info!(
            target,
            pid,
            exclude,
            sample_rate = live.sample_rate,
            channels = live.channels,
            "opened WASAPI process-loopback capture"
        );
    }

    let worker_shared = Arc::clone(&shared);
    let worker_tx = tx.clone();
    let worker_handle = thread::Builder::new()
        .name("wasapi-process-loopback".to_owned())
        .spawn(move || {
            process_loopback::run_process_capture(pid, mode, &worker_shared, &worker_tx);
        })
        .map_err(|e| Error::capture(format!("failed to spawn process-loopback thread: {e}")))?;

    // Drop our sender so the receiver observes a disconnect once the worker exits.
    drop(tx);

    Ok(LoopbackCapture::from_parts(rx, shared, vec![worker_handle]))
}
