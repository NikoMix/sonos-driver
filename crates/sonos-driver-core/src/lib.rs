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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use audio_core::{CodecProfile, Config, Error, LatencyClass, Result};

/// A cloneable, thread-safe handle that signals a running [`Bridge`] to stop.
///
/// Obtain one with [`Bridge::stop_handle`] *before* calling [`Bridge::run`]
/// (which blocks until stopped or a stage fails), then signal it from another
/// thread — typically a Ctrl-C handler — to end the session gracefully. The
/// handle only requests a stop; the bridge tears the session down (stopping the
/// player and the stream server) at the next loop boundary. Dropping every
/// handle does **not** stop the bridge.
#[derive(Debug, Clone)]
pub struct StopHandle {
    flag: Arc<AtomicBool>,
}

impl StopHandle {
    /// Request that the associated bridge stop at the next opportunity.
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether a stop has already been requested through this handle (or any
    /// clone of it).
    #[must_use]
    pub fn is_stop_requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

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
    stop: Arc<AtomicBool>,
}

impl Bridge {
    /// Create a bridge from a fully resolved [`Config`].
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            metrics: BridgeMetrics::default(),
            stop: Arc::new(AtomicBool::new(false)),
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

    /// A cloneable handle that stops this bridge while [`run`](Self::run) is
    /// blocking. Acquire it before calling `run`, e.g. to wire a Ctrl-C handler.
    #[must_use]
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle {
            flag: Arc::clone(&self.stop),
        }
    }

    /// The latency estimate for the configured streaming profile.
    #[must_use]
    pub fn latency_estimate(&self) -> LatencyEstimate {
        LatencyEstimate::for_profile(self.config.stream.profile)
    }

    /// Run the bridge until [stopped](StopHandle::stop) or a stage fails.
    ///
    /// # Errors
    ///
    /// Currently returns [`Error::NotImplemented`]; WS-F wires the capture,
    /// encode, transport and control stages together (observing the stop flag
    /// via [`stop_handle`](Self::stop_handle) at each loop boundary) and returns
    /// runtime failures from any stage.
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

    #[test]
    fn stop_handle_signals_through_clones() {
        let bridge = Bridge::new(Config::default());
        let handle = bridge.stop_handle();
        let clone = handle.clone();
        assert!(!handle.is_stop_requested());
        clone.stop();
        // Both the original handle and the bridge's flag observe the request.
        assert!(handle.is_stop_requested());
        assert!(bridge.stop_handle().is_stop_requested());
    }
}
