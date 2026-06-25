//! The shared event-driven capture pump used by both capture paths.
//!
//! Both the default-device loopback ([`super::session`]) and the per-application
//! process loopback ([`super::process_loopback`]) bring up an `IAudioClient` in
//! shared, event-driven loopback mode and then drain identical capture packets.
//! That common back half — the [`ClientSession`] handle plus the wait/drain loop,
//! decode, backpressure and teardown — lives here so the two front halves only
//! differ in *how the client is activated and initialised*.

use std::ops::ControlFlow;
use std::slice;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Instant;

use audio_core::{AudioFrame, Error, Result};
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, WaitForSingleObject, THREAD_PRIORITY_TIME_CRITICAL,
};

use super::com::{ComGuard, EventHandle};
use super::{Outcome, AUDCLNT_BUFFERFLAGS_SILENT};
use crate::capture::{CaptureItem, SharedState};
use crate::format::{self, WaveFormat};

/// How long to block on the buffer event before re-checking the stop flag.
const WAIT_TIMEOUT_MS: u32 = 200;

/// A live capture session: the COM objects plus the decoded wave format.
///
/// Field order matters for teardown: the COM interfaces are released before
/// `_com` runs `CoUninitialize`, because they are dropped top-to-bottom.
pub(super) struct ClientSession {
    /// The initialised, started loopback client.
    pub(super) client: IAudioClient,
    /// The capture service drained on each buffer event.
    pub(super) capture: IAudioCaptureClient,
    /// The auto-reset event WASAPI signals when a packet is ready.
    pub(super) event: EventHandle,
    /// The decoded layout of the captured buffers.
    pub(super) wave: WaveFormat,
    /// Keeps COM initialised for this thread for the session's lifetime.
    pub(super) _com: ComGuard,
}

/// Event loop: wait for the buffer event and drain packets until the session
/// ends, the consumer disconnects, or the device is invalidated.
pub(super) fn pump(
    session: &ClientSession,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) -> Result<Outcome> {
    loop {
        if shared.stop_requested() {
            stop_client(&session.client);
            return Ok(Outcome::Stop);
        }

        // SAFETY: waits on our valid event handle with a finite timeout.
        let wait = unsafe { WaitForSingleObject(session.event.raw(), WAIT_TIMEOUT_MS) };
        if wait == WAIT_OBJECT_0 {
            match drain(session, shared, tx) {
                Ok(ControlFlow::Continue(())) => {}
                Ok(ControlFlow::Break(outcome)) => {
                    stop_client(&session.client);
                    return Ok(outcome);
                }
                Err(err) => {
                    stop_client(&session.client);
                    return Err(err);
                }
            }
        } else if wait != WAIT_TIMEOUT {
            // WAIT_FAILED / abandoned: treat as a transient fault and rebuild.
            stop_client(&session.client);
            return Ok(Outcome::Reconnect);
        }
    }
}

/// Drain every packet currently queued on the capture client.
fn drain(
    session: &ClientSession,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) -> Result<ControlFlow<Outcome>> {
    loop {
        // SAFETY: queries the next packet size on a started capture client.
        let packet = match unsafe { session.capture.GetNextPacketSize() } {
            Ok(size) => size,
            Err(e) if is_invalidated(&e) => return Ok(ControlFlow::Break(Outcome::Reconnect)),
            Err(e) => return Err(super::win_err("GetNextPacketSize", &e)),
        };
        if packet == 0 {
            return Ok(ControlFlow::Continue(()));
        }
        if let ControlFlow::Break(outcome) = read_packet(session, shared, tx)? {
            return Ok(ControlFlow::Break(outcome));
        }
    }
}

/// Read, decode and forward a single capture packet.
fn read_packet(
    session: &ClientSession,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) -> Result<ControlFlow<Outcome>> {
    let mut data: *mut u8 = std::ptr::null_mut();
    let mut frames: u32 = 0;
    let mut flags: u32 = 0;
    // SAFETY: all out-pointers are valid locals; the returned buffer is owned
    // until the matching `ReleaseBuffer`.
    match unsafe {
        session
            .capture
            .GetBuffer(&raw mut data, &raw mut frames, &raw mut flags, None, None)
    } {
        Ok(()) => {}
        Err(e) if is_invalidated(&e) => return Ok(ControlFlow::Break(Outcome::Reconnect)),
        Err(e) => return Err(super::win_err("GetBuffer", &e)),
    }

    let samples = decode_samples(data, frames, flags, &session.wave);

    // Release promptly so the audio engine is never stalled by downstream work.
    // SAFETY: releases exactly the `frames` reported by `GetBuffer`.
    unsafe { session.capture.ReleaseBuffer(frames) }
        .map_err(|e| super::win_err("ReleaseBuffer", &e))?;

    if samples.is_empty() {
        return Ok(ControlFlow::Continue(()));
    }

    let frame = AudioFrame::with_timestamp(
        samples,
        session.wave.channels,
        session.wave.sample_rate,
        Instant::now(),
    )
    .map_err(|e| Error::capture(format!("failed to build captured frame: {e}")))?;

    match tx.try_send(Ok(frame)) {
        Ok(()) => Ok(ControlFlow::Continue(())),
        // Consumer fell behind: drop the newest block and count the overrun
        // rather than blocking this time-critical thread.
        Err(TrySendError::Full(_)) => {
            shared.record_overrun();
            Ok(ControlFlow::Continue(()))
        }
        // Receiver gone: the handle was dropped, so stop cleanly.
        Err(TrySendError::Disconnected(_)) => Ok(ControlFlow::Break(Outcome::Stop)),
    }
}

/// Decode a raw capture buffer into canonical interleaved `f32`.
fn decode_samples(data: *mut u8, frames: u32, flags: u32, wave: &WaveFormat) -> Vec<f32> {
    let total = frames as usize * wave.channels as usize;
    if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
        return vec![0.0; total];
    }
    let byte_len = frames as usize * wave.block_align();
    // SAFETY: WASAPI guarantees `data` is readable for `frames * block_align`
    // bytes until `ReleaseBuffer`; we copy out before releasing.
    let bytes = unsafe { slice::from_raw_parts(data.cast_const(), byte_len) };
    format::samples_to_f32(bytes, wave)
}

/// Stop a started client, ignoring teardown errors.
pub(super) fn stop_client(client: &IAudioClient) {
    // SAFETY: stopping a started client; any error here is irrelevant to teardown.
    unsafe {
        let _ = client.Stop();
    }
}

/// Whether an error means the device/service went away and we should reconnect.
pub(super) fn is_invalidated(err: &windows::core::Error) -> bool {
    let code = err.code();
    code == AUDCLNT_E_DEVICE_INVALIDATED || code == AUDCLNT_E_SERVICE_NOT_RUNNING
}

/// Raise the calling thread to time-critical priority for low-jitter capture.
pub(super) fn set_time_critical_priority() {
    // SAFETY: both calls only read/adjust the current thread's scheduling state
    // and take no ownership; a failure is non-fatal and intentionally ignored.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
}
