//! DIDL-Lite metadata construction for `SetAVTransportURI`.
//!
//! Sonos expects a small DIDL-Lite document describing the item being played
//! alongside the stream URI. For a continuous bridge stream the natural class is
//! `object.item.audioItem.audioBroadcast` (a live stream of unknown length). The
//! returned string is *not* pre-escaped for SOAP; the envelope builder escapes it
//! when it is embedded as the `CurrentURIMetaData` argument value.

use crate::xml::escape;

/// The Sonos "content directory unique device name" marker used for
/// locally-sourced / third-party streams.
const SONOS_CDUDN: &str = "SA_RINCON65031_";

/// Build a DIDL-Lite metadata blob for a live audio stream.
///
/// `title` is shown in the Sonos app, `uri` is the stream URL the player will
/// pull, and `content_type` is advertised in the resource's `protocolInfo`.
pub(crate) fn audio_broadcast_metadata(title: &str, uri: &str, content_type: &str) -> String {
    let title = escape(title);
    let uri = escape(uri);
    let protocol_info = format!("http-get:*:{}:*", escape(content_type));
    format!(
        concat!(
            "<DIDL-Lite ",
            "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
            "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" ",
            "xmlns:r=\"urn:schemas-rinconnetworks-com:metadata-1-0/\" ",
            "xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\">",
            "<item id=\"0\" parentID=\"-1\" restricted=\"true\">",
            "<dc:title>{title}</dc:title>",
            "<upnp:class>object.item.audioItem.audioBroadcast</upnp:class>",
            "<res protocolInfo=\"{protocol_info}\">{uri}</res>",
            "<desc id=\"cdudn\" ",
            "nameSpace=\"urn:schemas-rinconnetworks-com:metadata-1-0/\">{cdudn}</desc>",
            "</item>",
            "</DIDL-Lite>"
        ),
        title = title,
        protocol_info = protocol_info,
        uri = uri,
        cdudn = SONOS_CDUDN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_escaped_title_uri_and_protocol_info() {
        let didl = audio_broadcast_metadata(
            "Bridge & Co",
            "http://host:8080/stream?x=1&y=2",
            "audio/L16",
        );
        assert!(didl.contains("<dc:title>Bridge &amp; Co</dc:title>"));
        assert!(didl.contains("protocolInfo=\"http-get:*:audio/L16:*\""));
        assert!(didl.contains("http://host:8080/stream?x=1&amp;y=2"));
        assert!(didl.contains("object.item.audioItem.audioBroadcast"));
        assert!(didl.contains(SONOS_CDUDN));
    }
}
