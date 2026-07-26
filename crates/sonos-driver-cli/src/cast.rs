//! The `cast` subcommand — driverless streaming ([ADR-0019]).
//!
//! Deliberately thin. All the work lives in the `sonos_cast_service` library,
//! shared with the `sonos-cast-svc` Windows service, so what you try here from
//! a terminal is what the service will do once installed — a property that is
//! only worth anything if there is exactly one implementation of it.
//!
//! This module's whole job is turning flags into a [`Route`], which is also
//! what makes the first run possible without writing a config file.
//!
//! [ADR-0019]: https://github.com/NikoMix/sonos-driver/blob/main/docs/adr/0019-driverless-routing-mode.md

use anyhow::Context as _;
use sonos_cast_service::{worker, WorkerSettings};
use sonos_driver_core::StopHandle;
use sonos_routing::{RoomSelector, Route, Source};

use crate::cli::CastArgs;

/// Run the `cast` subcommand.
///
/// # Errors
///
/// Fails if the configuration cannot be read, if the routes cannot be resolved
/// against the discovered household, or if the Ctrl-C handler cannot be
/// installed.
pub(crate) fn run_cast(args: &CastArgs) -> anyhow::Result<()> {
    let settings = WorkerSettings {
        config_path: Some(
            args.routes
                .clone()
                .unwrap_or_else(sonos_cast_service::config::default_config_path),
        ),
        seed_host: args.seed_host.clone(),
        routes: route_from(args).map(|route| vec![route]),
        ..WorkerSettings::default()
    };

    if args.dry_run {
        return worker::dry_run(&settings);
    }

    let stop = StopHandle::new();
    let ctrl_c_stop = stop.clone();
    ctrlc::set_handler(move || ctrl_c_stop.stop()).context("installing the Ctrl-C handler")?;

    println!("Streaming. Press Ctrl-C to stop.");
    worker::run(&settings, &stop);
    Ok(())
}

/// Build the single route the flags describe, or `None` to use the file.
///
/// Rooms are the trigger: naming one means "ignore the file, do this". Without
/// a room the source flags have nowhere to play, so they are ignored too — and
/// `clap` has already rejected combinations that would be ambiguous.
fn route_from(args: &CastArgs) -> Option<Route> {
    let rooms = if args.all {
        RoomSelector::All
    } else if args.room.is_empty() {
        return None;
    } else {
        RoomSelector::Named(args.room.clone())
    };

    let source = if let Some(process) = args.process.clone() {
        Source::Process(process)
    } else if let Some(process) = args.except_process.clone() {
        Source::ExceptProcess(process)
    } else if let Some(device) = args.device.clone() {
        Source::Device(device)
    } else {
        Source::Default
    };

    Some(Route::new(source, rooms))
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{route_from, RoomSelector, Source};
    use crate::cli::{Cli, Command};

    fn parse(args: &[&str]) -> crate::cli::CastArgs {
        let cli = Cli::try_parse_from(args).expect("the arguments should parse");
        match cli.command {
            Command::Cast(cast) => cast,
            other => panic!("expected a cast command, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_cast_defers_to_the_configuration_file() {
        assert!(route_from(&parse(&["sonos-driver", "cast"])).is_none());
    }

    #[test]
    fn naming_rooms_overrides_the_file() {
        let route = route_from(&parse(&[
            "sonos-driver",
            "cast",
            "--room",
            "Kitchen",
            "--room",
            "Office",
        ]))
        .expect("naming rooms should build a route");
        assert_eq!(
            route.rooms,
            RoomSelector::Named(vec!["Kitchen".to_owned(), "Office".to_owned()])
        );
        assert_eq!(route.source, Source::Default);
    }

    #[test]
    fn all_selects_the_whole_household() {
        let route =
            route_from(&parse(&["sonos-driver", "cast", "--all"])).expect("--all builds a route");
        assert_eq!(route.rooms, RoomSelector::All);
    }

    #[test]
    fn a_process_flag_switches_the_source() {
        let route = route_from(&parse(&[
            "sonos-driver",
            "cast",
            "--all",
            "--process",
            "spotify.exe",
        ]))
        .expect("the route should build");
        assert_eq!(route.source, Source::Process("spotify.exe".to_owned()));
    }

    #[test]
    fn excluding_a_process_is_a_distinct_source() {
        let route = route_from(&parse(&[
            "sonos-driver",
            "cast",
            "--all",
            "--except-process",
            "teams.exe",
        ]))
        .expect("the route should build");
        assert_eq!(route.source, Source::ExceptProcess("teams.exe".to_owned()));
    }

    #[test]
    fn source_flags_without_rooms_still_defer_to_the_file() {
        // The file may well route that process somewhere; silently inventing a
        // destination here would be worse than ignoring the flag.
        assert!(route_from(&parse(&[
            "sonos-driver",
            "cast",
            "--process",
            "spotify.exe"
        ]))
        .is_none());
    }

    #[test]
    fn rooms_and_all_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from(["sonos-driver", "cast", "--all", "--room", "Kitchen"]).is_err()
        );
    }

    #[test]
    fn a_device_cannot_also_be_a_process() {
        assert!(Cli::try_parse_from([
            "sonos-driver",
            "cast",
            "--all",
            "--device",
            "Speakers",
            "--process",
            "spotify.exe",
        ])
        .is_err());
    }
}
