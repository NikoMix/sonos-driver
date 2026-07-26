//! The pure action/effect reducer at the heart of the app.
//!
//! [`reduce`] is the *only* way [`AppState`] changes. It takes an [`Action`]
//! (a click, a tray command, a background thread or a timer), mutates the
//! state, and returns [`Effect`]s for the shell to carry out (spawn a thread,
//! start/stop streaming, persist the config, drive the window). Keeping this
//! logic free of I/O makes every state transition unit-testable without a
//! display, a sound card or a Sonos player.
//!
//! Two rules are enforced here rather than in the UI, because they are the ones
//! that would otherwise produce confusing behaviour:
//!
//! 1. **A conflicting table is never started.** The planner would reject it a
//!    few seconds later from a background thread; refusing on the click, with
//!    the reason, is the difference between a mistake and a mystery.
//! 2. **Applying while streaming stops first.** Two workers would bind the same
//!    ports and fight over the same rooms, so a restart is queued and resumed
//!    when the old one confirms it has stopped.

use std::path::PathBuf;
use std::time::Duration;

use audio_core::Config;
use sonos_cast_service::{RouteReport, WorkerEvent};
use sonos_routing::{Route, RouteConfig};

use crate::driver_status::{DriverStatus, VadService};
use crate::routes::Destination;
use crate::state::{AppState, Output, Room, Scan, StreamStatus};

/// Discovery floor: shorter searches rarely collect every responder.
const MIN_DISCOVERY: Duration = Duration::from_secs(3);

/// An input to the reducer.
#[derive(Debug, Clone)]
pub enum Action {
    /// Enumerate the Windows render endpoints.
    RefreshOutputs,
    /// Endpoint enumeration completed.
    OutputsEnumerated(Vec<Output>),
    /// Endpoint enumeration failed, with a human-readable reason.
    OutputScanFailed(String),
    /// The Service Control Manager was asked about the `sonosvad` service.
    ///
    /// Sent alongside enumeration: together the two say whether the optional
    /// kernel-driver mode is installed, loaded, and actually streaming.
    VadServiceProbed(VadService),
    /// Begin background Sonos discovery.
    RefreshRooms,
    /// Discovery completed with the given rooms.
    RoomsDiscovered(Vec<Room>),
    /// Discovery failed with a human-readable reason.
    DiscoveryFailed(String),
    /// Send the output at `row` to `destination`.
    Assign {
        /// Index into [`RoutingTable::rows`](crate::routes::RoutingTable::rows).
        row: usize,
        /// Where that output's audio should go.
        destination: Destination,
    },
    /// Turn every route off.
    ClearRouting,
    /// Start streaming the current table, or restart it if already streaming.
    Apply,
    /// Stop streaming.
    Stop,
    /// The streaming worker reported something.
    Worker(WorkerEvent),
    /// Persist the current table and settings.
    Save,
    /// The configuration was saved successfully.
    Saved,
    /// Saving the configuration failed.
    SaveFailed(String),
    /// Reveal the main window.
    ShowWindow,
    /// Hide the main window to the tray.
    HideWindow,
    /// Quit the application.
    Quit,
}

/// A side effect for the shell to execute after a [`reduce`] call.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Enumerate the render endpoints off the UI thread.
    EnumerateOutputs,
    /// Spawn background discovery.
    SpawnDiscovery {
        /// Any one known player's address, or `None` for multicast.
        seed: Option<String>,
        /// How long to wait for responses.
        timeout: Duration,
    },
    /// Start the streaming worker.
    StartWorker {
        /// The validated audio settings.
        config: Box<Config>,
        /// The routes to run.
        routes: Vec<Route>,
        /// Seed host for the worker's own discovery.
        seed: Option<String>,
    },
    /// Ask the running worker to stop.
    StopWorker,
    /// Persist `config` and `routes` to `path`.
    Save {
        /// Destination file.
        path: PathBuf,
        /// The settings to write.
        config: Box<Config>,
        /// The routes to write.
        routes: Vec<RouteConfig>,
    },
    /// Reveal the main window.
    ShowWindow,
    /// Hide the main window to the tray.
    HideWindow,
    /// Quit the application.
    Quit,
}

/// Advance `state` by `action`, returning the side effects to run.
#[must_use]
pub fn reduce(state: &mut AppState, action: Action) -> Vec<Effect> {
    match action {
        Action::RefreshOutputs => refresh_outputs(state),
        Action::OutputsEnumerated(outputs) => {
            state.outputs = Scan::Done;
            state.driver = DriverStatus::new(&outputs, state.driver.service);
            state.table.sync_outputs(outputs);
            restore_saved(state);
            describe(state);
            Vec::new()
        }
        Action::OutputScanFailed(reason) => {
            state.status_line = format!("Could not list the Windows outputs: {reason}");
            state.outputs = Scan::Failed(reason);
            Vec::new()
        }
        Action::VadServiceProbed(service) => {
            state.driver.service = service;
            Vec::new()
        }
        Action::RefreshRooms => refresh_rooms(state),
        Action::RoomsDiscovered(rooms) => {
            state.discovery = Scan::Done;
            state.rooms = rooms;
            resolve_destinations(state);
            describe(state);
            Vec::new()
        }
        Action::DiscoveryFailed(reason) => {
            state.status_line = format!("Could not find any Sonos rooms: {reason}");
            state.discovery = Scan::Failed(reason);
            Vec::new()
        }
        Action::Assign { row, destination } => {
            if state.table.assign(row, destination) {
                state.unapplied = state.stream.is_active();
                describe(state);
            }
            Vec::new()
        }
        Action::ClearRouting => {
            state.table.clear();
            state.unapplied = state.stream.is_active();
            describe(state);
            Vec::new()
        }
        Action::Apply => apply(state),
        Action::Stop => stop(state),
        Action::Worker(event) => worker(state, event),
        Action::Save => save(state),
        Action::Saved => {
            state.status_line = format!("Saved to {}", state.config_path.display());
            Vec::new()
        }
        Action::SaveFailed(reason) => {
            state.status_line = format!("Could not save: {reason}");
            Vec::new()
        }
        Action::ShowWindow => vec![Effect::ShowWindow],
        Action::HideWindow => vec![Effect::HideWindow],
        Action::Quit => vec![Effect::Quit],
    }
}

fn refresh_outputs(state: &mut AppState) -> Vec<Effect> {
    if state.outputs.is_running() {
        return Vec::new();
    }
    state.outputs = Scan::Running;
    vec![Effect::EnumerateOutputs]
}

fn refresh_rooms(state: &mut AppState) -> Vec<Effect> {
    if state.discovery.is_running() {
        return Vec::new();
    }
    state.discovery = Scan::Running;
    status(state, "Looking for Sonos rooms…");
    vec![Effect::SpawnDiscovery {
        seed: state.settings.seed(),
        timeout: discovery_timeout(state),
    }]
}

fn discovery_timeout(state: &AppState) -> Duration {
    let configured = state
        .settings
        .discovery_timeout_secs
        .trim()
        .parse::<u64>()
        .unwrap_or(3);
    Duration::from_secs(configured).max(MIN_DISCOVERY)
}

/// Start streaming, or queue a restart when a worker is already running.
fn apply(state: &mut AppState) -> Vec<Effect> {
    if state.table.is_idle() {
        status(
            state,
            "Nothing is routed yet — pick a room next to an output first.",
        );
        return Vec::new();
    }

    let conflicts = state.table.conflicts();
    if let Some(conflict) = conflicts.first() {
        status(state, &conflict.message);
        return Vec::new();
    }

    if state.stream.is_active() {
        // Restart: stop now, start again on the Stopped event.
        state.unapplied = true;
        state.stream = StreamStatus::Stopping;
        status(state, "Applying changes…");
        return vec![Effect::StopWorker];
    }

    start(state)
}

/// Build and launch the worker. The caller has already checked the table.
fn start(state: &mut AppState) -> Vec<Effect> {
    let config = match state.settings.to_config() {
        Ok(config) => config,
        Err(error) => {
            state.status_line = format!("Check the settings: {error}");
            state.stream = StreamStatus::Stopped;
            state.unapplied = false;
            return Vec::new();
        }
    };

    let routes = state.table.to_routes(&state.rooms);
    if routes.is_empty() {
        state.status_line = if state.rooms.is_empty() {
            "No Sonos rooms have been found yet — try Find rooms, or set a seed host in \
             Settings."
                .to_owned()
        } else {
            "Every routed room is currently offline. Switch the speakers on, or pick \
             different rooms."
                .to_owned()
        };
        state.stream = StreamStatus::Stopped;
        state.unapplied = false;
        return Vec::new();
    }

    let seed = state.settings.seed();
    state.stream = StreamStatus::Starting;
    state.live.clear();
    state.unapplied = false;
    state.status_line = format!(
        "Starting {} route{}…",
        routes.len(),
        if routes.len() == 1 { "" } else { "s" }
    );

    vec![Effect::StartWorker {
        config: Box::new(config),
        routes,
        seed,
    }]
}

fn stop(state: &mut AppState) -> Vec<Effect> {
    if !state.stream.is_active() {
        return Vec::new();
    }
    // A queued restart is cancelled: an explicit Stop outranks it.
    state.unapplied = false;
    state.stream = StreamStatus::Stopping;
    status(state, "Stopping…");
    vec![Effect::StopWorker]
}

fn worker(state: &mut AppState, event: WorkerEvent) -> Vec<Effect> {
    match event {
        WorkerEvent::Discovering => {
            if state.stream != StreamStatus::Stopping {
                state.stream = StreamStatus::Starting;
                status(state, "Finding the rooms…");
            }
            Vec::new()
        }
        WorkerEvent::Streaming(reports) => {
            if state.stream != StreamStatus::Stopping {
                state.stream = StreamStatus::Streaming;
                state.status_line = summarise(&reports);
            }
            state.live = reports;
            Vec::new()
        }
        WorkerEvent::Failed(reason) => {
            state.live.clear();
            if state.stream != StreamStatus::Stopping {
                state.status_line = format!("{reason} — retrying");
                state.stream = StreamStatus::Retrying(reason);
            }
            Vec::new()
        }
        WorkerEvent::Stopped => {
            state.live.clear();
            state.stream = StreamStatus::Stopped;
            if state.unapplied {
                // The restart queued by `apply`.
                return start(state);
            }
            status(state, "Stopped");
            Vec::new()
        }
    }
}

fn save(state: &mut AppState) -> Vec<Effect> {
    match state.settings.to_config() {
        Ok(config) => vec![Effect::Save {
            path: state.config_path.clone(),
            config: Box::new(config),
            routes: state.table.to_configs(),
        }],
        Err(error) => {
            state.status_line = format!("Check the settings: {error}");
            Vec::new()
        }
    }
}

/// Replace the footer message, reusing the existing allocation.
///
/// The status line is rewritten on nearly every action, so this is both the
/// tidier call site and the one clippy's `assigning_clones` lint asks for.
fn status(state: &mut AppState, text: &str) {
    state.status_line.clear();
    state.status_line.push_str(text);
}

/// Place the routing read from disk, now that the outputs are known.
///
/// Runs once: the saved list is taken, so a later re-enumeration cannot undo
/// edits the user has made since.
fn restore_saved(state: &mut AppState) {
    if state.saved_routes.is_empty() {
        return;
    }
    let saved = std::mem::take(&mut state.saved_routes);
    state.table.apply_configs(&saved, &state.rooms);
}

/// Re-attach discovered rooms to assignments after discovery.
///
/// Two cases, both of which would otherwise look like a lost setting:
///
/// * a room **renamed** in the Sonos app keeps its `UUID`, so the assignment is
///   still correct and only the label the user sees is stale;
/// * a route **restored from disk** before discovery holds a room *name* where
///   a `UUID` belongs, because the saved file is written in names to stay
///   portable. It resolves the first time the household answers.
fn resolve_destinations(state: &mut AppState) {
    let updates: Vec<(usize, Destination)> = state
        .table
        .rows()
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let Destination::Room { uuid, name } = &row.destination else {
                return None;
            };
            let resolved = state
                .rooms
                .iter()
                .find(|room| &room.uuid == uuid)
                .or_else(|| {
                    state
                        .rooms
                        .iter()
                        .find(|room| room.name.eq_ignore_ascii_case(name))
                })?;
            (&resolved.uuid != uuid || &resolved.name != name).then(|| {
                (
                    index,
                    Destination::Room {
                        uuid: resolved.uuid.clone(),
                        name: resolved.name.clone(),
                    },
                )
            })
        })
        .collect();

    for (index, destination) in updates {
        state.table.assign(index, destination);
    }
}

/// Write the idle status line describing what the table currently says.
fn describe(state: &mut AppState) {
    if state.stream.is_active() {
        if state.unapplied {
            status(state, "Changes are not live yet — press Apply.");
        }
        return;
    }

    if let Some(conflict) = state.table.conflicts().first() {
        status(state, &conflict.message);
        return;
    }

    let routed = state.table.routed_count();
    state.status_line = if routed == 0 {
        if state.rooms.is_empty() {
            "No rooms found yet. Press Find rooms, or set a seed host in Settings.".to_owned()
        } else {
            "Pick a room next to an output, then press Start.".to_owned()
        }
    } else {
        format!(
            "{routed} output{} routed. Press Start.",
            if routed == 1 { "" } else { "s" }
        )
    };
}

/// One line describing everything that is live.
fn summarise(reports: &[RouteReport]) -> String {
    match reports {
        [] => "Streaming".to_owned(),
        [one] => format!("Streaming {}", one.label()),
        many => format!(
            "Streaming {} routes: {}",
            many.len(),
            many.iter()
                .map(RouteReport::label)
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{reduce, Action, Effect};
    use crate::routes::Destination;
    use crate::state::{AppState, Output, Room, Scan, StreamStatus};
    use sonos_cast_service::{RouteReport, WorkerEvent};

    fn outputs() -> Vec<Output> {
        vec![
            Output::new("id-speakers", "Speakers", "", true),
            Output::new("id-hdmi", "LG TV", "", false),
        ]
    }

    fn rooms() -> Vec<Room> {
        vec![Room::new("RINCON_1", "Küche"), Room::new("RINCON_2", "Bad")]
    }

    fn kitchen() -> Destination {
        Destination::Room {
            uuid: "RINCON_1".to_owned(),
            name: "Küche".to_owned(),
        }
    }

    /// A state with outputs and rooms already discovered.
    fn ready() -> AppState {
        let mut state = AppState::default();
        let _ = reduce(&mut state, Action::OutputsEnumerated(outputs()));
        let _ = reduce(&mut state, Action::RoomsDiscovered(rooms()));
        state
    }

    /// Drive a state all the way to streaming.
    fn streaming() -> AppState {
        let mut state = ready();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 1,
                destination: kitchen(),
            },
        );
        let _ = reduce(&mut state, Action::Apply);
        let _ = reduce(
            &mut state,
            Action::Worker(WorkerEvent::Streaming(vec![RouteReport {
                source: "Speakers".to_owned(),
                rooms: vec!["Küche".to_owned()],
                port: 8080,
                grouped: false,
            }])),
        );
        state
    }

    #[test]
    fn refreshing_outputs_is_not_started_twice() {
        let mut state = AppState::default();
        assert_eq!(reduce(&mut state, Action::RefreshOutputs).len(), 1);
        assert!(state.outputs.is_running());
        assert!(reduce(&mut state, Action::RefreshOutputs).is_empty());
    }

    #[test]
    fn discovery_carries_the_seed_host_and_honours_the_floor() {
        let mut state = AppState::default();
        state.settings.seed_host = " 192.168.1.50 ".to_owned();
        state.settings.discovery_timeout_secs = "1".to_owned();

        let effects = reduce(&mut state, Action::RefreshRooms);
        match effects.as_slice() {
            [Effect::SpawnDiscovery { seed, timeout }] => {
                assert_eq!(seed.as_deref(), Some("192.168.1.50"));
                assert_eq!(*timeout, std::time::Duration::from_secs(3));
            }
            other => panic!("expected one discovery effect, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_scan_explains_itself_without_clearing_the_table() {
        let mut state = ready();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 1,
                destination: kitchen(),
            },
        );
        let _ = reduce(
            &mut state,
            Action::DiscoveryFailed("no response".to_owned()),
        );

        assert_eq!(state.discovery, Scan::Failed("no response".to_owned()));
        assert!(state.status_line.contains("no response"));
        assert_eq!(state.table.routed_count(), 1, "the table is not discarded");
    }

    #[test]
    fn an_empty_table_refuses_to_start_and_says_why() {
        let mut state = ready();
        assert!(reduce(&mut state, Action::Apply).is_empty());
        assert_eq!(state.stream, StreamStatus::Stopped);
        assert!(state.status_line.contains("Nothing is routed"));
    }

    #[test]
    fn a_conflicting_table_refuses_to_start_with_the_planner_s_reason() {
        let mut state = ready();
        for row in [1, 2] {
            let _ = reduce(
                &mut state,
                Action::Assign {
                    row,
                    destination: kitchen(),
                },
            );
        }
        assert!(reduce(&mut state, Action::Apply).is_empty());
        assert_eq!(state.stream, StreamStatus::Stopped);
        assert!(state.status_line.contains("Küche"), "{}", state.status_line);
    }

    #[test]
    fn starting_builds_a_worker_effect_from_the_table() {
        let mut state = ready();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 0,
                destination: Destination::AllRooms,
            },
        );

        let effects = reduce(&mut state, Action::Apply);
        match effects.as_slice() {
            [Effect::StartWorker { routes, .. }] => assert_eq!(routes.len(), 1),
            other => panic!("expected one start effect, got {other:?}"),
        }
        assert_eq!(state.stream, StreamStatus::Starting);
        assert!(!state.unapplied);
    }

    #[test]
    fn routing_only_offline_rooms_explains_itself_instead_of_starting() {
        let mut state = ready();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 1,
                destination: Destination::Room {
                    uuid: "RINCON_GONE".to_owned(),
                    name: "Keller".to_owned(),
                },
            },
        );
        assert!(reduce(&mut state, Action::Apply).is_empty());
        assert_eq!(state.stream, StreamStatus::Stopped);
        assert!(
            state.status_line.contains("offline"),
            "{}",
            state.status_line
        );
    }

    #[test]
    fn bad_settings_are_reported_rather_than_streamed() {
        let mut state = ready();
        state.settings.port = "0".to_owned();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 0,
                destination: Destination::AllRooms,
            },
        );
        assert!(reduce(&mut state, Action::Apply).is_empty());
        assert!(state.status_line.contains("Check the settings"));
    }

    #[test]
    fn applying_while_streaming_stops_first_then_starts_again() {
        let mut state = streaming();
        assert_eq!(state.stream, StreamStatus::Streaming);

        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 2,
                destination: Destination::Room {
                    uuid: "RINCON_2".to_owned(),
                    name: "Bad".to_owned(),
                },
            },
        );
        assert!(state.unapplied, "an edit while live is not live yet");

        let effects = reduce(&mut state, Action::Apply);
        assert!(matches!(effects.as_slice(), [Effect::StopWorker]));
        assert_eq!(state.stream, StreamStatus::Stopping);

        // Only once the old worker confirms it is gone does the new one start.
        let effects = reduce(&mut state, Action::Worker(WorkerEvent::Stopped));
        match effects.as_slice() {
            [Effect::StartWorker { routes, .. }] => assert_eq!(routes.len(), 2),
            other => panic!("expected a restart, got {other:?}"),
        }
        assert_eq!(state.stream, StreamStatus::Starting);
    }

    #[test]
    fn stopping_cancels_a_queued_restart() {
        let mut state = streaming();
        let _ = reduce(&mut state, Action::ClearRouting);
        let _ = reduce(&mut state, Action::Apply);

        let effects = reduce(&mut state, Action::Stop);
        assert!(matches!(effects.as_slice(), [Effect::StopWorker]));
        assert!(!state.unapplied);

        assert!(reduce(&mut state, Action::Worker(WorkerEvent::Stopped)).is_empty());
        assert_eq!(state.stream, StreamStatus::Stopped);
    }

    #[test]
    fn stopping_when_nothing_runs_does_nothing() {
        let mut state = ready();
        assert!(reduce(&mut state, Action::Stop).is_empty());
    }

    #[test]
    fn a_worker_failure_is_shown_and_the_status_says_it_will_retry() {
        let mut state = streaming();
        let _ = reduce(
            &mut state,
            Action::Worker(WorkerEvent::Failed("no rooms answered".to_owned())),
        );
        assert_eq!(
            state.stream,
            StreamStatus::Retrying("no rooms answered".to_owned())
        );
        assert!(state.status_line.contains("retrying"));
        assert!(state.live.is_empty());
    }

    #[test]
    fn worker_events_arriving_during_teardown_do_not_resurrect_the_status() {
        // The worker is mid-retry when the user presses Stop; the late events
        // must not flip the UI back to "streaming".
        let mut state = streaming();
        let _ = reduce(&mut state, Action::Stop);
        let _ = reduce(&mut state, Action::Worker(WorkerEvent::Discovering));
        assert_eq!(state.stream, StreamStatus::Stopping);
        let _ = reduce(
            &mut state,
            Action::Worker(WorkerEvent::Streaming(Vec::new())),
        );
        assert_eq!(state.stream, StreamStatus::Stopping);
    }

    #[test]
    fn streaming_summarises_every_live_route() {
        let mut state = streaming();
        let _ = reduce(
            &mut state,
            Action::Worker(WorkerEvent::Streaming(vec![
                RouteReport {
                    source: "Speakers".to_owned(),
                    rooms: vec!["Küche".to_owned()],
                    port: 8080,
                    grouped: false,
                },
                RouteReport {
                    source: "LG TV".to_owned(),
                    rooms: vec!["Bad".to_owned(), "Küche".to_owned()],
                    port: 8081,
                    grouped: true,
                },
            ])),
        );
        assert!(state.status_line.contains("2 routes"));
        assert!(state.status_line.contains("Bad + Küche"));
        assert_eq!(state.live.len(), 2);
    }

    #[test]
    fn a_renamed_room_keeps_its_assignment_and_picks_up_the_new_name() {
        let mut state = ready();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 1,
                destination: kitchen(),
            },
        );

        let _ = reduce(
            &mut state,
            Action::RoomsDiscovered(vec![Room::new("RINCON_1", "Kochnische")]),
        );
        assert_eq!(
            state.table.rows()[1].destination,
            Destination::Room {
                uuid: "RINCON_1".to_owned(),
                name: "Kochnische".to_owned(),
            }
        );
    }

    #[test]
    fn a_saved_routing_is_placed_once_the_outputs_are_known() {
        use sonos_routing::{RoomsField, RouteConfig};
        let mut state = AppState {
            saved_routes: vec![RouteConfig {
                rooms: RoomsField::List(vec!["Küche".to_owned()]),
                device: Some("LG TV".to_owned()),
                process: None,
                except_process: None,
            }],
            ..AppState::default()
        };

        // Rooms first: nothing can be placed yet, and the routing is not lost.
        let _ = reduce(&mut state, Action::RoomsDiscovered(rooms()));
        assert!(state.table.is_idle());
        assert_eq!(state.saved_routes.len(), 1);

        let _ = reduce(&mut state, Action::OutputsEnumerated(outputs()));
        assert_eq!(state.table.rows()[2].destination, kitchen());
        assert!(state.saved_routes.is_empty(), "restored exactly once");
    }

    #[test]
    fn a_saved_room_name_resolves_to_a_uuid_when_discovery_lands_later() {
        use sonos_routing::{RoomsField, RouteConfig};
        let mut state = AppState {
            saved_routes: vec![RouteConfig {
                rooms: RoomsField::List(vec!["Küche".to_owned()]),
                device: None,
                process: None,
                except_process: None,
            }],
            ..AppState::default()
        };

        // Outputs arrive before any room does, so the name is all there is.
        let _ = reduce(&mut state, Action::OutputsEnumerated(outputs()));
        assert_eq!(
            state.table.rows()[0].destination,
            Destination::Room {
                uuid: "Küche".to_owned(),
                name: "Küche".to_owned(),
            }
        );

        let _ = reduce(&mut state, Action::RoomsDiscovered(rooms()));
        assert_eq!(state.table.rows()[0].destination, kitchen());
    }

    #[test]
    fn re_enumerating_outputs_does_not_resurrect_a_restored_routing() {
        use sonos_routing::{RoomsField, RouteConfig};
        let mut state = AppState {
            saved_routes: vec![RouteConfig {
                rooms: RoomsField::Keyword("all".to_owned()),
                device: None,
                process: None,
                except_process: None,
            }],
            ..AppState::default()
        };
        let _ = reduce(&mut state, Action::OutputsEnumerated(outputs()));
        let _ = reduce(&mut state, Action::ClearRouting);
        let _ = reduce(&mut state, Action::OutputsEnumerated(outputs()));
        assert!(state.table.is_idle(), "the user's clear must stick");
    }

    #[test]
    fn saving_writes_the_table_and_the_settings_together() {
        let mut state = ready();
        let _ = reduce(
            &mut state,
            Action::Assign {
                row: 2,
                destination: kitchen(),
            },
        );

        let effects = reduce(&mut state, Action::Save);
        match effects.as_slice() {
            [Effect::Save { path, routes, .. }] => {
                assert_eq!(path, &state.config_path);
                assert_eq!(routes.len(), 1);
                assert_eq!(routes[0].device.as_deref(), Some("LG TV"));
            }
            other => panic!("expected one save effect, got {other:?}"),
        }
    }

    #[test]
    fn saving_invalid_settings_is_refused() {
        let mut state = ready();
        state.settings.bind_address = "nope".to_owned();
        assert!(reduce(&mut state, Action::Save).is_empty());
        assert!(state.status_line.contains("Check the settings"));
    }

    #[test]
    fn window_actions_map_straight_to_effects() {
        let mut state = AppState::default();
        assert!(matches!(
            reduce(&mut state, Action::ShowWindow).as_slice(),
            [Effect::ShowWindow]
        ));
        assert!(matches!(
            reduce(&mut state, Action::HideWindow).as_slice(),
            [Effect::HideWindow]
        ));
        assert!(matches!(
            reduce(&mut state, Action::Quit).as_slice(),
            [Effect::Quit]
        ));
    }

    #[test]
    fn an_out_of_range_assignment_is_ignored() {
        let mut state = ready();
        assert!(reduce(
            &mut state,
            Action::Assign {
                row: 99,
                destination: kitchen(),
            }
        )
        .is_empty());
        assert!(state.table.is_idle());
    }
}
