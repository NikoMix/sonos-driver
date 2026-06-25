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

/// Resolve the advertised IP for a stream that a *specific* peer (a Sonos player)
/// will dial back on.
///
/// This is the multi-homed-correct variant of [`resolve_advertised_ip`]. A
/// concrete or loopback bind is still honoured verbatim (the operator's choice).
/// For an unspecified bind (`0.0.0.0`), instead of guessing the default-route
/// interface it selects the local interface that actually routes **to that
/// peer** — essential on hosts with several active networks (Ethernet + Wi-Fi,
/// or Hyper-V/WSL/Docker/VPN adapters) where the default route is frequently not
/// the speakers' subnet. It falls back to the default-route LAN IP, then
/// loopback, so it never returns an unspecified address.
pub(crate) fn advertised_ip_for_peer(bind: IpAddr, peer: IpAddr) -> IpAddr {
    if bind.is_loopback() || !bind.is_unspecified() {
        return bind;
    }
    if let IpAddr::V4(peer_v4) = peer {
        if let Some(local) = local_ipv4_towards(peer_v4) {
            return IpAddr::V4(local);
        }
    }
    resolve_advertised_ip(bind)
}

/// Best-effort discovery of the primary LAN IPv4 address.
///
/// "Connecting" a UDP socket selects the egress interface via the OS routing
/// table *without sending any packets*; the socket's resulting local address is
/// therefore the IP a LAN peer (a Sonos player) would use to reach this host.
/// Returns `None` when no suitable route exists (e.g. an isolated host).
pub(crate) fn local_ipv4() -> Option<Ipv4Addr> {
    // A stable, well-known routable address. No datagram is transmitted; this
    // only asks the kernel which local interface would be used to reach it.
    local_ipv4_towards(Ipv4Addr::new(8, 8, 8, 8))
}

/// The local IPv4 address the OS would use to reach `peer`.
///
/// Like [`local_ipv4`], this only *connects* a UDP socket (selecting the egress
/// interface from the routing table) and reads back the chosen local address —
/// no datagram is sent. Passing the target Sonos's own IP yields the interface
/// address on the speaker's subnet, which is the address it can dial back on.
/// Returns `None` when the resolved address is loopback/unspecified or no route
/// exists.
pub(crate) fn local_ipv4_towards(peer: Ipv4Addr) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // The port is irrelevant to interface selection; no packet is transmitted.
    socket.connect((peer, 1900)).ok()?;
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

    #[test]
    fn peer_aware_advertise_honours_concrete_and_loopback_binds() {
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));

        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(advertised_ip_for_peer(lan, peer), lan);

        let loop_v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(advertised_ip_for_peer(loop_v4, peer), loop_v4);
    }

    #[test]
    fn peer_aware_advertise_is_never_unspecified() {
        // For an unspecified bind the advertised address must always be something
        // the peer could dial back on — never `0.0.0.0`.
        let peer = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let resolved = advertised_ip_for_peer(IpAddr::V4(Ipv4Addr::UNSPECIFIED), peer);
        assert!(!resolved.is_unspecified());
    }
}
