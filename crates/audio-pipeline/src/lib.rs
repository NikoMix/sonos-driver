//! Audio pipeline: resampling, channel/format conversion and jitter buffering.
//!
//! # Workstream WS-E (Phase 1)
//!
//! Sits between capture and encoding. Responsibilities:
//!
//! * **Resample** the captured stream to the Sonos target rate (e.g. 44.1 kHz)
//!   using a high-quality polyphase resampler.
//! * **Convert** channel layout and sample type as required.
//! * **Buffer** a small, configurable amount of audio to absorb network jitter
//!   while exposing the depth as a latency knob.
//!
//! The [`Pipeline`] contract is defined in Phase 0; the DSP implementation lands
//! in WS-E. The identity fast-path below (no resample, matching format) is
//! provided so the orchestration crate has a working pass-through for tests.

use audio_core::{AudioFrame, PipelineConfig, Result};

/// Transforms captured [`AudioFrame`]s into the format the encoder expects.
#[derive(Debug)]
pub struct Pipeline {
    target_sample_rate: Option<u32>,
}

impl Pipeline {
    /// Build a pipeline from configuration.
    #[must_use]
    pub fn new(config: &PipelineConfig) -> Self {
        Self {
            target_sample_rate: config.target_sample_rate,
        }
    }

    /// Process one frame.
    ///
    /// Phase 0 behaviour is an identity pass-through when no resampling is
    /// required (target rate unset or already equal to the frame rate). Actual
    /// resampling/conversion is implemented in WS-E.
    pub fn process(&mut self, frame: AudioFrame) -> Result<AudioFrame> {
        match self.target_sample_rate {
            Some(rate) if rate != frame.sample_rate() => {
                // WS-E: insert polyphase resampling here.
                tracing::trace!(
                    from = frame.sample_rate(),
                    to = rate,
                    "resampling not yet implemented (WS-E); passing through"
                );
                Ok(frame)
            }
            _ => Ok(frame),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audio_core::AudioSource;

    #[test]
    fn pass_through_preserves_frame_shape() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            target_sample_rate: None,
            buffer_ms: 0,
        });
        let mut src = audio_core::SyntheticSource::new(48_000, 2, 440.0, 0.3, 256);
        let frame = src.read().expect("ok").expect("frame");
        let out = pipeline.process(frame).expect("processed");
        assert_eq!(out.frame_count(), 256);
        assert_eq!(out.channels(), 2);
    }
}
