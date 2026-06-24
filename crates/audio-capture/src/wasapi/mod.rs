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

mod com;
mod endpoint;
mod keepalive;
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
