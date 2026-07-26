//! The pure application state: the outputs, the rooms, the routing table
//! between them, and what the streaming worker is currently doing.
//!
//! Nothing here performs I/O or draws pixels. The rendering shell holds one
//! [`AppState`], mutates it only through [`reduce`](crate::action::reduce), and
//! reads it each frame to paint the UI.

use std::path::PathBuf;

use audio_core::Config;
use sonos_cast_service::RouteReport;
use sonos_routing::{RoomKey, RouteConfig, Source};

use crate::config_form::SettingsForm;
use crate::config_io::default_config_path;
use crate::routes::{RoutingTable, DEFAULT_OUTPUT_ID};

/// A Windows audio **render** endpoint: one row of the routing table.
///
/// A render-agnostic mirror of `audio_capture::EndpointInfo`, kept in the
/// headless library so the table stays unit-testable without WASAPI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Stable WASAPI endpoint id, or empty for the default-follower row.
    pub id: String,
    /// Human-friendly endpoint name, e.g. `Speakers (Realtek(R) Audio)`.
    pub name: String,
    /// Whether Windows currently plays new audio here by default.
    pub is_default_endpoint: bool,
}

impl Output {
    /// Describe a real render endpoint.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, is_default: bool) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            is_default_endpoint: is_default,
        }
    }

    /// The synthetic row that always tracks whatever Windows calls the default.
    #[must_use]
    pub fn default_follower() -> Self {
        Self {
            id: DEFAULT_OUTPUT_ID.to_owned(),
            name: "Default output".to_owned(),
            is_default_endpoint: false,
        }
    }

    /// Whether this is the synthetic default-follower row.
    #[must_use]
    pub fn is_follower(&self) -> bool {
        self.id == DEFAULT_OUTPUT_ID
    }

    /// What the streaming worker should capture for this row.
    ///
    /// The follower resolves late, so it stays correct when the user changes
    /// the default output in the Windows sound settings while streaming.
    #[must_use]
    pub fn source(&self) -> Source {
        if self.is_follower() {
            Source::Default
        } else {
            Source::Device(self.id.clone())
        }
    }

    /// How this row is written to the portable config file.
    ///
    /// The friendly name rather than the endpoint id: a saved file should be
    /// readable and should survive being copied to another machine, and
    /// `audio-capture` matches names as well as ids.
    #[must_use]
    pub fn config_selector(&self) -> Option<String> {
        (!self.is_follower()).then(|| self.name.clone())
    }

    /// A short explanation shown under the row's name.
    #[must_use]
    pub fn description(&self) -> &'static str {
        if self.is_follower() {
            "Whatever Windows is set to"
        } else if self.is_default_endpoint {
            "Windows default"
        } else {
            ""
        }
    }
}

/// A discovered Sonos room.
///
/// One entry per *room*, not per player: a stereo pair or a bonded surround set
/// is a single destination, which is what the sound picker shows and what the
/// user thinks in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    /// Stable device `UUID` of the room's coordinator.
    pub uuid: String,
    /// User-facing room/zone name.
    pub name: String,
}

impl Room {
    /// Describe a room.
    #[must_use]
    pub fn new(uuid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            name: name.into(),
        }
    }

    /// Project a discovered [`RoomKey`] into a UI [`Room`].
    #[must_use]
    pub fn from_key(key: &RoomKey) -> Self {
        Self::new(key.uuid.clone(), key.name.clone())
    }
}

/// The state of a background scan (discovery or endpoint enumeration).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scan {
    /// Never run.
    #[default]
    Idle,
    /// A scan is in flight.
    Running,
    /// The last scan finished successfully.
    Done,
    /// The last scan failed, with a human-readable reason.
    Failed(String),
}

impl Scan {
    /// Whether a scan is currently running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    /// The failure reason, if the last scan failed.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason.as_str()),
            _ => None,
        }
    }
}

/// What the streaming worker is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StreamStatus {
    /// Nothing is streaming.
    #[default]
    Stopped,
    /// The worker has been asked to start and is discovering/planning.
    Starting,
    /// Routes are live.
    Streaming,
    /// The last attempt failed. The worker retries on its own; the reason is
    /// kept so the UI can say *why* rather than just spinning.
    Retrying(String),
    /// A stop has been requested and teardown is in progress.
    Stopping,
}

impl StreamStatus {
    /// Whether the worker is alive in any form.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Stopped)
    }

    /// A short, fixed status label for display.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Streaming => "Streaming",
            Self::Retrying(_) => "Retrying",
            Self::Stopping => "Stopping",
        }
    }
}

/// The complete, render-agnostic application state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Discovered rooms, in discovery (alphabetical) order.
    pub rooms: Vec<Room>,
    /// Background discovery status.
    pub discovery: Scan,
    /// Background render-endpoint enumeration status.
    pub outputs: Scan,
    /// The output-to-room assignments.
    pub table: RoutingTable,
    /// What the streaming worker is doing.
    pub stream: StreamStatus,
    /// The routes the worker reports as live.
    pub live: Vec<RouteReport>,
    /// Whether the table has been edited since the worker was last started.
    pub unapplied: bool,
    /// The editable audio settings.
    pub settings: SettingsForm,
    /// The routes read from disk, held until the outputs are known.
    ///
    /// A saved route names an output, so it cannot be placed until the OS has
    /// said which outputs exist. Keeping it here rather than applying it early
    /// means a slow enumeration cannot silently discard the user's routing.
    pub saved_routes: Vec<RouteConfig>,
    /// A one-line status message for the UI footer.
    pub status_line: String,
    /// Where the routing table and settings are loaded from and saved to.
    pub config_path: PathBuf,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(default_config_path(), &Config::default())
    }
}

impl AppState {
    /// Build state for `config`, persisted at `config_path`.
    #[must_use]
    pub fn new(config_path: PathBuf, config: &Config) -> Self {
        Self {
            rooms: Vec::new(),
            discovery: Scan::Idle,
            outputs: Scan::Idle,
            table: RoutingTable::new(),
            stream: StreamStatus::Stopped,
            live: Vec::new(),
            unapplied: false,
            settings: SettingsForm::from_config(config),
            saved_routes: Vec::new(),
            status_line: "Looking for outputs and rooms…".to_owned(),
            config_path,
        }
    }

    /// Whether the table can be started: something is routed, and nothing in it
    /// contradicts itself.
    #[must_use]
    pub fn can_start(&self) -> bool {
        !self.table.is_idle() && self.table.conflicts().is_empty()
    }

    /// Room names, for the destination picker.
    #[must_use]
    pub fn room_names(&self) -> Vec<&str> {
        self.rooms.iter().map(|room| room.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, Output, Room, Scan, StreamStatus};
    use crate::routes::Destination;
    use sonos_routing::{RoomKey, Source};

    #[test]
    fn a_real_endpoint_captures_itself_by_id() {
        let output = Output::new("{0.0.0}.abc", "Speakers", true);
        assert!(!output.is_follower());
        assert_eq!(output.source(), Source::Device("{0.0.0}.abc".to_owned()));
        assert_eq!(output.config_selector().as_deref(), Some("Speakers"));
        assert_eq!(output.description(), "Windows default");
    }

    #[test]
    fn the_follower_row_resolves_late() {
        let output = Output::default_follower();
        assert!(output.is_follower());
        assert_eq!(output.source(), Source::Default);
        // No `device` key, so the config file says "the default", not a name
        // that would freeze today's default in place.
        assert_eq!(output.config_selector(), None);
    }

    #[test]
    fn a_room_projects_from_a_discovery_key() {
        let room = Room::from_key(&RoomKey::new("RINCON_1", "Küche"));
        assert_eq!(room, Room::new("RINCON_1", "Küche"));
    }

    #[test]
    fn a_scan_reports_only_its_own_failure() {
        assert_eq!(Scan::Failed("boom".to_owned()).error(), Some("boom"));
        assert_eq!(Scan::Done.error(), None);
        assert!(Scan::Running.is_running());
        assert!(!Scan::Done.is_running());
    }

    #[test]
    fn only_stopped_counts_as_inactive() {
        assert!(!StreamStatus::Stopped.is_active());
        for status in [
            StreamStatus::Starting,
            StreamStatus::Streaming,
            StreamStatus::Retrying("x".to_owned()),
            StreamStatus::Stopping,
        ] {
            assert!(status.is_active(), "{status:?}");
            assert!(!status.label().is_empty());
        }
    }

    #[test]
    fn an_empty_table_cannot_be_started() {
        let state = AppState::default();
        assert!(!state.can_start());
    }

    #[test]
    fn a_conflicting_table_cannot_be_started() {
        let mut state = AppState::default();
        state.table.sync_outputs(vec![
            Output::new("a", "A", true),
            Output::new("b", "B", false),
        ]);
        let kitchen = Destination::Room {
            uuid: "RINCON_1".to_owned(),
            name: "Küche".to_owned(),
        };
        state.table.assign(1, kitchen.clone());
        assert!(state.can_start());
        state.table.assign(2, kitchen);
        assert!(!state.can_start(), "two outputs claiming one room");
    }
}
