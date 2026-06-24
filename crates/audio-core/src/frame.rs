//! The canonical in-pipeline audio buffer: [`AudioFrame`].

use std::time::{Duration, Instant};

use crate::{Error, Result};

/// A block of captured audio in the pipeline's canonical representation:
/// **interleaved 32-bit float samples**.
///
/// Working in `f32` internally keeps DSP (gain, mixing, resampling) lossless
/// and branch-free; conversion to the wire format happens only in the encoder.
///
/// An optional [`Instant`] records when the block was captured so the pipeline
/// can measure the controllable portion of end-to-end latency.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
    captured_at: Option<Instant>,
}

impl AudioFrame {
    /// Wrap interleaved `f32` samples into a frame.
    ///
    /// Returns [`Error::InvalidFormat`] when `channels` is zero, `sample_rate`
    /// is zero, or the sample count is not a whole multiple of `channels`.
    pub fn new(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Result<Self> {
        Self::validate(&samples, channels, sample_rate)?;
        Ok(Self {
            samples,
            channels,
            sample_rate,
            captured_at: None,
        })
    }

    /// Like [`AudioFrame::new`] but stamps the frame with a capture instant for
    /// latency telemetry.
    pub fn with_timestamp(
        samples: Vec<f32>,
        channels: u16,
        sample_rate: u32,
        captured_at: Instant,
    ) -> Result<Self> {
        let mut frame = Self::new(samples, channels, sample_rate)?;
        frame.captured_at = Some(captured_at);
        Ok(frame)
    }

    fn validate(samples: &[f32], channels: u16, sample_rate: u32) -> Result<()> {
        if channels == 0 {
            return Err(Error::invalid_format("frame channels must be non-zero"));
        }
        if sample_rate == 0 {
            return Err(Error::invalid_format("frame sample_rate must be non-zero"));
        }
        if !samples.len().is_multiple_of(channels as usize) {
            return Err(Error::invalid_format(
                "sample count must be a whole multiple of the channel count",
            ));
        }
        Ok(())
    }

    /// Interleaved samples, `[L, R, L, R, ...]` for stereo.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Mutable access to the interleaved samples (for in-place DSP).
    #[must_use]
    pub fn samples_mut(&mut self) -> &mut [f32] {
        &mut self.samples
    }

    /// Number of channels.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// Sampling frequency in Hertz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Total number of individual samples across all channels.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Number of per-channel frames (samples divided by channel count).
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// Wall-clock instant the audio was captured, if recorded.
    #[must_use]
    pub const fn captured_at(&self) -> Option<Instant> {
        self.captured_at
    }

    /// The playback duration represented by this block.
    #[must_use]
    pub fn duration(&self) -> Duration {
        // Integer nanosecond math avoids any float-precision lints and is exact
        // for realistic frame sizes (`frames * 1e9` stays well within u64).
        let frames = self.frame_count() as u64;
        Duration::from_nanos(frames * 1_000_000_000 / u64::from(self.sample_rate))
    }

    /// Consume the frame and return the underlying sample buffer.
    #[must_use]
    pub fn into_samples(self) -> Vec<f32> {
        self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ragged_sample_count() {
        // 3 samples cannot be split evenly across 2 channels.
        assert!(AudioFrame::new(vec![0.0, 0.1, 0.2], 2, 48_000).is_err());
    }

    #[test]
    fn reports_frame_count_and_duration() {
        // 480 stereo frames at 48 kHz == 10 ms.
        let samples = vec![0.0_f32; 480 * 2];
        let frame = AudioFrame::new(samples, 2, 48_000).expect("valid frame");
        assert_eq!(frame.frame_count(), 480);
        assert_eq!(frame.sample_count(), 960);
        assert!((frame.duration().as_secs_f64() - 0.010).abs() < 1e-9);
    }
}
