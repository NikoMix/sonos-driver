//! Audio pipeline: resampling, channel/format conversion and jitter buffering.
//!
//! # Workstream WS-E (Phase 1)
//!
//! Sits between capture and encoding and turns whatever the capture endpoint
//! produces into the canonical stream the encoder expects. Responsibilities:
//!
//! * **Resample** the captured stream to the Sonos target rate (e.g. 44.1 kHz)
//!   with a high-quality, real-time-safe FFT resampler ([`mod@resample`]).
//! * **Convert** the channel layout as required, e.g. capture layout → stereo
//!   ([`convert_channels`]).
//! * **Buffer** a small, configurable amount of audio to absorb capture/network
//!   jitter while exposing the depth as the latency knob ([`JitterBuffer`]).
//!
//! The DSP transform ([`Pipeline::process`]) is deterministic and one-frame-in,
//! one-(possibly-empty)-frame-out. The jitter buffer is kept as a distinct,
//! independently testable stage that the orchestration layer drives via
//! [`Pipeline::push`] / [`Pipeline::pop`].
//!
//! The identity fast-path (target rate unset or already equal, and no channel
//! conversion requested) returns the frame untouched so a pass-through bridge
//! costs nothing.

#![forbid(unsafe_code)]

mod channels;
mod jitter;
mod resample;

pub use channels::convert_channels;
pub use jitter::JitterBuffer;

use audio_core::{AudioFrame, PipelineConfig, Result};
use resample::ResampleStage;

/// Transforms captured [`AudioFrame`]s into the format the encoder expects and
/// buffers them against jitter.
#[derive(Debug)]
pub struct Pipeline {
    target_sample_rate: Option<u32>,
    target_channels: Option<u16>,
    buffer_ms: u32,
    resampler: Option<ResampleStage>,
    jitter: Option<JitterBuffer>,
}

impl Pipeline {
    /// Build a pipeline from configuration.
    ///
    /// Channel conversion is opt-in via [`Pipeline::with_target_channels`]; by
    /// default the source channel layout is preserved.
    #[must_use]
    pub fn new(config: &PipelineConfig) -> Self {
        Self {
            target_sample_rate: config.target_sample_rate,
            target_channels: None,
            buffer_ms: config.buffer_ms,
            resampler: None,
            jitter: None,
        }
    }

    /// Request that processed frames be converted to `channels` interleaved
    /// channels (e.g. `2` to normalise everything to stereo for Sonos).
    ///
    /// A value of `0` is treated as "no conversion".
    #[must_use]
    pub fn with_target_channels(mut self, channels: u16) -> Self {
        self.target_channels = (channels != 0).then_some(channels);
        self
    }

    /// The effective output sample rate processed frames will carry, if a
    /// resampling target is configured.
    #[must_use]
    pub fn target_sample_rate(&self) -> Option<u32> {
        self.target_sample_rate
    }

    /// The configured target channel layout, if channel conversion is enabled.
    #[must_use]
    pub fn target_channels(&self) -> Option<u16> {
        self.target_channels
    }

    /// Process one frame: resample to the target rate (if any) and convert the
    /// channel layout (if requested).
    ///
    /// While resampling, early calls may return an empty frame as the resampler
    /// fills its internal buffers, and later calls may return more than one
    /// input block's worth of audio. The returned frame always carries the
    /// effective output rate and channel layout.
    ///
    /// # Errors
    ///
    /// Returns [`audio_core::Error::Pipeline`] if the resampler or channel
    /// converter fails.
    pub fn process(&mut self, frame: AudioFrame) -> Result<AudioFrame> {
        let resampled = self.resample(frame)?;
        self.convert(resampled)
    }

    /// Push a (typically already processed) frame into the jitter buffer.
    ///
    /// The buffer is created lazily from the frame's sample rate and the
    /// configured `buffer_ms` on first use.
    pub fn push(&mut self, frame: AudioFrame) {
        let buffer_ms = self.buffer_ms;
        let rate = frame.sample_rate();
        let jitter = self
            .jitter
            .get_or_insert_with(|| JitterBuffer::new(buffer_ms, rate));
        jitter.push(frame);
    }

    /// Pop the oldest buffered frame, or `None` if the buffer is empty or has
    /// not been created yet.
    pub fn pop(&mut self) -> Option<AudioFrame> {
        self.jitter.as_mut().and_then(JitterBuffer::pop)
    }

    /// Borrow the jitter buffer for telemetry (depth, underruns, overruns).
    ///
    /// Returns `None` until the first frame has been pushed.
    #[must_use]
    pub fn jitter(&self) -> Option<&JitterBuffer> {
        self.jitter.as_ref()
    }

    /// The resampler's group delay in output frames, or `0` when resampling is
    /// inactive (identity fast-path).
    ///
    /// This is a fixed contribution to end-to-end latency and is surfaced for
    /// the latency accounting described in `docs/LATENCY-BUDGET.md`.
    #[must_use]
    pub fn resampler_delay_frames(&self) -> usize {
        self.resampler
            .as_ref()
            .map_or(0, ResampleStage::output_delay)
    }

    fn resample(&mut self, frame: AudioFrame) -> Result<AudioFrame> {
        let input_rate = frame.sample_rate();
        let target = match self.target_sample_rate {
            Some(rate) if rate != input_rate => rate,
            // Identity fast-path: no target, or capture already at the target.
            _ => return Ok(frame),
        };

        let channels = usize::from(frame.channels());
        let needs_rebuild = self.resampler.as_ref().is_none_or(|stage| {
            !stage.matches(input_rate, channels) || stage.output_rate() != target
        });
        if needs_rebuild {
            tracing::debug!(input_rate, target, channels, "building resampler stage");
            self.resampler = Some(ResampleStage::new(input_rate, target, channels)?);
        }

        let stage = self
            .resampler
            .as_mut()
            .expect("resampler is initialised above");
        let output = stage.process(frame.samples())?;
        AudioFrame::new(output, frame.channels(), target)
    }

    fn convert(&self, frame: AudioFrame) -> Result<AudioFrame> {
        match self.target_channels {
            Some(channels) if channels != frame.channels() => convert_channels(&frame, channels),
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
        assert_eq!(out.sample_rate(), 48_000);
    }

    #[test]
    fn equal_target_rate_is_identity() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            target_sample_rate: Some(48_000),
            buffer_ms: 0,
        });
        let mut src = audio_core::SyntheticSource::new(48_000, 2, 440.0, 0.3, 128);
        let frame = src.read().expect("ok").expect("frame");
        let out = pipeline.process(frame).expect("processed");
        assert_eq!(out.frame_count(), 128);
        assert_eq!(out.sample_rate(), 48_000);
    }

    #[test]
    fn resampling_changes_rate_and_accumulates_output() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            target_sample_rate: Some(44_100),
            buffer_ms: 80,
        });
        let mut src = audio_core::SyntheticSource::new(48_000, 2, 440.0, 0.3, 480);

        // No resampler is built until the first rate-changing frame arrives.
        assert_eq!(pipeline.resampler_delay_frames(), 0);

        let mut produced = 0usize;
        for _ in 0..100 {
            let frame = src.read().expect("ok").expect("frame");
            let out = pipeline.process(frame).expect("processed");
            assert_eq!(out.channels(), 2);
            if out.frame_count() > 0 {
                assert_eq!(out.sample_rate(), 44_100);
            }
            produced += out.frame_count();
        }

        // Resampling is active, so it now contributes a non-zero fixed delay.
        assert!(pipeline.resampler_delay_frames() > 0);

        // 100 * 480 input frames at the 44100/48000 ratio, minus the resampler
        // delay and a partial chunk still buffered.
        let expected = 100 * 480 * 44_100 / 48_000;
        let lower = expected - 1_000;
        let upper = expected + 1_000;
        assert!(
            (lower..=upper).contains(&produced),
            "expected ~{expected} frames, produced {produced}",
        );
    }

    #[test]
    fn channel_conversion_normalises_to_stereo() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            target_sample_rate: None,
            buffer_ms: 0,
        })
        .with_target_channels(2);
        let mono = AudioFrame::new(vec![0.2_f32; 256], 1, 48_000).expect("frame");
        let out = pipeline.process(mono).expect("processed");
        assert_eq!(out.channels(), 2);
        assert_eq!(out.frame_count(), 256);
    }

    #[test]
    fn pipeline_jitter_buffer_round_trips_and_reports_depth() {
        let mut pipeline = Pipeline::new(&PipelineConfig {
            target_sample_rate: None,
            buffer_ms: 50,
        });
        assert!(pipeline.jitter().is_none());

        let frame = AudioFrame::new(vec![0.0_f32; 480 * 2], 2, 48_000).expect("frame");
        pipeline.push(frame);

        let jitter = pipeline.jitter().expect("jitter created on first push");
        assert_eq!(jitter.len_frames(), 480);
        // 50 ms at 48 kHz == 2400 frames of capacity.
        assert_eq!(jitter.capacity_frames(), 2400);

        let popped = pipeline.pop().expect("buffered frame");
        assert_eq!(popped.frame_count(), 480);
        assert!(pipeline.pop().is_none());
    }
}
