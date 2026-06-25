//! Streaming FLAC encoder backed by the pure-Rust [`flacenc`] crate.
//!
//! [`FlacEncoder`] turns the pipeline's canonical interleaved `f32` frames into
//! a standards-compliant FLAC stream:
//!
//! * [`prelude`](FlacEncoder::prelude) emits the `fLaC` magic and the
//!   `STREAMINFO` metadata block once, before any audio.
//! * [`encode`](FlacEncoder::encode) buffers samples and emits one FLAC frame
//!   per full encoder block, appending the bytes to the caller's buffer.
//! * [`finish`](FlacEncoder::finish) flushes the remaining partial block as a
//!   final (shorter) frame, which the FLAC format explicitly permits.
//!
//! Samples are quantised to 16-bit with the exact same mapping as the L16/WAV
//! encoders (see [`crate::pcm`]), so switching profiles never changes the
//! integer signal. The encoder is single-stream and stateful: construct one per
//! output stream and drive it through the `prelude -> encode* -> finish`
//! lifecycle.

use audio_core::{AudioFormat, AudioFrame, CodecProfile, Encoder, Error, Result};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::config::Encoder as FlacConfig;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Fill, FrameBuf};

use crate::pcm::{ensure_channels, f32_to_i16};

/// Sample depth emitted by the encoder. Fixed at 16-bit to match the L16/WAV
/// PCM encoders and the bridge's canonical CD-quality wire format.
const BITS_PER_SAMPLE: usize = 16;

/// Format a `flacenc` error (all of which derive [`Debug`]) into an
/// [`audio_core::Error`] carrying the codec context.
fn flac_error(context: &str, err: impl core::fmt::Debug) -> Error {
    Error::codec(format!("flac: {context}: {err:?}"))
}

/// Streaming FLAC encoder for one output stream.
///
/// See the [module documentation](self) for the lifecycle and quantisation
/// details.
#[derive(Debug)]
pub struct FlacEncoder {
    format: AudioFormat,
    config: Verified<FlacConfig>,
    stream_info: StreamInfo,
    /// Reusable per-frame sample buffer, allocated once at the encoder block
    /// size and refilled for every frame (including the shorter final frame).
    framebuf: FrameBuf,
    block_size: usize,
    frame_number: usize,
    /// Interleaved 16-bit samples (widened to `i32` for `flacenc`) captured so
    /// far but not yet emitted as a full FLAC frame.
    pending: Vec<i32>,
}

impl FlacEncoder {
    /// Create a FLAC encoder for the default CD-stereo output format.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] if the encoder configuration or stream metadata
    /// cannot be constructed (only for out-of-range formats).
    pub fn new() -> Result<Self> {
        Self::with_format(AudioFormat::cd_stereo())
    }

    /// Create a FLAC encoder for a specific output format.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] if `format` is outside the ranges FLAC supports
    /// (e.g. zero channels, more than eight channels, or a sample rate that is
    /// not representable in the 20-bit `STREAMINFO` field).
    pub fn with_format(format: AudioFormat) -> Result<Self> {
        let channels = usize::from(format.channels);
        let config = FlacConfig::default()
            .into_verified()
            .map_err(|err| flac_error("invalid encoder configuration", err))?;
        let block_size = config.block_size;

        let mut stream_info =
            StreamInfo::new(format.sample_rate as usize, channels, BITS_PER_SAMPLE)
                .map_err(|err| flac_error("invalid stream parameters", err))?;
        // Signal a fixed block size (the FLAC reference encoder's convention).
        // The trailing partial frame may legitimately be smaller; decoders are
        // required to accept that.
        stream_info
            .set_block_sizes(block_size, block_size)
            .map_err(|err| flac_error("invalid block size", err))?;
        // Frame sizes are unknown up front in a streaming context; 0 means
        // "unknown" per the FLAC specification.
        stream_info
            .set_frame_sizes(0, 0)
            .map_err(|err| flac_error("invalid frame size", err))?;

        let framebuf = FrameBuf::with_size(channels, block_size)
            .map_err(|err| flac_error("cannot allocate frame buffer", err))?;

        Ok(Self {
            format,
            config,
            stream_info,
            framebuf,
            block_size,
            frame_number: 0,
            pending: Vec::new(),
        })
    }

    /// Encode exactly the `interleaved` samples as one FLAC frame and append the
    /// bytes to `out`.
    ///
    /// `interleaved` may be shorter than a full block: it is loaded into the
    /// reusable [`FrameBuf`], whose `filled_size` then determines the frame's
    /// block size. This is how the final partial frame is emitted without
    /// re-allocating a sub-minimum buffer.
    ///
    /// Taken as free-standing field references (rather than `&mut self`) so the
    /// caller can borrow `pending` immutably for the sample slice while this
    /// borrows the buffer/output mutably.
    fn write_frame(
        config: &Verified<FlacConfig>,
        stream_info: &StreamInfo,
        framebuf: &mut FrameBuf,
        frame_number: usize,
        interleaved: &[i32],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        framebuf
            .fill_interleaved(interleaved)
            .map_err(|err| flac_error("cannot load samples", err))?;
        let frame = flacenc::encode_fixed_size_frame(config, framebuf, frame_number, stream_info)
            .map_err(|err| flac_error("frame encode failed", err))?;

        let mut sink = ByteSink::new();
        frame
            .write(&mut sink)
            // `ByteSink`'s sink error is `Infallible`, so this never fires; map
            // it without the (non-`Debug`) error value to keep `?` ergonomic.
            .map_err(|_| Error::codec("flac: failed to serialise frame"))?;
        out.extend_from_slice(sink.as_slice());
        Ok(())
    }
}

impl Encoder for FlacEncoder {
    fn profile(&self) -> CodecProfile {
        CodecProfile::Flac
    }

    fn prelude(&mut self) -> Result<Vec<u8>> {
        // A frame-less `Stream` serialises to exactly the `fLaC` magic plus the
        // `STREAMINFO` metadata block (marked as the last block), i.e. the
        // stream header that must precede the audio frames.
        let header = Stream::with_stream_info(self.stream_info.clone());
        let mut sink = ByteSink::new();
        header
            .write(&mut sink)
            .map_err(|_| Error::codec("flac: failed to serialise stream header"))?;
        Ok(sink.into_inner())
    }

    fn encode(&mut self, frame: &AudioFrame, out: &mut Vec<u8>) -> Result<()> {
        ensure_channels(frame, self.format.channels)?;

        let block_len = self.block_size * usize::from(self.format.channels);
        self.pending
            .extend(frame.samples().iter().map(|&s| i32::from(f32_to_i16(s))));

        let mut consumed = 0;
        while self.pending.len() - consumed >= block_len {
            let block = &self.pending[consumed..consumed + block_len];
            Self::write_frame(
                &self.config,
                &self.stream_info,
                &mut self.framebuf,
                self.frame_number,
                block,
                out,
            )?;
            self.frame_number += 1;
            consumed += block_len;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<u8>) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        // Move the remainder out so the sample slice and the buffer are borrowed
        // from disjoint places; emit it as the final (shorter) frame.
        let block = std::mem::take(&mut self.pending);
        Self::write_frame(
            &self.config,
            &self.stream_info,
            &mut self.framebuf,
            self.frame_number,
            &block,
            out,
        )?;
        self.frame_number += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claxon::FlacReader;
    use std::io::Cursor;

    /// Deterministic, non-clipping test signal in `[-0.9, 0.9]` so the
    /// `f32 -> i16` quantisation is exact and reproducible.
    ///
    /// The `usize -> f32` casts are lossless for the small, bounded indices used
    /// by these tests, and—because encode and the expected vector call this same
    /// function—any rounding would be identical on both sides regardless.
    #[allow(clippy::cast_precision_loss)]
    fn sample_at(index: usize, channel: usize) -> f32 {
        let t = index as f32;
        let phase = 0.05 * t + channel as f32 * 0.5;
        0.6 * phase.sin() + 0.3 * (0.013 * t).cos()
    }

    /// Build interleaved frames of `per_frame` samples-per-channel each, totalling
    /// `total` samples per channel.
    fn build_frames(total: usize, per_frame: usize, channels: u16) -> Vec<Vec<f32>> {
        let mut frames = Vec::new();
        let mut produced = 0;
        let mut index = 0;
        while produced < total {
            let this = per_frame.min(total - produced);
            let mut buf = Vec::with_capacity(this * usize::from(channels));
            for _ in 0..this {
                for ch in 0..usize::from(channels) {
                    buf.push(sample_at(index, ch));
                }
                index += 1;
            }
            frames.push(buf);
            produced += this;
        }
        frames
    }

    fn expected_i32(total: usize, channels: u16) -> Vec<i32> {
        let mut out = Vec::with_capacity(total * usize::from(channels));
        for index in 0..total {
            for ch in 0..usize::from(channels) {
                out.push(i32::from(f32_to_i16(sample_at(index, ch))));
            }
        }
        out
    }

    fn encode_stream(format: AudioFormat, frames: &[Vec<f32>]) -> Vec<u8> {
        let mut enc = FlacEncoder::with_format(format).expect("construct encoder");
        let mut out = enc.prelude().expect("prelude");
        for samples in frames {
            let frame = AudioFrame::new(samples.clone(), format.channels, format.sample_rate)
                .expect("frame");
            enc.encode(&frame, &mut out).expect("encode");
        }
        enc.finish(&mut out).expect("finish");
        out
    }

    fn decode_interleaved(bytes: &[u8]) -> (u32, u32, Vec<i32>) {
        let mut reader = FlacReader::new(Cursor::new(bytes)).expect("flac reader");
        let info = reader.streaminfo();
        let samples = reader
            .samples()
            .collect::<std::result::Result<Vec<i32>, _>>()
            .expect("decode samples");
        (info.sample_rate, info.channels, samples)
    }

    fn assert_round_trip(total: usize, per_frame: usize, format: AudioFormat) {
        let frames = build_frames(total, per_frame, format.channels);
        let bytes = encode_stream(format, &frames);

        // Sanity: the stream starts with the FLAC magic from the prelude.
        assert_eq!(&bytes[0..4], b"fLaC", "stream must begin with fLaC magic");

        let (sample_rate, channels, decoded) = decode_interleaved(&bytes);
        assert_eq!(
            sample_rate, format.sample_rate,
            "sample rate must round-trip"
        );
        assert_eq!(
            channels,
            u32::from(format.channels),
            "channel count must round-trip"
        );
        assert_eq!(
            decoded,
            expected_i32(total, format.channels),
            "FLAC must decode back to the exact quantised samples"
        );
    }

    #[test]
    fn round_trip_block_aligned_stereo() {
        // Two full 4096-sample blocks, no remainder: finish() is a no-op.
        assert_round_trip(8192, 1000, AudioFormat::cd_stereo());
    }

    #[test]
    fn round_trip_partial_final_frame_stereo() {
        // Full blocks plus a sizable remainder flushed by finish().
        assert_round_trip(10_000, 1024, AudioFormat::cd_stereo());
    }

    #[test]
    fn round_trip_tiny_final_frame_stereo() {
        // One full block plus a sub-MIN_BLOCK_SIZE tail (7 samples/channel),
        // proving the final short frame path is lossless.
        assert_round_trip(4103, 4096, AudioFormat::cd_stereo());
    }

    #[test]
    fn round_trip_shorter_than_one_block_stereo() {
        // The whole stream is a single partial frame emitted by finish().
        assert_round_trip(500, 128, AudioFormat::cd_stereo());
    }

    #[test]
    fn round_trip_mono() {
        let format =
            AudioFormat::new(48_000, 1, audio_core::SampleFormat::I16).expect("mono format");
        assert_round_trip(5000, 333, format);
    }

    #[test]
    fn prelude_is_flac_header() {
        let mut enc = FlacEncoder::new().expect("encoder");
        let header = enc.prelude().expect("prelude");
        assert_eq!(&header[0..4], b"fLaC", "header begins with fLaC magic");
        // "fLaC" (4) + metadata block header (4) + STREAMINFO body (34) = 42.
        assert_eq!(header.len(), 42, "streaming header is magic + STREAMINFO");
    }

    #[test]
    fn content_type_is_audio_flac() {
        let enc = FlacEncoder::new().expect("encoder");
        assert_eq!(enc.profile(), CodecProfile::Flac);
        assert_eq!(enc.content_type().as_ref(), "audio/flac");
    }

    #[test]
    fn channel_mismatch_is_rejected() {
        let mut enc = FlacEncoder::with_format(AudioFormat::cd_stereo()).expect("encoder");
        let mono = AudioFrame::new(vec![0.0, 0.0], 1, 44_100).expect("frame");
        assert!(matches!(
            enc.encode(&mono, &mut Vec::new()),
            Err(Error::Codec(_))
        ));
    }
}
