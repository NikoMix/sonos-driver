//! Codec abstractions: [`CodecProfile`], [`LatencyClass`] and the [`Encoder`]
//! trait implemented by the concrete encoders in the `audio-codec` crate.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::{AudioFrame, Result};

/// A coarse latency category used to communicate the realistic end-to-end delay
/// of a streaming profile to the user.
///
/// Sonos enforces a non-configurable internal playback buffer, so even the
/// lowest class cannot reach real-time (<100 ms) figures over the network — see
/// `docs/LATENCY-BUDGET.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LatencyClass {
    /// Lowest achievable network latency (~0.35–0.85 s). Uncompressed PCM.
    Low,
    /// A balance of latency and bandwidth (~0.4–1.0 s). Lossless compression.
    Balanced,
    /// Highest reliability/quality, multi-second buffering (~1.2–3.0 s).
    High,
}

impl LatencyClass {
    /// A human-readable, approximate end-to-end latency range, including the
    /// fixed Sonos buffer.
    #[must_use]
    pub const fn approx_range(self) -> &'static str {
        match self {
            Self::Low => "~350-850 ms",
            Self::Balanced => "~400-1000 ms",
            Self::High => "~1200-3000 ms",
        }
    }
}

/// The set of streaming codecs the bridge can produce.
///
/// Each profile pairs a wire codec with a [`LatencyClass`]. The default,
/// [`CodecProfile::Flac`], is the most broadly compatible with Sonos players,
/// which reject headerless `audio/L16` and live-edge `audio/wav` (see
/// ADR-0016); [`CodecProfile::L16Pcm`] stays selectable for the lowest latency
/// on renderers that accept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecProfile {
    /// Raw big-endian 16-bit PCM (`audio/L16`), the lowest-latency option.
    L16Pcm,
    /// RIFF/WAVE-framed little-endian 16-bit PCM, for broad client tolerance.
    Wav,
    /// FLAC lossless compression (pure-Rust encoder), the quality default.
    Flac,
    /// AAC — optional compatibility encoder, license permitting.
    Aac,
    /// MP3 — optional compatibility encoder, license permitting.
    Mp3,
}

impl CodecProfile {
    /// The default profile.
    ///
    /// [`CodecProfile::Flac`] is the default because it is the only profile
    /// reliably playable on real Sonos hardware over the live-edge HTTP
    /// broadcast: its frames are self-describing, so a player joining the
    /// stream mid-flight can decode them without the one-time container header,
    /// whereas raw `audio/L16` and live `audio/wav` are rejected (see
    /// ADR-0016). It stays lossless at roughly half the bandwidth of PCM.
    #[must_use]
    pub const fn default_profile() -> Self {
        Self::Flac
    }

    /// The MIME type advertised to Sonos for this profile.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::L16Pcm => "audio/L16",
            Self::Wav => "audio/wav",
            Self::Flac => "audio/flac",
            Self::Aac => "audio/aac",
            Self::Mp3 => "audio/mpeg",
        }
    }

    /// The latency category this profile belongs to.
    #[must_use]
    pub const fn latency_class(self) -> LatencyClass {
        match self {
            Self::L16Pcm => LatencyClass::Low,
            Self::Wav | Self::Flac => LatencyClass::Balanced,
            Self::Aac | Self::Mp3 => LatencyClass::High,
        }
    }

    /// Whether the codec preserves the source signal bit-for-bit.
    #[must_use]
    pub const fn is_lossless(self) -> bool {
        matches!(self, Self::L16Pcm | Self::Wav | Self::Flac)
    }

    /// Whether a licence-clean pure-Rust encoder is available for this profile.
    ///
    /// `false` for [`CodecProfile::Aac`] and [`CodecProfile::Mp3`], which have no
    /// shippable encoder (see ADR-0004). Selecting an unimplemented profile is a
    /// configuration error the bridge rejects up front rather than failing mid
    /// session; `audio_codec::encoder_for` is the runtime backstop.
    #[must_use]
    pub const fn has_encoder(self) -> bool {
        matches!(self, Self::L16Pcm | Self::Wav | Self::Flac)
    }

    /// A short description suitable for CLI help and logs.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::L16Pcm => "Uncompressed 16-bit PCM (audio/L16) — lowest latency",
            Self::Wav => "WAVE-framed 16-bit PCM — lossless, widely compatible",
            Self::Flac => "FLAC lossless compression — quality with moderate latency",
            Self::Aac => "AAC lossy compression — compatibility, higher latency",
            Self::Mp3 => "MP3 lossy compression — compatibility, higher latency",
        }
    }

    /// The stable lower-case identifier used in config files, the CLI and log
    /// messages, e.g. `"l16-pcm"`. Matches the `serde` kebab-case rename.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::L16Pcm => "l16-pcm",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::Mp3 => "mp3",
        }
    }
}

impl Default for CodecProfile {
    fn default() -> Self {
        Self::default_profile()
    }
}

/// A streaming encoder: converts canonical [`AudioFrame`]s into a byte stream
/// for one [`CodecProfile`].
///
/// Encoders are stateful and single-stream. The typical lifecycle is:
///
/// ```text
/// prelude()              // optional container/header bytes, emitted once
/// encode(frame, out)*    // called for every captured block
/// finish(out)            // flush any buffered tail when the stream ends
/// ```
///
/// Implementations append to the caller-provided `out` buffer to avoid
/// per-call allocation on the hot path.
pub trait Encoder: Send {
    /// The profile this encoder implements.
    fn profile(&self) -> CodecProfile;

    /// MIME type to advertise to the player.
    ///
    /// Defaults to the profile's nominal MIME type. Header-less wire formats —
    /// notably raw `audio/L16` (RFC 2586), whose byte stream carries no in-band
    /// sample rate or channel count — override this to append the `rate` and
    /// `channels` parameters the player needs to interpret the stream correctly,
    /// returning an owned value via [`Cow::Owned`]. Container formats (WAV, FLAC)
    /// carry this information in-band and keep the borrowed default.
    fn content_type(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.profile().content_type())
    }

    /// Container/stream header bytes emitted once before the first frame.
    ///
    /// Returns an empty vector for header-less streams such as `audio/L16`.
    fn prelude(&mut self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    /// Encode one frame, appending the encoded bytes to `out`.
    fn encode(&mut self, frame: &AudioFrame, out: &mut Vec<u8>) -> Result<()>;

    /// Flush any buffered samples at end-of-stream, appending to `out`.
    fn finish(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let _ = out;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_flac_for_sonos_compatibility() {
        // FLAC is the default because it is the only profile reliably playable
        // over the live-edge broadcast on real Sonos hardware (see ADR-0016).
        assert_eq!(CodecProfile::default(), CodecProfile::Flac);
        assert_eq!(
            CodecProfile::default().latency_class(),
            LatencyClass::Balanced
        );
        assert!(
            CodecProfile::default().is_lossless(),
            "the default must stay lossless"
        );
        assert!(
            CodecProfile::default().has_encoder(),
            "the default must have a shippable encoder"
        );
    }

    #[test]
    fn lossless_classification_is_correct() {
        assert!(CodecProfile::Flac.is_lossless());
        assert!(!CodecProfile::Aac.is_lossless());
    }

    #[test]
    fn has_encoder_is_true_only_for_shippable_profiles() {
        assert!(CodecProfile::L16Pcm.has_encoder());
        assert!(CodecProfile::Wav.has_encoder());
        assert!(CodecProfile::Flac.has_encoder());
        assert!(!CodecProfile::Aac.has_encoder());
        assert!(!CodecProfile::Mp3.has_encoder());
    }

    #[test]
    fn name_matches_serde_kebab_case() {
        assert_eq!(CodecProfile::L16Pcm.name(), "l16-pcm");
        assert_eq!(CodecProfile::Mp3.name(), "mp3");
    }

    #[test]
    fn latency_classes_order_low_to_high() {
        assert!(LatencyClass::Low < LatencyClass::Balanced);
        assert!(LatencyClass::Balanced < LatencyClass::High);
    }
}
