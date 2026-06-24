//! Sonos discovery and control.
//!
//! # Workstream WS-B (Phase 1)
//!
//! Two responsibilities:
//!
//! * **Discovery** — locate Sonos players on the LAN via SSDP and parse each
//!   device description to learn its room name, model and UUID.
//! * **Control** — drive playback over UPnP SOAP: `SetAVTransportURI` + `Play`
//!   / `Stop` on `AVTransport`, and volume on `RenderingControl`; resolve
//!   zone-group topology so a stereo pair or group is addressed by its
//!   coordinator.
//!
//! Everything is synchronous, plain HTTP (no TLS), and depends only on a tiny
//! hand-rolled HTTP client over `std::net` plus [`socket2`] for SSDP and
//! [`quick_xml`] for parsing — keeping the dependency and attack surface small.
//!
//! ## Example
//!
//! ```no_run
//! use std::time::Duration;
//! use sonos_control::{controller_for_room, discover};
//!
//! # fn main() -> audio_core::Result<()> {
//! // Enumerate rooms on the LAN.
//! let devices = discover(Duration::from_secs(2))?;
//! for device in &devices {
//!     println!("{} — {}", device.room_name, device.model_name);
//! }
//!
//! // Drive a room (resolved to its group coordinator) to pull our stream.
//! let controller = controller_for_room(Duration::from_secs(2), "Living Room")?;
//! controller.set_volume(25)?;
//! controller.set_stream_uri("http://192.168.1.10:8080/stream", "audio/L16")?;
//! controller.play()?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod description;
mod didl;
mod http;
mod soap;
mod ssdp;
mod topology;
mod xml;

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::time::Duration;

use audio_core::{Error, Result};

pub use description::{fetch_device, parse_device_description};
pub use ssdp::{SsdpResponse, SsdpSearch, SSDP_MULTICAST_ADDR, SSDP_PORT, ZONE_PLAYER_TARGET};
pub use topology::{find_room_coordinator, parse_zone_group_state, ZoneGroup, ZoneGroupMember};

/// The Sonos control port (fixed across the product line).
pub const SONOS_CONTROL_PORT: u16 = 1400;

/// Default per-request timeout for UPnP SOAP control calls.
const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for fetching an individual device description during discovery.
const DEVICE_FETCH_TIMEOUT: Duration = Duration::from_secs(3);

/// A discovered Sonos player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SonosDevice {
    /// Stable unique id, e.g. `RINCON_XXXXXXXXXXXX01400`.
    pub uuid: String,
    /// Room/zone name configured by the user, e.g. "Living Room".
    pub room_name: String,
    /// Marketing model name, e.g. "Sonos One".
    pub model_name: String,
    /// IPv4 address on the local network.
    pub ip: Ipv4Addr,
}

impl SonosDevice {
    /// Base URL for the device's HTTP control/description endpoints.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.ip, SONOS_CONTROL_PORT)
    }

    /// Full URL of the device-description XML document.
    #[must_use]
    pub fn device_description_url(&self) -> String {
        format!("{}/xml/device_description.xml", self.base_url())
    }

    /// Full URL of the `AVTransport` control endpoint.
    #[must_use]
    pub fn av_transport_control_url(&self) -> String {
        format!("{}/MediaRenderer/AVTransport/Control", self.base_url())
    }

    /// Full URL of the `RenderingControl` control endpoint.
    #[must_use]
    pub fn rendering_control_url(&self) -> String {
        format!("{}/MediaRenderer/RenderingControl/Control", self.base_url())
    }

    /// Full URL of the `ZoneGroupTopology` control endpoint.
    #[must_use]
    pub fn zone_group_topology_url(&self) -> String {
        format!("{}/ZoneGroupTopology/Control", self.base_url())
    }
}

/// Discover Sonos players on the local network.
///
/// Sends an SSDP `M-SEARCH` for `ZonePlayer` devices, then fetches and parses each
/// responder's device description. Players whose description cannot be fetched
/// are logged and skipped rather than failing the whole discovery.
///
/// # Errors
///
/// Returns an [`Error`] only if the SSDP socket cannot be created or the search
/// itself fails; individual device-description failures are tolerated.
pub fn discover(timeout: Duration) -> Result<Vec<SonosDevice>> {
    discover_with(&SsdpSearch::new(), timeout)
}

/// Discover Sonos players using a caller-supplied [`SsdpSearch`].
///
/// [`discover`] is the common entry point; this variant exposes the search
/// configuration so callers (and integration tests) can target a specific
/// interface or a unicast responder.
///
/// # Errors
///
/// Returns an [`Error`] only if the SSDP search fails; individual
/// device-description failures are tolerated and skipped.
pub fn discover_with(search: &SsdpSearch, timeout: Duration) -> Result<Vec<SonosDevice>> {
    let responses = search.search(timeout)?;
    let mut devices = Vec::new();
    let mut seen = HashSet::new();
    for response in responses {
        match fetch_device(&response.location, DEVICE_FETCH_TIMEOUT) {
            Ok(device) => {
                if seen.insert(device.uuid.clone()) {
                    devices.push(device);
                }
            }
            Err(err) => {
                tracing::warn!(location = %response.location, %err, "skipping unreachable device");
            }
        }
    }
    Ok(devices)
}

/// Discover the zone-group topology of the network.
///
/// Any single responder can report the full topology, so this issues one SSDP
/// search and queries the first responder's `ZoneGroupTopology` service.
///
/// # Errors
///
/// Returns [`Error::Control`] if no player responds, or a transport/parse error
/// if the topology query fails.
pub fn discover_zone_groups(timeout: Duration) -> Result<Vec<ZoneGroup>> {
    let responses = SsdpSearch::new().search(timeout)?;
    let first = responses
        .first()
        .ok_or_else(|| Error::control("no Sonos players responded to SSDP discovery"))?;
    let url = http::parse_http_url(&first.location)?;
    let topology_url = format!(
        "http://{}:{SONOS_CONTROL_PORT}/ZoneGroupTopology/Control",
        url.host
    );
    topology::fetch_zone_groups(&topology_url, timeout)
}

/// Build a [`SonosController`] for `room`, resolved to its group coordinator.
///
/// This is the ergonomic entry point for orchestration: it discovers the
/// topology, maps `room` to the coordinator that must receive transport
/// commands, and binds a controller to it.
///
/// # Errors
///
/// Returns [`Error::Control`] if discovery finds no matching room or the
/// coordinator has no usable address.
pub fn controller_for_room(timeout: Duration, room: &str) -> Result<SonosController> {
    let groups = discover_zone_groups(timeout)?;
    let coordinator = find_room_coordinator(&groups, room)
        .ok_or_else(|| Error::control(format!("no Sonos room named '{room}' was found")))?;
    let ip = coordinator
        .ip()
        .ok_or_else(|| Error::control("group coordinator has no usable IPv4 address"))?;
    let device = fetch_device(&coordinator.location, timeout).unwrap_or_else(|_| SonosDevice {
        uuid: coordinator.uuid.clone(),
        room_name: coordinator.zone_name.clone(),
        model_name: "Sonos".to_owned(),
        ip,
    });
    Ok(SonosController::new(device))
}

/// Issues UPnP commands to a single Sonos coordinator.
#[derive(Debug, Clone)]
pub struct SonosController {
    device: SonosDevice,
    av_transport_url: String,
    rendering_control_url: String,
    timeout: Duration,
}

impl SonosController {
    /// Create a controller bound to `device`, deriving its control endpoints
    /// from the device address (port [`SONOS_CONTROL_PORT`]).
    #[must_use]
    pub fn new(device: SonosDevice) -> Self {
        let av_transport_url = device.av_transport_control_url();
        let rendering_control_url = device.rendering_control_url();
        Self {
            device,
            av_transport_url,
            rendering_control_url,
            timeout: DEFAULT_CONTROL_TIMEOUT,
        }
    }

    /// Create a controller that talks to explicit control endpoints rather than
    /// deriving them from `device`.
    ///
    /// Useful for non-standard ports or for driving a mock SOAP endpoint in
    /// tests.
    #[must_use]
    pub fn with_endpoints(
        device: SonosDevice,
        av_transport_url: impl Into<String>,
        rendering_control_url: impl Into<String>,
    ) -> Self {
        Self {
            device,
            av_transport_url: av_transport_url.into(),
            rendering_control_url: rendering_control_url.into(),
            timeout: DEFAULT_CONTROL_TIMEOUT,
        }
    }

    /// Override the per-request SOAP timeout (default 5s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The device this controller drives.
    #[must_use]
    pub fn device(&self) -> &SonosDevice {
        &self.device
    }

    /// Point the player at `stream_url` (served by the bridge) and prepare it
    /// for playback. `content_type` is the codec MIME type, advertised in the
    /// DIDL-Lite `protocolInfo`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] on a SOAP fault and [`Error::Io`] on a
    /// transport failure.
    pub fn set_stream_uri(&self, stream_url: &str, content_type: &str) -> Result<()> {
        let metadata = didl::audio_broadcast_metadata("sonos-driver", stream_url, content_type);
        soap::call(
            &self.av_transport_url,
            soap::AV_TRANSPORT_SERVICE,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", stream_url),
                ("CurrentURIMetaData", &metadata),
            ],
            self.timeout,
        )?;
        Ok(())
    }

    /// Begin playback of the previously set URI.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] on a SOAP fault and [`Error::Io`] on a
    /// transport failure.
    pub fn play(&self) -> Result<()> {
        soap::call(
            &self.av_transport_url,
            soap::AV_TRANSPORT_SERVICE,
            "Play",
            &[("InstanceID", "0"), ("Speed", "1")],
            self.timeout,
        )?;
        Ok(())
    }

    /// Stop playback.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] on a SOAP fault and [`Error::Io`] on a
    /// transport failure.
    pub fn stop(&self) -> Result<()> {
        soap::call(
            &self.av_transport_url,
            soap::AV_TRANSPORT_SERVICE,
            "Stop",
            &[("InstanceID", "0")],
            self.timeout,
        )?;
        Ok(())
    }

    /// Set the player volume (`0..=100`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] if `volume` exceeds 100 or on a SOAP fault,
    /// and [`Error::Io`] on a transport failure.
    pub fn set_volume(&self, volume: u8) -> Result<()> {
        if volume > 100 {
            return Err(Error::control(format!(
                "volume out of range (0..=100): {volume}"
            )));
        }
        let desired = volume.to_string();
        soap::call(
            &self.rendering_control_url,
            soap::RENDERING_CONTROL_SERVICE,
            "SetVolume",
            &[
                ("InstanceID", "0"),
                ("Channel", "Master"),
                ("DesiredVolume", &desired),
            ],
            self.timeout,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_device() -> SonosDevice {
        SonosDevice {
            uuid: "RINCON_000E58FACE0101400".to_owned(),
            room_name: "Living Room".to_owned(),
            model_name: "Sonos One".to_owned(),
            ip: Ipv4Addr::new(192, 168, 1, 50),
        }
    }

    #[test]
    fn builds_control_urls_on_port_1400() {
        let dev = sample_device();
        assert_eq!(dev.base_url(), "http://192.168.1.50:1400");
        assert_eq!(
            dev.av_transport_control_url(),
            "http://192.168.1.50:1400/MediaRenderer/AVTransport/Control"
        );
        assert_eq!(
            dev.rendering_control_url(),
            "http://192.168.1.50:1400/MediaRenderer/RenderingControl/Control"
        );
        assert_eq!(
            dev.zone_group_topology_url(),
            "http://192.168.1.50:1400/ZoneGroupTopology/Control"
        );
        assert_eq!(
            dev.device_description_url(),
            "http://192.168.1.50:1400/xml/device_description.xml"
        );
    }

    #[test]
    fn set_volume_rejects_out_of_range_before_any_io() {
        let ctl = SonosController::new(sample_device());
        let err = ctl.set_volume(150).expect_err("should reject");
        assert!(matches!(err, Error::Control(_)));
        assert!(err.to_string().contains("150"));
    }
}
