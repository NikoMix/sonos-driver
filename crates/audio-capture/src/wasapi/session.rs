//! The default-device loopback capture session and its worker loop.
//!
//! [`run_capture`] owns a worker thread: it (re)builds a [`ClientSession`] for
//! the selected (or default) render endpoint, hands it to the shared
//! [`pump`](super::pump) to drain audio packets out over the channel, and
//! transparently re-establishes the session when the device is invalidated
//! (e.g. a default-device or format change).

use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use audio_core::{Error, Result};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::CLSCTX_ALL;

use super::com::{self, ComGuard, TaskMem};
use super::pump::{self, ClientSession};
use super::{endpoint, Outcome};
use crate::capture::{CaptureItem, SharedState};
use crate::device;
use crate::format::WaveFormat;

/// Give up (and report a fatal error) after this many consecutive failures to
/// (re)establish a session, so a permanently gone device cannot spin forever.
const MAX_RECONNECT_ATTEMPTS: u32 = 20;

/// Capture worker entry point: loop over sessions, reconnecting as needed, until
/// a stop is requested or a fatal error occurs.
pub(super) fn run_capture(
    selector: Option<&str>,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) {
    pump::set_time_critical_priority();
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
    pump::pump(&session, shared, tx)
}

/// Resolve the device and bring up a started, event-driven loopback client.
fn start_session(selector: Option<&str>, shared: &Arc<SharedState>) -> Result<ClientSession> {
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

    Ok(ClientSession {
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

/// Linear backoff capped at ~1 s between reconnect attempts.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(u64::from(attempt.min(10)) * 100)
}
