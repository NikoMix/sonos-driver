//! Shared, lock-free counters backing [`crate::BridgeMetrics`].
//!
//! The orchestration loop updates these atomics from the encode thread while
//! [`crate::Bridge::metrics`] reads a consistent-enough snapshot from any other
//! thread. They are observational only and never gate the audio hot path, so
//! the weakest ordering that keeps each counter individually monotonic
//! (`Relaxed`) is sufficient — mirroring the stream server's own telemetry.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::BridgeMetrics;

/// Atomic counters shared between the encode thread and metric readers.
#[derive(Debug, Default)]
pub(crate) struct SharedMetrics {
    frames_captured: AtomicU64,
    frames_streamed: AtomicU64,
    underruns: AtomicU64,
    reconnects: AtomicU64,
}

impl SharedMetrics {
    /// Record one audio block read from the source.
    pub(crate) fn inc_captured(&self) {
        self.frames_captured.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one encoded chunk published to the stream sink.
    pub(crate) fn inc_streamed(&self) {
        self.frames_streamed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one player reconnection (a re-issued `SetAVTransportURI`+`Play`).
    pub(crate) fn inc_reconnects(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Publish the latest jitter-buffer underrun count.
    ///
    /// The jitter buffer's own counter is monotonic, so `fetch_max` keeps the
    /// surfaced value monotonic even against an out-of-order update.
    pub(crate) fn set_underruns(&self, underruns: u64) {
        self.underruns.fetch_max(underruns, Ordering::Relaxed);
    }

    /// Take a point-in-time snapshot of every counter.
    pub(crate) fn snapshot(&self) -> BridgeMetrics {
        BridgeMetrics {
            frames_captured: self.frames_captured.load(Ordering::Relaxed),
            frames_streamed: self.frames_streamed.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SharedMetrics;

    #[test]
    fn counters_start_at_zero_and_increment() {
        let metrics = SharedMetrics::default();
        assert_eq!(metrics.snapshot(), crate::BridgeMetrics::default());

        metrics.inc_captured();
        metrics.inc_captured();
        metrics.inc_streamed();
        metrics.inc_reconnects();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.frames_captured, 2);
        assert_eq!(snapshot.frames_streamed, 1);
        assert_eq!(snapshot.reconnects, 1);
        assert_eq!(snapshot.underruns, 0);
    }

    #[test]
    fn underruns_are_monotonic() {
        let metrics = SharedMetrics::default();
        metrics.set_underruns(5);
        metrics.set_underruns(3); // a lower value must not regress the counter
        assert_eq!(metrics.snapshot().underruns, 5);
        metrics.set_underruns(8);
        assert_eq!(metrics.snapshot().underruns, 8);
    }
}
