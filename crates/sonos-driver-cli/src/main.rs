//! `sonos-driver` command-line front-end.
//!
//! Phase 0 provides a runnable CLI: `profiles` is fully functional, while
//! `list` and `stream` exercise the orchestration surface and report cleanly
//! when a capability is pending its workstream. Subcommand behaviour is filled
//! in by WS-G once the underlying stages exist.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use sonos_driver_core::core::{
    CodecProfile, Config, Error as CoreError, SonosConfig, StreamConfig,
};
use sonos_driver_core::{control, Bridge, LatencyEstimate};

/// Stream native Windows audio to Sonos speakers over the LAN.
#[derive(Debug, Parser)]
#[command(name = "sonos-driver", version, about, long_about = None)]
struct Cli {
    /// Increase log verbosity (`-v` = debug, `-vv` = trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the available codec/latency streaming profiles.
    Profiles,
    /// Discover Sonos players on the local network.
    List {
        /// Seconds to wait for discovery responses.
        #[arg(long, default_value_t = 3)]
        timeout: u64,
    },
    /// Start streaming system audio to a Sonos room.
    Stream {
        /// Target Sonos room/zone name.
        #[arg(long)]
        room: Option<String>,
        /// Streaming profile to use.
        #[arg(long, value_enum, default_value_t = ProfileArg::LowLatency)]
        profile: ProfileArg,
        /// TCP port for the local stream server.
        #[arg(long, default_value_t = audio_core::DEFAULT_STREAM_PORT)]
        port: u16,
    },
}

/// CLI-facing names for the codec profiles, mapped to [`CodecProfile`].
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProfileArg {
    /// Uncompressed L16 PCM — lowest latency.
    LowLatency,
    /// WAVE-framed PCM — lossless, compatible.
    Wav,
    /// FLAC lossless — quality (WS-D).
    Flac,
    /// AAC lossy — compatibility (WS-D).
    Aac,
}

impl From<ProfileArg> for CodecProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::LowLatency => Self::L16Pcm,
            ProfileArg::Wav => Self::Wav,
            ProfileArg::Flac => Self::Flac,
            ProfileArg::Aac => Self::Aac,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Pending-workstream capabilities are an expected, informational
            // outcome in Phase 0 rather than a hard failure.
            if let Some(CoreError::NotImplemented(msg)) = err.downcast_ref::<CoreError>() {
                eprintln!("not yet available: {msg}");
                return ExitCode::from(2);
            }
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Profiles => {
            print_profiles();
            Ok(())
        }
        Command::List { timeout } => list_devices(timeout),
        Command::Stream {
            room,
            profile,
            port,
        } => stream(room, profile.into(), port),
    }
}

fn print_profiles() {
    println!("Available streaming profiles (latency includes the fixed Sonos buffer):\n");
    for profile in [
        CodecProfile::L16Pcm,
        CodecProfile::Wav,
        CodecProfile::Flac,
        CodecProfile::Aac,
        CodecProfile::Mp3,
    ] {
        let est = LatencyEstimate::for_profile(profile);
        println!(
            "  {:<8} {:<11} {}\n           {}",
            format!("{profile:?}"),
            est.approx_range,
            if profile.is_lossless() {
                "lossless"
            } else {
                "lossy"
            },
            profile.description(),
        );
    }
}

fn list_devices(timeout_secs: u64) -> anyhow::Result<()> {
    let devices = control::discover(Duration::from_secs(timeout_secs))
        .context("discovering Sonos devices")?;
    if devices.is_empty() {
        println!("No Sonos devices found.");
    }
    for dev in devices {
        println!("{:<18} {:<16} {}", dev.room_name, dev.model_name, dev.ip);
    }
    Ok(())
}

fn stream(room: Option<String>, profile: CodecProfile, port: u16) -> anyhow::Result<()> {
    let config = Config {
        stream: StreamConfig {
            profile,
            port,
            ..StreamConfig::default()
        },
        sonos: SonosConfig {
            room,
            ..SonosConfig::default()
        },
        ..Config::default()
    };

    let mut bridge = Bridge::new(config);
    let est = bridge.latency_estimate();
    println!(
        "Starting bridge: profile {:?}, expected latency {}.",
        est.profile, est.approx_range
    );
    bridge.run().context("running the audio bridge")
}

fn init_tracing(verbosity: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default_level = match verbosity {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("sonos_driver={default_level},warn")));
    fmt().with_env_filter(filter).with_target(false).init();
}
