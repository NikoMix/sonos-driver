//! Controllable-latency telemetry: the software stages the bridge owns,
//! excluding the fixed Sonos device buffer.
//!
//! [`LatencyAccumulator`] records, per published chunk, how long elapsed
//! between a frame being read from the source and the encoded bytes being
//! handed to the transport — i.e. the resample + channel-convert + jitter-drain
//! + encode + publish cost. [`ControllableLatency`] is the public snapshot.
//!
//! Why only these stages? The pipeline rebuilds frames during resampling and
//! does not propagate the capture timestamp through every stage, and the
//! dominant, *fixed* latency contributor — the Sonos playback buffer — is
//! outside the bridge's control. Measuring the controllable software path is
//! both well defined and the only part worth optimising. See ADR-0007 and
//! `docs/LATENCY-BUDGET.md`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A snapshot of the controllable (software) end-to-end latency.
///
/// Every duration describes the per-chunk path from a source read to the
/// transport publish and deliberately excludes the fixed Sonos buffer. A
/// freshly created bridge, or one that has not yet published a chunk, reports
/// the [`Default`] (all-zero) value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllableLatency {
    /// Number of published chunks measured.
    pub samples: u64,
    /// Latency of the most recently published chunk.
    pub last: Duration,
    /// Mean latency across all measured chunks.
    pub mean: Duration,
    /// Largest single-chunk latency observed.
    pub max: Duration,
}

/// Atomic accumulator that records latency samples from the encode thread.
#[derive(Debug, Default)]
pub(crate) struct LatencyAccumulator {
    samples: AtomicU64,
    last_nanos: AtomicU64,
    total_nanos: AtomicU64,
    max_nanos: AtomicU64,
}

impl LatencyAccumulator {
    /// Record one controllable-latency sample.
    ///
    /// Implausibly large values saturate rather than wrap, so the telemetry can
    /// never overflow or panic.
    pub(crate) fn record(&self, elapsed: Duration) {
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.samples.fetch_add(1, Ordering::Relaxed);
        self.last_nanos.store(nanos, Ordering::Relaxed);
        self.total_nanos.fetch_add(nanos, Ordering::Relaxed);
        self.max_nanos.fetch_max(nanos, Ordering::Relaxed);
    }

    /// Take a consistent-enough snapshot of the accumulated latency.
    pub(crate) fn snapshot(&self) -> ControllableLatency {
        let samples = self.samples.load(Ordering::Relaxed);
        let total = self.total_nanos.load(Ordering::Relaxed);
        let mean = total.checked_div(samples).unwrap_or(0);
        ControllableLatency {
            samples,
            last: Duration::from_nanos(self.last_nanos.load(Ordering::Relaxed)),
            mean: Duration::from_nanos(mean),
            max: Duration::from_nanos(self.max_nanos.load(Ordering::Relaxed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ControllableLatency, LatencyAccumulator};

    #[test]
    fn empty_accumulator_snapshots_to_default() {
        let accumulator = LatencyAccumulator::default();
        assert_eq!(accumulator.snapshot(), ControllableLatency::default());
    }

    #[test]
    fn records_mean_last_and_max() {
        let accumulator = LatencyAccumulator::default();
        accumulator.record(Duration::from_millis(10));
        accumulator.record(Duration::from_millis(30));

        let snapshot = accumulator.snapshot();
        assert_eq!(snapshot.samples, 2);
        assert_eq!(snapshot.last, Duration::from_millis(30));
        assert_eq!(snapshot.mean, Duration::from_millis(20));
        assert_eq!(snapshot.max, Duration::from_millis(30));
    }
}
