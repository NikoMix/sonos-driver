//! Codec abstractions: [`CodecProfile`], [`LatencyClass`] and the [`Encoder`]
//! trait implemented by the concrete encoders in the `audio-codec` crate.

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
/// [`CodecProfile::L16Pcm`], targets the lowest latency Sonos permits.
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
    /// The default profile: lowest achievable latency.
    #[must_use]
    pub const fn default_profile() -> Self {
        Self::L16Pcm
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

    /// MIME type to advertise to the player (defaults to the profile's).
    fn content_type(&self) -> &'static str {
        self.profile().content_type()
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
    fn default_profile_is_lowest_latency() {
        assert_eq!(CodecProfile::default(), CodecProfile::L16Pcm);
        assert_eq!(CodecProfile::default().latency_class(), LatencyClass::Low);
    }

    #[test]
    fn lossless_classification_is_correct() {
        assert!(CodecProfile::Flac.is_lossless());
        assert!(!CodecProfile::Aac.is_lossless());
    }

    #[test]
    fn latency_classes_order_low_to_high() {
        assert!(LatencyClass::Low < LatencyClass::Balanced);
        assert!(LatencyClass::Balanced < LatencyClass::High);
    }
}
