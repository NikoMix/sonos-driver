//! Integration tests for SSDP discovery and device-description parsing against
//! loopback mocks (a UDP SSDP responder and a one-shot HTTP server) — no
//! multicast and no Sonos hardware required.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use sonos_control::{discover_with, fetch_device, SsdpSearch};

const DEVICE_DESCRIPTION: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
    <root xmlns=\"urn:schemas-upnp-org:device-1-0\">\
    <device>\
    <deviceType>urn:schemas-upnp-org:device:ZonePlayer:1</deviceType>\
    <friendlyName>127.0.0.1 - Sonos Test</friendlyName>\
    <roomName>Living Room</roomName>\
    <modelName>Sonos One</modelName>\
    <UDN>uuid:RINCON_000E58FACE0101400</UDN>\
    </device></root>";

#[test]
fn fetch_device_parses_mock_description() {
    let (addr, handle) = common::serve_http_once(common::http_ok(DEVICE_DESCRIPTION));
    let url = format!("http://{addr}/xml/device_description.xml");

    let device = fetch_device(&url, Duration::from_secs(5)).expect("fetch device");
    handle.join().expect("server thread");

    assert_eq!(device.room_name, "Living Room");
    assert_eq!(device.model_name, "Sonos One");
    assert_eq!(device.uuid, "RINCON_000E58FACE0101400");
    assert_eq!(device.ip, Ipv4Addr::LOCALHOST);
}

#[test]
fn ssdp_search_collects_unicast_response() {
    let location = "http://127.0.0.1:1400/xml/device_description.xml".to_owned();
    let (responder_addr, handle) = common::spawn_ssdp_responder(location.clone());

    let search = SsdpSearch::new()
        .with_destination(responder_addr)
        .with_bursts(1)
        .with_mx(1);
    let responses = search.search(Duration::from_secs(3)).expect("ssdp search");
    handle.join().expect("responder thread");

    assert_eq!(responses.len(), 1, "expected exactly one responder");
    assert_eq!(responses[0].location, location);
    assert_eq!(
        responses[0].search_target.as_deref(),
        Some("urn:schemas-upnp-org:device:ZonePlayer:1")
    );
}

#[test]
fn discover_with_resolves_device_end_to_end() {
    let (http_addr, http_handle) = common::serve_http_once(common::http_ok(DEVICE_DESCRIPTION));
    let location = format!("http://{http_addr}/xml/device_description.xml");
    let (responder_addr, ssdp_handle) = common::spawn_ssdp_responder(location);

    let search = SsdpSearch::new()
        .with_destination(responder_addr)
        .with_bursts(1)
        .with_mx(1);
    let devices = discover_with(&search, Duration::from_secs(3)).expect("discover");

    ssdp_handle.join().expect("responder thread");
    http_handle.join().expect("http thread");

    assert_eq!(devices.len(), 1, "discovery should yield one device");
    assert_eq!(devices[0].room_name, "Living Room");
    assert_eq!(devices[0].model_name, "Sonos One");
    assert_eq!(devices[0].uuid, "RINCON_000E58FACE0101400");
}
