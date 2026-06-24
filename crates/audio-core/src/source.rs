//! Audio source abstraction and a deterministic [`SyntheticSource`] for tests.

use crate::{AudioFormat, AudioFrame, Result};

/// A pull-based source of audio blocks.
///
/// The real implementation (`audio-capture`) wraps a WASAPI loopback client and
/// is driven from a dedicated capture thread that repeatedly calls
/// [`AudioSource::read`]. Returning `Ok(None)` signals a graceful end of stream.
pub trait AudioSource: Send {
    /// The native format of the audio yielded by this source.
    fn format(&self) -> AudioFormat;

    /// Block until the next audio block is available.
    ///
    /// * `Ok(Some(frame))` — a block of audio.
    /// * `Ok(None)` — the source has ended; stop reading.
    fn read(&mut self) -> Result<Option<AudioFrame>>;
}

/// A deterministic sine-wave generator implementing [`AudioSource`].
///
/// It needs no hardware, which makes it the backbone of unit tests, the
/// end-to-end latency harness and `--dry-run` modes. The same tone is written
/// to every channel.
#[derive(Debug, Clone)]
pub struct SyntheticSource {
    sample_rate: u32,
    channels: u16,
    frequency_hz: f32,
    amplitude: f32,
    frames_per_read: usize,
    max_frames: Option<u64>,
    phase: f32,
    produced_frames: u64,
}

impl SyntheticSource {
    /// Create a generator producing `frames_per_read` per-channel frames per
    /// [`AudioSource::read`] call.
    ///
    /// `frequency_hz` is the tone frequency and `amplitude` is clamped to
    /// `0.0..=1.0`. The stream is infinite unless bounded by
    /// [`SyntheticSource::with_limit`].
    #[must_use]
    pub fn new(
        sample_rate: u32,
        channels: u16,
        frequency_hz: f32,
        amplitude: f32,
        frames_per_read: usize,
    ) -> Self {
        Self {
            sample_rate,
            channels,
            frequency_hz,
            amplitude: amplitude.clamp(0.0, 1.0),
            frames_per_read: frames_per_read.max(1),
            max_frames: None,
            phase: 0.0,
            produced_frames: 0,
        }
    }

    /// A convenient default: a 440 Hz tone at the given format, 10 ms per read.
    #[must_use]
    pub fn tone_440(format: AudioFormat) -> Self {
        let frames_per_read = (format.sample_rate as usize / 100).max(1);
        Self::new(
            format.sample_rate,
            format.channels,
            440.0,
            0.25,
            frames_per_read,
        )
    }

    /// Bound the generator to emit at most `max_frames` per-channel frames in
    /// total, after which [`AudioSource::read`] returns `Ok(None)`.
    #[must_use]
    pub fn with_limit(mut self, max_frames: u64) -> Self {
        self.max_frames = Some(max_frames);
        self
    }
}

impl AudioSource for SyntheticSource {
    fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: self.channels,
            sample_format: crate::SampleFormat::F32,
        }
    }

    fn read(&mut self) -> Result<Option<AudioFrame>> {
        let remaining = match self.max_frames {
            Some(max) if self.produced_frames >= max => return Ok(None),
            Some(max) => {
                // Bounded by `frames_per_read` (a usize), so the conversion can
                // never truncate; the fallback is defensive only.
                let take = (max - self.produced_frames).min(self.frames_per_read as u64);
                usize::try_from(take).unwrap_or(self.frames_per_read)
            }
            None => self.frames_per_read,
        };

        let channels = self.channels as usize;
        let mut samples = Vec::with_capacity(remaining * channels);
        // 2*pi*f / fs is the per-sample phase increment of the tone. Audio
        // sample rates (<= 192 kHz) are < 2^24 and thus exact in f32, so the
        // cast is lossless in practice.
        #[allow(clippy::cast_precision_loss)]
        let phase_step = std::f32::consts::TAU * self.frequency_hz / self.sample_rate as f32;

        for _ in 0..remaining {
            let value = self.phase.sin() * self.amplitude;
            for _ in 0..channels {
                samples.push(value);
            }
            self.phase += phase_step;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
        }

        self.produced_frames += remaining as u64;
        AudioFrame::new(samples, self.channels, self.sample_rate).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_requested_shape() {
        let mut src = SyntheticSource::new(48_000, 2, 440.0, 0.5, 480);
        let frame = src.read().expect("ok").expect("some frame");
        assert_eq!(frame.channels(), 2);
        assert_eq!(frame.frame_count(), 480);
        assert_eq!(frame.sample_count(), 960);
    }

    #[test]
    fn honours_frame_limit_exactly() {
        let mut src = SyntheticSource::new(48_000, 1, 1_000.0, 0.5, 100).with_limit(250);
        let mut total = 0;
        while let Some(frame) = src.read().expect("ok") {
            total += frame.frame_count();
        }
        assert_eq!(total, 250);
        assert!(src.read().expect("ok").is_none());
    }

    #[test]
    fn amplitude_is_clamped() {
        let src = SyntheticSource::new(48_000, 2, 440.0, 9.0, 64);
        // Drive once and ensure no sample exceeds unity magnitude.
        let mut src = src;
        let frame = src.read().expect("ok").expect("frame");
        assert!(frame.samples().iter().all(|s| s.abs() <= 1.0));
    }
}
