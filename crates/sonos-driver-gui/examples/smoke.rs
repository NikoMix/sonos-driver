//! Headless end-to-end exercise of the routing app's engine.
//!
//! Runs the same reducer, routing table and streaming supervisor the window
//! drives, with the rendering shell replaced by `println!`. That makes the one
//! path a unit test cannot reach — real endpoints, real discovery, a real Sonos
//! stream — runnable on a machine that has speakers on the network.
//!
//! ```text
//! cargo run -p sonos-driver-gui --example smoke -- --seed 192.168.178.25 --room Schlafzimmer
//! ```
//!
//! Every argument is optional: without `--seed` it relies on multicast
//! discovery, and without `--room` it routes the default output to every room.
//! It streams for `--seconds` (default 20) and then stops, and it exits non-zero
//! if the worker never reached the streaming state, so it can gate a release.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use sonos_cast_service::WorkerEvent;
use sonos_driver_gui::{
    driver_status, reduce, Action, AppState, CastSupervisor, Destination, Effect, Output,
    StreamStatus,
};

/// How long to wait for each of discovery and the first streaming report.
const STEP_TIMEOUT: Duration = Duration::from_secs(45);

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt().init();

    let args = Args::parse();
    match run(&args) {
        Ok(()) => {
            println!("\nSMOKE PASSED");
            std::process::ExitCode::SUCCESS
        }
        Err(reason) => {
            println!("\nSMOKE FAILED: {reason}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run(args: &Args) -> Result<(), String> {
    let mut state = AppState::default();
    let (actions_tx, actions_rx) = mpsc::channel();
    let mut supervisor: Option<CastSupervisor> = None;

    if let Some(seed) = &args.seed {
        state.settings.seed_host.clone_from(seed);
    }

    // 1. Outputs. Enumerated inline rather than on a thread: there is no UI to
    //    keep responsive here, and a blocking call is easier to read.
    let endpoints = audio_capture::enumerate_render_endpoints()
        .map_err(|err| format!("could not list the Windows outputs: {err}"))?;
    println!("outputs:");
    for endpoint in &endpoints {
        let marker = if endpoint.is_default {
            "  (default)"
        } else {
            ""
        };
        let adapter = if endpoint.adapter.is_empty() {
            "<adapter unknown>".to_owned()
        } else {
            format!("[{}]", endpoint.adapter)
        };
        println!("  {} {adapter}{marker}", endpoint.name);
    }
    let outputs: Vec<Output> = endpoints
        .into_iter()
        .map(|endpoint| {
            Output::new(
                endpoint.id,
                endpoint.name,
                endpoint.adapter,
                endpoint.is_default,
            )
        })
        .collect();
    // The same probe the app runs, in the same order, so its result can be seen
    // on a machine where the driver is installed and on one where it is not.
    drive(
        &mut state,
        Action::VadServiceProbed(driver_status::probe_service()),
        &mut supervisor,
    );
    drive(
        &mut state,
        Action::OutputsEnumerated(outputs),
        &mut supervisor,
    );
    println!(
        "kernel-driver mode: {} driver endpoint(s), service {:?}",
        state.driver.endpoints, state.driver.service
    );
    if let Some(advice) = state.driver.advice() {
        println!("  note: {advice}");
    }

    // 2. Rooms, through the real reducer so the effect is the real one.
    for effect in reduce(&mut state, Action::RefreshRooms) {
        if let Effect::SpawnDiscovery { seed, timeout } = effect {
            sonos_driver_gui::discovery::spawn(seed, timeout, actions_tx.clone());
        }
    }
    let deadline = Instant::now() + STEP_TIMEOUT;
    while state.rooms.is_empty() && Instant::now() < deadline {
        if let Ok(action) = actions_rx.recv_timeout(Duration::from_millis(200)) {
            drive(&mut state, action, &mut supervisor);
        }
    }
    if state.rooms.is_empty() {
        let detail = state
            .discovery
            .error()
            .map_or_else(String::new, |reason| format!(": {reason}"));
        return Err(format!("no Sonos rooms found{detail}"));
    }
    println!("rooms:");
    for room in &state.rooms {
        println!("  {}  ({})", room.name, room.uuid);
    }

    // 3. Route the default output, exactly as picking from the drop-down would.
    let destination = match &args.room {
        Some(name) => {
            let room = state
                .rooms
                .iter()
                .find(|room| room.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("no room called {name:?}"))?;
            Destination::Room {
                uuid: room.uuid.clone(),
                name: room.name.clone(),
            }
        }
        None => Destination::AllRooms,
    };
    println!("\nrouting the default output to {}", destination.label());
    drive(
        &mut state,
        Action::Assign {
            row: 0,
            destination,
        },
        &mut supervisor,
    );

    // 4. Start, and wait for the worker to say it really is streaming.
    drive(&mut state, Action::Apply, &mut supervisor);
    if supervisor.is_none() {
        return Err(format!("Start was refused: {}", state.status_line));
    }
    let deadline = Instant::now() + STEP_TIMEOUT;
    while state.stream != StreamStatus::Streaming && Instant::now() < deadline {
        pump(&mut state, &mut supervisor);
        if let StreamStatus::Retrying(reason) = &state.stream {
            return Err(format!("the worker could not start: {reason}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if state.stream != StreamStatus::Streaming {
        return Err(format!("never started streaming: {}", state.status_line));
    }
    println!("\nstreaming:");
    for route in &state.live {
        println!("  {}  (port {})", route.label(), route.port);
    }

    // 5. Hold, then stop and confirm the worker actually wound down, because
    //    that is what splits any Sonos group it formed.
    println!("\nholding for {} seconds…", args.seconds);
    let until = Instant::now() + Duration::from_secs(args.seconds);
    while Instant::now() < until {
        pump(&mut state, &mut supervisor);
        std::thread::sleep(Duration::from_millis(200));
    }

    println!("stopping…");
    drive(&mut state, Action::Stop, &mut supervisor);
    let deadline = Instant::now() + STEP_TIMEOUT;
    while state.stream != StreamStatus::Stopped && Instant::now() < deadline {
        pump(&mut state, &mut supervisor);
        std::thread::sleep(Duration::from_millis(100));
    }
    if state.stream != StreamStatus::Stopped {
        return Err("the worker did not stop".to_owned());
    }
    if supervisor.is_some() {
        return Err("the supervisor outlived the stop".to_owned());
    }
    println!("stopped cleanly");
    Ok(())
}

/// Feed one action through the reducer and execute the effects it returns.
///
/// Mirrors the shell's `apply`, minus the effects only a window can serve.
fn drive(state: &mut AppState, action: Action, supervisor: &mut Option<CastSupervisor>) {
    for effect in reduce(state, action) {
        match effect {
            Effect::StartWorker {
                config,
                routes,
                seed,
            } => *supervisor = Some(CastSupervisor::spawn(*config, routes, seed)),
            Effect::StopWorker => {
                if let Some(running) = supervisor.as_ref() {
                    running.request_stop();
                }
            }
            other => println!("(effect ignored: {other:?})"),
        }
    }
}

/// Drain worker events, as the shell does on every frame.
fn pump(state: &mut AppState, supervisor: &mut Option<CastSupervisor>) {
    let mut events = Vec::new();
    if let Some(running) = supervisor.as_ref() {
        while let Some(event) = running.try_event() {
            events.push(event);
        }
    }
    for event in events {
        let stopped = matches!(event, WorkerEvent::Stopped);
        println!("worker: {event:?}");
        drive(state, Action::Worker(event), supervisor);
        // Same rule the window follows: only release the handle once the
        // reducer agrees we are stopped, or an Apply would kill the worker it
        // has just spawned.
        if stopped && state.stream == StreamStatus::Stopped {
            *supervisor = None;
        }
    }
}

/// The handful of switches this harness accepts.
struct Args {
    seed: Option<String>,
    room: Option<String>,
    seconds: u64,
}

impl Args {
    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self {
            seed: None,
            room: None,
            seconds: 20,
        };
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--seed" => parsed.seed = args.next(),
                "--room" => parsed.room = args.next(),
                "--seconds" => {
                    parsed.seconds = args.next().and_then(|v| v.parse().ok()).unwrap_or(20);
                }
                other => eprintln!("ignoring unknown argument {other:?}"),
            }
        }
        parsed
    }
}
