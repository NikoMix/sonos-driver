//! The [`StreamSink`] implementation: a non-blocking, fan-out broadcast buffer.

use std::sync::Arc;

use audio_core::{Error, Result, StreamSink};
use bytes::Bytes;
use tokio::sync::{broadcast, watch};

use crate::metrics::Metrics;

/// A broadcast-backed sink shared between the encoder thread and the HTTP
/// server.
///
/// [`StreamSink::write`] copies the chunk once into a reference-counted
/// [`Bytes`] and publishes it on a bounded [`tokio::sync::broadcast`] channel.
/// That channel is the per-stream ring buffer: when a slow client falls behind,
/// the *oldest* buffered chunks are discarded for that client only — the
/// producer never blocks and other clients keep receiving the live edge. The
/// single copy lets the fan-out to N connections be N cheap `Arc` clones.
#[derive(Debug)]
pub(crate) struct BroadcastSink {
    tx: broadcast::Sender<Bytes>,
    metrics: Arc<Metrics>,
    closed_tx: watch::Sender<bool>,
}

impl BroadcastSink {
    pub(crate) fn new(
        tx: broadcast::Sender<Bytes>,
        metrics: Arc<Metrics>,
        closed_tx: watch::Sender<bool>,
    ) -> Self {
        Self {
            tx,
            metrics,
            closed_tx,
        }
    }
}

impl StreamSink for BroadcastSink {
    fn write(&self, chunk: &[u8]) -> Result<()> {
        if *self.closed_tx.borrow() {
            return Err(Error::transport("stream sink is closed"));
        }
        if chunk.is_empty() {
            return Ok(());
        }

        let bytes = Bytes::copy_from_slice(chunk);
        let len = bytes.len() as u64;
        self.metrics.record_write(len);

        // `send` errors only when there are currently no subscribers, which is
        // normal before any player has connected. The bytes are still counted
        // above; there is simply no live client to receive them yet.
        let _ = self.tx.send(bytes);
        Ok(())
    }

    fn close(&self) {
        // Signal every connection handler to finish its response cleanly. A send
        // error just means the server has already been torn down.
        let _ = self.closed_tx.send(true);
    }
}
