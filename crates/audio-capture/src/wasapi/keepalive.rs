//! The silence keep-alive renderer.
//!
//! WASAPI loopback only raises capture events while the endpoint is actively
//! rendering. To keep frames flowing continuously — even when nothing else is
//! playing — this worker opens a render client on the same endpoint and feeds it
//! silence (using `AUDCLNT_BUFFERFLAGS_SILENT`, so the engine substitutes zeros
//! and the buffer contents are irrelevant). It makes no audible sound.
//!
//! Keep-alive failures are non-fatal to capture: real audio still drives loopback
//! events, so this worker simply retries rather than ending the stream.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use audio_core::Result;
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    IAudioClient, IAudioRenderClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::WaitForSingleObject;

use super::com::{self, ComGuard, EventHandle, TaskMem};
use super::{endpoint, Outcome, AUDCLNT_BUFFERFLAGS_SILENT};
use crate::capture::SharedState;
use crate::device;

/// How long to block on the render event before re-checking the stop flag.
const WAIT_TIMEOUT_MS: u32 = 200;

/// Delay between keep-alive (re)connection attempts.
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// A live keep-alive session: render client plus its driving objects.
///
/// Field order matters for teardown: COM interfaces are released before `_com`
/// runs `CoUninitialize`.
struct KeepAliveSession {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: EventHandle,
    buffer_frames: u32,
    _com: ComGuard,
}

/// Keep-alive worker entry point: keep a silence renderer running on the target
/// endpoint until a stop is requested.
pub(super) fn run_keepalive(selector: Option<&str>, shared: &Arc<SharedState>) {
    while !shared.stop_requested() {
        match run_session(selector, shared) {
            Ok(Outcome::Stop) => break,
            // Reconnect or a transient error: pause briefly, then retry. Capture is
            // unaffected (real audio still drives loopback events), so we keep these
            // at low severity and never surface them to the consumer.
            Ok(Outcome::Reconnect) => {
                tracing::trace!("keep-alive renderer reconnecting");
                thread::sleep(RETRY_DELAY);
            }
            Err(err) => {
                tracing::debug!(error = %err, "keep-alive renderer error; retrying");
                thread::sleep(RETRY_DELAY);
            }
        }
    }
    tracing::trace!("keep-alive worker exiting");
}

/// Build one keep-alive session and run it until it ends.
fn run_session(selector: Option<&str>, shared: &Arc<SharedState>) -> Result<Outcome> {
    let session = start_session(selector)?;
    pump(&session, shared)
}

/// Resolve the device and bring up a started, event-driven silence renderer.
fn start_session(selector: Option<&str>) -> Result<KeepAliveSession> {
    let com = ComGuard::new()?;
    let enumerator = endpoint::create_enumerator()?;
    let endpoints = endpoint::enumerate(&enumerator)?;
    let chosen = device::select(&endpoints, selector)?.id.clone();
    let target = endpoint::device_by_id(&enumerator, &chosen)?;

    // SAFETY: activates the audio client interface on a live render endpoint.
    let client: IAudioClient = unsafe { target.Activate(CLSCTX_ALL, None) }
        .map_err(|e| super::win_err("Activate(IAudioClient)", &e))?;

    initialize_render(&client)?;

    let event = com::create_event()?;
    // SAFETY: associates our auto-reset event with the initialised client.
    unsafe { client.SetEventHandle(event.raw()) }
        .map_err(|e| super::win_err("SetEventHandle", &e))?;

    // SAFETY: queries the shared buffer capacity of the initialised client.
    let buffer_frames =
        unsafe { client.GetBufferSize() }.map_err(|e| super::win_err("GetBufferSize", &e))?;

    // SAFETY: retrieves the render service from the initialised client.
    let render: IAudioRenderClient = unsafe { client.GetService() }
        .map_err(|e| super::win_err("GetService(IAudioRenderClient)", &e))?;

    prime_silence(&render, buffer_frames)?;

    // SAFETY: starts the render stream; the event then signals each period.
    unsafe { client.Start() }.map_err(|e| super::win_err("Start", &e))?;

    Ok(KeepAliveSession {
        client,
        render,
        event,
        buffer_frames,
        _com: com,
    })
}

/// Initialise the client for shared, event-driven rendering.
fn initialize_render(client: &IAudioClient) -> Result<()> {
    // SAFETY: `GetMixFormat` allocates a `WAVEFORMATEX` we own; `TaskMem` frees it
    // when this scope ends (after `Initialize` has consumed it).
    let ptr = unsafe { client.GetMixFormat() }.map_err(|e| super::win_err("GetMixFormat", &e))?;
    let _mem = TaskMem::new(ptr.cast());
    if ptr.is_null() {
        return Err(audio_core::Error::capture(
            "GetMixFormat returned a null format",
        ));
    }

    let mut default_period: i64 = 0;
    // SAFETY: writes the default device period through a valid local out-pointer.
    unsafe { client.GetDevicePeriod(Some(&raw mut default_period), None) }
        .map_err(|e| super::win_err("GetDevicePeriod", &e))?;

    // SAFETY: standard shared-mode event-driven render init with the live format.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            default_period,
            0,
            ptr,
            None,
        )
    }
    .map_err(|e| super::win_err("IAudioClient::Initialize", &e))
}

/// Pre-roll the whole buffer with silence before starting.
fn prime_silence(render: &IAudioRenderClient, buffer_frames: u32) -> Result<()> {
    render_silence(render, buffer_frames)
}

/// Render `frames` of silence into the shared buffer.
fn render_silence(render: &IAudioRenderClient, frames: u32) -> Result<()> {
    if frames == 0 {
        return Ok(());
    }
    // SAFETY: requests a writable region of `frames`; we never read it and mark
    // the release as silent so its contents are immaterial.
    let _buffer = unsafe { render.GetBuffer(frames) }
        .map_err(|e| super::win_err("IAudioRenderClient::GetBuffer", &e))?;
    // SAFETY: releases exactly the requested frame count, flagged as silence.
    unsafe { render.ReleaseBuffer(frames, AUDCLNT_BUFFERFLAGS_SILENT) }
        .map_err(|e| super::win_err("IAudioRenderClient::ReleaseBuffer", &e))
}

/// Event loop: top the buffer back up with silence each time it drains.
fn pump(session: &KeepAliveSession, shared: &Arc<SharedState>) -> Result<Outcome> {
    loop {
        if shared.stop_requested() {
            stop_client(&session.client);
            return Ok(Outcome::Stop);
        }

        // SAFETY: waits on our valid event handle with a finite timeout.
        let wait = unsafe { WaitForSingleObject(session.event.raw(), WAIT_TIMEOUT_MS) };
        if wait == WAIT_OBJECT_0 {
            if let Err(err) = top_up(session) {
                stop_client(&session.client);
                return Err(err);
            }
        } else if wait != WAIT_TIMEOUT {
            stop_client(&session.client);
            return Ok(Outcome::Reconnect);
        }
    }
}

/// Fill whatever portion of the buffer has drained with more silence.
fn top_up(session: &KeepAliveSession) -> Result<()> {
    // SAFETY: reads the current padding of the started client.
    let padding = unsafe { session.client.GetCurrentPadding() }
        .map_err(|e| super::win_err("GetCurrentPadding", &e))?;
    let available = session.buffer_frames.saturating_sub(padding);
    render_silence(&session.render, available)
}

/// Stop a started client, ignoring teardown errors.
fn stop_client(client: &IAudioClient) {
    // SAFETY: stopping a started client; any error here is irrelevant to teardown.
    unsafe {
        let _ = client.Stop();
    }
}
