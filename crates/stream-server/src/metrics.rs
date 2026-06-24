//! Lock-free server telemetry counters and their public snapshot.
//!
//! The counters are plain relaxed atomics: they are observational only and
//! never gate the audio hot path, so the weakest ordering that keeps each
//! counter individually monotonic is sufficient.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A consistent point-in-time view of the stream server's counters.
///
/// Returned by [`crate::StreamServer::metrics`] for dashboards, tests and the
/// orchestration layer. Values are monotonic for the lifetime of the server
/// except `connected_clients`, which rises and falls with live connections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerMetrics {
    /// Clients currently connected to the streaming endpoint.
    pub connected_clients: u64,
    /// Total bytes accepted from the encoder via `StreamSink::write`.
    pub total_bytes: u64,
    /// Total chunks accepted from the encoder.
    pub total_chunks: u64,
    /// Chunks dropped for lagging (slow) clients, summed across every client.
    pub dropped_chunks: u64,
}

/// Internal, shared, atomic counters backing [`ServerMetrics`].
#[derive(Debug, Default)]
pub(crate) struct Metrics {
    connected_clients: AtomicU64,
    total_bytes: AtomicU64,
    total_chunks: AtomicU64,
    dropped_chunks: AtomicU64,
}

impl Metrics {
    /// Record one chunk of `bytes` accepted from the producer.
    pub(crate) fn record_write(&self, bytes: u64) {
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.total_chunks.fetch_add(1, Ordering::Relaxed);
    }

    /// Record `chunks` dropped because a client could not keep up.
    pub(crate) fn record_drops(&self, chunks: u64) {
        self.dropped_chunks.fetch_add(chunks, Ordering::Relaxed);
    }

    /// Take a consistent-enough snapshot of all counters.
    pub(crate) fn snapshot(&self) -> ServerMetrics {
        ServerMetrics {
            connected_clients: self.connected_clients.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            total_chunks: self.total_chunks.load(Ordering::Relaxed),
            dropped_chunks: self.dropped_chunks.load(Ordering::Relaxed),
        }
    }

    /// Register a newly connected client, returning a guard that deregisters it
    /// when dropped. Tying the decrement to a guard makes the count correct even
    /// if a connection task ends early or panics.
    pub(crate) fn client_connected(self: &Arc<Self>) -> ClientGuard {
        self.connected_clients.fetch_add(1, Ordering::Relaxed);
        ClientGuard {
            metrics: Arc::clone(self),
        }
    }
}

/// Decrements the connected-client counter when a connection handler ends.
pub(crate) struct ClientGuard {
    metrics: Arc<Metrics>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.metrics
            .connected_clients
            .fetch_sub(1, Ordering::Relaxed);
    }
}
