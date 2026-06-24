//! Orchestration core: wires capture -> pipeline -> codec -> transport while
//! driving the Sonos target via UPnP.
//!
//! # Workstream WS-F (Phase 2)
//!
//! [`Bridge`] owns the session lifecycle: it resolves the Sonos target, starts
//! the stream server, points the player at it, then runs the capture → pipeline
//! → encode → publish loop on a dedicated thread, exposing live
//! [`BridgeMetrics`] and a [`ControllableLatency`] snapshot. [`Bridge::run`]
//! resolves the real capture source and Sonos controller and delegates to the
//! dependency-injection seam [`Bridge::run_with`], which the hardware-free
//! end-to-end latency harness drives with a synthetic source and an in-process
//! [`PlayerControl`]. See `docs/adr/0007-orchestration-testability-seams.md`.

#![forbid(unsafe_code)]

pub use audio_capture as capture;
pub use audio_codec as codec;
pub use audio_core as core;
pub use audio_pipeline as pipeline;
pub use sonos_control as control;
pub use stream_server as server;

mod latency;
mod metrics;
mod player;
mod session;

pub use latency::ControllableLatency;
pub use player::PlayerControl;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use audio_core::{AudioSource, CaptureConfig, CodecProfile, Config, LatencyClass, Result};

use crate::latency::LatencyAccumulator;
use crate::metrics::SharedMetrics;
use crate::player::resolve_target;

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
    metrics: Arc<SharedMetrics>,
    latency: Arc<LatencyAccumulator>,
    stop: Arc<AtomicBool>,
}

impl Bridge {
    /// Create a bridge from a fully resolved [`Config`].
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            metrics: Arc::new(SharedMetrics::default()),
            latency: Arc::new(LatencyAccumulator::default()),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The configuration backing this bridge.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A live snapshot of current metrics.
    ///
    /// Safe to call from another thread while [`run`](Self::run) is blocking;
    /// the counters are updated by the encode loop as the session progresses.
    #[must_use]
    pub fn metrics(&self) -> BridgeMetrics {
        self.metrics.snapshot()
    }

    /// A live snapshot of the *controllable* software latency (read → publish),
    /// excluding the fixed Sonos device buffer.
    ///
    /// See `docs/LATENCY-BUDGET.md` and
    /// `docs/adr/0007-orchestration-testability-seams.md`; for the full,
    /// Sonos-buffer-inclusive figure use [`latency_estimate`](Self::latency_estimate).
    #[must_use]
    pub fn controllable_latency(&self) -> ControllableLatency {
        self.latency.snapshot()
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
    /// Resolves the Sonos target from `config.sonos` (a `uuid` takes precedence
    /// over a `room`), opens the platform capture source, then delegates to
    /// [`run_with`](Self::run_with). The stop flag is observed at every loop
    /// boundary; on stop or end-of-stream the session tears down cleanly and
    /// returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns [`audio_core::Error::Config`] when no Sonos target is configured,
    /// [`audio_core::Error::Control`] when the target cannot be found,
    /// [`audio_core::Error::Capture`] when the capture device cannot be opened,
    /// and propagates any stage failure that ends the session.
    pub fn run(&mut self) -> Result<()> {
        let control = resolve_target(&self.config.sonos)?;
        let source = open_capture(&self.config.capture)?;
        self.run_with(source, Box::new(control))
    }

    /// Run the bridge against an injected source and player control.
    ///
    /// This is the dependency-injection seam behind [`run`](Self::run): it skips
    /// device discovery and capture, wiring the supplied [`AudioSource`] and
    /// [`PlayerControl`] through the same session lifecycle. The hardware-free
    /// end-to-end harness uses it to exercise the full loop with a synthetic
    /// source and an in-process player. See
    /// `docs/adr/0007-orchestration-testability-seams.md`.
    ///
    /// # Errors
    ///
    /// Propagates any stage failure (encoder construction, server start,
    /// playback control, or the encode loop) that ends the session.
    pub fn run_with(
        &mut self,
        source: Box<dyn AudioSource>,
        control: Box<dyn PlayerControl>,
    ) -> Result<()> {
        session::run_session(
            &self.config,
            source,
            control,
            &self.stop,
            &self.metrics,
            &self.latency,
        )
    }
}

/// Open the platform capture source for the configured device.
fn open_capture(config: &CaptureConfig) -> Result<Box<dyn AudioSource>> {
    log_capture_mode(config);
    Ok(Box::new(audio_capture::LoopbackCapture::open(config)?))
}

/// Emit a one-line summary of which capture path the configuration selects so
/// operators can confirm process-loopback vs default-device capture at a glance.
///
/// This mirrors the precedence applied inside
/// [`audio_capture::LoopbackCapture::open`]: a non-blank `capture.process`
/// selects per-application loopback (include or exclude depending on
/// `exclude_process`); otherwise the classic default-device loopback runs.
fn log_capture_mode(config: &CaptureConfig) {
    let process = config
        .process
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    match process {
        Some(app) if config.exclude_process => tracing::info!(
            process = app,
            mode = "process-loopback-exclude",
            "capturing the default endpoint while excluding one application's audio tree"
        ),
        Some(app) => tracing::info!(
            process = app,
            mode = "process-loopback-include",
            "capturing a single application's audio tree via WASAPI process loopback"
        ),
        None => tracing::info!(
            device = config.device.as_deref().unwrap_or("<default>"),
            mode = "default-device",
            "capturing a render endpoint via default-device loopback"
        ),
    }
}

#[cfg(test)]
mod tests {
    use audio_core::Error;

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
    fn run_requires_a_configured_target() {
        // The default config names neither a uuid nor a room, so `run` fails
        // fast with a configuration error and never touches the network.
        let mut bridge = Bridge::new(Config::default());
        assert!(matches!(bridge.run(), Err(Error::Config(_))));
    }

    #[test]
    fn fresh_bridge_reports_zeroed_metrics_and_latency() {
        let bridge = Bridge::new(Config::default());
        assert_eq!(bridge.metrics(), BridgeMetrics::default());
        assert_eq!(
            bridge.controllable_latency(),
            ControllableLatency::default()
        );
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
