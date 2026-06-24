//! Windows audio capture via WASAPI loopback (user-mode, pure Rust).
//!
//! # Workstream WS-A (Phase 1)
//!
//! This crate captures the system's (or a selected) **render** endpoint with an
//! event-driven [WASAPI] loopback client and exposes it as an
//! [`audio_core::AudioSource`]. Pull audio by constructing a [`LoopbackCapture`]
//! and repeatedly calling [`AudioSource::read`](audio_core::AudioSource::read).
//!
//! ## How it works
//!
//! * **Event-driven capture.** A dedicated, time-critical thread initialises an
//!   `IAudioClient` in shared loopback mode (`AUDCLNT_STREAMFLAGS_LOOPBACK |
//!   AUDCLNT_STREAMFLAGS_EVENTCALLBACK`) and waits on its buffer event, draining
//!   packets the moment the audio engine signals them. Captured blocks are
//!   converted to canonical interleaved `f32` and handed to the consumer over a
//!   bounded channel.
//! * **Silence keep-alive.** WASAPI loopback only raises events while audio is
//!   actually rendering, so a second thread runs a render client that writes
//!   genuine silence. This keeps the engine active and frames flowing
//!   continuously — essential for an always-on Sonos stream — without making any
//!   audible sound.
//! * **Device & format resilience.** The endpoint is chosen by id or
//!   friendly-name substring (or the default render device), and the capture
//!   thread re-resolves and re-initialises the client when Windows invalidates
//!   the device (e.g. a default-device switch or format change), surviving the
//!   change without ending the stream.
//!
//! ## Backpressure & telemetry
//!
//! The capture thread never blocks: if the consumer falls behind and the channel
//! fills, the newest block is dropped and an overrun is counted
//! ([`LoopbackCapture::overrun_count`]). Reinitialisations are likewise counted
//! ([`LoopbackCapture::reconnect_count`]) for the orchestration layer's metrics.
//!
//! ## `unsafe` boundary
//!
//! All COM/FFI lives in the private [`wasapi`](mod@wasapi) module (Windows only),
//! the single place in the workspace permitted to use `unsafe`. Every `unsafe`
//! block documents the invariant that makes it sound, so `cargo geiger` shows
//! `unsafe` confined to this crate. The pure logic — device matching
//! ([`device`](mod@device)) and sample conversion ([`format`](mod@format)) — is
//! FFI-free and unit-tested without hardware.
//!
//! ## Platform & testing
//!
//! Capture is **Windows-only**; on other targets [`LoopbackCapture::open`]
//! returns [`audio_core::Error::Capture`]. WASAPI cannot run in CI, so the
//! hardware path is verified with the manual checklist in
//! `docs/AUDIO-CAPTURE-MANUAL-CHECKLIST.md`.
//!
//! [WASAPI]: https://learn.microsoft.com/windows/win32/coreaudio/wasapi

mod capture;
mod device;
mod format;

#[cfg(windows)]
#[allow(unsafe_code)]
// The sole sanctioned `unsafe` island: WASAPI COM/FFI, fully documented within.
mod wasapi;

pub use capture::LoopbackCapture;
pub use device::EndpointInfo;

use audio_core::Result;

/// Enumerate the active audio **render** endpoints visible to WASAPI.
///
/// The returned [`EndpointInfo`] values can be inspected to discover device ids
/// and friendly names for [`audio_core::CaptureConfig::device`], and the entry
/// flagged [`EndpointInfo::is_default`] is the current default render device.
///
/// # Errors
///
/// Returns [`audio_core::Error::Capture`] when endpoint enumeration fails. On
/// non-Windows targets it always returns [`audio_core::Error::Capture`] noting
/// the platform requirement.
#[cfg(windows)]
pub fn enumerate_render_endpoints() -> Result<Vec<EndpointInfo>> {
    wasapi::enumerate_render_endpoints()
}

/// Enumerate the active audio **render** endpoints visible to WASAPI.
///
/// # Errors
///
/// Always returns [`audio_core::Error::Capture`] on non-Windows targets, where
/// WASAPI is unavailable.
#[cfg(not(windows))]
pub fn enumerate_render_endpoints() -> Result<Vec<EndpointInfo>> {
    Err(audio_core::Error::capture(
        "WASAPI endpoint enumeration is only available on Windows",
    ))
}
