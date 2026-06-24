//! The event-driven loopback capture session and its worker loop.
//!
//! [`run_capture`] owns a worker thread: it (re)builds a [`CaptureSession`],
//! pumps audio packets out over the channel in response to the WASAPI buffer
//! event, and transparently re-establishes the session when the device is
//! invalidated (e.g. a default-device or format change).

use std::ops::ControlFlow;
use std::slice;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use audio_core::{AudioFrame, Error, Result};
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, WaitForSingleObject, THREAD_PRIORITY_TIME_CRITICAL,
};

use super::com::{self, ComGuard, EventHandle, TaskMem};
use super::{endpoint, Outcome, AUDCLNT_BUFFERFLAGS_SILENT};
use crate::capture::{CaptureItem, SharedState};
use crate::device;
use crate::format::{self, WaveFormat};

/// Give up (and report a fatal error) after this many consecutive failures to
/// (re)establish a session, so a permanently gone device cannot spin forever.
const MAX_RECONNECT_ATTEMPTS: u32 = 20;

/// How long to block on the buffer event before re-checking the stop flag.
const WAIT_TIMEOUT_MS: u32 = 200;

/// A live capture session: the COM objects plus the decoded wave format.
///
/// Field order matters for teardown: the COM interfaces are released before
/// `_com` runs `CoUninitialize`, because they are dropped top-to-bottom.
struct CaptureSession {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: EventHandle,
    wave: WaveFormat,
    _com: ComGuard,
}

/// Capture worker entry point: loop over sessions, reconnecting as needed, until
/// a stop is requested or a fatal error occurs.
pub(super) fn run_capture(
    selector: Option<&str>,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) {
    set_time_critical_priority();
    let mut attempts: u32 = 0;
    while !shared.stop_requested() {
        match run_session(selector, shared, tx) {
            Ok(Outcome::Stop) => break,
            Ok(Outcome::Reconnect) => {
                attempts = 0;
                shared.record_reconnect();
                tracing::warn!(
                    reconnects = shared.reconnects(),
                    "loopback device invalidated; reconnecting"
                );
            }
            Err(err) => {
                attempts += 1;
                if attempts >= MAX_RECONNECT_ATTEMPTS {
                    tracing::error!(
                        attempts,
                        error = %err,
                        "capture failed to recover; ending stream"
                    );
                    let _ = tx.try_send(Err(err));
                    break;
                }
                tracing::warn!(
                    attempts,
                    error = %err,
                    "capture session error; retrying after backoff"
                );
                shared.record_reconnect();
                thread::sleep(backoff(attempts));
            }
        }
    }
    tracing::debug!("capture worker exiting");
}

/// Build one session and pump it until it ends.
fn run_session(
    selector: Option<&str>,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) -> Result<Outcome> {
    let session = start_session(selector, shared)?;
    pump(&session, shared, tx)
}

/// Resolve the device and bring up a started, event-driven loopback client.
fn start_session(selector: Option<&str>, shared: &Arc<SharedState>) -> Result<CaptureSession> {
    let com = ComGuard::new()?;
    let enumerator = endpoint::create_enumerator()?;
    let endpoints = endpoint::enumerate(&enumerator)?;
    let chosen = device::select(&endpoints, selector)?.id.clone();
    let target = endpoint::device_by_id(&enumerator, &chosen)?;

    // SAFETY: activates the audio client interface on a live render endpoint.
    let client: IAudioClient = unsafe { target.Activate(CLSCTX_ALL, None) }
        .map_err(|e| super::win_err("Activate(IAudioClient)", &e))?;

    let wave = initialize_capture(&client)?;
    shared.set_format(wave.to_audio_format()?);
    tracing::debug!(
        sample_rate = wave.sample_rate,
        channels = wave.channels,
        "capture session started"
    );

    let event = com::create_event()?;
    // SAFETY: associates our auto-reset event with the initialised client.
    unsafe { client.SetEventHandle(event.raw()) }
        .map_err(|e| super::win_err("SetEventHandle", &e))?;

    // SAFETY: retrieves the capture service from the initialised loopback client.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|e| super::win_err("GetService(IAudioCaptureClient)", &e))?;

    // SAFETY: starts the audio stream; events begin signalling once data flows.
    unsafe { client.Start() }.map_err(|e| super::win_err("Start", &e))?;

    Ok(CaptureSession {
        client,
        capture,
        event,
        wave,
        _com: com,
    })
}

/// Initialise the client for shared, event-driven loopback capture and return
/// the decoded mix format.
fn initialize_capture(client: &IAudioClient) -> Result<WaveFormat> {
    // SAFETY: `GetMixFormat` allocates a `WAVEFORMATEX` we own; `TaskMem` frees it
    // when this scope ends (after `Initialize` has consumed it).
    let ptr = unsafe { client.GetMixFormat() }.map_err(|e| super::win_err("GetMixFormat", &e))?;
    let _mem = TaskMem::new(ptr.cast());
    if ptr.is_null() {
        return Err(Error::capture("GetMixFormat returned a null format"));
    }
    let wave = endpoint::wave_format_from_ptr(ptr);

    let mut default_period: i64 = 0;
    // SAFETY: writes the default device period through a valid local out-pointer.
    unsafe { client.GetDevicePeriod(Some(&raw mut default_period), None) }
        .map_err(|e| super::win_err("GetDevicePeriod", &e))?;

    // SAFETY: standard shared-mode loopback init; `ptr` is the live mix format and
    // the buffer duration comes from `GetDevicePeriod`.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            default_period,
            0,
            ptr,
            None,
        )
    }
    .map_err(|e| super::win_err("IAudioClient::Initialize", &e))?;

    Ok(wave)
}

/// Event loop: wait for the buffer event and drain packets until the session
/// ends, the consumer disconnects, or the device is invalidated.
fn pump(
    session: &CaptureSession,
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
    session: &CaptureSession,
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
    session: &CaptureSession,
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
fn stop_client(client: &IAudioClient) {
    // SAFETY: stopping a started client; any error here is irrelevant to teardown.
    unsafe {
        let _ = client.Stop();
    }
}

/// Whether an error means the device/service went away and we should reconnect.
fn is_invalidated(err: &windows::core::Error) -> bool {
    let code = err.code();
    code == AUDCLNT_E_DEVICE_INVALIDATED || code == AUDCLNT_E_SERVICE_NOT_RUNNING
}

/// Raise the calling thread to time-critical priority for low-jitter capture.
fn set_time_critical_priority() {
    // SAFETY: both calls only read/adjust the current thread's scheduling state
    // and take no ownership; a failure is non-fatal and intentionally ignored.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
}

/// Linear backoff capped at ~1 s between reconnect attempts.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(u64::from(attempt.min(10)) * 100)
}
