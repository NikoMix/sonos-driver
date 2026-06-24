//! Transport abstractions: where encoded audio bytes are published for Sonos
//! to pull.

use std::sync::Arc;

use crate::Result;

/// A thread-safe sink for encoded audio chunks.
///
/// The encoder thread pushes freshly encoded bytes via [`StreamSink::write`];
/// the transport implementation (an HTTP server in `stream-server`) fans those
/// bytes out to every connected Sonos player. Implementations must be cheap to
/// clone-share (hence [`Arc`]) and safe to call from the real-time thread.
pub trait StreamSink: Send + Sync {
    /// Publish one encoded chunk. Should be non-blocking on the audio thread;
    /// implementations typically hand off to an internal broadcast buffer.
    fn write(&self, chunk: &[u8]) -> Result<()>;

    /// Signal that no further chunks will be written (end of stream), allowing
    /// connected clients to be closed cleanly.
    fn close(&self) {}
}

/// A network transport that exposes a published audio stream at a URL.
///
/// Sonos is instructed (via UPnP `SetAVTransportURI`) to fetch
/// [`Transport::stream_url`]; the bytes it receives are whatever has been
/// written to the transport's [`StreamSink`].
pub trait Transport: Send + Sync {
    /// The absolute URL a Sonos player should fetch to receive the stream,
    /// e.g. `http://192.168.1.50:8080/stream`.
    fn stream_url(&self) -> String;

    /// The MIME type the transport serves, matching the active codec.
    fn content_type(&self) -> &str;

    /// A handle to the sink that encoded chunks are written to.
    fn sink(&self) -> Arc<dyn StreamSink>;
}
