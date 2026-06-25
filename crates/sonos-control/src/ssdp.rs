//! SSDP discovery of Sonos players.
//!
//! Discovery sends an `M-SEARCH` datagram to the SSDP multicast group and
//! collects the unicast `200 OK` replies, de-duplicating by `USN`. Each reply
//! carries a `LOCATION` pointing at the player's device-description XML, which
//! the caller fetches to build a [`crate::SonosDevice`].
//!
//! On a multi-homed host (Ethernet *and* Wi-Fi, or virtual adapters from
//! Hyper-V/WSL/Docker/VPNs — the common case on Windows) the OS would otherwise
//! send the multicast `M-SEARCH` out only the default-route interface, which is
//! frequently *not* the one the speakers are on, so discovery silently finds no
//! rooms. To avoid that, a multicast search enumerates the local IPv4 interfaces
//! with [`if_addrs`] and sends from every one, explicitly selecting each as the
//! multicast egress interface. An explicit *unicast* destination keeps the
//! single-socket path (used by the loopback integration tests).
//!
//! The socket is *configured* with [`socket2`] (so `SO_REUSEADDR`, the multicast
//! TTL and the egress interface can be set) and then converted into a
//! [`std::net::UdpSocket`] for all sends and receives. That keeps every
//! byte-level operation in safe `std` code, preserving the crate-wide
//! `#![forbid(unsafe_code)]`.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::thread::sleep;
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
    /// A multicast destination (the default) is sent from every usable local
    /// IPv4 interface so multi-homed hosts are covered; an explicit unicast
    /// destination uses a single socket.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if no UDP socket can be created/bound. On the unicast
    /// path a send failure is also propagated; on the multicast path a send
    /// failure on one interface is logged and tolerated. Per-datagram receive
    /// timeouts are expected and never treated as errors.
    pub fn search(&self, timeout: Duration) -> Result<Vec<SsdpResponse>> {
        if self.destination.ip().is_multicast() {
            self.search_multicast(timeout)
        } else {
            self.search_unicast(timeout)
        }
    }

    /// Search a single (unicast) destination from one default-bound socket.
    fn search_unicast(&self, timeout: Duration) -> Result<Vec<SsdpResponse>> {
        let udp = Self::open_unicast_socket()?;
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
                        if seen.insert(dedup_key(&resp)) {
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

    /// Search the multicast group from every usable local IPv4 interface.
    fn search_multicast(&self, timeout: Duration) -> Result<Vec<SsdpResponse>> {
        let sockets = self.open_multicast_sockets()?;
        let destination = SocketAddr::V4(self.destination);
        let msearch = build_msearch(self.destination, &self.search_target, self.mx);
        for socket in &sockets {
            for _ in 0..self.bursts {
                // A send failure on one interface (e.g. a down or unsuitable
                // adapter) must not abort discovery on the others.
                if let Err(err) = socket.send_to(msearch.as_bytes(), destination) {
                    tracing::debug!(%err, "ssdp M-SEARCH send failed on an interface");
                }
            }
        }

        let deadline = Instant::now() + timeout;
        let mut seen: HashSet<String> = HashSet::new();
        let mut responses = Vec::new();
        let mut buf = [0u8; 2048];
        // Sockets are non-blocking; poll each in turn until the deadline, napping
        // briefly whenever a full pass found nothing so we never busy-spin.
        while Instant::now() < deadline {
            let mut idle = true;
            for socket in &sockets {
                loop {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _src)) => {
                            idle = false;
                            if let Some(resp) = parse_msearch_response(&buf[..n]) {
                                if seen.insert(dedup_key(&resp)) {
                                    tracing::debug!(location = %resp.location, "ssdp response");
                                    responses.push(resp);
                                }
                            }
                        }
                        Err(e)
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut =>
                        {
                            break;
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "ssdp recv error on an interface");
                            break;
                        }
                    }
                }
            }
            if idle {
                let nap = Duration::from_millis(20)
                    .min(deadline.saturating_duration_since(Instant::now()));
                if nap.is_zero() {
                    break;
                }
                sleep(nap);
            }
        }
        Ok(responses)
    }

    /// Open the single default-route socket used for a unicast search.
    fn open_unicast_socket() -> Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        let bind: SocketAddr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        socket.bind(&bind.into())?;
        Ok(socket.into())
    }

    /// Open one multicast-configured socket per usable local IPv4 interface.
    ///
    /// Each socket is bound to its interface address and has that interface set
    /// as the multicast egress (`IP_MULTICAST_IF`), so the `M-SEARCH` reaches
    /// every network the host is attached to rather than only the default route.
    /// An unspecified-address socket is always appended as a final safety net (and
    /// is the sole socket when interface enumeration yields nothing).
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] only if *no* socket — not even the unspecified
    /// fallback — could be opened.
    fn open_multicast_sockets(&self) -> Result<Vec<UdpSocket>> {
        let mut sockets = Vec::new();
        let mut handled: HashSet<Ipv4Addr> = HashSet::new();
        for iface in interface_ipv4_addrs() {
            if !handled.insert(iface) {
                continue;
            }
            match self.open_multicast_socket_on(iface) {
                Ok(socket) => sockets.push(socket),
                Err(err) => {
                    tracing::debug!(%iface, %err, "skipping interface for SSDP discovery");
                }
            }
        }
        match self.open_multicast_socket_on(Ipv4Addr::UNSPECIFIED) {
            Ok(socket) => sockets.push(socket),
            Err(err) if sockets.is_empty() => return Err(err),
            Err(err) => tracing::debug!(%err, "default-route SSDP socket unavailable"),
        }
        Ok(sockets)
    }

    /// Open and configure one non-blocking multicast socket bound to `iface`.
    ///
    /// `iface` may be [`Ipv4Addr::UNSPECIFIED`] for the OS default route, in which
    /// case no explicit egress interface is selected.
    fn open_multicast_socket_on(&self, iface: Ipv4Addr) -> Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_multicast_ttl_v4(u32::from(self.multicast_ttl))?;
        socket.set_multicast_loop_v4(true)?;
        if !iface.is_unspecified() {
            // Best effort: pick this interface for multicast egress. If the OS
            // rejects it (rare), the interface-address bind below still steers
            // most stacks to the right adapter.
            if let Err(err) = socket.set_multicast_if_v4(&iface) {
                tracing::debug!(%iface, %err, "could not set multicast egress interface");
            }
        }
        let bind: SocketAddr = SocketAddr::from((iface, 0));
        socket.bind(&bind.into())?;
        let udp: UdpSocket = socket.into();
        udp.set_nonblocking(true)?;
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

/// Enumerate the host's usable IPv4 interface addresses for multicast discovery.
///
/// Loopback is excluded (Sonos never lives there). Enumeration failure is logged
/// and yields an empty list, so the caller still falls back to the OS default
/// route via the unspecified-address socket.
fn interface_ipv4_addrs() -> Vec<Ipv4Addr> {
    match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces
            .into_iter()
            .filter(|iface| !iface.is_loopback())
            .filter_map(|iface| match iface.ip() {
                IpAddr::V4(addr) => Some(addr),
                IpAddr::V6(_) => None,
            })
            .collect(),
        Err(err) => {
            tracing::warn!(%err, "could not enumerate network interfaces for SSDP discovery");
            Vec::new()
        }
    }
}

/// The de-duplication key for a response: its `USN`, or the `LOCATION` when the
/// responder omitted a `USN`.
fn dedup_key(resp: &SsdpResponse) -> String {
    if resp.usn.is_empty() {
        resp.location.clone()
    } else {
        resp.usn.clone()
    }
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

    #[test]
    fn dedup_key_prefers_usn_then_location() {
        let with_usn = SsdpResponse {
            location: "http://10.0.0.5:1400/xml/device_description.xml".to_owned(),
            usn: "uuid:RINCON_ABC::urn".to_owned(),
            server: None,
            search_target: None,
        };
        assert_eq!(dedup_key(&with_usn), "uuid:RINCON_ABC::urn");

        let without_usn = SsdpResponse {
            location: "http://10.0.0.6:1400/xml/device_description.xml".to_owned(),
            usn: String::new(),
            server: None,
            search_target: None,
        };
        assert_eq!(
            dedup_key(&without_usn),
            "http://10.0.0.6:1400/xml/device_description.xml"
        );
    }

    #[test]
    fn interface_enumeration_excludes_loopback() {
        // Hermetic: only asserts a property of whatever the host reports — never
        // that any particular interface exists.
        for addr in interface_ipv4_addrs() {
            assert!(
                !addr.is_loopback(),
                "loopback address {addr} must not be returned"
            );
        }
    }

    #[test]
    fn multicast_search_opens_at_least_the_default_route() {
        // Even on a host with no enumerable interfaces, the unspecified-address
        // safety-net socket must always be opened.
        let search = SsdpSearch::new();
        let sockets = search
            .open_multicast_sockets()
            .expect("at least the default-route socket should open");
        assert!(!sockets.is_empty());
    }
}
