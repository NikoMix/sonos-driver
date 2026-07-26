//! The work the service actually does, independent of how it was started.
//!
//! Kept separate from the Service Control Manager glue so the exact same code
//! path runs under `sonosvad-svc console`, which is the only way to see what
//! the service is doing without reading a log file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use audio_core::Config;
use sonos_driver_core::vad_endpoint::{PlanOptions, SystemEndpoints};
use sonos_driver_core::{discover_rooms, MultiRoomBridge, RoomSession, StopHandle};

/// How long to wait before retrying after a failed startup attempt.
///
/// The service starts at boot, which is exactly when the network stack, DHCP
/// lease and Sonos players are least likely to be ready. Exiting on the first
/// failure would leave the SCM to apply its own restart policy with a much
/// coarser granularity, so the retry lives here instead.
const RETRY_MIN: Duration = Duration::from_secs(5);
/// Ceiling for the retry backoff, so a permanently absent household is retried
/// every minute rather than every few hours.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Everything the worker needs, resolved before the service reports RUNNING.
#[derive(Debug, Clone)]
pub(crate) struct WorkerSettings {
    /// Configuration file to load, if it exists.
    pub config_path: Option<PathBuf>,
    /// Seed host override for discovery.
    pub seed_host: Option<String>,
    /// Discovery timeout.
    pub discovery_timeout: Duration,
    /// Hide the driver's unused endpoints from the Windows sound UI.
    ///
    /// On by default in the service, because the service owns the machine's
    /// sound picker: the driver ships a fixed pool of slots and the user should
    /// see their rooms, not "Sonos Output 7".
    pub hide_spare_endpoints: bool,
    /// Expose the aggregate "All Sonos" output alongside the per-room ones.
    pub all_sonos_endpoint: bool,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            config_path: Some(default_config_path()),
            seed_host: None,
            discovery_timeout: Duration::from_secs(5),
            hide_spare_endpoints: true,
            all_sonos_endpoint: true,
        }
    }
}

/// The machine-wide configuration path.
///
/// `%ProgramData%` rather than a user profile: the service runs as `LocalSystem`
/// and its room bindings are a property of the machine, not of whoever happens
/// to be logged in.
#[must_use]
pub(crate) fn default_config_path() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    base.join("sonos-driver").join("config.toml")
}

/// The machine-wide log directory.
#[must_use]
pub(crate) fn log_dir() -> PathBuf {
    default_config_path()
        .parent()
        .unwrap_or(Path::new(r"C:\ProgramData\sonos-driver"))
        .join("logs")
}

/// Run until `stop` is signalled.
///
/// Infallible by design: startup failures are retried rather than returned,
/// because "the household was not reachable at boot" is a normal transient
/// condition on an auto-start service, not a reason to give up until the next
/// reboot.
pub(crate) fn run(settings: &WorkerSettings, stop: &StopHandle) {
    let mut backoff = RETRY_MIN;

    while !stop.is_stop_requested() {
        match run_once(settings, stop) {
            Ok(()) => {
                if stop.is_stop_requested() {
                    tracing::info!("stop requested; service shutting down");
                    return;
                }
                // A clean return without a stop request means every room bridge
                // ended on its own -- the household changed under us, or Sonos
                // dropped the connections. Re-plan from scratch.
                tracing::warn!("all room bridges ended unexpectedly; re-planning");
                backoff = RETRY_MIN;
            }
            Err(err) => {
                tracing::error!(
                    error = %format!("{err:#}"),
                    retry_in_secs = backoff.as_secs(),
                    "service iteration failed; retrying"
                );
                backoff = (backoff * 2).min(RETRY_MAX);
            }
        }

        if sleep_unless_stopped(stop, backoff) {
            return;
        }
    }
}

/// One full plan-and-stream cycle.
fn run_once(settings: &WorkerSettings, stop: &StopHandle) -> anyhow::Result<()> {
    let mut config = load_config(settings)?;
    let seed = sonos_driver_core::apply_seed_host(settings.seed_host.as_deref(), &mut config);

    tracing::info!(seed = ?seed, "discovering Sonos household");
    let rooms = discover_rooms(seed.as_deref(), settings.discovery_timeout)
        .context("discovering Sonos rooms")?;
    tracing::info!(rooms = rooms.len(), "discovered rooms");

    // A previous run that was killed mid-party left the household grouped.
    // Split it before planning, so the service always starts from the
    // topology the user configured rather than inheriting one it made.
    recover_stale_group(seed.as_deref(), settings.discovery_timeout);

    let mut supervisor = MultiRoomBridge::new(config);
    // Adopt the caller's stop flag so a SCM stop reaches the bridges.
    supervisor.adopt_stop_handle(stop);

    let sessions = supervisor
        .plan_with(
            &SystemEndpoints::new(),
            &rooms,
            PlanOptions {
                hide_spares: settings.hide_spare_endpoints,
                aggregate: settings.all_sonos_endpoint,
            },
        )
        .context("planning per-room sessions")?;

    if sessions.is_empty() {
        anyhow::bail!("no rooms could be bound to sonosvad endpoints");
    }
    log_plan(&sessions);

    supervisor
        .run_sessions(&sessions)
        .context("running the multi-room bridge")?;
    Ok(())
}

/// Undo an "All Sonos" group left behind by a run that never got to stop
/// cleanly.
///
/// Best-effort by design: failing to split a stale group is a reason to log and
/// carry on, not a reason to refuse to stream. The aggregate output re-forms
/// the party by itself the moment anything plays to it.
fn recover_stale_group(seed: Option<&str>, timeout: Duration) {
    let journal = sonos_driver_core::party::default_journal_path();
    #[allow(clippy::cast_possible_truncation)]
    let secs = timeout.as_secs().min(u64::from(u32::MAX)) as u32;
    match sonos_driver_core::disband(&journal, seed, secs) {
        Ok(0) => {}
        Ok(released) => {
            tracing::info!(released, "split rooms left grouped by a previous run");
        }
        Err(error) => {
            tracing::warn!(%error, "could not split rooms left grouped by a previous run");
        }
    }
}

fn load_config(settings: &WorkerSettings) -> anyhow::Result<Config> {
    let Some(path) = settings.config_path.as_deref() else {
        return Ok(Config::default());
    };
    if !path.exists() {
        tracing::info!(path = %path.display(), "no config file; using defaults");
        return Ok(Config::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    tracing::info!(path = %path.display(), "loaded configuration");
    Ok(config)
}

fn log_plan(sessions: &[RoomSession]) {
    for session in sessions {
        tracing::info!(
            endpoint = %session.device_name,
            room = %session.room_name,
            port = session.port,
            "bridging"
        );
    }
}

/// Sleep in short slices so a stop request is noticed promptly.
///
/// Returns `true` if the wait was cut short by a stop request. The SCM gives a
/// service a limited window to stop before it is reported as hung, so a plain
/// `sleep(60s)` in the retry path would be enough to blow that budget.
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
