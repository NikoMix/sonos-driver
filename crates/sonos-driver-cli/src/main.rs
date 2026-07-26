//! `sonos-driver` command-line front-end.
//!
//! The full control surface for the Windows -> Sonos audio bridge: discover
//! players (`list`), stream system audio to a room (`stream`, Ctrl-C to stop),
//! stream to several rooms at once with no driver installed (`cast`), halt
//! playback (`stop`), inspect latency profiles (`profiles`), and manage
//! configuration (`config example` / `config check`). Configuration precedence
//! is strict: command-line flags override file values, which override defaults.
//!
//! Parsing and per-command logic live in [`cli`], [`config`], [`commands`] and
//! [`cast`]; this module only wires argument parsing, logging, dispatch and
//! exit codes.

#![forbid(unsafe_code)]

mod cast;
mod cli;
mod commands;
mod config;
mod vad;

use std::process::ExitCode;

use clap::Parser;
use sonos_driver_core::core::Error as CoreError;

use crate::cli::{Cli, Command};

/// Crate targets whose `tracing` events the default log filter surfaces.
const TRACING_CRATES: [&str; 11] = [
    "sonos_driver_cli",
    "sonos_driver_core",
    "sonos_cast_service",
    "sonos_routing",
    "sonos_control",
    "sonos_vad_endpoint",
    "audio_capture",
    "audio_codec",
    "audio_pipeline",
    "stream_server",
    "audio_core",
];

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);

    match dispatch(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => report_error(&err),
    }
}

fn dispatch(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Profiles => {
            commands::run_profiles();
            Ok(())
        }
        Command::List(args) => commands::run_list(args),
        Command::Stream(args) => commands::run_stream(cli.config.as_deref(), args),
        Command::Stop(args) => commands::run_stop(args),
        Command::Cast(args) => cast::run_cast(args),
        Command::Vad(action) => vad::run_vad(action, cli.config.as_deref()),
        Command::Config(action) => commands::run_config(action, cli.config.as_deref()),
    }
}

/// Map an error to an exit code. A pending-workstream capability
/// ([`CoreError::NotImplemented`]) is an informational outcome (exit 2), not a
/// hard failure; everything else is a generic failure (exit 1).
fn report_error(err: &anyhow::Error) -> ExitCode {
    if let Some(CoreError::NotImplemented(msg)) = err.downcast_ref::<CoreError>() {
        eprintln!("not yet available: {msg}");
        return ExitCode::from(2);
    }
    eprintln!("error: {err:#}");
    ExitCode::FAILURE
}

fn init_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    let level = if quiet {
        "error"
    } else {
        match verbose {
            0 => "info",
            1 => "debug",
            _ => "trace",
        }
    };
    let directives = std::iter::once("warn".to_owned())
        .chain(
            TRACING_CRATES
                .iter()
                .map(|target| format!("{target}={level}")),
        )
        .collect::<Vec<_>>()
        .join(",");
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(directives));
    fmt().with_env_filter(filter).with_target(false).init();
}
