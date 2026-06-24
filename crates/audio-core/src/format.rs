//! Audio format description: sample format, sample rate and channel count.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// How a single PCM sample is represented on the wire.
///
/// The pipeline's canonical *internal* representation is always `f32`
/// (see [`crate::AudioFrame`]); this enum describes the formats we capture from
/// or encode to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SampleFormat {
    /// 16-bit signed integer, little-endian. The Sonos L16 streaming format.
    I16,
    /// 24-bit signed integer packed in 3 bytes, little-endian.
    I24,
    /// 32-bit signed integer, little-endian.
    I32,
    /// 32-bit IEEE-754 float. The native WASAPI mix format on most systems.
    F32,
}

impl SampleFormat {
    /// Number of bytes occupied by one sample of this format.
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::I32 | Self::F32 => 4,
        }
    }

    /// Whether the format stores samples as floating point.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32)
    }
}

/// A fully specified PCM audio format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Sampling frequency in Hertz (e.g. `44_100` or `48_000`).
    pub sample_rate: u32,
    /// Number of interleaved channels (e.g. `2` for stereo).
    pub channels: u16,
    /// Per-sample binary representation.
    pub sample_format: SampleFormat,
}

impl AudioFormat {
    /// Construct a format, validating the parameters.
    ///
    /// Returns [`Error::InvalidFormat`] when the sample rate or channel count
    /// is zero.
    pub fn new(sample_rate: u32, channels: u16, sample_format: SampleFormat) -> Result<Self> {
        if sample_rate == 0 {
            return Err(Error::invalid_format("sample_rate must be non-zero"));
        }
        if channels == 0 {
            return Err(Error::invalid_format("channels must be non-zero"));
        }
        Ok(Self {
            sample_rate,
            channels,
            sample_format,
        })
    }

    /// CD-quality stereo PCM (44.1 kHz, 16-bit) — the Sonos L16 target.
    #[must_use]
    pub const fn cd_stereo() -> Self {
        Self {
            sample_rate: 44_100,
            channels: 2,
            sample_format: SampleFormat::I16,
        }
    }

    /// 48 kHz / 32-bit float stereo — a common WASAPI mix format.
    #[must_use]
    pub const fn studio_stereo() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            sample_format: SampleFormat::F32,
        }
    }

    /// Number of bytes for one interleaved frame (one sample per channel).
    #[must_use]
    pub const fn bytes_per_frame(self) -> usize {
        self.sample_format.bytes_per_sample() * self.channels as usize
    }

    /// Raw byte throughput per second for this format.
    #[must_use]
    pub const fn bytes_per_second(self) -> usize {
        self.bytes_per_frame() * self.sample_rate as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_per_sample_matches_spec() {
        assert_eq!(SampleFormat::I16.bytes_per_sample(), 2);
        assert_eq!(SampleFormat::I24.bytes_per_sample(), 3);
        assert_eq!(SampleFormat::I32.bytes_per_sample(), 4);
        assert_eq!(SampleFormat::F32.bytes_per_sample(), 4);
    }

    #[test]
    fn cd_stereo_throughput_is_correct() {
        let fmt = AudioFormat::cd_stereo();
        // 44100 * 2 channels * 2 bytes = 176_400 B/s (the classic CD bitrate).
        assert_eq!(fmt.bytes_per_frame(), 4);
        assert_eq!(fmt.bytes_per_second(), 176_400);
    }

    #[test]
    fn zero_parameters_are_rejected() {
        assert!(AudioFormat::new(0, 2, SampleFormat::I16).is_err());
        assert!(AudioFormat::new(48_000, 0, SampleFormat::I16).is_err());
        assert!(AudioFormat::new(48_000, 2, SampleFormat::I16).is_ok());
    }
}
