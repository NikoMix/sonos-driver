//! The pure action/effect reducer at the heart of the UI.
//!
//! [`reduce`] is the *only* way [`AppState`] changes. It takes an [`Action`]
//! (originating from a click, a tray command, a background thread or a timer),
//! mutates the state, and returns a list of [`Effect`]s for the shell to carry
//! out (spawn a thread, start/stop the bridge, persist the config, drive the
//! window). Keeping this logic free of I/O makes every state transition
//! unit-testable without a display or a network.

use std::path::PathBuf;
use std::time::Duration;

use audio_core::Config;

use crate::bridge::SessionOutcome;
use crate::state::{AppState, BridgeStatus, CaptureDevice, DeviceScan, Discovery, Room};

/// Discovery floor: shorter searches rarely collect any responders.
const MIN_DISCOVERY: Duration = Duration::from_secs(3);

/// An input to the reducer.
#[derive(Debug, Clone)]
pub enum Action {
    /// Begin background Sonos discovery.
    StartDiscovery,
    /// Discovery completed with the given rooms.
    RoomsDiscovered(Vec<Room>),
    /// Discovery failed with a human-readable reason.
    DiscoveryFailed(String),
    /// Select the room at the given index.
    SelectRoom(usize),
    /// Enumerate the available capture (render) endpoints.
    RefreshDevices,
    /// Capture-device enumeration completed with the given endpoints.
    DevicesEnumerated(Vec<CaptureDevice>),
    /// Capture-device enumeration failed with a human-readable reason.
    DeviceScanFailed(String),
    /// Choose the capture device: `None` selects the system default.
    SelectDevice(Option<usize>),
    /// Start streaming to the configured target.
    StartBridge,
    /// Request that streaming stop.
    StopBridge,
    /// The bridge reported that streaming has begun.
    BridgeRunning,
    /// The bridge session ended (cleanly or with an error).
    BridgeEnded(SessionOutcome),
    /// A fresh live count of captured audio blocks.
    LiveCaptured(u64),
    /// Persist the current settings form to disk.
    SaveConfig,
    /// The configuration was saved successfully.
    ConfigSaved,
    /// Saving the configuration failed.
    ConfigSaveFailed(String),
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
    /// Spawn background discovery with the given timeout.
    SpawnDiscovery {
        /// How long to wait for `SSDP` responses.
        timeout: Duration,
    },
    /// Enumerate the available capture (render) endpoints off the UI thread.
    EnumerateDevices,
    /// Start the bridge worker for the given configuration.
    StartBridge {
        /// The fully validated configuration to stream with.
        config: Box<Config>,
    },
    /// Request that the running bridge worker stop.
    StopBridge,
    /// Persist `config` to `path`.
    SaveConfig {
        /// Destination file.
        path: PathBuf,
        /// The configuration to write.
        config: Box<Config>,
    },
    /// Reveal the main window.
    ShowWindow,
    /// Hide the main window to the tray.
    HideWindow,
    /// Quit the application.
    Quit,
}

/// Advance `state` by `action`, returning the effects the shell must perform.
#[must_use]
pub fn reduce(state: &mut AppState, action: Action) -> Vec<Effect> {
    match action {
        Action::StartDiscovery => start_discovery(state),
        Action::RoomsDiscovered(rooms) => {
            rooms_discovered(state, rooms);
            Vec::new()
        }
        Action::DiscoveryFailed(reason) => {
            state.discovery = Discovery::Failed(reason.clone());
            state.status_line = format!("Discovery failed: {reason}");
            Vec::new()
        }
        Action::SelectRoom(index) => {
            select_room(state, index);
            Vec::new()
        }
        Action::RefreshDevices => refresh_devices(state),
        Action::DevicesEnumerated(devices) => {
            devices_enumerated(state, devices);
            Vec::new()
        }
        Action::DeviceScanFailed(reason) => {
            state.device_scan = DeviceScan::Failed(reason.clone());
            state.status_line = format!("Device scan failed: {reason}");
            Vec::new()
        }
        Action::SelectDevice(index) => {
            select_device(state, index);
            Vec::new()
        }
        Action::StartBridge => start_bridge(state),
        Action::StopBridge => stop_bridge(state),
        Action::BridgeRunning => {
            bridge_running(state);
            Vec::new()
        }
        Action::BridgeEnded(outcome) => {
            bridge_ended(state, outcome);
            Vec::new()
        }
        Action::LiveCaptured(count) => {
            if state.bridge.is_active() {
                state.live_captured = count;
            }
            Vec::new()
        }
        Action::SaveConfig => save_config(state),
        Action::ConfigSaved => {
            "Settings saved".clone_into(&mut state.status_line);
            Vec::new()
        }
        Action::ConfigSaveFailed(reason) => {
            state.status_line = format!("Save failed: {reason}");
            Vec::new()
        }
        Action::ShowWindow => vec![Effect::ShowWindow],
        Action::HideWindow => vec![Effect::HideWindow],
        Action::Quit => quit(state),
    }
}

/// Begin discovery unless a search is already running.
fn start_discovery(state: &mut AppState) -> Vec<Effect> {
    if state.discovery.is_running() {
        return Vec::new();
    }
    state.discovery = Discovery::Running;
    "Discovering Sonos rooms".clone_into(&mut state.status_line);
    vec![Effect::SpawnDiscovery {
        timeout: discovery_timeout(state),
    }]
}

/// Record discovered rooms and clear the running flag.
fn rooms_discovered(state: &mut AppState, rooms: Vec<Room>) {
    let count = rooms.len();
    state.rooms.replace(rooms);
    state.discovery = Discovery::Idle;
    state.status_line = match count {
        0 => "No Sonos rooms found".to_owned(),
        1 => "Found 1 Sonos room".to_owned(),
        n => format!("Found {n} Sonos rooms"),
    };
}

/// Select a room and copy its identity into the settings form.
fn select_room(state: &mut AppState, index: usize) {
    if !state.rooms.select(index) {
        return;
    }
    if let Some(room) = state.rooms.selected_room() {
        state.form.room = room.name.clone();
        state.form.uuid = room.uuid.clone();
        state.status_line = format!("Target: {}", room.label());
    }
}

/// Begin capture-device enumeration unless one is already running.
fn refresh_devices(state: &mut AppState) -> Vec<Effect> {
    if state.device_scan.is_running() {
        return Vec::new();
    }
    state.device_scan = DeviceScan::Running;
    "Listing capture devices".clone_into(&mut state.status_line);
    vec![Effect::EnumerateDevices]
}

/// Record the enumerated capture devices and clear the running flag.
fn devices_enumerated(state: &mut AppState, devices: Vec<CaptureDevice>) {
    let count = devices.len();
    state.devices = devices;
    state.device_scan = DeviceScan::Idle;
    state.status_line = match count {
        0 => "No capture devices found".to_owned(),
        1 => "Found 1 capture device".to_owned(),
        n => format!("Found {n} capture devices"),
    };
}

/// Choose a capture device by index, writing its id into the form; `None`
/// selects the system default by clearing the device selector.
fn select_device(state: &mut AppState, index: Option<usize>) {
    if let Some(device) = index.and_then(|i| state.devices.get(i)) {
        device.id.clone_into(&mut state.form.device);
        state.status_line = format!("Capture device: {}", device.name);
    } else {
        state.form.device.clear();
        "Capture device: system default".clone_into(&mut state.status_line);
    }
}

/// Validate the form and start the bridge, or record why it cannot start.
fn start_bridge(state: &mut AppState) -> Vec<Effect> {
    if state.bridge.is_active() {
        return Vec::new();
    }
    if !state.form.has_target() {
        let reason = "no Sonos target: discover and select a room first".to_owned();
        state.bridge = BridgeStatus::Failed(reason.clone());
        state.status_line = reason;
        return Vec::new();
    }
    match state.form.to_config() {
        Ok(config) => {
            state.active_config = config.clone();
            state.bridge = BridgeStatus::Starting;
            state.live_captured = 0;
            "Starting bridge".clone_into(&mut state.status_line);
            vec![Effect::StartBridge {
                config: Box::new(config),
            }]
        }
        Err(err) => {
            let reason = err.to_string();
            state.bridge = BridgeStatus::Failed(reason.clone());
            state.status_line = format!("Invalid settings: {reason}");
            Vec::new()
        }
    }
}

/// Request a stop if a session is active.
fn stop_bridge(state: &mut AppState) -> Vec<Effect> {
    if !state.bridge.is_active() {
        return Vec::new();
    }
    state.bridge = BridgeStatus::Stopping;
    "Stopping bridge".clone_into(&mut state.status_line);
    vec![Effect::StopBridge]
}

/// Transition a starting session into the running state.
fn bridge_running(state: &mut AppState) {
    if matches!(
        state.bridge,
        BridgeStatus::Starting | BridgeStatus::Stopping
    ) {
        state.bridge = BridgeStatus::Running;
        "Streaming to Sonos".clone_into(&mut state.status_line);
    }
}

/// Record the outcome of a finished session.
fn bridge_ended(state: &mut AppState, outcome: SessionOutcome) {
    state.live_captured = outcome.metrics.captured;
    state.last_metrics = Some(outcome.metrics);
    state.last_latency = Some(outcome.latency);
    if let Some(reason) = outcome.error {
        state.bridge = BridgeStatus::Failed(reason.clone());
        state.status_line = format!("Bridge stopped: {reason}");
    } else {
        state.bridge = BridgeStatus::Stopped;
        "Bridge stopped".clone_into(&mut state.status_line);
    }
}

/// Validate the form and persist it, or record the validation error.
fn save_config(state: &mut AppState) -> Vec<Effect> {
    match state.form.to_config() {
        Ok(config) => {
            state.active_config = config.clone();
            vec![Effect::SaveConfig {
                path: state.config_path.clone(),
                config: Box::new(config),
            }]
        }
        Err(err) => {
            state.status_line = format!("Cannot save invalid settings: {err}");
            Vec::new()
        }
    }
}

/// Stop any active session and quit.
fn quit(state: &mut AppState) -> Vec<Effect> {
    let mut effects = Vec::new();
    if state.bridge.is_active() {
        state.bridge = BridgeStatus::Stopping;
        effects.push(Effect::StopBridge);
    }
    effects.push(Effect::Quit);
    effects
}

/// The discovery timeout from the form, floored at [`MIN_DISCOVERY`].
fn discovery_timeout(state: &AppState) -> Duration {
    let secs = state
        .form
        .discovery_timeout_secs
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    Duration::from_secs(secs).max(MIN_DISCOVERY)
}

#[cfg(test)]
mod tests {
    use super::{reduce, Action, Effect};
    use crate::bridge::SessionOutcome;
    use crate::metrics::MetricsSnapshot;
    use crate::state::{AppState, BridgeStatus, CaptureDevice, DeviceScan, Discovery, Room};
    use sonos_driver_core::control::SonosDevice;
    use sonos_driver_core::ControllableLatency;
    use std::net::Ipv4Addr;

    fn device(id: &str, name: &str, is_default: bool) -> CaptureDevice {
        CaptureDevice::new(id, name, is_default)
    }

    fn room(uuid: &str, name: &str) -> Room {
        Room::from_device(&SonosDevice {
            uuid: uuid.to_owned(),
            room_name: name.to_owned(),
            model_name: "Model".to_owned(),
            ip: Ipv4Addr::LOCALHOST,
        })
    }

    /// Apply an action for its state transition, discarding the effects.
    fn dispatch(state: &mut AppState, action: Action) {
        let _ = reduce(state, action);
    }

    #[test]
    fn start_discovery_runs_once() {
        let mut state = AppState::default();
        let effects = reduce(&mut state, Action::StartDiscovery);
        assert!(state.discovery.is_running());
        assert!(matches!(
            effects.as_slice(),
            [Effect::SpawnDiscovery { .. }]
        ));
        // A second request while running must be a no-op.
        let again = reduce(&mut state, Action::StartDiscovery);
        assert!(again.is_empty());
    }

    #[test]
    fn rooms_discovered_populates_and_clears_running() {
        let mut state = AppState::default();
        dispatch(&mut state, Action::StartDiscovery);
        let effects = reduce(
            &mut state,
            Action::RoomsDiscovered(vec![room("a", "A"), room("b", "B")]),
        );
        assert!(effects.is_empty());
        assert_eq!(state.discovery, Discovery::Idle);
        assert_eq!(state.rooms.len(), 2);
    }

    #[test]
    fn discovery_failed_is_recorded() {
        let mut state = AppState::default();
        dispatch(
            &mut state,
            Action::DiscoveryFailed("socket error".to_owned()),
        );
        assert!(matches!(state.discovery, Discovery::Failed(_)));
        assert!(state.status_line.contains("socket error"));
    }

    #[test]
    fn select_room_copies_target_into_form() {
        let mut state = AppState::default();
        dispatch(
            &mut state,
            Action::RoomsDiscovered(vec![room("a", "A"), room("rincon-b", "Bedroom")]),
        );
        dispatch(&mut state, Action::SelectRoom(1));
        assert_eq!(state.form.room, "Bedroom");
        assert_eq!(state.form.uuid, "rincon-b");
        assert!(state.form.has_target());
    }

    #[test]
    fn start_bridge_without_target_fails_with_no_effect() {
        let mut state = AppState::default();
        let effects = reduce(&mut state, Action::StartBridge);
        assert!(effects.is_empty());
        assert!(matches!(state.bridge, BridgeStatus::Failed(_)));
    }

    #[test]
    fn start_bridge_with_target_emits_start_effect() {
        let mut state = AppState::default();
        dispatch(&mut state, Action::RoomsDiscovered(vec![room("a", "Den")]));
        dispatch(&mut state, Action::SelectRoom(0));
        let effects = reduce(&mut state, Action::StartBridge);
        assert_eq!(state.bridge, BridgeStatus::Starting);
        assert!(matches!(effects.as_slice(), [Effect::StartBridge { .. }]));
    }

    #[test]
    fn bridge_running_transitions_from_starting() {
        let mut state = AppState {
            bridge: BridgeStatus::Starting,
            ..AppState::default()
        };
        dispatch(&mut state, Action::BridgeRunning);
        assert_eq!(state.bridge, BridgeStatus::Running);
    }

    #[test]
    fn bridge_ended_with_error_records_failure_and_metrics() {
        let mut state = AppState {
            bridge: BridgeStatus::Running,
            ..AppState::default()
        };
        let outcome = SessionOutcome {
            metrics: MetricsSnapshot {
                captured: 5,
                streamed: 4,
                underruns: 1,
                reconnects: 0,
            },
            latency: ControllableLatency::default(),
            error: Some("device vanished".to_owned()),
        };
        dispatch(&mut state, Action::BridgeEnded(outcome));
        assert!(matches!(state.bridge, BridgeStatus::Failed(_)));
        assert_eq!(state.last_metrics.expect("metrics").captured, 5);
        assert_eq!(state.live_captured, 5);
    }

    #[test]
    fn bridge_ended_cleanly_returns_to_stopped() {
        let mut state = AppState {
            bridge: BridgeStatus::Stopping,
            ..AppState::default()
        };
        let outcome = SessionOutcome {
            metrics: MetricsSnapshot::default(),
            latency: ControllableLatency::default(),
            error: None,
        };
        dispatch(&mut state, Action::BridgeEnded(outcome));
        assert_eq!(state.bridge, BridgeStatus::Stopped);
    }

    #[test]
    fn stop_bridge_when_inactive_is_a_no_op() {
        let mut state = AppState::default();
        assert!(reduce(&mut state, Action::StopBridge).is_empty());
        assert_eq!(state.bridge, BridgeStatus::Stopped);
    }

    #[test]
    fn live_captured_updates_only_while_active() {
        let mut state = AppState::default();
        dispatch(&mut state, Action::LiveCaptured(42));
        assert_eq!(state.live_captured, 0);
        state.bridge = BridgeStatus::Running;
        dispatch(&mut state, Action::LiveCaptured(42));
        assert_eq!(state.live_captured, 42);
    }

    #[test]
    fn save_config_emits_effect_with_path() {
        let mut state = AppState::default();
        let effects = reduce(&mut state, Action::SaveConfig);
        match effects.as_slice() {
            [Effect::SaveConfig { path, .. }] => assert_eq!(path, &state.config_path),
            other => panic!("expected a single SaveConfig effect, got {other:?}"),
        }
    }

    #[test]
    fn quit_while_active_stops_then_quits() {
        let mut state = AppState {
            bridge: BridgeStatus::Running,
            ..AppState::default()
        };
        let effects = reduce(&mut state, Action::Quit);
        assert!(matches!(
            effects.as_slice(),
            [Effect::StopBridge, Effect::Quit]
        ));
    }

    #[test]
    fn refresh_devices_runs_once() {
        let mut state = AppState::default();
        let effects = reduce(&mut state, Action::RefreshDevices);
        assert!(state.device_scan.is_running());
        assert!(matches!(effects.as_slice(), [Effect::EnumerateDevices]));
        // A second request while running must be a no-op.
        assert!(reduce(&mut state, Action::RefreshDevices).is_empty());
    }

    #[test]
    fn devices_enumerated_populates_and_clears_running() {
        let mut state = AppState::default();
        dispatch(&mut state, Action::RefreshDevices);
        let effects = reduce(
            &mut state,
            Action::DevicesEnumerated(vec![
                device("id-a", "Speakers", true),
                device("id-b", "Headphones", false),
            ]),
        );
        assert!(effects.is_empty());
        assert_eq!(state.device_scan, DeviceScan::Idle);
        assert_eq!(state.devices.len(), 2);
        assert!(state.status_line.contains('2'));
    }

    #[test]
    fn device_scan_failed_is_recorded() {
        let mut state = AppState::default();
        dispatch(&mut state, Action::DeviceScanFailed("COM error".to_owned()));
        assert!(matches!(state.device_scan, DeviceScan::Failed(_)));
        assert!(state.status_line.contains("COM error"));
    }

    #[test]
    fn select_device_writes_id_into_form() {
        let mut state = AppState::default();
        dispatch(
            &mut state,
            Action::DevicesEnumerated(vec![
                device("id-a", "Speakers", true),
                device("id-b", "Headphones", false),
            ]),
        );
        dispatch(&mut state, Action::SelectDevice(Some(1)));
        assert_eq!(state.form.device, "id-b");
        // Selecting the default clears the explicit device selector.
        dispatch(&mut state, Action::SelectDevice(None));
        assert!(state.form.device.is_empty());
    }

    #[test]
    fn select_device_out_of_range_selects_default() {
        let mut state = AppState {
            form: crate::config_form::SettingsForm {
                device: "stale".to_owned(),
                ..crate::config_form::SettingsForm::default()
            },
            ..AppState::default()
        };
        dispatch(&mut state, Action::SelectDevice(Some(9)));
        assert!(state.form.device.is_empty());
    }
}
