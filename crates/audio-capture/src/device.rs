//! Render-endpoint description and the pure device-selection logic.
//!
//! Enumerating endpoints touches WASAPI (see [`crate::wasapi`]), but *matching* a
//! user's [`audio_core::CaptureConfig::device`] string against the enumerated
//! endpoints is plain data logic kept here so it can be unit-tested with fakes and
//! no audio hardware.

use audio_core::{Error, Result};

/// A Windows audio **render** endpoint discovered during enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    /// Stable WASAPI endpoint id, e.g. `{0.0.0.00000000}.{guid}`.
    pub id: String,
    /// Human-friendly name, e.g. `Speakers (Realtek(R) Audio)`.
    pub name: String,
    /// Whether this endpoint is the current default render device.
    pub is_default: bool,
}

impl EndpointInfo {
    /// Construct an endpoint descriptor.
    #[must_use]
    pub(crate) fn new(id: impl Into<String>, name: impl Into<String>, is_default: bool) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            is_default,
        }
    }
}

/// Resolve a `CaptureConfig::device` selector against the enumerated endpoints.
///
/// * `None` → the current default render endpoint.
/// * `Some(s)` → an **exact, case-insensitive id match** if one exists, otherwise
///   the first endpoint whose friendly name *contains* `s` (case-insensitive).
///
/// Matching id before name means an exact endpoint id always wins over an
/// ambiguous name fragment.
///
/// # Errors
///
/// Returns [`Error::Capture`] when the endpoint list is empty, a `None` selector
/// finds no default, the selector is blank, or a `Some` selector matches nothing.
pub(crate) fn select<'a>(
    endpoints: &'a [EndpointInfo],
    selector: Option<&str>,
) -> Result<&'a EndpointInfo> {
    if endpoints.is_empty() {
        return Err(Error::capture("no active render endpoints were found"));
    }

    let Some(want) = selector else {
        return endpoints
            .iter()
            .find(|endpoint| endpoint.is_default)
            .or_else(|| endpoints.first())
            .ok_or_else(|| Error::capture("no default render endpoint is available"));
    };

    let want = want.trim();
    if want.is_empty() {
        // An explicitly blank selector is a configuration mistake rather than an
        // implicit "use the default"; surface it instead of silently guessing.
        return Err(Error::capture("capture device selector is empty"));
    }

    if let Some(exact) = endpoints
        .iter()
        .find(|endpoint| endpoint.id.eq_ignore_ascii_case(want))
    {
        return Ok(exact);
    }

    let needle = want.to_ascii_lowercase();
    endpoints
        .iter()
        .find(|endpoint| endpoint.name.to_ascii_lowercase().contains(&needle))
        .ok_or_else(|| Error::capture(format!("no render endpoint matches device {want:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints() -> Vec<EndpointInfo> {
        vec![
            EndpointInfo::new("{0.0.0.00000000}.{aaaa}", "Speakers (Realtek Audio)", false),
            EndpointInfo::new("{0.0.0.00000000}.{bbbb}", "Headphones (USB DAC)", true),
            EndpointInfo::new("{0.0.0.00000000}.{cccc}", "Digital Output (HDMI)", false),
        ]
    }

    #[test]
    fn empty_list_is_an_error() {
        assert!(select(&[], None).is_err());
        assert!(select(&[], Some("anything")).is_err());
    }

    #[test]
    fn none_selects_the_default_endpoint() {
        let list = endpoints();
        let chosen = select(&list, None).expect("default exists");
        assert_eq!(chosen.id, "{0.0.0.00000000}.{bbbb}");
        assert!(chosen.is_default);
    }

    #[test]
    fn none_falls_back_to_first_when_no_default_flagged() {
        let mut list = endpoints();
        for endpoint in &mut list {
            endpoint.is_default = false;
        }
        let chosen = select(&list, None).expect("falls back to first");
        assert_eq!(chosen.id, "{0.0.0.00000000}.{aaaa}");
    }

    #[test]
    fn exact_id_match_is_case_insensitive() {
        let list = endpoints();
        let chosen = select(&list, Some("{0.0.0.00000000}.{CCCC}")).expect("id match");
        assert_eq!(chosen.name, "Digital Output (HDMI)");
    }

    #[test]
    fn name_substring_match_is_case_insensitive() {
        let list = endpoints();
        let chosen = select(&list, Some("usb dac")).expect("name match");
        assert_eq!(chosen.id, "{0.0.0.00000000}.{bbbb}");
    }

    #[test]
    fn id_match_takes_precedence_over_name() {
        // A selector that is simultaneously a valid id and a name fragment must
        // resolve via the id.
        let list = vec![
            EndpointInfo::new("Headphones", "Speakers (Realtek Audio)", false),
            EndpointInfo::new("{0.0.0.00000000}.{bbbb}", "Headphones (USB DAC)", true),
        ];
        let chosen = select(&list, Some("Headphones")).expect("resolves");
        assert_eq!(chosen.id, "Headphones");
    }

    #[test]
    fn unknown_selector_is_a_clear_error() {
        let list = endpoints();
        let err = select(&list, Some("Bluetooth Soundbar")).unwrap_err();
        assert!(matches!(err, Error::Capture(_)));
        assert!(err.to_string().contains("Bluetooth Soundbar"));
    }

    #[test]
    fn blank_selector_is_rejected() {
        let list = endpoints();
        assert!(select(&list, Some("   ")).is_err());
    }
}
