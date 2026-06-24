//! The endpoint mix-format model and pure sample → `f32` conversion.
//!
//! The capture thread reads the device's `WAVEFORMATEX` through WASAPI (in
//! [`crate::wasapi`]) and extracts it into the plain [`WaveFormat`] struct below.
//! Everything in this module is FFI-free and unit-tested, so the
//! format-classification and PCM-to-`f32` decoding logic — the parts most likely
//! to harbour off-by-one or endianness bugs — are verified without hardware.

use audio_core::{AudioFormat, Error, Result, SampleFormat};

/// The salient fields of a WASAPI `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE`,
/// lifted into a plain, `Copy` struct so format handling stays testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaveFormat {
    /// Sampling frequency in Hertz (`nSamplesPerSec`).
    pub(crate) sample_rate: u32,
    /// Interleaved channel count (`nChannels`).
    pub(crate) channels: u16,
    /// Container size of one sample in bits (`wBitsPerSample`); a multiple of 8.
    pub(crate) container_bits: u16,
    /// Valid bits within the container (`wValidBitsPerSample`); equals
    /// `container_bits` for non-extensible formats.
    pub(crate) valid_bits: u16,
    /// Whether samples are IEEE-754 floating point.
    pub(crate) is_float: bool,
}

/// How one interleaved sample is decoded into canonical `f32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decode {
    F32,
    I16,
    /// 24-bit signed PCM packed in exactly 3 bytes.
    I24Packed,
    /// 32-bit signed PCM (also used for 24-valid-in-32 layouts, treated as full
    /// 32-bit and divided by `2^31`; the unused low bits are simply zero).
    I32,
}

impl WaveFormat {
    /// Bytes occupied by one sample (one channel), i.e. `container_bits / 8`.
    pub(crate) const fn bytes_per_sample(self) -> usize {
        self.container_bits as usize / 8
    }

    /// Bytes occupied by one interleaved frame (one sample per channel).
    pub(crate) const fn block_align(self) -> usize {
        self.bytes_per_sample() * self.channels as usize
    }

    /// The canonical [`AudioFormat`] this source reports to the pipeline.
    ///
    /// Captured blocks are always handed on as interleaved `f32` (the workspace's
    /// canonical representation), so the reported [`SampleFormat`] is always
    /// [`SampleFormat::F32`]; the sample rate and channel count reflect the live
    /// endpoint mix format. The native layout is validated here so an
    /// unsupported device format fails fast at `open` rather than mid-stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFormat`] when the native layout cannot be decoded
    /// to `f32`, or when the rate/channel count is zero.
    pub(crate) fn to_audio_format(self) -> Result<AudioFormat> {
        self.decode()?;
        AudioFormat::new(self.sample_rate, self.channels, SampleFormat::F32)
    }

    /// Classify the native byte layout into a [`Decode`] strategy.
    fn decode(self) -> Result<Decode> {
        if self.is_float {
            return match self.container_bits {
                32 => Ok(Decode::F32),
                other => Err(Error::invalid_format(format!(
                    "unsupported {other}-bit float capture format"
                ))),
            };
        }
        match self.container_bits {
            16 => Ok(Decode::I16),
            24 => Ok(Decode::I24Packed),
            32 => Ok(Decode::I32),
            other => Err(Error::invalid_format(format!(
                "unsupported {other}-bit integer capture format"
            ))),
        }
    }
}

/// Decode an interleaved WASAPI block into canonical interleaved `f32`.
///
/// `bytes` is the raw capture buffer; `wave` describes its layout. Any trailing
/// bytes that do not form a whole sample are ignored (WASAPI never produces a
/// partial sample, so this only guards against a malformed call).
///
/// The format is assumed to have been validated via [`WaveFormat::to_audio_format`];
/// an unvalidated, unsupported format yields an empty buffer rather than panicking.
pub(crate) fn samples_to_f32(bytes: &[u8], wave: &WaveFormat) -> Vec<f32> {
    let stride = wave.bytes_per_sample();
    let Ok(decode) = wave.decode() else {
        return Vec::new();
    };
    if stride == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(bytes.len() / stride);
    for chunk in bytes.chunks_exact(stride) {
        out.push(match decode {
            Decode::F32 => f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            Decode::I16 => i16_to_f32([chunk[0], chunk[1]]),
            Decode::I24Packed => i24_to_f32([chunk[0], chunk[1], chunk[2]]),
            Decode::I32 => i32_to_f32([chunk[0], chunk[1], chunk[2], chunk[3]]),
        });
    }
    out
}

/// 16-bit signed PCM → `f32`. `f32::from(i16)` is lossless, so this is exact.
fn i16_to_f32(bytes: [u8; 2]) -> f32 {
    f32::from(i16::from_le_bytes(bytes)) / 32_768.0
}

/// 24-bit signed PCM (packed, little-endian) → `f32`.
#[allow(clippy::cast_precision_loss)] // |value| <= 2^23, exactly representable in f32.
fn i24_to_f32(bytes: [u8; 3]) -> f32 {
    let raw = i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
    // Sign-extend the 24-bit value into the full i32 range.
    let value = (raw << 8) >> 8;
    value as f32 / 8_388_608.0
}

/// 32-bit signed PCM → `f32`.
#[allow(clippy::cast_precision_loss)] // 32-bit PCM exceeds the f32 mantissa; sub-LSB loss is inaudible.
fn i32_to_f32(bytes: [u8; 4]) -> f32 {
    i32::from_le_bytes(bytes) as f32 / 2_147_483_648.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn float_stereo_48k() -> WaveFormat {
        WaveFormat {
            sample_rate: 48_000,
            channels: 2,
            container_bits: 32,
            valid_bits: 32,
            is_float: true,
        }
    }

    #[test]
    fn block_align_and_sample_size() {
        let wave = float_stereo_48k();
        assert_eq!(wave.bytes_per_sample(), 4);
        assert_eq!(wave.block_align(), 8);
    }

    #[test]
    fn reports_canonical_f32_with_live_rate_and_channels() {
        let fmt = float_stereo_48k().to_audio_format().expect("supported");
        assert_eq!(fmt.sample_format, SampleFormat::F32);
        assert_eq!(fmt.sample_rate, 48_000);
        assert_eq!(fmt.channels, 2);
    }

    #[test]
    fn rejects_unsupported_native_layouts() {
        let weird = WaveFormat {
            sample_rate: 48_000,
            channels: 2,
            container_bits: 24,
            valid_bits: 24,
            is_float: true, // 24-bit float does not exist.
        };
        assert!(weird.to_audio_format().is_err());

        let twenty_bit = WaveFormat {
            container_bits: 20,
            valid_bits: 20,
            is_float: false,
            ..float_stereo_48k()
        };
        assert!(twenty_bit.to_audio_format().is_err());
    }

    #[test]
    fn converts_f32_samples_verbatim() {
        let wave = float_stereo_48k();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.0_f32.to_le_bytes());
        bytes.extend_from_slice(&(-0.5_f32).to_le_bytes());
        let out = samples_to_f32(&bytes, &wave);
        assert_eq!(out, vec![1.0, -0.5]);
    }

    #[test]
    fn converts_i16_full_scale() {
        let wave = WaveFormat {
            container_bits: 16,
            valid_bits: 16,
            is_float: false,
            ..float_stereo_48k()
        };
        // i16::MIN, 0, i16::MAX
        let bytes = [0x00, 0x80, 0x00, 0x00, 0xFF, 0x7F];
        let out = samples_to_f32(&bytes, &wave);
        assert_eq!(out.len(), 3);
        assert!((out[0] - -1.0).abs() < 1e-7);
        assert!(out[1].abs() < 1e-7);
        assert!((out[2] - 0.999_97).abs() < 1e-4);
    }

    #[test]
    fn converts_i24_packed_full_scale() {
        let wave = WaveFormat {
            container_bits: 24,
            valid_bits: 24,
            is_float: false,
            ..float_stereo_48k()
        };
        // -2^23 (0x800000 LE) and 2^23 - 1 (0x7FFFFF LE)
        let bytes = [0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F];
        let out = samples_to_f32(&bytes, &wave);
        assert_eq!(out.len(), 2);
        assert!((out[0] - -1.0).abs() < 1e-7);
        assert!((out[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn converts_i32_full_scale() {
        let wave = WaveFormat {
            container_bits: 32,
            valid_bits: 32,
            is_float: false,
            ..float_stereo_48k()
        };
        // i32::MIN and i32::MAX (LE)
        let bytes = [0x00, 0x00, 0x00, 0x80, 0xFF, 0xFF, 0xFF, 0x7F];
        let out = samples_to_f32(&bytes, &wave);
        assert_eq!(out.len(), 2);
        assert!((out[0] - -1.0).abs() < 1e-7);
        assert!((out[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ignores_trailing_partial_sample() {
        let wave = WaveFormat {
            container_bits: 16,
            valid_bits: 16,
            is_float: false,
            ..float_stereo_48k()
        };
        // 3 bytes: one whole i16 sample plus a dangling byte.
        let bytes = [0x00, 0x00, 0x11];
        let out = samples_to_f32(&bytes, &wave);
        assert_eq!(out, vec![0.0]);
    }
}
