//! Sonos device-description (`/xml/device_description.xml`) fetch and parsing.
//!
//! The SSDP `LOCATION` URL points at this document; parsing it yields the room
//! name, model, and stable UUID needed to build a [`SonosDevice`].

use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::Duration;

use audio_core::{Error, Result};
use serde::Deserialize;

use crate::http;
use crate::SonosDevice;

#[derive(Debug, Deserialize)]
struct DeviceDescription {
    device: DeviceNode,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeviceNode {
    #[serde(rename = "UDN")]
    udn: String,
    #[serde(rename = "roomName")]
    room_name: String,
    #[serde(rename = "modelName")]
    model_name: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "friendlyName")]
    friendly_name: String,
}

/// Fetch and parse the device description at `location_url`, deriving the
/// device's IPv4 address from the URL host.
///
/// # Errors
///
/// Returns [`Error::Control`] if the URL is malformed, the host is not an IPv4
/// literal, the HTTP request fails, or the XML cannot be parsed.
pub fn fetch_device(location_url: &str, timeout: Duration) -> Result<SonosDevice> {
    let parsed = http::parse_http_url(location_url)?;
    let ip = Ipv4Addr::from_str(&parsed.host).map_err(|_| {
        Error::control(format!(
            "device location host is not an IPv4 address: {}",
            parsed.host
        ))
    })?;
    let response = http::get(location_url, timeout)?;
    if !response.is_success() {
        return Err(Error::control(format!(
            "device description fetch failed: HTTP {}",
            response.status
        )));
    }
    parse_device_description(&response.body_str(), ip)
}

/// Parse a device-description XML document into a [`SonosDevice`].
///
/// # Errors
///
/// Returns [`Error::Control`] if the XML is not a recognisable device
/// description.
pub fn parse_device_description(xml: &str, ip: Ipv4Addr) -> Result<SonosDevice> {
    let parsed: DeviceDescription = quick_xml::de::from_str(xml)
        .map_err(|e| Error::control(format!("invalid device description XML: {e}")))?;
    let device = parsed.device;

    let uuid = device
        .udn
        .strip_prefix("uuid:")
        .unwrap_or(&device.udn)
        .trim()
        .to_owned();
    if uuid.is_empty() {
        return Err(Error::control("device description missing UDN"));
    }

    let room_name = first_non_empty([device.room_name, device.friendly_name], "Sonos");
    let model_name = first_non_empty([device.model_name, device.display_name], "Sonos");

    Ok(SonosDevice {
        uuid,
        room_name,
        model_name,
        ip,
    })
}

fn first_non_empty<const N: usize>(candidates: [String; N], fallback: &str) -> String {
    candidates
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType>
    <friendlyName>192.168.1.50 - Sonos One</friendlyName>
    <manufacturer>Sonos, Inc.</manufacturer>
    <modelName>Sonos One</modelName>
    <UDN>uuid:RINCON_000E58FACE0101400</UDN>
    <roomName>Living Room</roomName>
    <deviceList>
      <device><UDN>uuid:RINCON_NESTED</UDN><roomName>Ignored</roomName></device>
    </deviceList>
  </device>
</root>"#;

    #[test]
    fn parses_room_model_and_uuid() {
        let dev = parse_device_description(SAMPLE, Ipv4Addr::new(192, 168, 1, 50)).expect("parse");
        assert_eq!(dev.uuid, "RINCON_000E58FACE0101400");
        assert_eq!(dev.room_name, "Living Room");
        assert_eq!(dev.model_name, "Sonos One");
        assert_eq!(dev.ip, Ipv4Addr::new(192, 168, 1, 50));
    }

    #[test]
    fn falls_back_when_optional_fields_absent() {
        let xml = r"<root><device><UDN>uuid:RINCON_X</UDN></device></root>";
        let dev = parse_device_description(xml, Ipv4Addr::LOCALHOST).expect("parse");
        assert_eq!(dev.uuid, "RINCON_X");
        assert_eq!(dev.room_name, "Sonos");
        assert_eq!(dev.model_name, "Sonos");
    }

    #[test]
    fn rejects_non_device_xml() {
        assert!(parse_device_description("<nope/>", Ipv4Addr::LOCALHOST).is_err());
    }
}
