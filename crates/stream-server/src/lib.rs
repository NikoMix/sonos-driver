//! Local HTTP server that publishes the encoded audio stream to Sonos.
//!
//! # Workstream WS-C (Phase 1)
//!
//! Sonos pulls audio over HTTP, so the bridge runs a small async server that:
//!
//! * serves a single chunked-transfer endpoint per active stream,
//! * fans the encoder's output out to every connected player (grouped rooms),
//! * keeps per-connection buffering minimal to avoid adding latency, and
//! * advertises the codec's MIME type via the `Content-Type` header.
//!
//! Phase 0 fixes the configuration surface ([`ServerConfig`]) and the
//! [`StreamServer`] handle; the async implementation (and the
//! [`audio_core::Transport`]/[`audio_core::StreamSink`] impls) land in WS-C.

use std::net::{IpAddr, Ipv4Addr};

use audio_core::{Error, Result, StreamConfig};

/// Bind/runtime settings for the stream server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// IP address to bind to. `0.0.0.0` exposes the stream to the LAN.
    pub bind_address: IpAddr,
    /// TCP port to listen on.
    pub port: u16,
    /// Path of the streaming endpoint, e.g. `/stream`.
    pub path: String,
    /// MIME type to advertise to clients (matches the active codec).
    pub content_type: String,
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
        }
    }
}

/// A handle to the running HTTP stream server.
#[derive(Debug)]
pub struct StreamServer {
    config: ServerConfig,
}

impl StreamServer {
    /// Bind and start serving according to `config`.
    ///
    /// # Errors
    ///
    /// Currently returns [`Error::NotImplemented`]; WS-C will return bind/listen
    /// failures.
    // The by-value `config` matches the final API: WS-C moves it into `Self`
    // (mirroring `Bridge::new`). The stub returns early before that move.
    #[allow(clippy::needless_pass_by_value)]
    pub fn start(config: ServerConfig) -> Result<Self> {
        let _ = &config;
        Err(Error::NotImplemented(
            "stream-server: async HTTP streaming is implemented in WS-C",
        ))
    }

    /// The configuration this server was started with.
    #[must_use]
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_config_from_stream_settings() {
        let cfg = ServerConfig::from_stream_config(&StreamConfig::default());
        assert_eq!(cfg.port, audio_core::DEFAULT_STREAM_PORT);
        assert_eq!(cfg.path, "/stream");
        assert_eq!(cfg.content_type, "audio/L16");
        assert_eq!(cfg.bind_address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn start_reports_not_implemented_for_now() {
        let cfg = ServerConfig::from_stream_config(&StreamConfig::default());
        assert!(matches!(
            StreamServer::start(cfg),
            Err(Error::NotImplemented(_))
        ));
    }
}
