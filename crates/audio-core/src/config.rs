//! Strongly typed, serde-serialisable configuration shared across the bridge.
//!
//! The CLI loads a `Config` from TOML (or builds one from flags) and hands the
//! relevant sub-sections to each subsystem. Every field has a sensible default
//! so a zero-config run targets the default output device with the
//! lowest-latency profile.

use serde::{Deserialize, Serialize};

use crate::CodecProfile;

/// The default TCP port the local stream server binds to.
pub const DEFAULT_STREAM_PORT: u16 = 8080;

/// Top-level configuration aggregating every subsystem's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Audio capture settings.
    pub capture: CaptureConfig,
    /// Pipeline / DSP settings.
    pub pipeline: PipelineConfig,
    /// Streaming codec and HTTP server settings.
    pub stream: StreamConfig,
    /// Sonos target / discovery settings.
    pub sonos: SonosConfig,
}

/// Which Windows audio endpoint to capture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct CaptureConfig {
    /// Endpoint to capture. `None` selects the current default render device.
    /// May be a device id or a substring of the friendly name.
    pub device: Option<String>,
    /// Per-application capture target: a process name (e.g. `"spotify.exe"`,
    /// `.exe` optional) or a numeric PID. When set, the bridge captures that
    /// application's audio via WASAPI process loopback instead of the whole
    /// render endpoint (requires Windows 11 / Server 2022, build 20348+).
    /// `None` (the default) preserves the classic default-device loopback.
    pub process: Option<String>,
    /// Invert [`CaptureConfig::process`]: capture *everything except* the
    /// target application's audio (process-tree exclude). Ignored unless
    /// `process` is set. Defaults to `false` (capture only the target app).
    pub exclude_process: bool,
}

/// Pipeline behaviour: resampling target and jitter buffering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PipelineConfig {
    /// Resample captured audio to this rate before encoding. `None` keeps the
    /// capture rate. Sonos commonly expects `44_100`.
    pub target_sample_rate: Option<u32>,
    /// Target jitter-buffer depth in milliseconds. Lower means less latency but
    /// a higher risk of underruns on a busy network.
    pub buffer_ms: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            target_sample_rate: Some(44_100),
            buffer_ms: 80,
        }
    }
}

/// Streaming codec selection and HTTP server bind settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct StreamConfig {
    /// Codec/latency profile to stream with.
    pub profile: CodecProfile,
    /// Address the HTTP stream server binds to. `0.0.0.0` exposes it to the LAN
    /// so Sonos can reach it.
    pub bind_address: String,
    /// TCP port for the HTTP stream server.
    pub port: u16,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            profile: CodecProfile::default(),
            bind_address: "0.0.0.0".to_owned(),
            port: DEFAULT_STREAM_PORT,
        }
    }
}

/// How to pick the Sonos target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SonosConfig {
    /// Target room/zone name (e.g. "Living Room"). `None` requires an explicit
    /// selection at runtime.
    pub room: Option<String>,
    /// Target device UUID, taking precedence over `room` when set.
    pub uuid: Option<String>,
    /// Seconds to wait for SSDP discovery responses.
    pub discovery_timeout_secs: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sonos_compatible_and_lan_exposed() {
        let cfg = Config::default();
        assert_eq!(cfg.stream.profile, CodecProfile::Flac);
        assert_eq!(cfg.stream.port, DEFAULT_STREAM_PORT);
        assert_eq!(cfg.stream.bind_address, "0.0.0.0");
        assert_eq!(cfg.pipeline.target_sample_rate, Some(44_100));
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string(&cfg).expect("serialise");
        let parsed: Config = toml::from_str(&text).expect("deserialise");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn capture_defaults_preserve_default_device_loopback() {
        let cfg = CaptureConfig::default();
        assert!(cfg.device.is_none());
        assert!(cfg.process.is_none());
        assert!(!cfg.exclude_process);
    }

    #[test]
    fn capture_omits_legacy_toml_still_parses() {
        // A config written before process-loopback existed (no `process` /
        // `exclude-process` keys) must still deserialise with the new defaults.
        let legacy = r#"
            [capture]
            device = "Speakers"
        "#;
        let cfg: Config = toml::from_str(legacy).expect("legacy parses");
        assert_eq!(cfg.capture.device.as_deref(), Some("Speakers"));
        assert!(cfg.capture.process.is_none());
        assert!(!cfg.capture.exclude_process);
    }

    #[test]
    fn capture_process_fields_round_trip() {
        let mut cfg = Config::default();
        cfg.capture.process = Some("spotify.exe".to_owned());
        cfg.capture.exclude_process = true;
        let text = toml::to_string(&cfg).expect("serialise");
        let parsed: Config = toml::from_str(&text).expect("deserialise");
        assert_eq!(parsed.capture.process.as_deref(), Some("spotify.exe"));
        assert!(parsed.capture.exclude_process);
        assert_eq!(cfg, parsed);
    }
}
