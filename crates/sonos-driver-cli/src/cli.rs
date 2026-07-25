//! Command-line surface: the `clap` types that define every subcommand and
//! flag, plus the mapping from CLI profile names to [`CodecProfile`].
//!
//! Parsing is kept free of side effects so it can be exhaustively unit-tested
//! with [`clap::Parser::try_parse_from`]; the actual work lives in
//! [`crate::commands`].

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use sonos_driver_core::core::CodecProfile;

/// Stream native Windows audio to Sonos speakers over the LAN.
#[derive(Debug, Parser)]
#[command(name = "sonos-driver", version, about, long_about = None)]
pub(crate) struct Cli {
    /// Increase log verbosity (`-v` = debug, `-vv` = trace).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub(crate) verbose: u8,

    /// Silence all logs except errors (overrides `-v`).
    #[arg(short, long, global = true)]
    pub(crate) quiet: bool,

    /// Load configuration from this TOML file; explicit flags override its values.
    #[arg(short = 'c', long, global = true, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,

    /// The subcommand to run.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// The top-level subcommands exposed by the `sonos-driver` binary.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List the available codec/latency streaming profiles.
    Profiles,
    /// Discover Sonos players on the local network.
    List(ListArgs),
    /// Start streaming system audio to a Sonos room.
    Stream(StreamArgs),
    /// Stop playback on a Sonos room (sends an `AVTransport` `Stop`).
    Stop(StopArgs),
    /// Manage the virtual audio outputs exposed by the `sonosvad` driver.
    #[command(subcommand)]
    Vad(VadCommand),
    /// Inspect or generate configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
}

/// The `vad` (virtual audio device) subcommand family.
#[derive(Debug, Subcommand)]
pub(crate) enum VadCommand {
    /// List the render endpoints exposed by the `sonosvad` driver.
    Status(VadStatusArgs),
    /// Discover Sonos rooms and rename the driver's endpoints to match.
    ///
    /// Requires an elevated shell: renaming an audio endpoint writes
    /// `PKEY_Device_FriendlyName` on its property store.
    Bind(VadBindArgs),
    /// Restore every endpoint's factory name ("Sonos Output N").
    Release,
    /// Loopback-capture one endpoint and report whether its clock runs true.
    ///
    /// This is the driver's core correctness check and needs no Sonos hardware:
    /// a virtual audio device must advance its stream position at exactly real
    /// time, so N seconds of capture must yield N seconds of frames.
    Probe(VadProbeArgs),
    /// Bind every discovered room, then stream each virtual output to its room.
    Serve(VadServeArgs),
}

/// Arguments for `vad probe`.
#[derive(Debug, Args)]
pub(crate) struct VadProbeArgs {
    /// Endpoint to capture: a device id, or a substring of its name.
    ///
    /// Defaults to the first `sonosvad` endpoint.
    #[arg(long, value_name = "ID|NAME")]
    pub(crate) device: Option<String>,
    /// How many seconds to capture for.
    #[arg(long, default_value_t = 5)]
    pub(crate) seconds: u64,
    /// Maximum tolerated clock drift, as a percentage of elapsed time.
    #[arg(long, default_value_t = 2.0, value_name = "PERCENT")]
    pub(crate) tolerance: f64,
    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    pub(crate) json: bool,
}

/// Arguments for `vad status`.
#[derive(Debug, Args)]
pub(crate) struct VadStatusArgs {
    /// Emit machine-readable JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json: bool,
    /// Inspect a different audio adapter instead of the `sonosvad` driver.
    ///
    /// Matched against `PKEY_DeviceInterface_FriendlyName`. Useful for
    /// diagnosing endpoint enumeration on a machine where the driver is not
    /// installed; `--adapter ""` lists every render endpoint on the system.
    #[arg(long, value_name = "NAME")]
    pub(crate) adapter: Option<String>,
}

/// Arguments for `vad bind`.
#[derive(Debug, Args)]
pub(crate) struct VadBindArgs {
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP. Use
    /// this when SSDP is blocked: NAT'd VMs, VLAN-segmented or guest wifi, or
    /// APs without working IGMP.
    #[arg(long, value_name = "HOST")]
    pub(crate) seed_host: Option<String>,
    /// Seconds to wait for discovery responses.
    #[arg(long, default_value_t = 3)]
    pub(crate) timeout: u64,
    /// Show the bindings that would be applied without renaming anything.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Plan against a hypothetical driver exposing this many factory-named
    /// endpoints, instead of the ones actually installed.
    ///
    /// Lets the room-to-endpoint plan be checked on a machine where the driver
    /// is not (or cannot be) installed, and lets a different endpoint count be
    /// sized before rebuilding the driver. Requires `--dry-run`.
    #[arg(long, value_name = "N", requires = "dry_run")]
    pub(crate) assume_endpoints: Option<u32>,
}

/// Arguments for `vad serve`.
#[derive(Debug, Args)]
pub(crate) struct VadServeArgs {
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP. Use
    /// this when SSDP is blocked: NAT'd VMs, VLAN-segmented or guest wifi, or
    /// APs without working IGMP.
    #[arg(long, value_name = "HOST")]
    pub(crate) seed_host: Option<String>,
    /// Seconds to wait for discovery responses.
    #[arg(long, default_value_t = 3)]
    pub(crate) timeout: u64,
    /// Streaming profile to use for every room.
    #[arg(long, value_enum)]
    pub(crate) profile: Option<ProfileArg>,
    /// Base TCP port; each room's stream server binds to `port + n`.
    #[arg(long)]
    pub(crate) port: Option<u16>,
    /// Address the stream servers bind to (`0.0.0.0` exposes them on the LAN).
    #[arg(long, visible_alias = "bind", value_name = "IP")]
    pub(crate) bind_address: Option<String>,
    /// Resample to this rate (Hz) before encoding; `0` keeps the capture rate.
    #[arg(long, visible_alias = "target-rate", value_name = "HZ")]
    pub(crate) target_sample_rate: Option<u32>,
    /// Jitter-buffer depth in milliseconds.
    #[arg(long, value_name = "MS")]
    pub(crate) buffer_ms: Option<u32>,
    /// Print the room-to-endpoint plan and exit without streaming.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

/// Arguments for the `list` subcommand.
#[derive(Debug, Args)]
pub(crate) struct ListArgs {
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP. Use
    /// this when SSDP is blocked: NAT'd VMs, VLAN-segmented or guest wifi, or
    /// APs without working IGMP.
    #[arg(long, value_name = "HOST")]
    pub(crate) seed_host: Option<String>,
    /// Seconds to wait for discovery responses.
    #[arg(long, default_value_t = 3)]
    pub(crate) timeout: u64,
    /// Emit machine-readable JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json: bool,
}

/// Arguments for the `stream` subcommand.
///
/// Every config-bearing flag is an [`Option`] so a missing flag falls through
/// to the loaded config file (or [`Config::default`](sonos_driver_core::core::Config::default)),
/// giving a strict `flags > file > defaults` precedence.
#[derive(Debug, Args)]
pub(crate) struct StreamArgs {
    /// Target Sonos room/zone name (e.g. "Living Room").
    #[arg(long)]
    pub(crate) room: Option<String>,
    /// Target Sonos device UUID (takes precedence over `--room`).
    #[arg(long)]
    pub(crate) uuid: Option<String>,
    /// Streaming profile to use.
    #[arg(long, value_enum)]
    pub(crate) profile: Option<ProfileArg>,
    /// Capture device id or friendly-name substring (default: system default render device).
    #[arg(long)]
    pub(crate) device: Option<String>,
    /// Capture a single application's audio via WASAPI process loopback: a
    /// process name (e.g. "spotify.exe", the `.exe` is optional) or a numeric
    /// PID. Takes precedence over `--device`. Requires Windows 11 / Windows
    /// Server 2022 (build 20348+).
    #[arg(long, value_name = "NAME|PID")]
    pub(crate) process: Option<String>,
    /// Invert `--process`: capture everything *except* the target application's
    /// audio tree. Has no effect unless `--process` is set.
    #[arg(long)]
    pub(crate) exclude_process: bool,
    /// TCP port for the local stream server.
    #[arg(long)]
    pub(crate) port: Option<u16>,
    /// Address the stream server binds to (`0.0.0.0` exposes it on the LAN).
    #[arg(long, visible_alias = "bind", value_name = "IP")]
    pub(crate) bind_address: Option<String>,
    /// Resample to this rate (Hz) before encoding; `0` keeps the capture rate.
    #[arg(long, visible_alias = "target-rate", value_name = "HZ")]
    pub(crate) target_sample_rate: Option<u32>,
    /// Jitter-buffer depth in milliseconds.
    #[arg(long, value_name = "MS")]
    pub(crate) buffer_ms: Option<u32>,
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP. Use
    /// this when SSDP is blocked: NAT'd VMs, VLAN-segmented or guest wifi, or
    /// APs without working IGMP.
    #[arg(long, value_name = "HOST")]
    pub(crate) seed_host: Option<String>,
    /// Seconds to wait for SSDP discovery responses.
    #[arg(long, value_name = "SECS")]
    pub(crate) discovery_timeout: Option<u32>,

    /// Print the effective configuration and exit without streaming.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

/// Arguments for the `stop` subcommand: exactly one of `--room`/`--uuid`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("stop_target").required(true).args(["room", "uuid"])))]
pub(crate) struct StopArgs {
    /// Target Sonos room/zone name.
    #[arg(long)]
    pub(crate) room: Option<String>,
    /// Target Sonos device UUID.
    #[arg(long)]
    pub(crate) uuid: Option<String>,
    /// Address of a known Sonos player, used instead of SSDP multicast.
    ///
    /// Any single player reports the whole household over unicast HTTP. Use
    /// this when SSDP is blocked: NAT'd VMs, VLAN-segmented or guest wifi, or
    /// APs without working IGMP.
    #[arg(long, value_name = "HOST")]
    pub(crate) seed_host: Option<String>,
    /// Seconds to wait for discovery responses.
    #[arg(long, default_value_t = 3)]
    pub(crate) timeout: u64,
}

/// The `config` subcommand family.
#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Print a fully-commented example configuration to stdout.
    Example,
    /// Load (via `--config`), validate, and report the effective configuration.
    Check,
}

/// CLI-facing names for the codec profiles, mapped to [`CodecProfile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProfileArg {
    /// Uncompressed L16 PCM — lowest latency.
    LowLatency,
    /// WAVE-framed PCM — lossless, compatible.
    Wav,
    /// FLAC lossless — quality.
    Flac,
    /// AAC lossy — compatibility (license permitting).
    Aac,
    /// MP3 lossy — compatibility (license permitting).
    Mp3,
}

impl From<ProfileArg> for CodecProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::LowLatency => Self::L16Pcm,
            ProfileArg::Wav => Self::Wav,
            ProfileArg::Flac => Self::Flac,
            ProfileArg::Aac => Self::Aac,
            ProfileArg::Mp3 => Self::Mp3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("should parse")
    }

    #[test]
    fn profiles_parses() {
        assert!(matches!(
            parse(&["sonos-driver", "profiles"]).command,
            Command::Profiles
        ));
    }

    #[test]
    fn list_defaults_and_flags() {
        let cli = parse(&["sonos-driver", "list"]);
        let Command::List(args) = cli.command else {
            panic!("expected list")
        };
        assert_eq!(args.timeout, 3);
        assert!(!args.json);

        let cli = parse(&["sonos-driver", "list", "--timeout", "7", "--json"]);
        let Command::List(args) = cli.command else {
            panic!("expected list")
        };
        assert_eq!(args.timeout, 7);
        assert!(args.json);
    }

    #[test]
    fn stream_minimal_leaves_overrides_unset() {
        let cli = parse(&["sonos-driver", "stream", "--room", "Living Room"]);
        let Command::Stream(args) = cli.command else {
            panic!("expected stream")
        };
        assert_eq!(args.room.as_deref(), Some("Living Room"));
        assert!(args.uuid.is_none());
        assert!(args.profile.is_none());
        assert!(args.port.is_none());
        assert!(args.bind_address.is_none());
        assert!(args.target_sample_rate.is_none());
        assert!(args.buffer_ms.is_none());
        assert!(args.discovery_timeout.is_none());
        assert!(args.device.is_none());
        assert!(args.process.is_none());
        assert!(!args.exclude_process);
        assert!(!args.dry_run);
    }

    #[test]
    fn stream_full_flag_set() {
        let cli = parse(&[
            "sonos-driver",
            "stream",
            "--uuid",
            "RINCON_X",
            "--profile",
            "flac",
            "--device",
            "Speakers",
            "--process",
            "Spotify",
            "--exclude-process",
            "--port",
            "9000",
            "--bind-address",
            "127.0.0.1",
            "--target-sample-rate",
            "48000",
            "--buffer-ms",
            "120",
            "--discovery-timeout",
            "5",
            "--dry-run",
        ]);
        let Command::Stream(args) = cli.command else {
            panic!("expected stream")
        };
        assert_eq!(args.uuid.as_deref(), Some("RINCON_X"));
        assert_eq!(args.profile, Some(ProfileArg::Flac));
        assert_eq!(args.device.as_deref(), Some("Speakers"));
        assert_eq!(args.process.as_deref(), Some("Spotify"));
        assert!(args.exclude_process);
        assert_eq!(args.port, Some(9000));
        assert_eq!(args.bind_address.as_deref(), Some("127.0.0.1"));
        assert_eq!(args.target_sample_rate, Some(48_000));
        assert_eq!(args.buffer_ms, Some(120));
        assert_eq!(args.discovery_timeout, Some(5));
        assert!(args.dry_run);
    }

    #[test]
    fn stream_accepts_short_flag_aliases() {
        let cli = parse(&[
            "sonos-driver",
            "stream",
            "--room",
            "Den",
            "--bind",
            "0.0.0.0",
            "--target-rate",
            "44100",
        ]);
        let Command::Stream(args) = cli.command else {
            panic!("expected stream")
        };
        assert_eq!(args.bind_address.as_deref(), Some("0.0.0.0"));
        assert_eq!(args.target_sample_rate, Some(44_100));
    }

    #[test]
    fn profile_alias_maps_to_codec_profile() {
        assert_eq!(
            CodecProfile::from(ProfileArg::LowLatency),
            CodecProfile::L16Pcm
        );
        assert_eq!(CodecProfile::from(ProfileArg::Mp3), CodecProfile::Mp3);
    }

    #[test]
    fn stop_requires_exactly_one_target() {
        assert!(Cli::try_parse_from(["sonos-driver", "stop", "--room", "Kitchen"]).is_ok());
        assert!(Cli::try_parse_from(["sonos-driver", "stop", "--uuid", "RINCON_X"]).is_ok());
        // Neither target -> error.
        assert!(Cli::try_parse_from(["sonos-driver", "stop"]).is_err());
        // Both targets -> error (group is single-valued).
        assert!(
            Cli::try_parse_from(["sonos-driver", "stop", "--room", "K", "--uuid", "X"]).is_err()
        );
    }

    #[test]
    fn global_config_flag_parses_before_or_after_subcommand() {
        let cli = parse(&["sonos-driver", "--config", "a.toml", "list"]);
        assert_eq!(
            cli.config
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("a.toml".to_owned())
        );

        let cli = parse(&[
            "sonos-driver",
            "stream",
            "--room",
            "X",
            "--config",
            "b.toml",
        ]);
        assert_eq!(
            cli.config
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("b.toml".to_owned())
        );
    }

    #[test]
    fn config_subcommands_parse() {
        assert!(matches!(
            parse(&["sonos-driver", "config", "example"]).command,
            Command::Config(ConfigCommand::Example)
        ));
        assert!(matches!(
            parse(&["sonos-driver", "config", "check", "--config", "x.toml"]).command,
            Command::Config(ConfigCommand::Check)
        ));
    }

    #[test]
    fn verbosity_and_quiet_flags() {
        assert_eq!(parse(&["sonos-driver", "-vv", "profiles"]).verbose, 2);
        assert!(parse(&["sonos-driver", "-q", "profiles"]).quiet);
    }

    #[test]
    fn vad_subcommands_parse() {
        assert!(matches!(
            parse(&["sonos-driver", "vad", "release"]).command,
            Command::Vad(VadCommand::Release)
        ));

        let Command::Vad(VadCommand::Status(args)) =
            parse(&["sonos-driver", "vad", "status", "--json"]).command
        else {
            panic!("expected vad status")
        };
        assert!(args.json);

        let Command::Vad(VadCommand::Bind(args)) =
            parse(&["sonos-driver", "vad", "bind", "--timeout", "9", "--dry-run"]).command
        else {
            panic!("expected vad bind")
        };
        assert_eq!(args.timeout, 9);
        assert!(args.dry_run);
    }

    #[test]
    fn vad_serve_flags_default_to_the_config_file() {
        let Command::Vad(VadCommand::Serve(args)) =
            parse(&["sonos-driver", "vad", "serve"]).command
        else {
            panic!("expected vad serve")
        };
        assert_eq!(args.timeout, 3);
        assert!(args.profile.is_none());
        assert!(args.port.is_none());
        assert!(args.bind_address.is_none());
        assert!(args.target_sample_rate.is_none());
        assert!(args.buffer_ms.is_none());
        assert!(!args.dry_run);

        let Command::Vad(VadCommand::Serve(args)) = parse(&[
            "sonos-driver",
            "vad",
            "serve",
            "--profile",
            "flac",
            "--port",
            "9100",
            "--bind",
            "0.0.0.0",
            "--target-rate",
            "44100",
            "--buffer-ms",
            "120",
            "--dry-run",
        ])
        .command
        else {
            panic!("expected vad serve")
        };
        assert_eq!(args.profile, Some(ProfileArg::Flac));
        assert_eq!(args.port, Some(9100));
        assert_eq!(args.bind_address.as_deref(), Some("0.0.0.0"));
        assert_eq!(args.target_sample_rate, Some(44_100));
        assert_eq!(args.buffer_ms, Some(120));
        assert!(args.dry_run);
    }
}
