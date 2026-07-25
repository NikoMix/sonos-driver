//! `sonosvad-svc` — the headless host for the Sonos virtual-audio bridge.
//!
//! There is no user interface anywhere in this product. The driver publishes
//! render endpoints, this service renames them to room names and streams
//! whatever Windows plays to them, and the only thing the user ever touches is
//! the normal Windows output picker.
//!
//! # Why one session-0 service, and not a per-user helper
//!
//! The obvious worry with hosting audio capture in a service is session 0
//! isolation. It does not apply here: Microsoft documents that "WASAPI loopback
//! by default contains the mix of all audio being played, regardless of the
//! Terminal Services session the audio originated from … you can run a loopback
//! client in a service running in session 0 and capture audio from all user
//! sessions". The loopback tap sits after the per-endpoint mix in `audiodg.exe`,
//! which is global, not per-session.
//!
//! That makes a single `LocalSystem` service the *better* design here, not merely
//! an acceptable one:
//!
//! * Renaming an endpoint writes to `HKLM\…\MMDevices`, which needs admin.
//!   `LocalSystem` has it; a per-user helper would not.
//! * The stream server's listening port and the room-to-endpoint map are
//!   machine-wide facts. Per-user helpers would race for ports and fight over
//!   bindings.
//! * With Fast User Switching, one global mix is exactly right: Sonos plays what
//!   *this machine* is playing.
//!
//! The one documented caveat is that loopback buffers are flagged silent until
//! some user has logged on since boot, because `audiodg.exe` does not initialise
//! an endpoint graph before then. For this product that is not a limitation:
//! with nobody logged on there is no audio to stream.

mod service;
mod worker;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sonos_driver_core::StopHandle;

use crate::worker::WorkerSettings;

#[derive(Debug, Parser)]
#[command(
    name = "sonosvad-svc",
    about = "Headless Windows service streaming each sonosvad endpoint to its Sonos room",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Register the service with the Service Control Manager (needs admin).
    Install {
        /// Address of a known Sonos player, baked into the service's command
        /// line. Use on networks where SSDP multicast does not work.
        #[arg(long, value_name = "HOST")]
        seed_host: Option<String>,
    },
    /// Stop and deregister the service (needs admin).
    Uninstall,
    /// Service entry point. Invoked by the SCM; not useful interactively.
    Run {
        #[arg(long, value_name = "HOST")]
        seed_host: Option<String>,
    },
    /// Run the identical worker in the foreground, logging to the console.
    ///
    /// The supported way to see what the service does without reading logs.
    Console {
        #[arg(long, value_name = "HOST")]
        seed_host: Option<String>,
        /// Configuration file. Defaults to the machine-wide one.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Install { seed_host } => service::install(seed_host.as_deref()),
        Command::Uninstall => service::uninstall(),
        Command::Run { seed_host } => {
            init_file_logging();
            let settings = WorkerSettings {
                seed_host,
                ..WorkerSettings::default()
            };
            service::run_as_service(settings)
        }
        Command::Console { seed_host, config } => {
            init_console_logging();
            let settings = WorkerSettings {
                config_path: config.or_else(|| Some(worker::default_config_path())),
                seed_host,
                ..WorkerSettings::default()
            };
            let stop = StopHandle::new();
            let ctrl_c_stop = stop.clone();
            ctrlc::set_handler(move || ctrl_c_stop.stop())
                .map_err(|err| anyhow::anyhow!("installing Ctrl-C handler: {err}"))?;
            println!("Running in the foreground. Press Ctrl-C to stop.");
            worker::run(&settings, &stop);
            Ok(())
        }
    }
}

fn init_console_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

/// Log to a file under `%ProgramData%`.
///
/// A service has no console, so without this every diagnostic would be lost.
/// Failures here are deliberately swallowed: losing logging is not a reason to
/// refuse to stream audio.
fn init_file_logging() {
    let dir = worker::log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("sonosvad-svc.log");
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(move || file.try_clone().expect("cloning the log file handle"))
        .try_init();
}
