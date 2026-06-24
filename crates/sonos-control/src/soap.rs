//! UPnP SOAP envelope construction, dispatch, and fault handling.
//!
//! Sonos services are controlled with SOAP 1.1 over HTTP. Each call posts an
//! envelope to a service's control URL with a `SOAPACTION` header naming the
//! `serviceType#action`. Successful calls return `200 OK`; failures return
//! `500` with an `<s:Fault>` carrying a UPnP `errorCode`. Both are mapped to a
//! typed [`audio_core::Error`].

use std::fmt::Write as _;
use std::time::Duration;

use audio_core::{Error, Result};

use crate::http;
use crate::xml::{escape, first_element_text};

/// `AVTransport` service type (playback transport: URI, play, stop).
pub(crate) const AV_TRANSPORT_SERVICE: &str = "urn:schemas-upnp-org:service:AVTransport:1";
/// `RenderingControl` service type (volume, mute).
pub(crate) const RENDERING_CONTROL_SERVICE: &str =
    "urn:schemas-upnp-org:service:RenderingControl:1";
/// `ZoneGroupTopology` service type (group / coordinator state).
pub(crate) const ZONE_GROUP_TOPOLOGY_SERVICE: &str =
    "urn:schemas-upnp-org:service:ZoneGroupTopology:1";

/// Build a SOAP 1.1 envelope for `action` on `service_type` with `args`.
///
/// Argument values are XML-escaped, so callers pass raw strings (including
/// nested XML such as DIDL-Lite metadata) without escaping them first.
pub(crate) fn build_envelope(service_type: &str, action: &str, args: &[(&str, &str)]) -> String {
    let mut body = String::with_capacity(256 + args.len() * 32);
    body.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>");
    body.push_str(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>",
    );
    write!(body, "<u:{action} xmlns:u=\"{service_type}\">")
        .expect("writing to a String is infallible");
    for (name, value) in args {
        write!(body, "<{name}>{}</{name}>", escape(value))
            .expect("writing to a String is infallible");
    }
    write!(body, "</u:{action}></s:Body></s:Envelope>").expect("writing to a String is infallible");
    body
}

/// The quoted `SOAPACTION` header value for `service_type#action`.
pub(crate) fn soap_action(service_type: &str, action: &str) -> String {
    format!("\"{service_type}#{action}\"")
}

/// Post a SOAP `action` to `control_url` and return the response body on
/// success, or a typed error describing the SOAP fault / transport failure.
pub(crate) fn call(
    control_url: &str,
    service_type: &str,
    action: &str,
    args: &[(&str, &str)],
    timeout: Duration,
) -> Result<String> {
    let envelope = build_envelope(service_type, action, args);
    let action_header = soap_action(service_type, action);
    let headers = [
        ("Content-Type", "text/xml; charset=\"utf-8\""),
        ("SOAPACTION", action_header.as_str()),
    ];

    tracing::debug!(control_url, action, "issuing SOAP call");
    let response = http::post(control_url, &headers, envelope.as_bytes(), timeout)?;
    if response.is_success() {
        Ok(response.body_str())
    } else {
        Err(fault_to_error(
            service_type,
            action,
            response.status,
            &response.body_str(),
        ))
    }
}

/// Translate a non-2xx SOAP response into a clear [`Error::Control`].
fn fault_to_error(service_type: &str, action: &str, status: u16, body: &str) -> Error {
    let code = first_element_text(body, "errorCode");
    let description = first_element_text(body, "errorDescription");
    let fault_string = first_element_text(body, "faultstring");

    let detail = match (&code, &description, &fault_string) {
        (Some(code), _, _) => {
            let meaning = description
                .clone()
                .or_else(|| upnp_error_meaning(code).map(ToOwned::to_owned))
                .unwrap_or_else(|| "unknown UPnP error".to_owned());
            format!("UPnP error {code} ({meaning})")
        }
        (None, _, Some(fault)) => fault.clone(),
        (None, _, None) => format!("HTTP {status}"),
    };

    Error::control(format!("{service_type}#{action} failed: {detail}"))
}

/// Human-readable meanings for the UPnP/Sonos error codes we are most likely to
/// surface, so failures are actionable without a spec lookup.
fn upnp_error_meaning(code: &str) -> Option<&'static str> {
    let meaning = match code {
        "401" => "invalid action",
        "402" => "invalid arguments",
        "501" => "action failed",
        "600" => "argument value invalid",
        "701" => "transition not available",
        "711" => "illegal seek target",
        "714" => "illegal MIME type",
        "715" => "content busy",
        "718" => "invalid instance id",
        "719" => "track index out of range",
        _ => return None,
    };
    Some(meaning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_envelope_with_escaped_arguments() {
        let env = build_envelope(
            AV_TRANSPORT_SERVICE,
            "SetAVTransportURI",
            &[("InstanceID", "0"), ("CurrentURI", "http://h/s?a=1&b=2")],
        );
        assert!(env.contains(
            "<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"
        ));
        assert!(env.contains("<InstanceID>0</InstanceID>"));
        assert!(env.contains("<CurrentURI>http://h/s?a=1&amp;b=2</CurrentURI>"));
        assert!(env.contains("</s:Envelope>"));
    }

    #[test]
    fn formats_soap_action_header() {
        assert_eq!(
            soap_action(AV_TRANSPORT_SERVICE, "Play"),
            "\"urn:schemas-upnp-org:service:AVTransport:1#Play\""
        );
    }

    #[test]
    fn maps_upnp_error_code_to_message() {
        let body = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
            <s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>
            <detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0">
            <errorCode>718</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#;
        let err = fault_to_error(AV_TRANSPORT_SERVICE, "SetAVTransportURI", 500, body);
        let msg = err.to_string();
        assert!(msg.contains("718"), "{msg}");
        assert!(msg.contains("invalid instance id"), "{msg}");
    }

    #[test]
    fn falls_back_to_http_status_without_fault() {
        let err = fault_to_error(RENDERING_CONTROL_SERVICE, "SetVolume", 503, "service down");
        assert!(err.to_string().contains("HTTP 503"));
    }
}
