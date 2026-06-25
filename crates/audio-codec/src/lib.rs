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
//! * [`CodecProfile::Flac`] — implemented via the pure-Rust `flacenc` crate
//!   ([`FlacEncoder`]), the lossless quality default.
//! * [`CodecProfile::Aac`] / [`CodecProfile::Mp3`] — unsupported: no
//!   licence-clean, pure-Rust encoder exists. [`encoder_for`] returns
//!   [`audio_core::Error::NotImplemented`] for them (see
//!   `docs/adr/0004-aac-mp3-codec-evaluation.md`).

#![forbid(unsafe_code)]

mod flac;
mod pcm;

pub use flac::FlacEncoder;
pub use pcm::{L16PcmEncoder, WavEncoder};

use audio_core::{AudioFormat, CodecProfile, Encoder, Error, Result};

/// Construct a boxed [`Encoder`] for the requested profile and output format.
///
/// The `format` describes the wire format (sample rate and channel count) the
/// encoder targets; it parameterises the WAV header, the FLAC `STREAMINFO`
/// block, and channel validation.
///
/// # Errors
///
/// Returns [`Error::NotImplemented`] for the AAC and MP3 profiles, which have no
/// licence-clean pure-Rust encoder (see `docs/adr/0004-aac-mp3-codec-evaluation.md`).
/// Returns [`Error::Codec`] if the FLAC encoder cannot be constructed for
/// `format` (only for formats outside FLAC's supported ranges).
pub fn encoder_for(profile: CodecProfile, format: AudioFormat) -> Result<Box<dyn Encoder>> {
    match profile {
        CodecProfile::L16Pcm => Ok(Box::new(L16PcmEncoder::with_format(format))),
        CodecProfile::Wav => Ok(Box::new(WavEncoder::with_format(format))),
        CodecProfile::Flac => Ok(Box::new(FlacEncoder::with_format(format)?)),
        CodecProfile::Aac => Err(Error::NotImplemented(
            "audio-codec: no licence-clean pure-Rust AAC encoder (see ADR-0004)",
        )),
        CodecProfile::Mp3 => Err(Error::NotImplemented(
            "audio-codec: no licence-clean pure-Rust MP3 encoder (see ADR-0004)",
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
    fn returns_encoder_for_flac_profile() {
        let enc = encoder_for(CodecProfile::Flac, AudioFormat::cd_stereo()).expect("flac encoder");
        assert_eq!(enc.profile(), CodecProfile::Flac);
        assert_eq!(enc.content_type().as_ref(), "audio/flac");
    }

    #[test]
    fn lossy_profiles_pending_workstream() {
        for profile in [CodecProfile::Aac, CodecProfile::Mp3] {
            assert!(matches!(
                encoder_for(profile, AudioFormat::cd_stereo()),
                Err(Error::NotImplemented(_))
            ));
        }
    }
}
