//! Uncompressed PCM encoders: Sonos `audio/L16` (big-endian, RFC 2586) and
//! streaming RIFF/WAVE (little-endian).
//!
//! Both consume the pipeline's canonical interleaved `f32` frames and emit
//! 16-bit integer PCM. They are the lowest-latency encoders because there is no
//! compression stage on the audio hot path.

use audio_core::{AudioFormat, AudioFrame, CodecProfile, Encoder, Error, Result};

/// Convert a single `f32` sample in `[-1.0, 1.0]` to 16-bit signed PCM.
///
/// Rust float-to-int casts saturate, so out-of-range inputs map to the i16
/// bounds rather than wrapping; the explicit clamp keeps rounding symmetric.
///
/// Shared by every 16-bit encoder (L16, WAV, FLAC) so the integer mapping is
/// identical across profiles.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn f32_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * f32::from(i16::MAX)).round() as i16
}

pub(crate) fn ensure_channels(frame: &AudioFrame, expected: u16) -> Result<()> {
    if frame.channels() != expected {
        return Err(Error::codec(format!(
            "frame has {} channels but encoder was configured for {expected}",
            frame.channels()
        )));
    }
    Ok(())
}

/// Encoder for Sonos `audio/L16`: raw **big-endian** interleaved 16-bit PCM
/// with no container or header.
#[derive(Debug, Clone)]
pub struct L16PcmEncoder {
    format: AudioFormat,
}

impl L16PcmEncoder {
    /// Create an L16 encoder for the given output format.
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: AudioFormat::cd_stereo(),
        }
    }

    /// Create an L16 encoder for a specific output format.
    #[must_use]
    pub fn with_format(format: AudioFormat) -> Self {
        Self { format }
    }
}

impl Default for L16PcmEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for L16PcmEncoder {
    fn profile(&self) -> CodecProfile {
        CodecProfile::L16Pcm
    }

    fn encode(&mut self, frame: &AudioFrame, out: &mut Vec<u8>) -> Result<()> {
        ensure_channels(frame, self.format.channels)?;
        out.reserve(frame.sample_count() * 2);
        for &sample in frame.samples() {
            out.extend_from_slice(&f32_to_i16(sample).to_be_bytes());
        }
        Ok(())
    }
}

/// Encoder for streaming RIFF/WAVE: **little-endian** 16-bit PCM preceded by a
/// 44-byte header. Sizes are written as `u32::MAX` so the stream can be
/// unbounded, which Sonos tolerates for live WAV sources.
#[derive(Debug, Clone)]
pub struct WavEncoder {
    format: AudioFormat,
}

impl WavEncoder {
    /// Create a WAV encoder for the default CD-stereo output format.
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: AudioFormat::cd_stereo(),
        }
    }

    /// Create a WAV encoder for a specific output format.
    #[must_use]
    pub fn with_format(format: AudioFormat) -> Self {
        Self { format }
    }

    fn streaming_header(&self) -> [u8; 44] {
        const BITS_PER_SAMPLE: u16 = 16;
        let channels = self.format.channels;
        let sample_rate = self.format.sample_rate;
        let block_align = channels * (BITS_PER_SAMPLE / 8);
        let byte_rate = sample_rate * u32::from(block_align);

        let mut h = [0_u8; 44];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&u32::MAX.to_le_bytes()); // streaming: unknown size
        h[8..12].copy_from_slice(b"WAVE");
        h[12..16].copy_from_slice(b"fmt ");
        h[16..20].copy_from_slice(&16_u32.to_le_bytes()); // PCM fmt chunk size
        h[20..22].copy_from_slice(&1_u16.to_le_bytes()); // PCM format tag
        h[22..24].copy_from_slice(&channels.to_le_bytes());
        h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
        h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
        h[32..34].copy_from_slice(&block_align.to_le_bytes());
        h[34..36].copy_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&u32::MAX.to_le_bytes()); // streaming: unknown size
        h
    }
}

impl Default for WavEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder for WavEncoder {
    fn profile(&self) -> CodecProfile {
        CodecProfile::Wav
    }

    fn prelude(&mut self) -> Result<Vec<u8>> {
        Ok(self.streaming_header().to_vec())
    }

    fn encode(&mut self, frame: &AudioFrame, out: &mut Vec<u8>) -> Result<()> {
        ensure_channels(frame, self.format.channels)?;
        out.reserve(frame.sample_count() * 2);
        for &sample in frame.samples() {
            out.extend_from_slice(&f32_to_i16(sample).to_le_bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_maps_to_full_i16_range() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(-1.0), -i16::MAX);
        // Saturation rather than wrap-around for out-of-range input.
        assert_eq!(f32_to_i16(2.0), i16::MAX);
        assert_eq!(f32_to_i16(-2.0), -i16::MAX);
    }

    #[test]
    fn l16_is_big_endian_two_bytes_per_sample() {
        let mut enc = L16PcmEncoder::with_format(AudioFormat::cd_stereo());
        let frame = AudioFrame::new(vec![1.0, -1.0], 2, 44_100).expect("frame");
        let mut out = Vec::new();
        enc.encode(&frame, &mut out).expect("encoded");
        // +full-scale big-endian, then -full-scale big-endian.
        assert_eq!(out, vec![0x7F, 0xFF, 0x80, 0x01]);
    }

    #[test]
    fn wav_prelude_is_44_byte_riff_header() {
        let mut enc = WavEncoder::with_format(AudioFormat::cd_stereo());
        let header = enc.prelude().expect("header");
        assert_eq!(header.len(), 44);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[36..40], b"data");
    }

    #[test]
    fn channel_mismatch_is_rejected() {
        let mut enc = L16PcmEncoder::with_format(AudioFormat::cd_stereo());
        let mono = AudioFrame::new(vec![0.0, 0.0], 1, 44_100).expect("frame");
        assert!(matches!(
            enc.encode(&mono, &mut Vec::new()),
            Err(Error::Codec(_))
        ));
    }
}
