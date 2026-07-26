//! The work the driverless host actually does, independent of how it started.
//!
//! Mirrors `sonos-vad-service`'s worker so `console` and the service run an
//! identical code path, but plans from configured routes instead of from the
//! driver's endpoints.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use audio_core::{CaptureConfig, Config, Error};
use sonos_driver_core::{
    discover_rooms, Arbiter, Bridge, MultiRoomBridge, RoomSession, SessionTarget, StopHandle,
};
use sonos_routing::{plan, PlannedRoute, Route, Source};

use crate::observer::{Observer, RouteReport, WorkerEvent};

/// How long to wait before retrying after a failed startup attempt.
///
/// The service starts at boot, which is exactly when the network stack, DHCP
/// lease and Sonos players are least likely to be ready.
const RETRY_MIN: Duration = Duration::from_secs(5);
/// Ceiling for the retry backoff, so a permanently absent household is retried
/// every minute rather than every few hours.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Everything the worker needs, resolved before the service reports RUNNING.
#[derive(Debug, Clone)]
pub struct WorkerSettings {
    /// Configuration file to load.
    pub config_path: Option<PathBuf>,
    /// Audio settings supplied by the caller, overriding the file's.
    ///
    /// Together with [`routes`](Self::routes) this lets an interactive front
    /// end apply a change without a round trip through disk — and without a
    /// half-written or hand-corrupted file being able to stop it.
    pub config: Option<Config>,
    /// Seed host override for discovery.
    pub seed_host: Option<String>,
    /// Discovery timeout.
    pub discovery_timeout: Duration,
    /// Routes supplied on the command line, overriding the file's.
    ///
    /// Lets `sonos-cast-svc console --room Küche` work without writing a config
    /// file first, which is how most people will try this the first time.
    pub routes: Option<Vec<Route>>,
    /// Where to report lifecycle changes, for front ends that display them.
    ///
    /// Silent by default: the service and `sonos-driver cast` use logs.
    pub observer: Observer,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            config_path: Some(crate::config::default_config_path()),
            config: None,
            seed_host: None,
            discovery_timeout: Duration::from_secs(5),
            routes: None,
            observer: Observer::silent(),
        }
    }
}

impl WorkerSettings {
    /// The audio settings and routes to use for one attempt.
    ///
    /// Caller-supplied values win over the file. When both are supplied the
    /// file is not read at all, so an interactive front end that has already
    /// validated everything cannot be blocked by a config file it is not using.
    ///
    /// # Errors
    ///
    /// Fails when the file has to be read and cannot be, or does not parse.
    fn resolve(&self) -> anyhow::Result<(Config, Vec<Route>)> {
        if let (Some(config), Some(routes)) = (self.config.clone(), self.routes.clone()) {
            return Ok((config, routes));
        }
        let (loaded, file_routes) = crate::config::load(self.config_path.as_deref())?;
        Ok((
            self.config.clone().unwrap_or(loaded),
            self.routes.clone().unwrap_or(file_routes),
        ))
    }
}

/// Run until `stop` is signalled.
///
/// Infallible by design: startup failures are retried rather than returned,
/// because "the household was not reachable at boot" is a normal transient
/// condition on an auto-start service. Callers that need to know attach an
/// [`Observer`](crate::Observer).
pub fn run(settings: &WorkerSettings, stop: &StopHandle) {
    let mut backoff = RETRY_MIN;

    while !stop.is_stop_requested() {
        match run_once(settings, stop) {
            Ok(()) => {
                if stop.is_stop_requested() {
                    tracing::info!("stop requested; service shutting down");
                    settings.observer.emit(WorkerEvent::Stopped);
                    return;
                }
                tracing::warn!("all route bridges ended unexpectedly; re-planning");
                backoff = RETRY_MIN;
            }
            Err(err) => {
                let reason = format!("{err:#}");
                tracing::error!(
                    error = %reason,
                    retry_in_secs = backoff.as_secs(),
                    "service iteration failed; retrying"
                );
                settings.observer.emit(WorkerEvent::Failed(reason));
                backoff = (backoff * 2).min(RETRY_MAX);
            }
        }

        if sleep_unless_stopped(stop, backoff) {
            break;
        }
    }

    settings.observer.emit(WorkerEvent::Stopped);
}

/// One full plan-and-stream cycle.
fn run_once(settings: &WorkerSettings, stop: &StopHandle) -> anyhow::Result<()> {
    let (mut config, routes) = settings.resolve()?;
    if routes.is_empty() {
        anyhow::bail!(
            "no routes are configured; add a [[route]] to {}",
            settings
                .config_path
                .as_deref()
                .unwrap_or(std::path::Path::new("the config file"))
                .display()
        );
    }

    let seed = sonos_driver_core::apply_seed_host(settings.seed_host.as_deref(), &mut config);

    settings.observer.emit(WorkerEvent::Discovering);
    tracing::info!(seed = ?seed, "discovering Sonos household");
    let rooms = discover_rooms(seed.as_deref(), settings.discovery_timeout)
        .context("discovering Sonos rooms")?;
    tracing::info!(rooms = rooms.len(), "discovered rooms");

    // A previous run killed mid-stream may have left rooms grouped. Split them
    // before planning, so a route always starts from the topology the user
    // configured rather than one a crash left behind.
    recover_stale_group(seed.as_deref(), settings.discovery_timeout);

    let planned = plan(&routes, &rooms, config.stream.port).context("planning routes")?;
    log_plan(&planned);
    settings.observer.emit(WorkerEvent::Streaming(
        planned.iter().map(RouteReport::from_planned).collect(),
    ));

    let mut supervisor = MultiRoomBridge::new(config);
    supervisor.adopt_stop_handle(stop);
    run_plan(&mut supervisor, &planned, seed).context("running the route bridges")?;
    Ok(())
}

/// Start one bridge per planned route and block until they all end.
fn run_plan(
    supervisor: &mut MultiRoomBridge,
    planned: &[PlannedRoute],
    seed: Option<String>,
) -> Result<(), Error> {
    let sessions: Vec<RoomSession> = planned.iter().map(session_for).collect();
    // The supervisor derives a session's capture settings from its endpoint,
    // which is meaningless here, so each route's real source is looked up by
    // port -- already unique per session and stable across restarts.
    let sources: HashMap<u16, Source> = planned
        .iter()
        .map(|route| (route.port, route.source.clone()))
        .collect();

    let timeout = supervisor.config().sonos.discovery_timeout_secs;
    // One shared map, so a route that groups rooms and a route that owns one
    // of them hand the speaker back and forth instead of fighting over it.
    let arbiter = Arc::new(Arbiter::new());

    supervisor.run_sessions_with(&sessions, move |session, mut config, stop| {
        let source = sources.get(&session.port).ok_or_else(|| {
            Error::config(format!(
                "no audio source was planned for port {}",
                session.port
            ))
        })?;
        apply_source(source, &mut config.capture);

        let mut bridge = Bridge::new(config);
        bridge.adopt_stop_flag(stop);
        bridge.arbitrate(
            Arc::clone(&arbiter),
            session.arbiter_id(),
            session.claimed_rooms(),
        );
        bridge.run_target(&session.target, seed.as_deref(), timeout)
    })
}

/// Point `capture` at what the route says to capture.
///
/// Overwrites every selector rather than merging, so a stale `capture.device`
/// left in the config file cannot quietly override a route's source.
fn apply_source(source: &Source, capture: &mut CaptureConfig) {
    capture.device = source.device().map(str::to_owned);
    capture.process = source.process().map(str::to_owned);
    capture.exclude_process = source.excludes_process();
}

/// Describe a planned route in the shape the supervisor expects.
///
/// `device_name`/`device_id` normally name a Windows endpoint. Here there is no
/// endpoint, so they carry the source's label and descriptor: the supervisor
/// only uses them for logging and for keying the plan, and the real capture
/// settings are applied by [`apply_source`].
fn session_for(route: &PlannedRoute) -> RoomSession {
    let coordinator = route.coordinator();
    RoomSession {
        room_uuid: coordinator.uuid.clone(),
        room_name: coordinator.name.clone(),
        device_name: route.source.label(),
        device_id: route.source.descriptor(),
        port: route.port,
        target: if route.is_group() {
            SessionTarget::AllRooms {
                uuids: route.room_uuids(),
            }
        } else {
            SessionTarget::Room {
                uuid: coordinator.uuid.clone(),
            }
        },
    }
}

/// Undo a group left behind by a run that never got to stop cleanly.
///
/// Best-effort: failing to split a stale group is a reason to log and carry on,
/// not a reason to refuse to stream.
fn recover_stale_group(seed: Option<&str>, timeout: Duration) {
    let journal = sonos_driver_core::party::default_journal_path();
    #[allow(clippy::cast_possible_truncation)]
    let secs = timeout.as_secs().min(u64::from(u32::MAX)) as u32;
    match sonos_driver_core::disband(&journal, seed, secs) {
        Ok(0) => {}
        Ok(released) => tracing::info!(released, "split rooms left grouped by a previous run"),
        Err(error) => {
            tracing::warn!(%error, "could not split rooms left grouped by a previous run");
        }
    }
}

fn log_plan(planned: &[PlannedRoute]) {
    for route in planned {
        tracing::info!(
            route = %route.label(),
            port = route.port,
            grouped = route.is_group(),
            "streaming"
        );
    }
}

/// Resolve the configuration and print the plan without streaming anything.
///
/// The cheap way to check a config file: it does discovery and full planning —
/// so conflicts, unknown rooms and bad sources all surface — but never opens a
/// capture or touches a speaker.
///
/// # Errors
///
/// Fails for the same reasons a real run would fail to start.
pub fn dry_run(settings: &WorkerSettings) -> anyhow::Result<()> {
    let (config, routes) = settings.resolve()?;
    if routes.is_empty() {
        anyhow::bail!("no routes are configured");
    }

    let seed = settings
        .seed_host
        .as_deref()
        .or(config.sonos.seed_host.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let rooms = discover_rooms(seed, settings.discovery_timeout).context("discovering rooms")?;
    let planned = plan(&routes, &rooms, config.stream.port).context("planning routes")?;

    println!("Discovered {} room(s).\n", rooms.len());
    for route in &planned {
        let capture = capture_for(route, &config);
        let mode = if let Some(process) = capture.process.as_deref() {
            let direction = if capture.exclude_process {
                "everything except"
            } else {
                "only"
            };
            format!("process loopback ({direction} {process})")
        } else if let Some(device) = capture.device.as_deref() {
            format!("loopback of \"{device}\"")
        } else {
            "loopback of the default device".to_owned()
        };

        println!("{}", route.label());
        println!("  capture: {mode}");
        println!("  stream:  http://<this-pc>:{}/", route.port);
        println!(
            "  sonos:   {}\n",
            if route.is_group() {
                format!("grouped, coordinated by {}", route.coordinator().name)
            } else {
                "single room".to_owned()
            }
        );
    }
    Ok(())
}

/// Sleep in short slices so a stop request is noticed promptly.
///
/// Returns `true` if the wait was cut short by a stop request.
fn sleep_unless_stopped(stop: &StopHandle, total: Duration) -> bool {
    const SLICE: Duration = Duration::from_millis(250);
    let mut slept = Duration::ZERO;
    while slept < total {
        if stop.is_stop_requested() {
            return true;
        }
        std::thread::sleep(SLICE);
        slept += SLICE;
    }
    stop.is_stop_requested()
}

/// The audio settings a route resolves to.
#[must_use]
fn capture_for(route: &PlannedRoute, template: &Config) -> CaptureConfig {
    let mut capture = template.capture.clone();
    apply_source(&route.source, &mut capture);
    capture
}

#[cfg(test)]
mod tests {
    use super::{capture_for, session_for};
    use audio_core::Config;
    use sonos_driver_core::SessionTarget;
    use sonos_routing::{PlannedRoute, RoomKey, Source};

    fn route(source: Source, rooms: Vec<RoomKey>) -> PlannedRoute {
        PlannedRoute {
            source,
            rooms,
            port: 8080,
        }
    }

    #[test]
    fn a_single_room_route_targets_that_room_directly() {
        let planned = route(Source::Default, vec![RoomKey::new("RINCON_1", "Küche")]);
        let session = session_for(&planned);

        assert_eq!(session.room_name, "Küche");
        assert_eq!(
            session.target,
            SessionTarget::Room {
                uuid: "RINCON_1".into()
            }
        );
        assert!(!session.is_aggregate());
    }

    #[test]
    fn a_multi_room_route_becomes_a_group_led_by_the_first_room() {
        let planned = route(
            Source::Default,
            vec![
                RoomKey::new("RINCON_1", "Wohnzimmer"),
                RoomKey::new("RINCON_2", "Bad"),
            ],
        );
        let session = session_for(&planned);

        // Grouping, not parallel streams: independent buffering drifts.
        assert!(session.is_aggregate());
        assert_eq!(session.room_uuid, "RINCON_1");
        assert_eq!(
            session.target,
            SessionTarget::AllRooms {
                uuids: vec!["RINCON_1".into(), "RINCON_2".into()]
            }
        );
    }

    #[test]
    fn every_route_gets_a_distinct_session_identity() {
        let a = session_for(&PlannedRoute {
            source: Source::Process("spotify.exe".into()),
            rooms: vec![RoomKey::new("RINCON_1", "Küche")],
            port: 8080,
        });
        let b = session_for(&PlannedRoute {
            source: Source::ExceptProcess("spotify.exe".into()),
            rooms: vec![RoomKey::new("RINCON_2", "Bad")],
            port: 8081,
        });

        // Colliding arbiter ids would make one route's audio release the
        // other's rooms.
        assert_ne!(a.arbiter_id(), b.arbiter_id());
        assert_ne!(a.device_id, b.device_id);
    }

    #[test]
    fn a_process_route_switches_capture_to_process_loopback() {
        let planned = route(
            Source::Process("spotify.exe".into()),
            vec![RoomKey::new("RINCON_1", "Küche")],
        );
        let capture = capture_for(&planned, &Config::default());

        assert_eq!(capture.process.as_deref(), Some("spotify.exe"));
        assert!(!capture.exclude_process);
        assert!(capture.device.is_none());
    }

    #[test]
    fn an_except_route_inverts_the_process_filter() {
        let planned = route(
            Source::ExceptProcess("teams.exe".into()),
            vec![RoomKey::new("RINCON_1", "Küche")],
        );
        let capture = capture_for(&planned, &Config::default());

        assert_eq!(capture.process.as_deref(), Some("teams.exe"));
        assert!(capture.exclude_process);
    }

    #[test]
    fn a_stale_selector_in_the_config_cannot_override_a_route() {
        let mut template = Config::default();
        template.capture.device = Some("Some Old Endpoint".into());
        template.capture.process = Some("leftover.exe".into());
        template.capture.exclude_process = true;

        let planned = route(Source::Default, vec![RoomKey::new("RINCON_1", "Küche")]);
        let capture = capture_for(&planned, &template);

        // The route is the single source of truth; merging would silently
        // stream the wrong audio.
        assert!(capture.device.is_none());
        assert!(capture.process.is_none());
        assert!(!capture.exclude_process);
    }

    #[test]
    fn a_device_route_pins_the_endpoint_without_process_loopback() {
        let planned = route(
            Source::Device("Realtek".into()),
            vec![RoomKey::new("RINCON_1", "Küche")],
        );
        let capture = capture_for(&planned, &Config::default());

        assert_eq!(capture.device.as_deref(), Some("Realtek"));
        assert!(capture.process.is_none());
    }
}
