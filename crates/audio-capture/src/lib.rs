//! Windows audio capture via WASAPI loopback (user-mode).
//!
//! # Workstream WS-A (Phase 1)
//!
//! This crate captures the system (or a selected) render endpoint using an
//! event-driven WASAPI loopback client and exposes it as an
//! [`audio_core::AudioSource`]. The capture loop runs on a dedicated,
//! time-critical thread and hands blocks to the pipeline through a bounded
//! channel.
//!
//! The contract surface ([`LoopbackCapture`]) is defined here in Phase 0 so the
//! orchestration and pipeline crates can compile and test against it; the
//! WASAPI implementation lands in WS-A. Until then constructors return
//! [`audio_core::Error::NotImplemented`] and tests rely on
//! [`audio_core::SyntheticSource`].

use audio_core::{AudioFormat, AudioFrame, AudioSource, CaptureConfig, Error, Result};

/// A handle to a WASAPI loopback capture session.
///
/// Implements [`AudioSource`] once WS-A is complete.
#[derive(Debug)]
pub struct LoopbackCapture {
    format: AudioFormat,
}

impl LoopbackCapture {
    /// Open a loopback capture session for the endpoint selected by `config`.
    ///
    /// # Errors
    ///
    /// Currently returns [`Error::NotImplemented`]; WS-A will return capture
    /// errors such as device-not-found or WASAPI initialisation failures.
    pub fn open(config: &CaptureConfig) -> Result<Self> {
        let _ = config;
        Err(Error::NotImplemented(
            "audio-capture: WASAPI loopback capture is implemented in WS-A",
        ))
    }
}

impl AudioSource for LoopbackCapture {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn read(&mut self) -> Result<Option<AudioFrame>> {
        Err(Error::NotImplemented(
            "audio-capture: WASAPI loopback read is implemented in WS-A",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_reports_not_implemented_for_now() {
        let err = LoopbackCapture::open(&CaptureConfig::default()).unwrap_err();
        assert!(matches!(err, Error::NotImplemented(_)));
    }
}
