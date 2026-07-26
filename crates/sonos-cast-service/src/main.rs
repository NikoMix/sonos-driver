//! `sonos-cast-svc` — the Windows service front end for the driverless host
//! ([ADR-0019]).
//!
//! The routing and streaming live in the `sonos_cast_service` library, shared
//! with `sonos-driver cast`; this binary only adds the Service Control Manager
//! wiring and a console mode for trying a configuration before installing it.
//!
//! [ADR-0019]: https://github.com/NikoMix/sonos-driver/blob/main/docs/adr/0019-driverless-routing-mode.md

mod service;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use sonos_cast_service::{config, worker};
use sonos_driver_core::StopHandle;
use sonos_routing::{RoomSelector, Route, Source};

use crate::worker::WorkerSettings;

#[derive(Debug, Parser)]
#[command(
    name = "sonos-cast-svc",
    about = "Headless Windows service streaming audio to Sonos rooms without a driver",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Register the service with the Service Control Manager (needs admin).
    ///
    /// Also writes a starter configuration if none exists, so the service has
    /// something to run and you have something to edit.
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
    /// The supported way to try a configuration before installing anything.
    Console(ConsoleArgs),
}

/// Arguments for `console`.
#[derive(Debug, Args)]
struct ConsoleArgs {
    /// Address of a known Sonos player, used instead of SSDP multicast.
    #[arg(long, value_name = "HOST")]
    seed_host: Option<String>,
    /// Configuration file. Defaults to the machine-wide one.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Stream to this room. Repeat for several; they play as one Sonos group.
    ///
    /// Overrides the configuration file's routes entirely, so a first run needs
    /// no config file at all.
    #[arg(long, value_name = "ROOM")]
    room: Vec<String>,
    /// Stream to every discovered room, as one synchronised group.
    #[arg(long, conflicts_with = "room")]
    all: bool,
    /// Capture only this application's audio instead of the whole system.
    #[arg(long, value_name = "EXE")]
    process: Option<String>,
    /// Capture everything except this application's audio.
    #[arg(long, value_name = "EXE", conflicts_with = "process")]
    except_process: Option<String>,
    /// Capture this render endpoint instead of the default device.
    #[arg(long, value_name = "ID|NAME", conflicts_with_all = ["process", "except_process"])]
    device: Option<String>,
    /// Resolve and print the plan, then exit without streaming.
    #[arg(long)]
    dry_run: bool,
}

impl ConsoleArgs {
    /// The route these arguments describe, if they describe one.
    ///
    /// `None` means "use the configuration file", which is what the service
    /// itself always does.
    fn route(&self) -> Option<Route> {
        let rooms = if self.all {
            RoomSelector::All
        } else if self.room.is_empty() {
            return None;
        } else {
            RoomSelector::Named(self.room.clone())
        };

        let source = if let Some(process) = self.process.clone() {
            Source::Process(process)
        } else if let Some(process) = self.except_process.clone() {
            Source::ExceptProcess(process)
        } else if let Some(device) = self.device.clone() {
            Source::Device(device)
        } else {
            Source::Default
        };

        Some(Route::new(source, rooms))
    }
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
        Command::Console(args) => run_console(&args),
    }
}

fn run_console(args: &ConsoleArgs) -> anyhow::Result<()> {
    init_console_logging();
    let settings = WorkerSettings {
        config_path: args
            .config
            .clone()
            .or_else(|| Some(config::default_config_path())),
        seed_host: args.seed_host.clone(),
        routes: args.route().map(|route| vec![route]),
        ..WorkerSettings::default()
    };

    if args.dry_run {
        return worker::dry_run(&settings);
    }

    let stop = StopHandle::new();
    let ctrl_c_stop = stop.clone();
    ctrlc::set_handler(move || ctrl_c_stop.stop())
        .map_err(|err| anyhow::anyhow!("installing Ctrl-C handler: {err}"))?;
    println!("Running in the foreground. Press Ctrl-C to stop.");
    worker::run(&settings, &stop);
    Ok(())
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
    let dir = config::log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("sonos-cast-svc.log"))
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

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;
    use sonos_routing::{RoomSelector, Source};

    fn console(args: &[&str]) -> super::ConsoleArgs {
        let mut command_line = vec!["sonos-cast-svc", "console"];
        command_line.extend_from_slice(args);
        match Cli::parse_from(command_line).command {
            Command::Console(args) => args,
            other => panic!("expected console, got {other:?}"),
        }
    }

    #[test]
    fn no_room_arguments_defer_to_the_config_file() {
        assert!(console(&[]).route().is_none());
    }

    #[test]
    fn repeated_rooms_become_one_grouped_route() {
        let route = console(&["--room", "Küche", "--room", "Bad"])
            .route()
            .expect("a route");
        assert_eq!(
            route.rooms,
            RoomSelector::Named(vec!["Küche".into(), "Bad".into()])
        );
        assert_eq!(route.source, Source::Default);
    }

    #[test]
    fn process_flags_select_the_matching_source() {
        let only = console(&["--all", "--process", "spotify.exe"])
            .route()
            .expect("a route");
        assert_eq!(only.source, Source::Process("spotify.exe".into()));
        assert_eq!(only.rooms, RoomSelector::All);

        let except = console(&["--all", "--except-process", "teams.exe"])
            .route()
            .expect("a route");
        assert_eq!(except.source, Source::ExceptProcess("teams.exe".into()));
    }

    #[test]
    fn contradictory_source_flags_are_rejected_by_the_parser() {
        // Better here than as a silently ignored flag once audio is playing.
        for argv in [
            vec!["--all", "--process", "a.exe", "--except-process", "b.exe"],
            vec!["--all", "--process", "a.exe", "--device", "Realtek"],
            vec!["--all", "--room", "Küche"],
        ] {
            let mut full = vec!["sonos-cast-svc", "console"];
            full.extend_from_slice(&argv);
            assert!(
                Cli::try_parse_from(full).is_err(),
                "{argv:?} must not parse"
            );
        }
    }
}
