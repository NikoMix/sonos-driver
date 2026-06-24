//! Configuration loading, flag-over-file merging, validation, and the
//! commented example template.
//!
//! Precedence is strict: **flags > file > defaults**. A [`Config`] is built by
//! starting from the loaded file (or [`Config::default`] when none is given) and
//! applying only the CLI flags the user actually set (every override flag is an
//! [`Option`]).

use std::net::IpAddr;
use std::path::Path;

use anyhow::{bail, Context};
use sonos_driver_core::core::Config;

use crate::cli::StreamArgs;

/// Read and parse a [`Config`] from a TOML file.
pub(crate) fn load_config(path: &Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    let config =
        toml::from_str(&text).with_context(|| format!("parsing config file {}", path.display()))?;
    Ok(config)
}

/// Resolve the effective streaming config: load the file (if any), then layer
/// the explicit CLI flags on top.
pub(crate) fn effective_stream_config(
    config_path: Option<&Path>,
    args: &StreamArgs,
) -> anyhow::Result<Config> {
    let base = match config_path {
        Some(path) => load_config(path)?,
        None => Config::default(),
    };
    Ok(apply_stream_overrides(base, args))
}

/// Overlay the explicitly-set `stream` flags onto `config` (flags win).
#[must_use]
pub(crate) fn apply_stream_overrides(mut config: Config, args: &StreamArgs) -> Config {
    if let Some(room) = &args.room {
        config.sonos.room = Some(room.clone());
    }
    if let Some(uuid) = &args.uuid {
        config.sonos.uuid = Some(uuid.clone());
    }
    if let Some(profile) = args.profile {
        config.stream.profile = profile.into();
    }
    if let Some(device) = &args.device {
        config.capture.device = Some(device.clone());
    }
    if let Some(port) = args.port {
        config.stream.port = port;
    }
    if let Some(bind) = &args.bind_address {
        config.stream.bind_address.clone_from(bind);
    }
    if let Some(rate) = args.target_sample_rate {
        // `0` is the explicit "keep the native capture rate" sentinel.
        config.pipeline.target_sample_rate = (rate != 0).then_some(rate);
    }
    if let Some(buffer_ms) = args.buffer_ms {
        config.pipeline.buffer_ms = buffer_ms;
    }
    if let Some(secs) = args.discovery_timeout {
        config.sonos.discovery_timeout_secs = secs;
    }
    config
}

/// Whether a non-empty streaming target (room or uuid) is configured.
#[must_use]
pub(crate) fn has_target(config: &Config) -> bool {
    non_empty(config.sonos.room.as_deref()) || non_empty(config.sonos.uuid.as_deref())
}

fn non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|s| !s.trim().is_empty())
}

/// Validate the invariants the bridge relies on, returning a clear error for the
/// first violation. When `require_target` is set, a room or uuid must be present.
pub(crate) fn validate(config: &Config, require_target: bool) -> anyhow::Result<()> {
    if config.stream.port == 0 {
        bail!("stream.port must be non-zero");
    }
    let bind = config.stream.bind_address.trim();
    if bind.is_empty() {
        bail!("stream.bind-address must not be empty");
    }
    if bind.parse::<IpAddr>().is_err() {
        bail!(
            "stream.bind-address '{}' is not a valid IP address",
            config.stream.bind_address
        );
    }
    if let Some(rate) = config.pipeline.target_sample_rate {
        if !(8_000..=384_000).contains(&rate) {
            bail!("pipeline.target-sample-rate {rate} Hz is outside the supported 8000..=384000 Hz range");
        }
    }
    if require_target && !has_target(config) {
        bail!(
            "a streaming target is required: pass --room or --uuid (or set sonos.room / sonos.uuid in the config file)"
        );
    }
    Ok(())
}

/// A fully-commented example configuration. Every active value equals the
/// built-in default for that field; optional targets are shown commented out.
/// The emitted document always parses back into a valid [`Config`].
#[must_use]
pub(crate) fn example_toml() -> &'static str {
    EXAMPLE_TOML
}

const EXAMPLE_TOML: &str = r#"# sonos-driver configuration.
#
# Every value is optional and shown at its built-in default. CLI flags always
# override what you set here (precedence: flags > file > defaults). Generate this
# file with `sonos-driver config example > sonos-driver.toml`, edit it, then run
# `sonos-driver stream --config sonos-driver.toml`.

[capture]
# Windows audio endpoint to capture: a device id or a substring of the friendly
# name. Leave unset to follow the current default render (playback) device.
# device = "Speakers"

[pipeline]
# Resample captured audio to this rate (Hz) before encoding. Sonos commonly
# expects 44100. Set to 0 to keep the native capture rate.
target-sample-rate = 44100
# Jitter-buffer depth in milliseconds: lower is less latency but more prone to
# underruns on a busy network.
buffer-ms = 80

[stream]
# Codec / latency profile: "l16-pcm" (lowest latency, default), "wav", "flac",
# "aac" or "mp3". See `sonos-driver profiles` and docs/LATENCY-BUDGET.md.
profile = "l16-pcm"
# Address the local HTTP stream server binds to. 0.0.0.0 exposes it on the LAN
# so the Sonos player can reach it.
bind-address = "0.0.0.0"
# TCP port for the local HTTP stream server.
port = 8080

[sonos]
# Target room / zone name, e.g. "Living Room". Required for streaming unless a
# uuid is set (either here or with --room on the command line).
# room = "Living Room"
# Target device UUID; takes precedence over `room` when set.
# uuid = "RINCON_XXXXXXXXXXXX01400"
# Seconds to wait for SSDP discovery responses.
discovery-timeout-secs = 3
"#;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use sonos_driver_core::core::CodecProfile;

    use super::*;
    use crate::cli::ProfileArg;

    fn empty_stream_args() -> StreamArgs {
        StreamArgs {
            room: None,
            uuid: None,
            profile: None,
            device: None,
            port: None,
            bind_address: None,
            target_sample_rate: None,
            buffer_ms: None,
            discovery_timeout: None,
            dry_run: false,
        }
    }

    /// Write `contents` to a unique temp file and return a guard that deletes it
    /// when dropped.
    struct TempToml(PathBuf);

    impl TempToml {
        fn new(contents: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sonos-driver-test-{}-{n}.toml", std::process::id()));
            std::fs::write(&path, contents).expect("write temp config");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempToml {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn flags_override_file_values() {
        let file = TempToml::new(
            r#"
            [stream]
            profile = "flac"
            port = 9000
            bind-address = "10.0.0.1"
            [sonos]
            room = "Office"
            discovery-timeout-secs = 9
            "#,
        );
        let mut args = empty_stream_args();
        args.port = Some(7000);
        args.profile = Some(ProfileArg::Wav);

        let cfg = effective_stream_config(Some(file.path()), &args).expect("merge");
        // Flags win where set...
        assert_eq!(cfg.stream.port, 7000);
        assert_eq!(cfg.stream.profile, CodecProfile::Wav);
        // ...file values survive where no flag was given...
        assert_eq!(cfg.stream.bind_address, "10.0.0.1");
        assert_eq!(cfg.sonos.room.as_deref(), Some("Office"));
        assert_eq!(cfg.sonos.discovery_timeout_secs, 9);
    }

    #[test]
    fn file_values_apply_without_flags() {
        let file = TempToml::new(
            r#"
            [stream]
            port = 9100
            [sonos]
            uuid = "RINCON_ABC"
            "#,
        );
        let cfg = effective_stream_config(Some(file.path()), &empty_stream_args()).expect("load");
        assert_eq!(cfg.stream.port, 9100);
        assert_eq!(cfg.sonos.uuid.as_deref(), Some("RINCON_ABC"));
        // Untouched fields keep their defaults.
        assert_eq!(cfg.stream.profile, CodecProfile::L16Pcm);
    }

    #[test]
    fn flags_only_layer_over_defaults() {
        let mut args = empty_stream_args();
        args.port = Some(1234);
        args.room = Some("Den".to_owned());
        let cfg = effective_stream_config(None, &args).expect("defaults+flags");
        assert_eq!(cfg.stream.port, 1234);
        assert_eq!(cfg.sonos.room.as_deref(), Some("Den"));
        assert_eq!(
            cfg.stream.bind_address,
            Config::default().stream.bind_address
        );
    }

    #[test]
    fn zero_target_sample_rate_disables_resampling() {
        let mut args = empty_stream_args();
        args.target_sample_rate = Some(0);
        let cfg = apply_stream_overrides(Config::default(), &args);
        assert_eq!(cfg.pipeline.target_sample_rate, None);
    }

    #[test]
    fn load_config_reports_missing_file() {
        let err = load_config(Path::new("does-not-exist-xyz.toml")).expect_err("missing");
        assert!(err.to_string().contains("reading config file"));
    }

    #[test]
    fn validate_rejects_invalid_invariants() {
        let mut cfg = Config::default();
        cfg.sonos.room = Some("Living Room".to_owned());

        let mut bad_port = cfg.clone();
        bad_port.stream.port = 0;
        assert!(validate(&bad_port, true).is_err());

        let mut bad_bind = cfg.clone();
        bad_bind.stream.bind_address = "not-an-ip".to_owned();
        assert!(validate(&bad_bind, true).is_err());

        let mut bad_rate = cfg.clone();
        bad_rate.pipeline.target_sample_rate = Some(7_999);
        assert!(validate(&bad_rate, true).is_err());

        // Valid with a target.
        assert!(validate(&cfg, true).is_ok());
    }

    #[test]
    fn validate_requires_target_only_when_asked() {
        let cfg = Config::default(); // no room/uuid
        assert!(validate(&cfg, true).is_err());
        assert!(validate(&cfg, false).is_ok());
    }

    #[test]
    fn example_round_trips_into_valid_config() {
        let cfg: Config = toml::from_str(example_toml()).expect("example parses");
        // The shipped example is itself a valid standalone config.
        assert!(validate(&cfg, false).is_ok());
        // Active values match the documented defaults; targets stay unset.
        assert_eq!(cfg.stream.profile, CodecProfile::L16Pcm);
        assert_eq!(cfg.stream.port, 8080);
        assert_eq!(cfg.stream.bind_address, "0.0.0.0");
        assert_eq!(cfg.pipeline.target_sample_rate, Some(44_100));
        assert_eq!(cfg.pipeline.buffer_ms, 80);
        assert_eq!(cfg.sonos.discovery_timeout_secs, 3);
        assert!(!has_target(&cfg));
    }
}
