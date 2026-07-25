//! Player-control abstraction and Sonos target resolution.
//!
//! [`PlayerControl`] is the minimal AVTransport surface the orchestration loop
//! needs — point the player at a URL, start, stop. Abstracting it (rather than
//! hard-wiring [`sonos_control::SonosController`]) lets the end-to-end latency
//! harness drive the full run loop with an in-process mock, with no Sonos on
//! the LAN. See ADR-0007.

use std::net::IpAddr;
use std::time::Duration;

use audio_core::{Error, Result, SonosConfig};
use sonos_control::{controller_for_room, controller_for_uuid, SonosController};

/// Discovery floor. [`SonosConfig::discovery_timeout_secs`] defaults to `0`,
/// which would make an SSDP search return before any player could answer; three
/// seconds is the shortest interval that reliably collects responses on a
/// typical LAN.
const MIN_DISCOVERY: Duration = Duration::from_secs(3);

/// The subset of Sonos AVTransport control the bridge drives.
///
/// Implemented for [`sonos_control::SonosController`] in production and by an
/// in-process mock in the end-to-end harness. Implementors must be [`Send`] so
/// the orchestration layer can own one across the supervising and teardown
/// steps on the calling thread.
pub trait PlayerControl: Send {
    /// Point the player at `stream_url`, advertising `content_type`.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the underlying control call fails.
    fn set_stream_uri(&self, stream_url: &str, content_type: &str) -> Result<()>;

    /// Begin playback of the previously set URI.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the underlying control call fails.
    fn play(&self) -> Result<()>;

    /// Stop playback.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the underlying control call fails.
    fn stop(&self) -> Result<()>;

    /// The player's own LAN IP address, when known.
    ///
    /// The session uses this to advertise a stream URL on the local interface
    /// that actually routes to *this* player — correct on multi-homed hosts.
    /// Implementors with no addressable peer (such as the in-process test mock)
    /// return `None`, and the session then falls back to the server's
    /// default-resolved URL.
    fn peer_ip(&self) -> Option<IpAddr> {
        None
    }
}

impl PlayerControl for SonosController {
    fn set_stream_uri(&self, stream_url: &str, content_type: &str) -> Result<()> {
        // Fully-qualified so this dispatches to the inherent method rather than
        // recursing into this trait method.
        SonosController::set_stream_uri(self, stream_url, content_type)
    }

    fn play(&self) -> Result<()> {
        SonosController::play(self)
    }

    fn stop(&self) -> Result<()> {
        SonosController::stop(self)
    }

    fn peer_ip(&self) -> Option<IpAddr> {
        Some(IpAddr::V4(self.device().ip))
    }
}

/// Resolve the configured Sonos target into a controller.
///
/// `uuid` takes precedence over `room`; when neither is set this is a
/// configuration error rather than an I/O attempt, so it fails fast and is
/// testable without a network. The discovery timeout is taken from
/// [`SonosConfig::discovery_timeout_secs`], floored at [`MIN_DISCOVERY`].
///
/// # Errors
///
/// Returns [`Error::Config`] when no target is configured, and
/// [`Error::Control`] when discovery finds no matching device.
pub(crate) fn resolve_target(config: &SonosConfig) -> Result<SonosController> {
    let timeout = Duration::from_secs(u64::from(config.discovery_timeout_secs)).max(MIN_DISCOVERY);
    let seed = config
        .seed_host
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(uuid) = config
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        // A UUID may identify a non-coordinator member (e.g. one speaker of a
        // stereo pair); resolve it to the group coordinator that accepts
        // transport commands.
        return controller_for_uuid(seed, timeout, uuid);
    }

    if let Some(room) = config
        .room
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return controller_for_room(seed, timeout, room);
    }

    Err(Error::config(
        "no Sonos target configured: set sonos.uuid or sonos.room",
    ))
}

#[cfg(test)]
mod tests {
    use audio_core::{Error, SonosConfig};

    use super::resolve_target;

    #[test]
    fn missing_target_is_a_fast_config_error() {
        // Default config has neither a uuid nor a room: this must fail without
        // any network I/O so it is safe in CI.
        let config = SonosConfig::default();
        assert!(matches!(resolve_target(&config), Err(Error::Config(_))));
    }

    #[test]
    fn blank_target_fields_are_treated_as_unset() {
        let config = SonosConfig {
            room: Some("   ".to_owned()),
            uuid: Some(String::new()),
            seed_host: None,
            discovery_timeout_secs: 0,
        };
        assert!(matches!(resolve_target(&config), Err(Error::Config(_))));
    }
}
