//! Conversion between the editable settings form and the typed
//! [`audio_core::Config`].
//!
//! The form holds raw, user-editable strings (so text fields bind directly to
//! `egui` widgets) and validates them into a strongly typed [`Config`] on save.
//! The validation rules mirror the CLI's `validate()` so the GUI and CLI accept
//! exactly the same configurations: a non-zero `TCP` port, a parseable bind
//! address, a target sample rate within `8000..=384000` Hz (with `0`/empty
//! meaning "keep the native rate"), and trimmed-empty room/uuid treated as
//! unset.

use std::net::IpAddr;

use audio_core::{
    CaptureConfig, CodecProfile, Config, Error, PipelineConfig, Result, SonosConfig, StreamConfig,
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

/// The editable, string-backed mirror of [`Config`] bound to the settings UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    /// Capture device id or friendly-name substring; empty selects the default.
    pub device: String,
    /// Per-application capture target (process name or PID) carried verbatim
    /// from the loaded [`Config`]. The settings UI does not expose process
    /// selection — use the CLI `--process` flag — but the value is preserved so
    /// saving from the GUI never silently drops a `[capture] process` setting.
    pub process: Option<String>,
    /// Companion to [`SettingsForm::process`]: capture everything *except* the
    /// target app. Preserved verbatim on round-trip; not editable in the UI.
    pub exclude_process: bool,
    /// Target resample rate in Hz; empty or `0` keeps the capture rate.
    pub target_sample_rate: String,
    /// Jitter-buffer depth in milliseconds.
    pub buffer_ms: String,
    /// Streaming codec/latency profile.
    pub profile: CodecProfile,
    /// Address the local stream server binds to.
    pub bind_address: String,
    /// `TCP` port for the local stream server.
    pub port: String,
    /// Target room/zone name; empty means unset.
    pub room: String,
    /// Target device `UUID`; takes precedence over the room when set.
    pub uuid: String,
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP, so this
    /// is the escape hatch for networks where SSDP multicast does not work.
    pub seed_host: String,
    /// Seconds to wait for `SSDP` discovery responses.
    pub discovery_timeout_secs: String,
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
            device: config.capture.device.clone().unwrap_or_default(),
            process: config.capture.process.clone(),
            exclude_process: config.capture.exclude_process,
            target_sample_rate: config
                .pipeline
                .target_sample_rate
                .map(|rate| rate.to_string())
                .unwrap_or_default(),
            buffer_ms: config.pipeline.buffer_ms.to_string(),
            profile: config.stream.profile,
            bind_address: config.stream.bind_address.clone(),
            port: config.stream.port.to_string(),
            room: config.sonos.room.clone().unwrap_or_default(),
            uuid: config.sonos.uuid.clone().unwrap_or_default(),
            seed_host: config.sonos.seed_host.clone().unwrap_or_default(),
            discovery_timeout_secs: config.sonos.discovery_timeout_secs.to_string(),
        }
    }

    /// Validate the form into a typed [`Config`].
    ///
    /// # Errors
    ///
    /// Returns [`audio_core::Error::Config`] when any field is malformed: a
    /// non-numeric or zero port, an unparseable bind address, a target sample
    /// rate outside `8000..=384000` Hz, a codec profile without a shippable
    /// encoder (AAC/MP3, see ADR-0004), or `exclude-process` set without a
    /// process to exclude.
    pub fn to_config(&self) -> Result<Config> {
        let device = optional_trimmed(&self.device);
        let target_sample_rate = parse_sample_rate(&self.target_sample_rate)?;
        let buffer_ms = parse_u32_or(&self.buffer_ms, 80, "buffer-ms")?;
        let port = parse_port(&self.port)?;
        let bind_address = parse_bind_address(&self.bind_address)?;
        let room = optional_trimmed(&self.room);
        let uuid = optional_trimmed(&self.uuid);
        let seed_host = optional_trimmed(&self.seed_host);
        let discovery_timeout_secs =
            parse_u32_or(&self.discovery_timeout_secs, 0, "discovery-timeout-secs")?;

        if !self.profile.has_encoder() {
            return Err(Error::config(format!(
                "codec profile '{}' has no encoder yet (see ADR-0004); choose l16-pcm, wav or flac",
                self.profile.name()
            )));
        }
        if self.exclude_process && optional_trimmed(self.process.as_deref().unwrap_or("")).is_none()
        {
            return Err(Error::config(
                "exclude-process requires a capture process to exclude; set one or disable exclude-process",
            ));
        }

        Ok(Config {
            capture: CaptureConfig {
                device,
                process: self.process.clone(),
                exclude_process: self.exclude_process,
            },
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
                room,
                uuid,
                seed_host,
                discovery_timeout_secs,
            },
        })
    }

    /// Whether a Sonos target (room or `UUID`) is configured.
    #[must_use]
    pub fn has_target(&self) -> bool {
        !self.room.trim().is_empty() || !self.uuid.trim().is_empty()
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
    use super::{SettingsForm, ALL_PROFILES};
    use audio_core::{CodecProfile, Config};

    #[test]
    fn default_form_round_trips_to_default_config() {
        let form = SettingsForm::default();
        assert_eq!(form.to_config().expect("valid"), Config::default());
    }

    #[test]
    fn round_trip_preserves_a_custom_config() {
        let mut config = Config::default();
        config.capture.device = Some("Speakers".to_owned());
        config.pipeline.target_sample_rate = Some(48_000);
        config.pipeline.buffer_ms = 120;
        config.stream.profile = CodecProfile::Flac;
        config.stream.bind_address = "192.168.1.5".to_owned();
        config.stream.port = 9000;
        config.sonos.room = Some("Kitchen".to_owned());
        config.sonos.uuid = Some("RINCON_1234".to_owned());
        config.sonos.discovery_timeout_secs = 5;

        let form = SettingsForm::from_config(&config);
        assert_eq!(form.to_config().expect("valid"), config);
    }

    #[test]
    fn process_capture_fields_round_trip() {
        // Regression: the GUI form must carry per-application capture settings
        // (added by the process-loopback workstream) through a load -> save
        // cycle verbatim, even though the UI does not expose editing them.
        // Defaulting them here would silently drop a user's `[capture] process`.
        let mut config = Config::default();
        config.capture.process = Some("Spotify".to_owned());
        config.capture.exclude_process = true;

        let form = SettingsForm::from_config(&config);
        assert_eq!(form.process.as_deref(), Some("Spotify"));
        assert!(form.exclude_process);

        let round_tripped = form.to_config().expect("valid");
        assert_eq!(round_tripped.capture.process.as_deref(), Some("Spotify"));
        assert!(round_tripped.capture.exclude_process);
        assert_eq!(round_tripped, config);
    }

    #[test]
    fn native_sample_rate_maps_to_none() {
        let mut form = SettingsForm {
            target_sample_rate: "0".to_owned(),
            ..SettingsForm::default()
        };
        assert_eq!(
            form.to_config().expect("valid").pipeline.target_sample_rate,
            None
        );
        form.target_sample_rate = String::new();
        assert_eq!(
            form.to_config().expect("valid").pipeline.target_sample_rate,
            None
        );
    }

    #[test]
    fn zero_and_non_numeric_ports_are_rejected() {
        let mut form = SettingsForm {
            port: "0".to_owned(),
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
        form.port = "not-a-port".to_owned();
        assert!(form.to_config().is_err());
    }

    #[test]
    fn invalid_bind_address_is_rejected() {
        let form = SettingsForm {
            bind_address: "definitely not an ip".to_owned(),
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
    }

    #[test]
    fn out_of_range_sample_rate_is_rejected() {
        let mut form = SettingsForm {
            target_sample_rate: "1000".to_owned(),
            ..SettingsForm::default()
        };
        assert!(form.to_config().is_err());
        form.target_sample_rate = "500000".to_owned();
        assert!(form.to_config().is_err());
    }

    #[test]
    fn has_target_reflects_room_and_uuid() {
        let mut form = SettingsForm::default();
        assert!(!form.has_target());
        form.room = "  ".to_owned();
        assert!(!form.has_target());
        form.room = "Office".to_owned();
        assert!(form.has_target());
        form.room = String::new();
        form.uuid = "RINCON_9".to_owned();
        assert!(form.has_target());
    }

    #[test]
    fn all_profiles_have_encoders_and_lead_with_the_default() {
        assert_eq!(ALL_PROFILES.len(), 3);
        assert_eq!(ALL_PROFILES[0], CodecProfile::default());
        assert_eq!(ALL_PROFILES[0], CodecProfile::Flac);
        assert!(ALL_PROFILES.iter().all(|profile| profile.has_encoder()));
    }

    #[test]
    fn to_config_rejects_a_profile_without_an_encoder() {
        let mut form = SettingsForm::from_config(&Config::default());
        form.profile = CodecProfile::Aac;
        assert!(form.to_config().is_err());
    }

    #[test]
    fn to_config_rejects_exclude_process_without_a_process() {
        let mut form = SettingsForm::from_config(&Config::default());
        form.exclude_process = true;
        form.process = None;
        assert!(form.to_config().is_err());
    }
}
