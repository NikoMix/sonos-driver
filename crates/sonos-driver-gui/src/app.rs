//! The `eframe`/`egui` rendering shell.
//!
//! [`GuiApp`] owns the [`AppState`] and the live handles the headless core
//! cannot: the bridge worker ([`BridgeSupervisor`]), the system tray, and the
//! channels background threads post [`Action`]s on. Each frame it drains those
//! event sources into the pure [`reduce`] function, executes the returned
//! [`Effect`]s (the only place that performs I/O or drives the window), then
//! paints the state.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use audio_core::Config;
use eframe::egui::{self, Context, ViewportCommand};
use eframe::{App, CreationContext, Frame};
use sonos_driver_gui::config_form::ALL_PROFILES;
use sonos_driver_gui::{
    config_io, discovery, reduce, Action, AppState, BridgeEvent, BridgeStatus, BridgeSupervisor,
    CaptureDevice, DeviceScan, Effect,
};

use crate::icon;
use crate::tray::Tray;

/// How often the background pump nudges a repaint so tray clicks are serviced
/// even when the window is hidden and otherwise idle.
const REPAINT_INTERVAL: Duration = Duration::from_millis(200);

/// The application: render-shell state plus the live worker/tray/channel
/// handles that the pure core deliberately does not own.
pub(crate) struct GuiApp {
    state: AppState,
    actions_tx: Sender<Action>,
    actions_rx: Receiver<Action>,
    supervisor: Option<BridgeSupervisor>,
    tray: Option<Tray>,
    quitting: bool,
}

impl GuiApp {
    /// Construct the app: load the saved config, build the tray, and start the
    /// background repaint pump.
    pub(crate) fn new(cc: &CreationContext<'_>) -> Self {
        spawn_repaint_pump(cc.egui_ctx.clone());

        let tray = Tray::new(icon::tray_icon());
        if tray.is_none() {
            tracing::warn!("continuing without a system tray");
        }

        let (actions_tx, actions_rx) = mpsc::channel();
        let path = config_io::default_config_path();
        // A missing file is the normal first-run case: start from defaults
        // silently. A file that exists but fails to parse is surfaced in the
        // status line so the user notices before a save would overwrite it.
        let (config, load_error) = match config_io::load(&path) {
            Ok(config) => (config, None),
            Err(_) if !path.exists() => (Config::default(), None),
            Err(error) => (Config::default(), Some(error.to_string())),
        };
        let mut state = AppState::new(path, config);
        if let Some(error) = load_error {
            tracing::warn!(%error, "failed to load saved configuration; using defaults");
            state.status_line =
                format!("Failed to load config (using defaults — saving overwrites it): {error}");
        }

        Self {
            state,
            actions_tx,
            actions_rx,
            supervisor: None,
            tray,
            quitting: false,
        }
    }

    /// Feed one action through the reducer and execute the resulting effects.
    fn apply(&mut self, ctx: &Context, action: Action) {
        for effect in reduce(&mut self.state, action) {
            self.run_effect(ctx, effect);
        }
    }

    /// Execute a single side effect.
    fn run_effect(&mut self, ctx: &Context, effect: Effect) {
        match effect {
            Effect::SpawnDiscovery { timeout } => {
                discovery::spawn(timeout, self.actions_tx.clone());
            }
            Effect::StartBridge { config } => {
                self.supervisor = Some(BridgeSupervisor::spawn(*config));
            }
            Effect::StopBridge => {
                if let Some(supervisor) = &self.supervisor {
                    supervisor.request_stop();
                }
            }
            Effect::SaveConfig { path, config } => self.spawn_save(path, *config),
            Effect::EnumerateDevices => self.spawn_enumerate(),
            Effect::ShowWindow => {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            Effect::HideWindow => ctx.send_viewport_cmd(ViewportCommand::Visible(false)),
            Effect::Quit => {
                self.quitting = true;
                // Dropping the supervisor requests a stop and joins the worker.
                self.supervisor = None;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// Persist `config` to `path` on a background thread.
    fn spawn_save(&self, path: PathBuf, config: Config) {
        let actions = self.actions_tx.clone();
        let spawned = thread::Builder::new()
            .name("config-save".to_owned())
            .spawn(move || {
                let action = match config_io::save(&path, &config) {
                    Ok(()) => Action::ConfigSaved,
                    Err(err) => Action::ConfigSaveFailed(err.to_string()),
                };
                let _ = actions.send(action);
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "failed to spawn the config-save thread");
        }
    }

    /// Enumerate render endpoints on a background thread. The enumeration
    /// initialises its own COM apartment, so it is never run on the UI thread.
    fn spawn_enumerate(&self) {
        let actions = self.actions_tx.clone();
        let spawned = thread::Builder::new()
            .name("device-enumerate".to_owned())
            .spawn(move || {
                let action = match audio_capture::enumerate_render_endpoints() {
                    Ok(endpoints) => {
                        let devices = endpoints
                            .into_iter()
                            .map(|endpoint| {
                                CaptureDevice::new(endpoint.id, endpoint.name, endpoint.is_default)
                            })
                            .collect();
                        Action::DevicesEnumerated(devices)
                    }
                    Err(err) => Action::DeviceScanFailed(err.to_string()),
                };
                let _ = actions.send(action);
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "failed to spawn the device-enumerate thread");
        }
    }

    /// Drain tray/menu interactions into actions.
    fn pump_tray(&mut self, ctx: &Context) {
        let mut actions = Vec::new();
        if let Some(tray) = &self.tray {
            tray.collect_actions(&mut actions);
        }
        for action in actions {
            self.apply(ctx, action);
        }
    }

    /// Drain bridge lifecycle events into actions.
    fn pump_bridge(&mut self, ctx: &Context) {
        let mut events = Vec::new();
        if let Some(supervisor) = &self.supervisor {
            while let Some(event) = supervisor.try_event() {
                events.push(event);
            }
        }
        let mut ended = false;
        for event in events {
            match event {
                BridgeEvent::Running => self.apply(ctx, Action::BridgeRunning),
                BridgeEvent::Ended(outcome) => {
                    self.apply(ctx, Action::BridgeEnded(outcome));
                    ended = true;
                }
            }
        }
        if ended {
            self.supervisor = None;
        }
    }

    /// Sample the lock-free live counter while a session is active.
    fn pump_live(&mut self, ctx: &Context) {
        let captured = self
            .supervisor
            .as_ref()
            .map(BridgeSupervisor::live_captured);
        if let Some(count) = captured {
            self.apply(ctx, Action::LiveCaptured(count));
        }
    }

    /// Drain actions posted by background threads (discovery, config save).
    fn pump_channel(&mut self, ctx: &Context) {
        let mut actions = Vec::new();
        while let Ok(action) = self.actions_rx.try_recv() {
            actions.push(action);
        }
        for action in actions {
            self.apply(ctx, action);
        }
    }

    /// Intercept the window close request and hide to tray instead of exiting.
    fn handle_close(&mut self, ctx: &Context) {
        if ctx.input(|input| input.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.apply(ctx, Action::HideWindow);
        }
    }

    /// Reflect the bridge state in the tray's Start/Stop items.
    fn sync_tray(&self) {
        if let Some(tray) = &self.tray {
            tray.set_active(self.state.bridge.is_active());
        }
    }

    /// Paint the whole UI, collecting and then applying user intents.
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut pending = Vec::new();

        egui::Panel::top("header").show_inside(ui, |ui| {
            ui.heading("Sonos Driver");
            ui.label("Stream this PC's audio to a Sonos player.");
        });

        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Status:");
                ui.label(&self.state.status_line);
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.rooms_section(ui, &mut pending);
                ui.separator();
                self.settings_section(ui, &mut pending);
                ui.separator();
                self.controls_section(ui, &mut pending);
                ui.separator();
                self.telemetry_section(ui);
            });
        });

        for action in pending {
            self.apply(&ctx, action);
        }
    }

    /// Discovery controls and the discovered-room list.
    fn rooms_section(&self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        ui.heading("Rooms");
        ui.horizontal(|ui| {
            let running = self.state.discovery.is_running();
            if ui
                .add_enabled(!running, egui::Button::new("Discover rooms"))
                .clicked()
            {
                pending.push(Action::StartDiscovery);
            }
            ui.label(self.discovery_label());
        });

        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("rooms")
            .show(ui, |ui| {
                if self.state.rooms.is_empty() {
                    ui.weak("No rooms discovered yet.");
                    return;
                }
                for (index, room) in self.state.rooms.rooms().iter().enumerate() {
                    let selected = self.state.rooms.selected_index() == Some(index);
                    if ui.selectable_label(selected, room.label()).clicked() {
                        pending.push(Action::SelectRoom(index));
                    }
                }
            });
    }

    /// A short label describing the current discovery state.
    fn discovery_label(&self) -> String {
        use sonos_driver_gui::Discovery;
        match &self.state.discovery {
            Discovery::Idle => format!("{} room(s) known", self.state.rooms.len()),
            Discovery::Running => "Searching…".to_owned(),
            Discovery::Failed(reason) => format!("Failed: {reason}"),
        }
    }

    /// The editable settings form.
    fn settings_section(&mut self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        ui.heading("Settings");

        // Snapshot the device-picker inputs as owned values before borrowing the
        // form mutably for the text rows, so the two borrows never overlap.
        let current_device = self.state.form.device.clone();
        let default_selected = current_device.is_empty();
        let device_options: Vec<(usize, String, bool)> = self
            .state
            .devices
            .iter()
            .enumerate()
            .map(|(index, device)| (index, device.label(), device.id == current_device))
            .collect();
        let device_text = if default_selected {
            "System default".to_owned()
        } else if let Some((_, label, _)) = device_options.iter().find(|(_, _, selected)| *selected)
        {
            label.clone()
        } else {
            current_device.clone()
        };
        let scanning = self.state.device_scan.is_running();
        let scan_label = match &self.state.device_scan {
            DeviceScan::Idle => None,
            DeviceScan::Running => Some("Scanning…".to_owned()),
            DeviceScan::Failed(reason) => Some(format!("Scan failed: {reason}")),
        };

        let form = &mut self.state.form;
        egui::Grid::new("settings")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Capture device");
                ui.vertical(|ui| {
                    egui::ComboBox::from_id_salt("capture-device")
                        .selected_text(device_text)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(default_selected, "System default")
                                .clicked()
                            {
                                pending.push(Action::SelectDevice(None));
                            }
                            for (index, label, selected) in &device_options {
                                if ui.selectable_label(*selected, label).clicked() {
                                    pending.push(Action::SelectDevice(Some(*index)));
                                }
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!scanning, egui::Button::new("Refresh devices"))
                            .clicked()
                        {
                            pending.push(Action::RefreshDevices);
                        }
                        if let Some(label) = &scan_label {
                            ui.weak(label);
                        }
                    });
                });
                ui.end_row();

                ui.label("Target room");
                ui.text_edit_singleline(&mut form.room);
                ui.end_row();

                ui.label("Device UUID");
                ui.text_edit_singleline(&mut form.uuid);
                ui.end_row();

                ui.label("Seed host").on_hover_text(
                    "Address of any Sonos player. Use when SSDP multicast discovery is blocked.",
                );
                ui.text_edit_singleline(&mut form.seed_host);
                ui.end_row();

                ui.label("Bind address");
                ui.text_edit_singleline(&mut form.bind_address);
                ui.end_row();

                ui.label("Port");
                ui.text_edit_singleline(&mut form.port);
                ui.end_row();

                ui.label("Target sample rate (Hz)");
                ui.text_edit_singleline(&mut form.target_sample_rate);
                ui.end_row();

                ui.label("Buffer (ms)");
                ui.text_edit_singleline(&mut form.buffer_ms);
                ui.end_row();

                ui.label("Discovery timeout (s)");
                ui.text_edit_singleline(&mut form.discovery_timeout_secs);
                ui.end_row();

                ui.label("Codec profile");
                egui::ComboBox::from_id_salt("profile")
                    .selected_text(form.profile.description())
                    .show_ui(ui, |ui| {
                        for profile in ALL_PROFILES {
                            ui.selectable_value(&mut form.profile, profile, profile.description());
                        }
                    });
                ui.end_row();
            });
    }

    /// Start/Stop/Save controls.
    fn controls_section(&self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        ui.heading("Controls");
        ui.horizontal(|ui| {
            let active = self.state.bridge.is_active();
            if ui
                .add_enabled(!active, egui::Button::new("Start streaming"))
                .clicked()
            {
                pending.push(Action::StartBridge);
            }
            if ui
                .add_enabled(active, egui::Button::new("Stop streaming"))
                .clicked()
            {
                pending.push(Action::StopBridge);
            }
            if ui.button("Save settings").clicked() {
                pending.push(Action::SaveConfig);
            }
        });
    }

    /// Live and end-of-session telemetry.
    fn telemetry_section(&self, ui: &mut egui::Ui) {
        ui.heading("Status");
        ui.label(format!("Bridge: {}", self.state.bridge.label()));
        if let BridgeStatus::Failed(reason) = &self.state.bridge {
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), reason);
        }
        if self.state.bridge.is_active() {
            ui.label(format!("Captured blocks: {}", self.state.live_captured));
        }
        if let Some(metrics) = self.state.last_metrics {
            ui.label(format!(
                "Last session — captured {}, streamed {}, underruns {}, reconnects {}",
                metrics.captured, metrics.streamed, metrics.underruns, metrics.reconnects
            ));
        }
        if let Some(latency) = self.state.last_latency {
            ui.label(format!(
                "Controllable latency — mean {:.1} ms, max {:.1} ms over {} chunks",
                latency.mean.as_secs_f64() * 1000.0,
                latency.max.as_secs_f64() * 1000.0,
                latency.samples
            ));
        }
    }
}

impl App for GuiApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut Frame) {
        // Runs before each paint and also while the window is hidden (when a
        // repaint is requested), so tray interactions keep working even when
        // the only visible surface is the tray icon.
        self.pump_tray(ctx);
        self.pump_bridge(ctx);
        self.pump_live(ctx);
        self.pump_channel(ctx);
        self.handle_close(ctx);
        self.sync_tray();
        ctx.request_repaint_after(REPAINT_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        self.render(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Stop and join the worker so audio teardown completes before exit.
        self.supervisor = None;
    }
}

/// Spawn a detached thread that periodically requests a repaint, so the UI keeps
/// servicing tray events even while hidden and idle.
fn spawn_repaint_pump(ctx: egui::Context) {
    let spawned = thread::Builder::new()
        .name("repaint-pump".to_owned())
        .spawn(move || loop {
            thread::sleep(REPAINT_INTERVAL);
            ctx.request_repaint();
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "failed to spawn the repaint pump; the tray may feel sluggish");
    }
}
