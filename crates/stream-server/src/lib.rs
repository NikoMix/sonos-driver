//! Local HTTP server that publishes the encoded audio stream to Sonos.
//!
//! # Workstream WS-C (Phase 1)
//!
//! Sonos pulls audio over HTTP, so the bridge runs a small async server that:
//!
//! * serves a single **chunked-transfer** endpoint per active stream,
//! * fans the encoder's output out to every connected player (grouped rooms),
//! * keeps per-connection buffering minimal — a slow player drops the *oldest*
//!   chunks for itself and never stalls the audio thread or other players, and
//! * advertises the codec's MIME type via the `Content-Type` header.
//!
//! ## Design
//!
//! [`StreamServer::start`] binds a TCP listener, then owns a multi-threaded
//! `tokio` runtime that runs an [`axum`] service. The encoder thread publishes
//! encoded bytes through an [`audio_core::StreamSink`]; internally that is a
//! bounded [`tokio::sync::broadcast`] channel acting as a per-stream ring
//! buffer. Each connection subscribes at the live edge and streams chunks out;
//! when a connection cannot keep up it receives a lag signal and the broadcast
//! discards the oldest chunks for that subscriber only. The producer's
//! [`StreamSink::write`] is wait-free with respect to clients — it copies the
//! chunk once and returns immediately.
//!
//! The choice of `axum`/`hyper` over `tiny_http`, and of a broadcast ring buffer
//! for drop-oldest fan-out, is recorded in `docs/adr/0004-stream-server-http.md`.
//!
//! ```no_run
//! use audio_core::{StreamConfig, Transport};
//! use stream_server::{ServerConfig, StreamServer};
//!
//! let config = ServerConfig::from_stream_config(&StreamConfig::default());
//! let server = StreamServer::start(config).expect("bind stream server");
//!
//! // The encoder thread writes encoded chunks to the sink…
//! let sink = server.sink();
//! sink.write(b"<encoded audio frame>").unwrap();
//!
//! // …and Sonos is pointed at this URL via UPnP `SetAVTransportURI`.
//! println!("streaming at {}", server.stream_url());
//! ```

#![forbid(unsafe_code)]

mod http;
mod metrics;
mod net;
mod sink;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use audio_core::{Error, Result, StreamConfig, StreamSink, Transport};
use axum::http::HeaderValue;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, watch};

pub use metrics::ServerMetrics;

use http::AppState;
use metrics::Metrics;
use sink::BroadcastSink;

/// Default depth of the per-stream broadcast ring buffer, in chunks.
///
/// This bounds how far a slow client may fall behind before the *oldest* chunks
/// are dropped for it. It is deliberately shallow to cap the latency a lagging
/// client can add; at a typical ~10 ms encode cadence this is well under a
/// second of buffering.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 32;

/// How long teardown waits for in-flight connection tasks to finish before the
/// runtime is forcibly stopped.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// Bind/runtime settings for the stream server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// IP address to bind to. `0.0.0.0` exposes the stream to the LAN.
    pub bind_address: IpAddr,
    /// TCP port to listen on. `0` lets the OS choose a free port (useful in
    /// tests); the chosen port is then reflected by [`StreamServer::local_addr`]
    /// and [`StreamServer::stream_url`].
    pub port: u16,
    /// Path of the streaming endpoint, e.g. `/stream`.
    pub path: String,
    /// MIME type to advertise to clients (matches the active codec).
    pub content_type: String,
    /// Depth of the per-stream broadcast ring buffer, in chunks. See
    /// [`DEFAULT_CHANNEL_CAPACITY`].
    pub channel_capacity: usize,
}

impl ServerConfig {
    /// Derive a server configuration from the shared [`StreamConfig`].
    #[must_use]
    pub fn from_stream_config(stream: &StreamConfig) -> Self {
        let bind_address = stream
            .bind_address
            .parse()
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        Self {
            bind_address,
            port: stream.port,
            path: "/stream".to_owned(),
            content_type: stream.profile.content_type().to_owned(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

/// A handle to the running HTTP stream server.
///
/// Dropping the handle (or calling [`StreamServer::stop`]) shuts the server
/// down: it stops accepting connections, ends every in-flight response, and
/// joins the runtime. The handle implements [`audio_core::Transport`], so the
/// orchestration layer can treat it uniformly with any other transport.
///
/// # Panics
///
/// Teardown joins the owned runtime and therefore must run in a *synchronous*
/// context. Do not drop a [`StreamServer`] from inside an async task running on
/// another `tokio` runtime.
#[derive(Debug)]
pub struct StreamServer {
    config: ServerConfig,
    local_addr: SocketAddr,
    stream_url: String,
    sink: Arc<BroadcastSink>,
    metrics: Arc<Metrics>,
    shutdown_tx: watch::Sender<bool>,
    runtime: Option<Runtime>,
}

impl StreamServer {
    /// Bind and start serving according to `config`.
    ///
    /// The listener is bound synchronously so the actual local address (and, for
    /// `port = 0`, the OS-assigned port) is known before this returns. The
    /// async accept loop and a telemetry task are then spawned on a runtime
    /// owned by the returned handle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] for an invalid `content_type`, and
    /// [`Error::Transport`] (or [`Error::Io`]) for bind/listen/runtime failures.
    pub fn start(config: ServerConfig) -> Result<Self> {
        let path = net::normalize_path(&config.path);
        let content_type = HeaderValue::from_str(&config.content_type).map_err(|_| {
            Error::config(format!(
                "invalid stream content-type: {:?}",
                config.content_type
            ))
        })?;

        // Bind with std so the resolved address/port is available immediately.
        let std_listener = std::net::TcpListener::bind((config.bind_address, config.port))
            .map_err(|error| {
                Error::transport(format!(
                    "failed to bind stream server to {}:{}: {error}",
                    config.bind_address, config.port
                ))
            })?;
        std_listener.set_nonblocking(true)?;
        let local_addr = std_listener.local_addr()?;

        let advertised = net::resolve_advertised_ip(config.bind_address);
        let stream_url = format!(
            "http://{}{path}",
            SocketAddr::new(advertised, local_addr.port())
        );

        let capacity = config.channel_capacity.max(1);
        let (tx, _rx) = broadcast::channel::<Bytes>(capacity);
        let metrics = Arc::new(Metrics::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (sink_closed_tx, sink_closed_rx) = watch::channel(false);

        let sink = Arc::new(BroadcastSink::new(
            tx.clone(),
            Arc::clone(&metrics),
            sink_closed_tx,
        ));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("stream-server")
            .build()
            .map_err(|error| {
                Error::transport(format!("failed to build stream-server runtime: {error}"))
            })?;

        // `from_std` registers the listener with the reactor, so it must run
        // inside the runtime; surfacing any error here keeps `start` honest.
        let listener = runtime
            .block_on(async move { TcpListener::from_std(std_listener) })
            .map_err(|error| {
                Error::transport(format!("failed to register stream listener: {error}"))
            })?;

        let state = AppState {
            tx,
            metrics: Arc::clone(&metrics),
            content_type,
            shutdown_rx: shutdown_rx.clone(),
            sink_closed_rx,
        };

        runtime.spawn(http::serve(listener, path, state, shutdown_rx.clone()));
        runtime.spawn(http::telemetry_loop(Arc::clone(&metrics), shutdown_rx));

        tracing::info!(
            %stream_url,
            content_type = %config.content_type,
            channel_capacity = capacity,
            "stream-server listening"
        );

        Ok(Self {
            config,
            local_addr,
            stream_url,
            sink,
            metrics,
            shutdown_tx,
            runtime: Some(runtime),
        })
    }

    /// The configuration this server was started with.
    #[must_use]
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The socket address the server is actually bound to (with the real port
    /// when the configuration requested `0`).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The LAN-reachable URL a Sonos player should fetch, e.g.
    /// `http://192.168.1.50:8080/stream`.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.stream_url
    }

    /// A snapshot of the live server telemetry counters.
    #[must_use]
    pub fn metrics(&self) -> ServerMetrics {
        self.metrics.snapshot()
    }

    /// Stop the server: end every in-flight response, stop accepting, and join
    /// the runtime. Idempotent; also invoked by `Drop`.
    pub fn stop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(SHUTDOWN_GRACE);
        }
    }
}

impl Transport for StreamServer {
    fn stream_url(&self) -> String {
        self.stream_url.clone()
    }

    fn content_type(&self) -> &str {
        &self.config.content_type
    }

    fn sink(&self) -> Arc<dyn StreamSink> {
        Arc::clone(&self.sink) as Arc<dyn StreamSink>
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback_config() -> ServerConfig {
        ServerConfig {
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            path: "/stream".to_owned(),
            content_type: "audio/L16".to_owned(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    #[test]
    fn derives_config_from_stream_settings() {
        let cfg = ServerConfig::from_stream_config(&StreamConfig::default());
        assert_eq!(cfg.port, audio_core::DEFAULT_STREAM_PORT);
        assert_eq!(cfg.path, "/stream");
        assert_eq!(cfg.content_type, "audio/L16");
        assert_eq!(cfg.bind_address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(cfg.channel_capacity, DEFAULT_CHANNEL_CAPACITY);
    }

    #[test]
    fn starts_binds_and_exposes_a_loopback_url() {
        let server = StreamServer::start(loopback_config()).expect("server starts");
        let port = server.local_addr().port();
        assert_ne!(port, 0, "an ephemeral port must be assigned");
        assert_eq!(
            server.stream_url(),
            format!("http://127.0.0.1:{port}/stream")
        );
        assert_eq!(Transport::content_type(&server), "audio/L16");
        assert_eq!(server.metrics(), ServerMetrics::default());
    }

    #[test]
    fn rejects_invalid_content_type() {
        let mut cfg = loopback_config();
        cfg.content_type = "inva\nlid".to_owned();
        assert!(matches!(StreamServer::start(cfg), Err(Error::Config(_))));
    }

    #[test]
    fn sink_counts_bytes_and_rejects_writes_after_close() {
        let server = StreamServer::start(loopback_config()).expect("server starts");
        let sink = server.sink();

        sink.write(b"abc").expect("write ok");
        sink.write(b"").expect("empty write is a no-op");
        sink.write(b"de").expect("write ok");

        let metrics = server.metrics();
        assert_eq!(metrics.total_bytes, 5);
        assert_eq!(metrics.total_chunks, 2, "empty writes are not counted");

        sink.close();
        assert!(
            matches!(sink.write(b"more"), Err(Error::Transport(_))),
            "writes after close are rejected"
        );
    }

    #[test]
    fn stop_is_idempotent() {
        let mut server = StreamServer::start(loopback_config()).expect("server starts");
        server.stop();
        server.stop();
    }
}
