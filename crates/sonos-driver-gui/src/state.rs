//! The pure application state: room list, discovery + bridge status, the
//! settings form and the latest telemetry.
//!
//! Nothing here performs I/O or draws pixels. The rendering shell holds one
//! [`AppState`], mutates it only through [`reduce`](crate::action::reduce), and
//! reads it each frame to paint the UI.

use std::path::PathBuf;

use audio_core::Config;
use sonos_driver_core::control::SonosDevice;
use sonos_driver_core::ControllableLatency;

use crate::config_form::SettingsForm;
use crate::config_io::default_config_path;
use crate::metrics::MetricsSnapshot;

/// A discovered Sonos room, in a form decoupled from the control crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    /// Stable device `UUID`.
    pub uuid: String,
    /// User-facing room/zone name.
    pub name: String,
    /// Marketing model name.
    pub model: String,
    /// `IPv4` address, formatted for display.
    pub ip: String,
}

impl Room {
    /// Project a discovered [`SonosDevice`] into a UI [`Room`].
    #[must_use]
    pub fn from_device(device: &SonosDevice) -> Self {
        Self {
            uuid: device.uuid.clone(),
            name: device.room_name.clone(),
            model: device.model_name.clone(),
            ip: device.ip.to_string(),
        }
    }

    /// A combined, human-readable label, e.g. `Living Room (Sonos One)`.
    #[must_use]
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.model)
        }
    }
}

/// An ordered list of discovered rooms with a sticky selection.
#[derive(Debug, Clone, Default)]
pub struct RoomList {
    rooms: Vec<Room>,
    selected: Option<usize>,
}

impl RoomList {
    /// An empty list with no selection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the rooms, preserving the selection by `UUID` when possible and
    /// otherwise selecting the first room (or nothing, when empty).
    pub fn replace(&mut self, rooms: Vec<Room>) {
        let previous = self.selected_room().map(|room| room.uuid.clone());
        self.rooms = rooms;
        self.selected = previous
            .and_then(|uuid| self.rooms.iter().position(|room| room.uuid == uuid))
            .or(if self.rooms.is_empty() { None } else { Some(0) });
    }

    /// Select by index. Returns `false` when the index is out of bounds.
    pub fn select(&mut self, index: usize) -> bool {
        if index < self.rooms.len() {
            self.selected = Some(index);
            true
        } else {
            false
        }
    }

    /// The selected index, if any.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// The selected room, if any.
    #[must_use]
    pub fn selected_room(&self) -> Option<&Room> {
        self.selected.and_then(|index| self.rooms.get(index))
    }

    /// The rooms, in presentation order.
    #[must_use]
    pub fn rooms(&self) -> &[Room] {
        &self.rooms
    }

    /// The number of rooms.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }
}

/// The state of background Sonos discovery.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Discovery {
    /// No discovery in flight.
    #[default]
    Idle,
    /// A discovery search is running.
    Running,
    /// The last discovery failed, with a human-readable reason.
    Failed(String),
}

impl Discovery {
    /// Whether a discovery search is currently running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// A Windows audio **render** endpoint offered in the capture-device picker.
///
/// This is a render-agnostic mirror of `audio_capture::EndpointInfo`, kept in the
/// headless library so the picker state stays unit-testable without WASAPI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDevice {
    /// Stable WASAPI endpoint id (the exact, unambiguous selector).
    pub id: String,
    /// Human-friendly endpoint name, e.g. `Speakers (Realtek(R) Audio)`.
    pub name: String,
    /// Whether this is the current default render endpoint.
    pub is_default: bool,
}

impl CaptureDevice {
    /// Construct a capture-device descriptor.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, is_default: bool) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            is_default,
        }
    }

    /// A display label that flags the default endpoint.
    #[must_use]
    pub fn label(&self) -> String {
        if self.is_default {
            format!("{} (default)", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The state of background capture-device enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceScan {
    /// No enumeration in flight.
    #[default]
    Idle,
    /// An enumeration is running.
    Running,
    /// The last enumeration failed, with a human-readable reason.
    Failed(String),
}

impl DeviceScan {
    /// Whether an enumeration is currently running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// The lifecycle state of the audio bridge.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BridgeStatus {
    /// Not streaming.
    #[default]
    Stopped,
    /// Target resolution and capture start-up are in progress.
    Starting,
    /// The bridge is streaming audio to the player.
    Running,
    /// A stop has been requested and teardown is in progress.
    Stopping,
    /// The session ended with an error, carrying the reason.
    Failed(String),
}

impl BridgeStatus {
    /// Whether a session is currently starting, running or stopping.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    /// A short, fixed status label for display.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Stopping => "Stopping",
            Self::Failed(_) => "Failed",
        }
    }
}

/// The complete, render-agnostic application state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Discovered rooms and the current selection.
    pub rooms: RoomList,
    /// Background discovery status.
    pub discovery: Discovery,
    /// Available capture (render) endpoints for the device picker.
    pub devices: Vec<CaptureDevice>,
    /// Background capture-device enumeration status.
    pub device_scan: DeviceScan,
    /// Audio bridge lifecycle status.
    pub bridge: BridgeStatus,
    /// The editable settings form.
    pub form: SettingsForm,
    /// The configuration backing the most recent start request.
    pub active_config: Config,
    /// Live count of captured audio blocks for the running session.
    pub live_captured: u64,
    /// Authoritative metrics from the most recently ended session.
    pub last_metrics: Option<MetricsSnapshot>,
    /// Controllable-latency snapshot from the most recently ended session.
    pub last_latency: Option<ControllableLatency>,
    /// A one-line status message for the UI footer.
    pub status_line: String,
    /// Where the configuration is loaded from and saved to.
    pub config_path: PathBuf,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(default_config_path(), Config::default())
    }
}

impl AppState {
    /// Build state for `config`, persisted at `config_path`.
    #[must_use]
    pub fn new(config_path: PathBuf, config: Config) -> Self {
        Self {
            rooms: RoomList::new(),
            discovery: Discovery::Idle,
            devices: Vec::new(),
            device_scan: DeviceScan::Idle,
            bridge: BridgeStatus::Stopped,
            form: SettingsForm::from_config(&config),
            active_config: config,
            live_captured: 0,
            last_metrics: None,
            last_latency: None,
            status_line: "Idle".to_owned(),
            config_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BridgeStatus, CaptureDevice, DeviceScan, Discovery, Room, RoomList};
    use sonos_driver_core::control::SonosDevice;
    use std::net::Ipv4Addr;

    fn device(uuid: &str, room: &str) -> SonosDevice {
        SonosDevice {
            uuid: uuid.to_owned(),
            room_name: room.to_owned(),
            model_name: "Sonos One".to_owned(),
            ip: Ipv4Addr::new(192, 168, 1, 20),
        }
    }

    #[test]
    fn from_device_projects_all_fields() {
        let room = Room::from_device(&device("RINCON_1", "Living Room"));
        assert_eq!(room.uuid, "RINCON_1");
        assert_eq!(room.name, "Living Room");
        assert_eq!(room.label(), "Living Room (Sonos One)");
        assert_eq!(room.ip, "192.168.1.20");
    }

    #[test]
    fn replace_selects_first_room_by_default() {
        let mut list = RoomList::new();
        assert!(list.is_empty());
        list.replace(vec![
            Room::from_device(&device("a", "A")),
            Room::from_device(&device("b", "B")),
        ]);
        assert_eq!(list.len(), 2);
        assert_eq!(list.selected_index(), Some(0));
        assert_eq!(
            list.selected_room().map(|room| room.uuid.as_str()),
            Some("a")
        );
    }

    #[test]
    fn replace_preserves_selection_by_uuid() {
        let mut list = RoomList::new();
        list.replace(vec![
            Room::from_device(&device("a", "A")),
            Room::from_device(&device("b", "B")),
        ]);
        assert!(list.select(1));
        // Re-discovery returns the rooms in a different order; the "b" selection
        // must follow the device, not the index.
        list.replace(vec![
            Room::from_device(&device("c", "C")),
            Room::from_device(&device("b", "B")),
        ]);
        assert_eq!(
            list.selected_room().map(|room| room.uuid.as_str()),
            Some("b")
        );
    }

    #[test]
    fn select_rejects_out_of_bounds() {
        let mut list = RoomList::new();
        list.replace(vec![Room::from_device(&device("a", "A"))]);
        assert!(!list.select(5));
        assert_eq!(list.selected_index(), Some(0));
    }

    #[test]
    fn empty_replace_clears_selection() {
        let mut list = RoomList::new();
        list.replace(vec![Room::from_device(&device("a", "A"))]);
        list.replace(Vec::new());
        assert!(list.is_empty());
        assert_eq!(list.selected_index(), None);
        assert!(list.selected_room().is_none());
    }

    #[test]
    fn status_predicates_are_correct() {
        assert!(!BridgeStatus::Stopped.is_active());
        assert!(BridgeStatus::Starting.is_active());
        assert!(BridgeStatus::Running.is_active());
        assert!(BridgeStatus::Stopping.is_active());
        assert!(!BridgeStatus::Failed("boom".to_owned()).is_active());
        assert_eq!(BridgeStatus::Running.label(), "Running");
        assert!(Discovery::Running.is_running());
        assert!(!Discovery::Idle.is_running());
    }

    #[test]
    fn capture_device_label_flags_the_default() {
        let normal = CaptureDevice::new("id-a", "Speakers (Realtek)", false);
        assert_eq!(normal.label(), "Speakers (Realtek)");
        let default = CaptureDevice::new("id-b", "Headphones (USB)", true);
        assert_eq!(default.label(), "Headphones (USB) (default)");
    }

    #[test]
    fn device_scan_running_predicate() {
        assert!(DeviceScan::Running.is_running());
        assert!(!DeviceScan::Idle.is_running());
        assert!(!DeviceScan::Failed("nope".to_owned()).is_running());
    }
}
