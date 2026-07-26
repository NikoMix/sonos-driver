//! Conversion between the editable settings form and the typed
//! [`audio_core::Config`].
//!
//! The form holds raw, user-editable strings (so text fields bind directly to
//! `egui` widgets) and validates them into a strongly typed [`Config`] on save.
//! The validation rules mirror the CLI's `validate()` so the app and the CLI
//! accept exactly the same configurations: a non-zero `TCP` port, a parseable
//! bind address, and a target sample rate within `8000..=384000` Hz (with
//! `0`/empty meaning "keep the native rate").
//!
//! Deliberately absent: the capture device and the Sonos target. Those are the
//! routing table's job now, and the worker overwrites `[capture]` per route, so
//! offering them here as well would let the two disagree.

use std::net::IpAddr;

use audio_core::{
    CaptureConfig, CodecProfile, Config, Error, IdleConfig, PipelineConfig, Result, SonosConfig,
    StreamConfig,
};

/// Every selectable [`CodecProfile`], in presentation order, for the picker.
/// Leads with the default ([`CodecProfile::Flac`]).
///
/// Only profiles with a shippable encoder are offered. AAC and MP3 have no
/// licence-clean pure-Rust encoder (see ADR-0004) and would fail at start, so
/// they are intentionally excluded; [`CodecProfile::has_encoder`] is the single
/// source of truth and [`SettingsForm::to_config`] rejects any other profile.
pub const ALL_PROFILES: [CodecProfile; 3] =
    [CodecProfile::Flac, CodecProfile::Wav, CodecProfile::L16Pcm];

/// A one-line description of what a profile costs, for the picker's help text.
#[must_use]
pub fn profile_hint(profile: CodecProfile) -> &'static str {
    match profile {
        CodecProfile::L16Pcm => "Lowest latency (~0.4-0.9 s), highest bandwidth (~1.4 Mbit/s)",
        CodecProfile::Wav => "Uncompressed, widest compatibility (~0.4-1.0 s)",
        CodecProfile::Flac => "Lossless, about half the bandwidth (~0.4-1.0 s) - recommended",
        CodecProfile::Aac | CodecProfile::Mp3 => "No shippable encoder (see ADR-0004)",
    }
}

/// The editable, string-backed mirror of [`Config`] bound to the settings UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    /// Target resample rate in Hz; empty or `0` keeps the capture rate.
    pub target_sample_rate: String,
    /// Jitter-buffer depth in milliseconds.
    pub buffer_ms: String,
    /// Streaming codec/latency profile.
    pub profile: CodecProfile,
    /// Address the local stream server binds to.
    pub bind_address: String,
    /// First `TCP` port for the local stream servers; routes count up from it.
    pub port: String,
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP, so this
    /// is the escape hatch for networks where SSDP multicast does not work.
    pub seed_host: String,
    /// Seconds to wait for `SSDP` discovery responses.
    pub discovery_timeout_secs: String,
    /// Seconds of silence after which a room is handed back; `0` never releases.
    pub idle_stop_after_secs: String,
}

impl Default for SettingsForm {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl SettingsForm {
    /// Build a form pre-populated from an existing [`Config`].
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            target_sample_rate: config
                .pipeline
                .target_sample_rate
                .map(|rate| rate.to_string())
                .unwrap_or_default(),
            buffer_ms: config.pipeline.buffer_ms.to_string(),
            profile: config.stream.profile,
            bind_address: config.stream.bind_address.clone(),
            port: config.stream.port.to_string(),
            seed_host: config.sonos.seed_host.clone().unwrap_or_default(),
            discovery_timeout_secs: config.sonos.discovery_timeout_secs.to_string(),
            idle_stop_after_secs: config.idle.stop_after_secs.to_string(),
        }
    }

    /// Validate the form into a typed [`Config`].
    ///
    /// # Errors
    ///
    /// Returns [`audio_core::Error::Config`] when any field is malformed: a
    /// non-numeric or zero port, an unparseable bind address, a target sample
    /// rate outside `8000..=384000` Hz, or a codec profile without a shippable
    /// encoder (AAC/MP3, see ADR-0004).
    pub fn to_config(&self) -> Result<Config> {
        let target_sample_rate = parse_sample_rate(&self.target_sample_rate)?;
        let buffer_ms = parse_u32_or(&self.buffer_ms, 80, "buffer-ms")?;
        let port = parse_port(&self.port)?;
        let bind_address = parse_bind_address(&self.bind_address)?;
        let seed_host = optional_trimmed(&self.seed_host);
        let discovery_timeout_secs =
            parse_u32_or(&self.discovery_timeout_secs, 3, "discovery-timeout-secs")?;
        let stop_after_secs = parse_u32_or(&self.idle_stop_after_secs, 0, "idle stop-after-secs")?;

        if !self.profile.has_encoder() {
            return Err(Error::config(format!(
                "codec profile '{}' has no encoder yet (see ADR-0004); choose l16-pcm, wav or flac",
                self.profile.name()
            )));
        }

        Ok(Config {
            // Every route overwrites these from its own source, so the base
            // config deliberately carries none.
            capture: CaptureConfig::default(),
            pipeline: PipelineConfig {
                target_sample_rate,
                buffer_ms,
            },
            stream: StreamConfig {
                profile: self.profile,
                bind_address,
                port,
            },
            sonos: SonosConfig {
                room: None,
                uuid: None,
                seed_host,
                discovery_timeout_secs,
            },
            idle: IdleConfig {
                stop_after_secs,
                ..IdleConfig::default()
            },
        })
    }

    /// The seed host, trimmed, or `None` when the field is blank.
    #[must_use]
    pub fn seed(&self) -> Option<String> {
        optional_trimmed(&self.seed_host)
    }
}

/// Trim `raw`, returning `None` when nothing remains.
fn optional_trimmed(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Parse an optional target sample rate, honouring the `0`/empty "native"
/// sentinel and the supported range.
fn parse_sample_rate(raw: &str) -> Result<Option<u32>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0" {
        return Ok(None);
    }
    let value: u32 = trimmed
        .parse()
        .map_err(|_| Error::config(format!("target sample rate '{trimmed}' is not a number")))?;
    if !(8_000..=384_000).contains(&value) {
        return Err(Error::config(format!(
            "target sample rate {value} Hz is outside the supported 8000-384000 Hz range"
        )));
    }
    Ok(Some(value))
}

/// Parse a `u32` field, falling back to `default` when the field is empty.
fn parse_u32_or(raw: &str, default: u32, field: &str) -> Result<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    trimmed
        .parse()
        .map_err(|_| Error::config(format!("{field} '{trimmed}' is not a number")))
}

/// Parse a non-zero `TCP` port.
fn parse_port(raw: &str) -> Result<u16> {
    let trimmed = raw.trim();
    let port: u16 = trimmed
        .parse()
        .map_err(|_| Error::config(format!("port '{trimmed}' is not a valid TCP port")))?;
    if port == 0 {
        return Err(Error::config("port must be between 1 and 65535"));
    }
    Ok(port)
}

/// Validate and normalise the stream server bind address.
fn parse_bind_address(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::config("bind address must not be empty"));
    }
    trimmed.parse::<IpAddr>().map_err(|_| {
        Error::config(format!(
            "bind address '{trimmed}' is not a valid IP address"
        ))
    })?;
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{profile_hint, SettingsForm, ALL_PROFILES};
    use audio_core::{CodecProfile, Config};

    #[test]
    fn default_form_round_trips_to_default_config() {
        let form = SettingsForm::default();
        assert_eq!(form.to_config().expect("valid"), Config::default());
    }

    #[test]
    fn round_trip_preserves_every_edited_field() {
        let mut config = Config::default();
        config.pipeline.target_sample_rate = Some(48_000);
        config.pipeline.buffer_ms = 120;
        config.stream.profile = CodecProfile::Wav;
        config.stream.port = 9100;
        config.sonos.seed_host = Some("192.168.1.50".to_owned());
        config.sonos.discovery_timeout_secs = 7;
        config.idle.stop_after_secs = 45;

        let form = SettingsForm::from_config(&config);
        assert_eq!(form.to_config().expect("valid"), config);
    }

    #[test]
    fn the_capture_device_and_room_are_never_written_back() {
        // They belong to the routing table; a stale copy here would silently
        // override a route's own source.
        let mut config = Config::default();
        config.capture.device = Some("Realtek".to_owned());
        config.sonos.room = Some("Küche".to_owned());

        let rebuilt = SettingsForm::from_config(&config)
            .to_config()
            .expect("valid");
        assert_eq!(rebuilt.capture.device, None);
        assert_eq!(rebuilt.sonos.room, None);
    }

    #[test]
    fn a_blank_seed_host_is_unset_rather_than_empty() {
        let form = SettingsForm {
            seed_host: "   ".to_owned(),
            ..SettingsForm::default()
        };
        assert_eq!(form.seed(), None);
        assert_eq!(form.to_config().expect("valid").sonos.seed_host, None);
    }

    #[test]
    fn a_seed_host_is_trimmed() {
        let form = SettingsForm {
            seed_host: "  192.168.1.50 ".to_owned(),
            ..SettingsForm::default()
        };
        assert_eq!(form.seed().as_deref(), Some("192.168.1.50"));
    }

    #[test]
    fn a_zero_or_blank_sample_rate_means_native() {
        for raw in ["", "0", "  "] {
            let form = SettingsForm {
                target_sample_rate: raw.to_owned(),
                ..SettingsForm::default()
            };
            assert_eq!(
                form.to_config().expect("valid").pipeline.target_sample_rate,
                None
            );
        }
    }

    #[test]
    fn an_out_of_range_sample_rate_is_rejected() {
        let form = SettingsForm {
            target_sample_rate: "1000".to_owned(),
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
    }

    #[test]
    fn a_zero_port_is_rejected() {
        let form = SettingsForm {
            port: "0".to_owned(),
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
    }

    #[test]
    fn a_malformed_bind_address_is_rejected() {
        let form = SettingsForm {
            bind_address: "not-an-ip".to_owned(),
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
    }

    #[test]
    fn an_encoderless_profile_is_rejected() {
        let form = SettingsForm {
            profile: CodecProfile::Aac,
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
    }

    #[test]
    fn every_offered_profile_has_an_encoder_and_a_hint() {
        for profile in ALL_PROFILES {
            assert!(profile.has_encoder(), "{profile:?}");
            assert!(!profile_hint(profile).is_empty(), "{profile:?}");
        }
    }

    #[test]
    fn idle_release_mirrors_the_core_default_and_round_trips() {
        let form = SettingsForm::default();
        assert_eq!(
            form.idle_stop_after_secs,
            Config::default().idle.stop_after_secs.to_string(),
            "the form must show what the pipeline will actually do"
        );

        let edited = SettingsForm {
            idle_stop_after_secs: "0".to_owned(),
            ..SettingsForm::default()
        };
        assert_eq!(
            edited.to_config().expect("valid").idle.stop_after_secs,
            0,
            "0 must survive as 'never release', not fall back to the default"
        );
    }
}
