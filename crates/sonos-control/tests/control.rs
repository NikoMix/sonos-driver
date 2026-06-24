//! Integration tests for UPnP SOAP control against a mock SOAP endpoint.
//!
//! Each test spins up a one-shot loopback HTTP server, drives a
//! [`SonosController`] bound to it with [`SonosController::with_endpoints`], and
//! asserts on the exact request the controller emitted: the `SOAPACTION` header,
//! the SOAP envelope contents, and (for faults) the mapped error.

mod common;

use std::net::Ipv4Addr;

use sonos_control::{SonosController, SonosDevice};

fn test_device() -> SonosDevice {
    SonosDevice {
        uuid: "RINCON_TEST01400".to_owned(),
        room_name: "Test Room".to_owned(),
        model_name: "Sonos Test".to_owned(),
        ip: Ipv4Addr::LOCALHOST,
    }
}

fn controller_for(addr: std::net::SocketAddr) -> SonosController {
    let av = format!("http://{addr}/MediaRenderer/AVTransport/Control");
    let rc = format!("http://{addr}/MediaRenderer/RenderingControl/Control");
    SonosController::with_endpoints(test_device(), av, rc)
}

#[test]
fn set_av_transport_uri_emits_action_and_didl_metadata() {
    let (addr, handle) = common::serve_http_once(common::http_ok(
        "<u:SetAVTransportURIResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"/>",
    ));
    let controller = controller_for(addr);

    controller
        .set_stream_uri("http://127.0.0.1:8080/stream.wav", "audio/L16")
        .expect("control call should succeed");

    let request = handle.join().expect("server thread");
    assert!(
        request.contains("POST /MediaRenderer/AVTransport/Control HTTP/1.1"),
        "request line / path wrong:\n{request}"
    );
    assert!(
        request.contains(
            "SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI\""
        ),
        "missing/incorrect SOAPACTION header:\n{request}"
    );
    assert!(request
        .contains("<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"));
    assert!(request.contains("<InstanceID>0</InstanceID>"));
    assert!(request.contains("<CurrentURI>http://127.0.0.1:8080/stream.wav</CurrentURI>"));
    // DIDL-Lite metadata is passed as an argument value, so it must arrive
    // escaped inside CurrentURIMetaData.
    assert!(
        request.contains("&lt;DIDL-Lite"),
        "DIDL metadata not escaped into the envelope:\n{request}"
    );
    assert!(request.contains("audio/L16"));
}

#[test]
fn play_emits_play_action() {
    let (addr, handle) = common::serve_http_once(common::http_ok(
        "<u:PlayResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"/>",
    ));
    let controller = controller_for(addr);

    controller.play().expect("play should succeed");

    let request = handle.join().expect("server thread");
    assert!(request.contains("SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#Play\""));
    assert!(request.contains("<u:Play xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"));
    assert!(request.contains("<Speed>1</Speed>"));
}

#[test]
fn stop_emits_stop_action() {
    let (addr, handle) = common::serve_http_once(common::http_ok(
        "<u:StopResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"/>",
    ));
    let controller = controller_for(addr);

    controller.stop().expect("stop should succeed");

    let request = handle.join().expect("server thread");
    assert!(request.contains("SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#Stop\""));
    assert!(request.contains("<u:Stop xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"));
}

#[test]
fn set_volume_targets_rendering_control() {
    let (addr, handle) = common::serve_http_once(common::http_ok(
        "<u:SetVolumeResponse xmlns:u=\"urn:schemas-upnp-org:service:RenderingControl:1\"/>",
    ));
    let controller = controller_for(addr);

    controller
        .set_volume(30)
        .expect("set_volume should succeed");

    let request = handle.join().expect("server thread");
    assert!(
        request.contains("POST /MediaRenderer/RenderingControl/Control HTTP/1.1"),
        "set_volume must hit the RenderingControl endpoint:\n{request}"
    );
    assert!(request
        .contains("SOAPACTION: \"urn:schemas-upnp-org:service:RenderingControl:1#SetVolume\""));
    assert!(request.contains("<Channel>Master</Channel>"));
    assert!(request.contains("<DesiredVolume>30</DesiredVolume>"));
}

#[test]
fn soap_fault_is_mapped_to_typed_control_error() {
    let fault = "<?xml version=\"1.0\"?>\
        <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>\
        <s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>\
        <detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
        <errorCode>701</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>";
    let (addr, handle) =
        common::serve_http_once(common::http_status(500, "Internal Server Error", fault));
    let controller = controller_for(addr);

    let err = controller
        .play()
        .expect_err("a SOAP fault must surface as an error");
    handle.join().expect("server thread");

    let message = err.to_string();
    assert!(
        message.contains("701"),
        "error should carry the UPnP code: {message}"
    );
    assert!(
        message.contains("transition not available"),
        "error should explain the UPnP code: {message}"
    );
}
