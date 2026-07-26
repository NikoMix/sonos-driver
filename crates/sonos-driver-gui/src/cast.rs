//! Running the multi-route streaming worker without ever blocking the UI.
//!
//! The worker is the same code the `sonoscast` service and `sonos-driver cast`
//! run — it plans the routes, forms and splits Sonos groups, arbitrates rooms
//! between routes and retries transient failures. All this supervisor adds is a
//! thread to run it on, a stop handle, and a channel to hear about it.
//!
//! Changing the routing is deliberately a **stop, then start**, not a live
//! edit. Two workers overlapping would both bind the same ports and both try to
//! own the same rooms, and the failure would look like a Sonos problem rather
//! than ours. Waiting for [`WorkerEvent::Stopped`] costs a second and removes
//! the whole class of bug.

use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use audio_core::Config;
use sonos_cast_service::{Observer, WorkerEvent, WorkerSettings};
use sonos_driver_core::StopHandle;
use sonos_routing::Route;

/// Discovery floor: shorter searches rarely collect every responder.
const MIN_DISCOVERY: Duration = Duration::from_secs(3);

/// Owns the streaming worker thread and the UI-facing handles into it.
///
/// Dropping the supervisor requests a stop and joins the worker, so streaming
/// can never outlive the window — a Sonos group left behind by a process that
/// simply vanished has to be cleaned up by hand.
#[derive(Debug)]
pub struct CastSupervisor {
    stop: StopHandle,
    events: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
}

impl CastSupervisor {
    /// Start streaming `routes` with `config`.
    ///
    /// If the operating system refuses to create the thread, the failure is
    /// reported through the normal event channel rather than panicking, so the
    /// UI returns cleanly to idle.
    #[must_use]
    pub fn spawn(config: Config, routes: Vec<Route>, seed: Option<String>) -> Self {
        let (sender, events) = mpsc::channel();
        let stop = StopHandle::new();

        let discovery_timeout =
            Duration::from_secs(u64::from(config.sonos.discovery_timeout_secs)).max(MIN_DISCOVERY);
        let settings = WorkerSettings {
            // Nothing is read from disk: the caller has already validated both
            // halves, and a stale or unreadable file must not be able to
            // countermand what the user just clicked.
            config_path: None,
            config: Some(config),
            routes: Some(routes),
            seed_host: seed,
            discovery_timeout,
            observer: Observer::new(sender.clone()),
        };

        let worker_stop = stop.clone();
        let spawned = thread::Builder::new()
            .name("sonos-cast".to_owned())
            .spawn(move || sonos_cast_service::run(&settings, &worker_stop));

        match spawned {
            Ok(handle) => Self {
                stop,
                events,
                handle: Some(handle),
            },
            Err(error) => {
                tracing::error!(%error, "failed to spawn the streaming thread");
                let _ = sender.send(WorkerEvent::Failed(
                    "could not start the streaming thread".to_owned(),
                ));
                let _ = sender.send(WorkerEvent::Stopped);
                Self {
                    stop,
                    events,
                    handle: None,
                }
            }
        }
    }

    /// Ask the worker to wind down. Returns immediately; the UI learns it
    /// finished from [`WorkerEvent::Stopped`].
    pub fn request_stop(&self) {
        self.stop.stop();
    }

    /// Take the next lifecycle event, if one is waiting.
    #[must_use]
    pub fn try_event(&self) -> Option<WorkerEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for CastSupervisor {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(handle) = self.handle.take() {
            // Blocking here is the point: the worker hands the rooms back and
            // splits any group it formed on the way out.
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CastSupervisor;
    use audio_core::Config;
    use sonos_cast_service::WorkerEvent;

    #[test]
    fn a_supervisor_with_no_routes_reports_a_failure_and_keeps_running() {
        // No network is touched: the worker rejects an empty route list before
        // it discovers anything, which is exactly the guard being tested.
        let supervisor = CastSupervisor::spawn(Config::default(), Vec::new(), None);

        let mut failure = None;
        for _ in 0..200 {
            if let Some(WorkerEvent::Failed(reason)) = supervisor.try_event() {
                failure = Some(reason);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let reason = failure.expect("the worker must report why it cannot start");
        assert!(reason.contains("no routes"), "{reason}");
    }

    #[test]
    fn dropping_a_supervisor_stops_and_joins_the_worker() {
        let supervisor = CastSupervisor::spawn(Config::default(), Vec::new(), None);
        supervisor.request_stop();
        // Drop joins; the test simply must not hang or panic.
        drop(supervisor);
    }
}
