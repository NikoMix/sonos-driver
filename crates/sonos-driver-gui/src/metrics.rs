//! Telemetry shared between the audio worker and the UI.
//!
//! Two complementary mechanisms keep the UI responsive while never touching the
//! real-time audio path:
//!
//! * [`LiveCounter`] is a lock-free, shared atomic the capture decorator
//!   increments per block and the UI reads each frame for a *live*
//!   `frames-captured` signal.
//! * [`MetricsSnapshot`] is the authoritative end-of-session copy of
//!   [`sonos_driver_core::BridgeMetrics`], delivered to the UI over a channel
//!   once the session ends.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sonos_driver_core::BridgeMetrics;

/// A lock-free counter of captured audio blocks, shared between the capture
/// decorator (writer) and the UI (reader).
///
/// Cloning yields another handle onto the *same* underlying counter, so the
/// audio worker and the UI observe one shared value without locking.
#[derive(Debug, Clone, Default)]
pub struct LiveCounter {
    captured: Arc<AtomicU64>,
}

impl LiveCounter {
    /// Create a fresh counter starting at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that one audio block was captured.
    pub fn record_captured(&self) {
        self.captured.fetch_add(1, Ordering::Relaxed);
    }

    /// The number of audio blocks captured so far.
    #[must_use]
    pub fn captured(&self) -> u64 {
        self.captured.load(Ordering::Relaxed)
    }
}

/// A plain, copyable view of the bridge's session counters for display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Audio blocks captured since the session started.
    pub captured: u64,
    /// Audio blocks successfully encoded and published.
    pub streamed: u64,
    /// Buffer underruns observed (capture starved the encoder).
    pub underruns: u64,
    /// Times the player connection was re-established.
    pub reconnects: u64,
}

impl From<BridgeMetrics> for MetricsSnapshot {
    fn from(metrics: BridgeMetrics) -> Self {
        Self {
            captured: metrics.frames_captured,
            streamed: metrics.frames_streamed,
            underruns: metrics.underruns,
            reconnects: metrics.reconnects,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveCounter, MetricsSnapshot};
    use sonos_driver_core::BridgeMetrics;

    #[test]
    fn live_counter_is_shared_across_clones() {
        let writer = LiveCounter::new();
        let reader = writer.clone();
        assert_eq!(reader.captured(), 0);
        writer.record_captured();
        writer.record_captured();
        // The clone observes the writer's increments: one shared counter.
        assert_eq!(reader.captured(), 2);
    }

    #[test]
    fn snapshot_maps_every_bridge_metric() {
        let metrics = BridgeMetrics {
            frames_captured: 11,
            frames_streamed: 9,
            underruns: 2,
            reconnects: 1,
        };
        let snapshot = MetricsSnapshot::from(metrics);
        assert_eq!(
            snapshot,
            MetricsSnapshot {
                captured: 11,
                streamed: 9,
                underruns: 2,
                reconnects: 1,
            }
        );
    }
}
