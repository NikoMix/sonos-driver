//! Reporting what the worker is doing, for front ends that have to show it.
//!
//! The service only ever needed logs: nobody watches a service start. An
//! interactive front end is different — [`run`](crate::run) retries forever by
//! design, so without a channel back out the UI would report "streaming" while
//! the worker quietly failed and slept. Every event here exists because a user
//! would otherwise be misled.
//!
//! The sink is a plain [`Sender`], so a dropped receiver (the window closed) is
//! a no-op rather than an error the worker has to handle.

use std::sync::mpsc::Sender;

use sonos_routing::PlannedRoute;

/// One live route, flattened for display.
///
/// Owned strings rather than borrows from the plan: this crosses a thread
/// boundary and outlives the planning that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteReport {
    /// What is being captured, e.g. `System audio` or `App "spotify.exe"`.
    pub source: String,
    /// The rooms it plays in, coordinator first.
    pub rooms: Vec<String>,
    /// TCP port this route's stream server listens on.
    pub port: u16,
    /// Whether the rooms had to be joined into a Sonos group.
    pub grouped: bool,
}

impl RouteReport {
    /// Flatten a planned route into a report.
    #[must_use]
    pub fn from_planned(route: &PlannedRoute) -> Self {
        Self {
            source: route.source.label(),
            rooms: route.rooms.iter().map(|room| room.name.clone()).collect(),
            port: route.port,
            grouped: route.is_group(),
        }
    }

    /// A one-line description, e.g. `System audio -> Küche + Bad`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} -> {}", self.source, self.rooms.join(" + "))
    }
}

/// A lifecycle notification from the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    /// Discovery and planning have begun.
    Discovering,
    /// Planning succeeded and these routes are now streaming.
    Streaming(Vec<RouteReport>),
    /// The attempt failed. The worker will retry; the message says why it
    /// failed so the user can decide whether waiting will help.
    Failed(String),
    /// The worker has stopped and its thread is finishing.
    Stopped,
}

/// Where the worker sends [`WorkerEvent`]s, if anywhere.
///
/// Defaults to discarding them, so the service and `sonos-driver cast` are
/// unaffected by this existing.
#[derive(Debug, Clone, Default)]
pub struct Observer(Option<Sender<WorkerEvent>>);

impl Observer {
    /// Report events to `sender`.
    #[must_use]
    pub const fn new(sender: Sender<WorkerEvent>) -> Self {
        Self(Some(sender))
    }

    /// Discard every event.
    #[must_use]
    pub const fn silent() -> Self {
        Self(None)
    }

    /// Send `event`, ignoring a receiver that has gone away.
    pub(crate) fn emit(&self, event: WorkerEvent) {
        if let Some(sender) = &self.0 {
            let _ = sender.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Observer, RouteReport, WorkerEvent};
    use sonos_routing::{PlannedRoute, RoomKey, Source};

    fn room(uuid: &str, name: &str) -> RoomKey {
        RoomKey::new(uuid, name)
    }

    #[test]
    fn a_report_flattens_a_planned_route() {
        let planned = PlannedRoute {
            source: Source::Default,
            rooms: vec![room("a", "Küche"), room("b", "Bad")],
            port: 8080,
        };
        let report = RouteReport::from_planned(&planned);
        assert_eq!(report.label(), "System audio -> Küche + Bad");
        assert_eq!(report.port, 8080);
        assert!(report.grouped);
    }

    #[test]
    fn a_single_room_route_is_not_grouped() {
        let planned = PlannedRoute {
            source: Source::Process("spotify.exe".to_owned()),
            rooms: vec![room("a", "Küche")],
            port: 8081,
        };
        let report = RouteReport::from_planned(&planned);
        assert!(!report.grouped);
        assert_eq!(report.rooms, ["Küche"]);
    }

    #[test]
    fn a_silent_observer_discards_events() {
        // The service path: nothing is listening and nothing may fail.
        Observer::silent().emit(WorkerEvent::Discovering);
    }

    #[test]
    fn events_reach_the_receiver_in_order() {
        let (tx, rx) = std::sync::mpsc::channel();
        let observer = Observer::new(tx);
        observer.emit(WorkerEvent::Discovering);
        observer.emit(WorkerEvent::Failed("no rooms".to_owned()));
        assert_eq!(rx.recv().expect("first"), WorkerEvent::Discovering);
        assert_eq!(
            rx.recv().expect("second"),
            WorkerEvent::Failed("no rooms".to_owned())
        );
    }

    #[test]
    fn emitting_after_the_receiver_is_dropped_is_not_an_error() {
        // The window closed while the worker was still winding down.
        let (tx, rx) = std::sync::mpsc::channel();
        let observer = Observer::new(tx);
        drop(rx);
        observer.emit(WorkerEvent::Stopped);
    }
}
