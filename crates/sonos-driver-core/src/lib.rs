//! Orchestration core: wires capture -> pipeline -> codec -> transport while
//! driving the Sonos target via UPnP.
//!
//! # Workstream WS-F (Phase 2)
//!
//! [`Bridge`] owns the session lifecycle: it starts the stream server, points
//! the selected Sonos room at it, then runs the capture/encode loop on a
//! dedicated thread, exposing live [`BridgeMetrics`]. Phase 0 establishes the
//! public surface and a re-export of the building-block crates so the CLI and
//! integration tests compile against a stable API; the run loop is implemented
//! in WS-F.

pub use audio_capture as capture;
pub use audio_codec as codec;
pub use audio_core as core;
pub use audio_pipeline as pipeline;
pub use sonos_control as control;
pub use stream_server as server;

use audio_core::{CodecProfile, Config, Error, LatencyClass, Result};

/// Live counters describing a running bridge session, surfaced for telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BridgeMetrics {
    /// Audio blocks captured since the session started.
    pub frames_captured: u64,
    /// Audio blocks successfully encoded and published.
    pub frames_streamed: u64,
    /// Buffer underruns observed (capture starved the encoder).
    pub underruns: u64,
    /// Times the player connection was re-established.
    pub reconnects: u64,
}

/// A coarse, human-readable estimate of end-to-end latency for a profile,
/// including the fixed Sonos buffer. See `docs/LATENCY-BUDGET.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyEstimate {
    /// The profile this estimate is for.
    pub profile: CodecProfile,
    /// The latency category.
    pub class: LatencyClass,
    /// Approximate range, e.g. "~350-850 ms".
    pub approx_range: &'static str,
}

impl LatencyEstimate {
    /// Compute the estimate for a profile.
    #[must_use]
    pub fn for_profile(profile: CodecProfile) -> Self {
        let class = profile.latency_class();
        Self {
            profile,
            class,
            approx_range: class.approx_range(),
        }
    }
}

/// The end-to-end audio bridge.
#[derive(Debug)]
pub struct Bridge {
    config: Config,
    metrics: BridgeMetrics,
}

impl Bridge {
    /// Create a bridge from a fully resolved [`Config`].
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            metrics: BridgeMetrics::default(),
        }
    }

    /// The configuration backing this bridge.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A snapshot of current metrics.
    #[must_use]
    pub fn metrics(&self) -> BridgeMetrics {
        self.metrics
    }

    /// The latency estimate for the configured streaming profile.
    #[must_use]
    pub fn latency_estimate(&self) -> LatencyEstimate {
        LatencyEstimate::for_profile(self.config.stream.profile)
    }

    /// Run the bridge until cancelled.
    ///
    /// # Errors
    ///
    /// Currently returns [`Error::NotImplemented`]; WS-F wires the capture,
    /// encode, transport and control stages together and returns runtime
    /// failures from any stage.
    pub fn run(&mut self) -> Result<()> {
        Err(Error::NotImplemented(
            "sonos-driver-core: the capture/encode/stream loop is implemented in WS-F",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_estimate_tracks_profile() {
        let bridge = Bridge::new(Config::default());
        let est = bridge.latency_estimate();
        assert_eq!(est.profile, CodecProfile::L16Pcm);
        assert_eq!(est.class, LatencyClass::Low);
        assert_eq!(est.approx_range, "~350-850 ms");
    }

    #[test]
    fn run_reports_not_implemented_for_now() {
        let mut bridge = Bridge::new(Config::default());
        assert!(matches!(bridge.run(), Err(Error::NotImplemented(_))));
    }
}
