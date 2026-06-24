//! SSDP discovery of Sonos players.
//!
//! Discovery sends an `M-SEARCH` datagram to the SSDP multicast group and
//! collects the unicast `200 OK` replies, de-duplicating by `USN`. Each reply
//! carries a `LOCATION` pointing at the player's device-description XML, which
//! the caller fetches to build a [`crate::SonosDevice`].
//!
//! The socket is *configured* with [`socket2`] (so `SO_REUSEADDR` and the
//! multicast TTL can be set) and then converted into a [`std::net::UdpSocket`]
//! for all sends and receives. That keeps every byte-level operation in safe
//! `std` code, preserving the crate-wide `#![forbid(unsafe_code)]`.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use audio_core::{Error, Result};
use socket2::{Domain, Protocol, Socket, Type};

/// The IPv4 SSDP multicast group address.
pub const SSDP_MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
/// The SSDP UDP port.
pub const SSDP_PORT: u16 = 1900;
/// The SSDP search target that matches every Sonos player.
pub const ZONE_PLAYER_TARGET: &str = "urn:schemas-upnp-org:device:ZonePlayer:1";

/// A single parsed SSDP `M-SEARCH` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsdpResponse {
    /// `LOCATION` header: URL of the device-description XML.
    pub location: String,
    /// `USN` header: unique service name, used for de-duplication.
    pub usn: String,
    /// `SERVER` header, if present.
    pub server: Option<String>,
    /// `ST` (search target) header echoed by the responder, if present.
    pub search_target: Option<String>,
}

/// A configurable SSDP search.
///
/// [`SsdpSearch::default`] targets every Sonos `ZonePlayer` on the standard
/// multicast group. The destination can be overridden (e.g. to a unicast
/// loopback responder) which is how the integration tests exercise discovery
/// without real multicast or hardware.
#[derive(Debug, Clone)]
pub struct SsdpSearch {
    search_target: String,
    destination: SocketAddrV4,
    mx: u8,
    multicast_ttl: u8,
    bursts: u8,
}

impl Default for SsdpSearch {
    fn default() -> Self {
        Self {
            search_target: ZONE_PLAYER_TARGET.to_owned(),
            destination: SocketAddrV4::new(SSDP_MULTICAST_ADDR, SSDP_PORT),
            mx: 1,
            multicast_ttl: 2,
            bursts: 2,
        }
    }
}

impl SsdpSearch {
    /// A standard multicast search for Sonos players.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the SSDP search target (`ST`).
    #[must_use]
    pub fn with_search_target(mut self, target: impl Into<String>) -> Self {
        self.search_target = target.into();
        self
    }

    /// Send the search to an explicit address instead of the multicast group.
    ///
    /// Primarily intended for testing against a unicast responder; the multicast
    /// TTL is only applied when the destination is itself a multicast address.
    #[must_use]
    pub fn with_destination(mut self, destination: SocketAddrV4) -> Self {
        self.destination = destination;
        self
    }

    /// Set the `MX` value (maximum response delay hint, in seconds).
    #[must_use]
    pub fn with_mx(mut self, mx: u8) -> Self {
        self.mx = mx;
        self
    }

    /// Set how many `M-SEARCH` datagrams are sent (UDP is lossy; a small burst
    /// improves reliability on real networks).
    #[must_use]
    pub fn with_bursts(mut self, bursts: u8) -> Self {
        self.bursts = bursts.max(1);
        self
    }

    /// Run the search, collecting de-duplicated responses until `timeout`.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the UDP socket cannot be created/bound or a send
    /// fails. Per-datagram receive timeouts are expected and not treated as
    /// errors.
    pub fn search(&self, timeout: Duration) -> Result<Vec<SsdpResponse>> {
        let udp = self.open_socket()?;
        let destination = SocketAddr::V4(self.destination);
        let msearch = build_msearch(self.destination, &self.search_target, self.mx);
        for _ in 0..self.bursts {
            udp.send_to(msearch.as_bytes(), destination)?;
        }

        let deadline = Instant::now() + timeout;
        let mut seen: HashSet<String> = HashSet::new();
        let mut responses = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            udp.set_read_timeout(Some(remaining.min(Duration::from_millis(500))))?;
            match udp.recv_from(&mut buf) {
                Ok((n, _src)) => {
                    if let Some(resp) = parse_msearch_response(&buf[..n]) {
                        let key = if resp.usn.is_empty() {
                            resp.location.clone()
                        } else {
                            resp.usn.clone()
                        };
                        if seen.insert(key) {
                            tracing::debug!(location = %resp.location, "ssdp response");
                            responses.push(resp);
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Ok(responses)
    }

    fn open_socket(&self) -> Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        let bind: SocketAddr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        socket.bind(&bind.into())?;
        let udp: UdpSocket = socket.into();
        if self.destination.ip().is_multicast() {
            udp.set_multicast_ttl_v4(u32::from(self.multicast_ttl))?;
            udp.set_multicast_loop_v4(true)?;
        }
        Ok(udp)
    }
}

/// Render an `M-SEARCH` request targeting `host`.
pub(crate) fn build_msearch(host: SocketAddrV4, search_target: &str, mx: u8) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: {host}\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: {mx}\r\n\
         ST: {search_target}\r\n\r\n"
    )
}

/// Parse an SSDP reply, returning `None` when it carries no `LOCATION`.
pub(crate) fn parse_msearch_response(buf: &[u8]) -> Option<SsdpResponse> {
    let text = std::str::from_utf8(buf).ok()?;
    let mut location = None;
    let mut usn = None;
    let mut server = None;
    let mut search_target = None;

    for line in text.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        match name.trim().to_ascii_uppercase().as_str() {
            "LOCATION" => location = Some(value),
            "USN" => usn = Some(value),
            "SERVER" => server = Some(value),
            "ST" => search_target = Some(value),
            _ => {}
        }
    }

    Some(SsdpResponse {
        location: location?,
        usn: usn.unwrap_or_default(),
        server,
        search_target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msearch_contains_required_headers() {
        let host = SocketAddrV4::new(SSDP_MULTICAST_ADDR, SSDP_PORT);
        let req = build_msearch(host, ZONE_PLAYER_TARGET, 1);
        assert!(req.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(req.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(req.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(req.contains("MX: 1\r\n"));
        assert!(req.contains("ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn parses_location_and_usn() {
        let raw = b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.50:1400/xml/device_description.xml\r\nUSN: uuid:RINCON_1::urn:schemas-upnp-org:device:ZonePlayer:1\r\nST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\r\n";
        let resp = parse_msearch_response(raw).expect("parse");
        assert_eq!(
            resp.location,
            "http://192.168.1.50:1400/xml/device_description.xml"
        );
        assert!(resp.usn.contains("RINCON_1"));
        assert_eq!(resp.search_target.as_deref(), Some(ZONE_PLAYER_TARGET));
    }

    #[test]
    fn rejects_response_without_location() {
        let raw = b"HTTP/1.1 200 OK\r\nUSN: uuid:RINCON_1\r\n\r\n";
        assert!(parse_msearch_response(raw).is_none());
    }
}
