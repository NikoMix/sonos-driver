//! Zone-group topology: resolving a room to the coordinator that must receive
//! transport commands.
//!
//! A stereo pair or a bonded group is driven through a single **coordinator**
//! player; sending `SetAVTransportURI`/`Play` to a non-coordinator member fails
//! or is ignored. `GetZoneGroupState` on the `ZoneGroupTopology` service returns
//! the full group layout (as an escaped XML string), which this module parses so
//! a room name can be mapped to its coordinator.

use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::Duration;

use audio_core::{Error, Result};
use serde::Deserialize;

use crate::xml::first_element_text;
use crate::{http, soap};

/// A Sonos zone group (a single room, a bonded stereo pair, or a multi-room
/// group), driven through its coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneGroup {
    /// Opaque group identifier reported by Sonos.
    pub id: String,
    /// UUID of the member that coordinates the group.
    pub coordinator_uuid: String,
    /// All members of the group, including the coordinator.
    pub members: Vec<ZoneGroupMember>,
}

impl ZoneGroup {
    /// The coordinator member, if present among [`ZoneGroup::members`].
    #[must_use]
    pub fn coordinator(&self) -> Option<&ZoneGroupMember> {
        self.members
            .iter()
            .find(|m| m.uuid == self.coordinator_uuid)
    }

    /// Whether any member's zone name matches `room` (case-insensitive).
    #[must_use]
    pub fn contains_room(&self, room: &str) -> bool {
        self.members
            .iter()
            .any(|m| m.zone_name.eq_ignore_ascii_case(room))
    }
}

/// One player within a [`ZoneGroup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneGroupMember {
    /// Stable player UUID (e.g. `RINCON_…01400`).
    pub uuid: String,
    /// The room/zone name shown to the user.
    pub zone_name: String,
    /// Device-description URL, from which the IPv4 address is derived.
    pub location: String,
}

impl ZoneGroupMember {
    /// The member's IPv4 address, parsed from its [`location`](Self::location).
    #[must_use]
    pub fn ip(&self) -> Option<Ipv4Addr> {
        http::parse_http_url(&self.location)
            .ok()
            .and_then(|url| Ipv4Addr::from_str(&url.host).ok())
    }
}

/// Find the coordinator that drives the group containing `room`.
#[must_use]
pub fn find_room_coordinator<'a>(
    groups: &'a [ZoneGroup],
    room: &str,
) -> Option<&'a ZoneGroupMember> {
    groups
        .iter()
        .find(|g| g.contains_room(room))
        .and_then(ZoneGroup::coordinator)
}

/// Parse a `ZoneGroupState` document into the list of [`ZoneGroup`]s.
///
/// Accepts both the `<ZoneGroupState><ZoneGroups>…` wrapper form and a bare
/// `<ZoneGroups>…` document.
///
/// # Errors
///
/// Returns [`Error::Control`] if the XML matches neither shape.
pub fn parse_zone_group_state(xml: &str) -> Result<Vec<ZoneGroup>> {
    if let Ok(state) = quick_xml::de::from_str::<ZoneGroupStateXml>(xml) {
        return Ok(state.zone_groups.into_groups());
    }
    let groups: ZoneGroupsXml = quick_xml::de::from_str(xml)
        .map_err(|e| Error::control(format!("invalid zone group state XML: {e}")))?;
    Ok(groups.into_groups())
}

/// Fetch and parse the zone-group topology from a player's
/// `ZoneGroupTopology` control endpoint.
pub(crate) fn fetch_zone_groups(control_url: &str, timeout: Duration) -> Result<Vec<ZoneGroup>> {
    let body = soap::call(
        control_url,
        soap::ZONE_GROUP_TOPOLOGY_SERVICE,
        "GetZoneGroupState",
        &[],
        timeout,
    )?;
    let state = first_element_text(&body, "ZoneGroupState")
        .ok_or_else(|| Error::control("GetZoneGroupState response missing ZoneGroupState"))?;
    parse_zone_group_state(&state)
}

#[derive(Debug, Deserialize)]
struct ZoneGroupStateXml {
    #[serde(rename = "ZoneGroups")]
    zone_groups: ZoneGroupsXml,
}

#[derive(Debug, Deserialize)]
struct ZoneGroupsXml {
    #[serde(rename = "ZoneGroup", default)]
    groups: Vec<ZoneGroupXml>,
}

impl ZoneGroupsXml {
    fn into_groups(self) -> Vec<ZoneGroup> {
        self.groups
            .into_iter()
            .map(|g| ZoneGroup {
                id: g.id,
                coordinator_uuid: g.coordinator,
                members: g
                    .members
                    .into_iter()
                    .map(|m| ZoneGroupMember {
                        uuid: m.uuid,
                        zone_name: m.zone_name,
                        location: m.location,
                    })
                    .collect(),
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct ZoneGroupXml {
    #[serde(rename = "@Coordinator", default)]
    coordinator: String,
    #[serde(rename = "@ID", default)]
    id: String,
    #[serde(rename = "ZoneGroupMember", default)]
    members: Vec<ZoneGroupMemberXml>,
}

#[derive(Debug, Deserialize)]
struct ZoneGroupMemberXml {
    #[serde(rename = "@UUID", default)]
    uuid: String,
    #[serde(rename = "@ZoneName", default)]
    zone_name: String,
    #[serde(rename = "@Location", default)]
    location: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = r#"<ZoneGroupState><ZoneGroups>
        <ZoneGroup Coordinator="RINCON_AAA01400" ID="RINCON_AAA01400:1">
            <ZoneGroupMember UUID="RINCON_AAA01400" ZoneName="Living Room"
                Location="http://192.168.1.50:1400/xml/device_description.xml"/>
            <ZoneGroupMember UUID="RINCON_BBB01400" ZoneName="Living Room"
                Location="http://192.168.1.51:1400/xml/device_description.xml"/>
        </ZoneGroup>
        <ZoneGroup Coordinator="RINCON_CCC01400" ID="RINCON_CCC01400:5">
            <ZoneGroupMember UUID="RINCON_CCC01400" ZoneName="Kitchen"
                Location="http://192.168.1.60:1400/xml/device_description.xml"/>
        </ZoneGroup>
    </ZoneGroups></ZoneGroupState>"#;

    #[test]
    fn parses_groups_and_members() {
        let groups = parse_zone_group_state(STATE).expect("parse");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].coordinator_uuid, "RINCON_AAA01400");
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].coordinator().unwrap().uuid, "RINCON_AAA01400");
    }

    #[test]
    fn resolves_stereo_pair_to_coordinator_case_insensitively() {
        let groups = parse_zone_group_state(STATE).expect("parse");
        let coord = find_room_coordinator(&groups, "living room").expect("coordinator");
        assert_eq!(coord.uuid, "RINCON_AAA01400");
        assert_eq!(coord.ip(), Some(Ipv4Addr::new(192, 168, 1, 50)));
    }

    #[test]
    fn unknown_room_resolves_to_none() {
        let groups = parse_zone_group_state(STATE).expect("parse");
        assert!(find_room_coordinator(&groups, "Garage").is_none());
    }

    #[test]
    fn parses_bare_zone_groups_form() {
        let bare = r#"<ZoneGroups><ZoneGroup Coordinator="RINCON_X" ID="RINCON_X:1">
            <ZoneGroupMember UUID="RINCON_X" ZoneName="Den"
                Location="http://10.0.0.2:1400/xml/device_description.xml"/>
        </ZoneGroup></ZoneGroups>"#;
        let groups = parse_zone_group_state(bare).expect("parse");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members[0].zone_name, "Den");
    }

    #[test]
    fn fetch_zone_groups_round_trips_get_zone_group_state() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        // A realistic GetZoneGroupState response: the ZoneGroupState out-argument
        // carries the topology as an escaped XML string.
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nConnection: close\r\n\r\n\
            <?xml version=\"1.0\"?>\
            <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>\
            <u:GetZoneGroupStateResponse xmlns:u=\"urn:schemas-upnp-org:service:ZoneGroupTopology:1\">\
            <ZoneGroupState>&lt;ZoneGroups&gt;\
            &lt;ZoneGroup Coordinator=&quot;RINCON_AAA01400&quot; ID=&quot;RINCON_AAA01400:1&quot;&gt;\
            &lt;ZoneGroupMember UUID=&quot;RINCON_AAA01400&quot; ZoneName=&quot;Living Room&quot; \
            Location=&quot;http://127.0.0.1:1400/xml/device_description.xml&quot;/&gt;\
            &lt;ZoneGroupMember UUID=&quot;RINCON_BBB01400&quot; ZoneName=&quot;Living Room&quot; \
            Location=&quot;http://127.0.0.1:1401/xml/device_description.xml&quot;/&gt;\
            &lt;/ZoneGroup&gt;&lt;/ZoneGroups&gt;</ZoneGroupState>\
            </u:GetZoneGroupStateResponse></s:Body></s:Envelope>";

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            stream.write_all(response.as_bytes()).expect("write");
            stream.flush().expect("flush");
        });

        let url = format!("http://{addr}/ZoneGroupTopology/Control");
        let groups = fetch_zone_groups(&url, Duration::from_secs(5)).expect("fetch");
        handle.join().expect("join");

        assert_eq!(groups.len(), 1);
        let coord = find_room_coordinator(&groups, "Living Room").expect("coordinator");
        assert_eq!(coord.uuid, "RINCON_AAA01400");
        assert_eq!(coord.ip(), Some(Ipv4Addr::LOCALHOST));
    }
}
