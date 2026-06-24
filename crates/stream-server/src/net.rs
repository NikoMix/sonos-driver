//! Network helpers: advertised-address resolution and endpoint-path
//! normalisation.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// Normalise a configured endpoint path into the form `axum` requires: a
/// non-empty string beginning with `/`. An empty path falls back to `/stream`.
pub(crate) fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        "/stream".to_owned()
    } else if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// Resolve the IP address to advertise in `stream_url` for a given bind address.
///
/// * A concrete, non-loopback bind address is advertised verbatim — the operator
///   chose it deliberately.
/// * A loopback bind address is advertised as loopback (local-only runs/tests).
/// * An unspecified bind address (`0.0.0.0` / `::`) triggers LAN-IP discovery so
///   Sonos receives an address it can actually dial back on, falling back to
///   loopback when no route can be found.
pub(crate) fn resolve_advertised_ip(bind: IpAddr) -> IpAddr {
    if bind.is_loopback() {
        return bind;
    }
    if !bind.is_unspecified() {
        return bind;
    }
    local_ipv4().map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V4)
}

/// Best-effort discovery of the primary LAN IPv4 address.
///
/// "Connecting" a UDP socket selects the egress interface via the OS routing
/// table *without sending any packets*; the socket's resulting local address is
/// therefore the IP a LAN peer (a Sonos player) would use to reach this host.
/// Returns `None` when no suitable route exists (e.g. an isolated host).
pub(crate) fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A stable, well-known routable address. No datagram is transmitted; this
    // only asks the kernel which local interface would be used to reach it.
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 53)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(addr) => {
            let ip = *addr.ip();
            (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
        }
        SocketAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_paths() {
        assert_eq!(normalize_path("/stream"), "/stream");
        assert_eq!(normalize_path("stream"), "/stream");
        assert_eq!(normalize_path("  /live  "), "/live");
        assert_eq!(normalize_path(""), "/stream");
        assert_eq!(normalize_path("   "), "/stream");
    }

    #[test]
    fn advertises_concrete_and_loopback_binds_verbatim() {
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(resolve_advertised_ip(lan), lan);

        let loop_v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(resolve_advertised_ip(loop_v4), loop_v4);
    }

    #[test]
    fn unspecified_bind_resolves_to_a_reachable_address() {
        // Whatever the environment, the advertised address for `0.0.0.0` must
        // never itself be unspecified — Sonos could not dial `0.0.0.0` back.
        let resolved = resolve_advertised_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert!(!resolved.is_unspecified());
    }

    #[test]
    fn discovered_lan_ip_is_routable_when_present() {
        if let Some(ip) = local_ipv4() {
            assert!(!ip.is_loopback());
            assert!(!ip.is_unspecified());
        }
    }
}
