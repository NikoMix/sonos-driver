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
//! Phase 0 defines the data model ([`SonosDevice`]) and control surface
//! ([`SonosController`]) that the orchestration crate builds against. The SSDP
//! and SOAP implementations land in WS-B.

use std::net::Ipv4Addr;
use std::time::Duration;

use audio_core::{Error, Result};

/// The Sonos control port (fixed across the product line).
pub const SONOS_CONTROL_PORT: u16 = 1400;

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
}

/// Discover Sonos players on the local network.
///
/// # Errors
///
/// Currently returns [`Error::NotImplemented`]; WS-B will return SSDP socket or
/// parsing errors.
pub fn discover(timeout: Duration) -> Result<Vec<SonosDevice>> {
    let _ = timeout;
    Err(Error::NotImplemented(
        "sonos-control: SSDP discovery is implemented in WS-B",
    ))
}

/// Issues UPnP commands to a single Sonos coordinator.
#[derive(Debug, Clone)]
pub struct SonosController {
    device: SonosDevice,
}

impl SonosController {
    /// Create a controller bound to `device`.
    #[must_use]
    pub fn new(device: SonosDevice) -> Self {
        Self { device }
    }

    /// The device this controller drives.
    #[must_use]
    pub fn device(&self) -> &SonosDevice {
        &self.device
    }

    /// Point the player at `stream_url` (served by the bridge) and prepare it
    /// for playback. `content_type` is the codec MIME type.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until WS-B; thereafter SOAP transport
    /// errors.
    pub fn set_stream_uri(&self, stream_url: &str, content_type: &str) -> Result<()> {
        let _ = (stream_url, content_type);
        Err(Error::NotImplemented(
            "sonos-control: SetAVTransportURI is implemented in WS-B",
        ))
    }

    /// Begin playback of the previously set URI.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until WS-B.
    pub fn play(&self) -> Result<()> {
        Err(Error::NotImplemented(
            "sonos-control: Play is implemented in WS-B",
        ))
    }

    /// Stop playback.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until WS-B.
    pub fn stop(&self) -> Result<()> {
        Err(Error::NotImplemented(
            "sonos-control: Stop is implemented in WS-B",
        ))
    }

    /// Set the player volume (`0..=100`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until WS-B.
    pub fn set_volume(&self, volume: u8) -> Result<()> {
        let _ = volume;
        Err(Error::NotImplemented(
            "sonos-control: SetVolume is implemented in WS-B",
        ))
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
    }

    #[test]
    fn control_ops_pending_workstream() {
        let ctl = SonosController::new(sample_device());
        assert!(matches!(ctl.play(), Err(Error::NotImplemented(_))));
        assert!(matches!(
            ctl.set_stream_uri("http://host/stream", "audio/L16"),
            Err(Error::NotImplemented(_))
        ));
    }
}
