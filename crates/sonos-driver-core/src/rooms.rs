//! Turning a discovered Sonos household into the room list the binder consumes.
//!
//! Shared by the CLI and the Windows service so both agree on exactly which
//! players become Windows outputs.

use std::time::Duration;

use audio_core::{Config, Error, Result};
use sonos_control::{self as control, ZoneGroup};
use sonos_vad_control::RoomKey;

/// Resolve the effective seed host and fold it into `config`.
///
/// A seed may arrive two ways: as a command-line flag / service argument
/// (`arg`), or from the configuration file (`config.sonos.seed_host`). The flag
/// wins, and blank values are treated as absent.
///
/// Writing the result *back* into `config` is the point of this function, not a
/// side effect. Both service hosts resolve the seed once for discovery, then
/// hand `config` to [`MultiRoomBridge`](crate::MultiRoomBridge) as the template
/// every per-room session is cloned from — and a single-room bridge later
/// resolves its own player from `sonos.seed_host`. Leaving the file's (often
/// absent) value in the template silently downgraded those bridges to SSDP
/// multicast even though the caller had named a seed, which fails on any
/// network that does not carry multicast.
///
/// Returns the resolved seed for the caller's own discovery calls.
pub fn apply_seed_host(arg: Option<&str>, config: &mut Config) -> Option<String> {
    let seed = arg
        .or(config.sonos.seed_host.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    config.sonos.seed_host.clone_from(&seed);
    seed
}

/// Discover Sonos rooms, de-duplicated to one entry per zone-group coordinator
/// where the topology is available.
///
/// Grouped players share one transport, so binding every member would create
/// several Windows outputs that all fight over the same speakers. A four-piece
/// home-theatre set (soundbar + sub + two surrounds) is a single room, not four.
///
/// `seed` selects the discovery route: `None` uses SSDP multicast, `Some(host)`
/// asks a known player over unicast.
///
/// # Errors
///
/// Returns [`Error::Control`] when discovery fails or finds nothing.
pub fn discover_rooms(seed: Option<&str>, timeout: Duration) -> Result<Vec<RoomKey>> {
    let devices = control::discover_household(seed, timeout)?;
    if devices.is_empty() {
        return Err(Error::control(
            "no Sonos devices were found on the local network",
        ));
    }

    // Topology is a refinement, not a requirement: without it every player
    // becomes its own output, which is wrong for grouped rooms but still
    // usable, and far better than refusing to start.
    let groups = control::household_zone_groups(seed, timeout).unwrap_or_else(|err| {
        tracing::debug!(%err, "zone-group topology unavailable; binding every player");
        Vec::new()
    });

    Ok(rooms_from_topology(&devices, &groups))
}

/// Reduce a device list plus its zone-group topology to one [`RoomKey`] per
/// addressable room.
///
/// Split out from [`discover_rooms`] so the de-duplication is testable without
/// a network.
#[must_use]
pub fn rooms_from_topology(devices: &[control::SonosDevice], groups: &[ZoneGroup]) -> Vec<RoomKey> {
    let mut rooms: Vec<RoomKey> = Vec::new();
    for device in devices {
        if is_non_coordinator(groups, &device.uuid) {
            tracing::debug!(room = %device.room_name, "skipping grouped non-coordinator");
            continue;
        }
        rooms.push(RoomKey::new(device.uuid.clone(), device.room_name.clone()));
    }

    // Deterministic order keeps slot assignment stable between runs, so a room
    // keeps the same Windows output across restarts.
    rooms.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.uuid.cmp(&b.uuid)));
    rooms
}

/// Whether `uuid` belongs to a multi-member zone group but is not its
/// coordinator.
#[must_use]
pub fn is_non_coordinator(groups: &[ZoneGroup], uuid: &str) -> bool {
    groups.iter().any(|group| {
        group.members.len() > 1
            && group.contains_uuid(uuid)
            && !group.coordinator_uuid.eq_ignore_ascii_case(uuid)
    })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use sonos_control::{SonosDevice, ZoneGroupMember};

    use super::*;

    fn member(uuid: &str) -> ZoneGroupMember {
        ZoneGroupMember {
            uuid: uuid.to_owned(),
            zone_name: uuid.to_owned(),
            location: "http://192.168.1.10:1400/xml/device_description.xml".to_owned(),
        }
    }

    fn group(coordinator: &str, members: &[&str]) -> ZoneGroup {
        ZoneGroup {
            id: format!("{coordinator}:1"),
            coordinator_uuid: coordinator.to_owned(),
            members: members.iter().copied().map(member).collect(),
        }
    }

    fn device(uuid: &str, room: &str, last_octet: u8) -> SonosDevice {
        SonosDevice {
            uuid: uuid.to_owned(),
            room_name: room.to_owned(),
            model_name: "Sonos One".to_owned(),
            ip: Ipv4Addr::new(192, 168, 1, last_octet),
        }
    }

    #[test]
    fn a_lone_player_is_never_a_non_coordinator() {
        assert!(!is_non_coordinator(&[group("A", &["A"])], "A"));
    }

    #[test]
    fn a_seed_argument_overrides_the_configured_one() {
        let mut config = Config::default();
        config.sonos.seed_host = Some("192.168.1.10".to_owned());

        let seed = apply_seed_host(Some("192.168.1.99"), &mut config);

        assert_eq!(seed.as_deref(), Some("192.168.1.99"));
        assert_eq!(config.sonos.seed_host.as_deref(), Some("192.168.1.99"));
    }

    #[test]
    fn a_seed_argument_reaches_the_session_template() {
        // The regression this guards: the seed used to be resolved for
        // discovery only, leaving `sonos.seed_host` unset in the template every
        // per-room bridge is cloned from. Those bridges then fell back to SSDP
        // multicast and failed on networks that do not carry it, even though
        // discovery itself had just succeeded over unicast.
        let mut config = Config::default();
        assert!(config.sonos.seed_host.is_none());

        let seed = apply_seed_host(Some("192.168.1.10"), &mut config);

        assert_eq!(seed.as_deref(), Some("192.168.1.10"));
        assert_eq!(config.sonos.seed_host.as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn a_configured_seed_survives_an_absent_argument() {
        let mut config = Config::default();
        config.sonos.seed_host = Some("  192.168.1.10  ".to_owned());

        let seed = apply_seed_host(None, &mut config);

        // Trimmed, so the value the bridge dials matches the one discovery used.
        assert_eq!(seed.as_deref(), Some("192.168.1.10"));
        assert_eq!(config.sonos.seed_host.as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn a_blank_seed_is_treated_as_absent() {
        let mut config = Config::default();
        config.sonos.seed_host = Some("   ".to_owned());

        // Blank must clear rather than persist: an empty string is not a host,
        // and leaving it set would send the bridge to a nonsense address
        // instead of falling back to multicast discovery.
        assert_eq!(apply_seed_host(Some("  "), &mut config), None);
        assert_eq!(config.sonos.seed_host, None);
    }

    #[test]
    fn grouped_members_other_than_the_coordinator_are_skipped() {
        let groups = [group("A", &["A", "B"])];
        assert!(!is_non_coordinator(&groups, "A"));
        assert!(is_non_coordinator(&groups, "B"));
    }

    #[test]
    fn unknown_uuids_are_left_alone() {
        assert!(!is_non_coordinator(&[group("A", &["A", "B"])], "Z"));
    }

    #[test]
    fn a_bonded_home_theatre_collapses_to_one_room() {
        // The real shape this exists for: a Playbar bonded with a Sub and two
        // Play:5 surrounds is one room, not four outputs fighting each other.
        let devices = [
            device("PLAYBAR", "Wohnzimmer", 25),
            device("SUB", "Wohnzimmer", 31),
            device("SURROUND_L", "Wohnzimmer", 32),
            device("SURROUND_R", "Wohnzimmer", 37),
        ];
        let groups = [group(
            "PLAYBAR",
            &["PLAYBAR", "SUB", "SURROUND_L", "SURROUND_R"],
        )];

        let rooms = rooms_from_topology(&devices, &groups);
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].uuid, "PLAYBAR");
        assert_eq!(rooms[0].name, "Wohnzimmer");
    }

    #[test]
    fn ungrouped_players_each_become_a_room() {
        let devices = [device("KUECHE", "Küche", 26), device("BAD", "Bad", 27)];
        let rooms = rooms_from_topology(&devices, &[]);
        assert_eq!(rooms.len(), 2);
        // Sorted by name: "Bad" before "Küche".
        assert_eq!(rooms[0].name, "Bad");
        assert_eq!(rooms[1].name, "Küche");
    }

    #[test]
    fn ordering_is_stable_regardless_of_discovery_order() {
        let forwards = [device("B", "Beta", 2), device("A", "Alpha", 1)];
        let backwards = [device("A", "Alpha", 1), device("B", "Beta", 2)];
        assert_eq!(
            rooms_from_topology(&forwards, &[]),
            rooms_from_topology(&backwards, &[])
        );
    }
}
