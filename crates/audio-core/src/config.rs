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
    /// When to release the speakers because nothing is playing.
    pub idle: IdleConfig,
}

/// When to hand the speakers back because Windows has gone quiet.
///
/// A bridge that streams continuously would occupy the room forever: the Sonos
/// app would show "Windows" as the active source around the clock, and playing
/// anything from Spotify or the radio would mean manually kicking us off first.
/// So when the captured audio has been silent for [`IdleConfig::stop_after_secs`]
/// the transport is stopped, and it is started again the moment sound returns.
///
/// The cost of the release is one extra buffer fill (~1 s) before the first
/// sound after a quiet spell is audible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct IdleConfig {
    /// Stop the room after this many seconds of continuous silence. `0`
    /// disables idle release, keeping the room occupied for as long as the
    /// bridge runs.
    pub stop_after_secs: u32,
    /// Peak level below which a block counts as silent, in dBFS.
    ///
    /// Not "exactly zero": WASAPI loopback of an idle-but-open endpoint, and
    /// dithered digital silence, both produce samples that are inaudible
    /// without being `0.0`. -120 dBFS is two bits below the noise floor of
    /// 16-bit audio, so nothing audible can hide under it.
    pub silence_dbfs: i32,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            stop_after_secs: 30,
            silence_dbfs: -120,
        }
    }
}

impl IdleConfig {
    /// The silence window, or `None` when idle release is disabled.
    #[must_use]
    pub fn stop_after(&self) -> Option<std::time::Duration> {
        (self.stop_after_secs > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.stop_after_secs)))
    }

    /// [`IdleConfig::silence_dbfs`] as a peak sample magnitude in `[0.0, 1.0]`.
    #[must_use]
    pub fn silence_threshold(&self) -> f32 {
        // Clamped so a nonsensical config cannot make every block "audible"
        // (positive dBFS) and defeat idle release entirely.
        let dbfs = f64::from(self.silence_dbfs.min(0));
        #[allow(clippy::cast_possible_truncation)]
        {
            10f64.powf(dbfs / 20.0) as f32
        }
    }
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
    /// Address of a known Sonos player used to enumerate the household over
    /// plain unicast HTTP instead of SSDP multicast.
    ///
    /// SSDP needs multicast, which NAT'd hypervisor networks (Windows Sandbox,
    /// WSL, Docker), VLAN-segmented or guest wifi, and APs without working IGMP
    /// all drop. Any single player knows the whole topology, so one reachable
    /// address is enough. `None` uses SSDP.
    pub seed_host: Option<String>,
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
