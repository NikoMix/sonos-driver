//! Streaming audio encoders for the bridge.
//!
//! Every encoder implements [`audio_core::Encoder`], turning canonical
//! [`audio_core::AudioFrame`]s into a byte stream for one
//! [`audio_core::CodecProfile`]. Use [`encoder_for`] to obtain a boxed encoder
//! for a profile.
//!
//! # Status
//!
//! * [`CodecProfile::L16Pcm`] and [`CodecProfile::Wav`] — implemented here
//!   (uncompressed, the lowest-latency paths).
//! * [`CodecProfile::Flac`] — implemented in WS-D (pure-Rust `flacenc`).
//! * [`CodecProfile::Aac`] / [`CodecProfile::Mp3`] — optional compatibility
//!   encoders evaluated in WS-D, license permitting.

mod pcm;

pub use pcm::{L16PcmEncoder, WavEncoder};

use audio_core::{AudioFormat, CodecProfile, Encoder, Error, Result};

/// Construct a boxed [`Encoder`] for the requested profile and output format.
///
/// The `format` describes the wire format (sample rate and channel count) the
/// encoder targets; for the uncompressed profiles it parameterises the WAV
/// header and channel validation.
///
/// # Errors
///
/// Returns [`Error::NotImplemented`] for profiles whose encoders are scheduled
/// for a later workstream (FLAC/AAC/MP3).
pub fn encoder_for(profile: CodecProfile, format: AudioFormat) -> Result<Box<dyn Encoder>> {
    match profile {
        CodecProfile::L16Pcm => Ok(Box::new(L16PcmEncoder::with_format(format))),
        CodecProfile::Wav => Ok(Box::new(WavEncoder::with_format(format))),
        CodecProfile::Flac => Err(Error::NotImplemented(
            "audio-codec: FLAC encoder is implemented in WS-D",
        )),
        CodecProfile::Aac => Err(Error::NotImplemented(
            "audio-codec: AAC encoder is implemented in WS-D",
        )),
        CodecProfile::Mp3 => Err(Error::NotImplemented(
            "audio-codec: MP3 encoder is implemented in WS-D",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_encoders_for_uncompressed_profiles() {
        let fmt = AudioFormat::cd_stereo();
        assert!(encoder_for(CodecProfile::L16Pcm, fmt).is_ok());
        assert!(encoder_for(CodecProfile::Wav, fmt).is_ok());
    }

    #[test]
    fn compressed_profiles_pending_workstream() {
        assert!(matches!(
            encoder_for(CodecProfile::Flac, AudioFormat::cd_stereo()),
            Err(Error::NotImplemented(_))
        ));
    }
}
