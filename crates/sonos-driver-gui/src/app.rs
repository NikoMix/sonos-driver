//! The `eframe`/`egui` rendering shell.
//!
//! [`GuiApp`] owns the [`AppState`] and the live handles the headless core
//! cannot: the streaming worker ([`CastSupervisor`]), the system tray, and the
//! channels background threads post [`Action`]s on. Each frame it drains those
//! event sources into the pure [`reduce`] function, executes the returned
//! [`Effect`]s (the only place that performs I/O or drives the window), then
//! paints the state.
//!
//! The window is one table — every Windows output, and where it plays — and
//! everything else is either a button above it or a detail below it. That is
//! deliberate: the app has exactly one job, and it should be obvious on sight.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use audio_core::Config;
use eframe::egui::{self, Context, ViewportCommand};
use eframe::{App, CreationContext, Frame};
use sonos_cast_service::WorkerEvent;
use sonos_driver_gui::config_form::{profile_hint, ALL_PROFILES};
use sonos_driver_gui::routes::Destination;
use sonos_driver_gui::{
    config_io, discovery, driver_status, reduce, Action, AppState, CastSupervisor, Effect, Output,
    StreamStatus,
};
use sonos_routing::RouteConfig;

use crate::icon;
use crate::tray::Tray;

/// How often the background pump nudges a repaint so tray clicks are serviced
/// even when the window is hidden and otherwise idle.
const REPAINT_INTERVAL: Duration = Duration::from_millis(200);

/// Width reserved for the output-name column of the routing grid.
///
/// Endpoint names such as "Speakers (Realtek(R) Audio)" are long, and a grid
/// column otherwise shrinks to whatever fits, wrapping every name into a
/// three-line stack. Fixing the width keeps rows one line tall and the two
/// columns aligned no matter which devices are plugged in.
const NAME_COLUMN_WIDTH: f32 = 260.0;

/// Width of the destination drop-down.
const DESTINATION_WIDTH: f32 = 190.0;

/// The application: render-shell state plus the live worker/tray/channel
/// handles that the pure core deliberately does not own.
pub(crate) struct GuiApp {
    state: AppState,
    actions_tx: Sender<Action>,
    actions_rx: Receiver<Action>,
    supervisor: Option<CastSupervisor>,
    tray: Option<Tray>,
    quitting: bool,
    /// Set once, to kick off the first enumeration and discovery from inside
    /// the first frame (where a repaint can actually be requested).
    started: bool,
}

impl GuiApp {
    /// Construct the app: load the saved routing table, build the tray, and
    /// start the background repaint pump.
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
        let (config, routes, load_error) = match config_io::load(&path) {
            Ok((config, routes)) => (config, routes, None),
            Err(_) if !path.exists() => (Config::default(), Vec::new(), None),
            Err(error) => (Config::default(), Vec::new(), Some(error.to_string())),
        };

        let mut state = AppState::new(path, &config);
        state.saved_routes = routes;
        if let Some(error) = load_error {
            tracing::warn!(%error, "failed to load the saved routing; using defaults");
            state.status_line =
                format!("Could not read the saved routing (saving replaces it): {error}");
        }

        Self {
            state,
            actions_tx,
            actions_rx,
            supervisor: None,
            tray,
            quitting: false,
            started: false,
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
            Effect::EnumerateOutputs => self.spawn_enumerate(),
            Effect::SpawnDiscovery { seed, timeout } => {
                discovery::spawn(seed, timeout, self.actions_tx.clone());
            }
            Effect::StartWorker {
                config,
                routes,
                seed,
            } => {
                // Replacing the handle drops the previous one, which stops and
                // joins it. By construction it has already finished.
                self.supervisor = Some(CastSupervisor::spawn(*config, routes, seed));
            }
            Effect::StopWorker => {
                if let Some(supervisor) = &self.supervisor {
                    supervisor.request_stop();
                }
            }
            Effect::Save {
                path,
                config,
                routes,
            } => self.spawn_save(path, *config, routes),
            Effect::ShowWindow => {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            Effect::HideWindow => ctx.send_viewport_cmd(ViewportCommand::Visible(false)),
            Effect::Quit => {
                self.quitting = true;
                // Dropping the supervisor stops streaming and joins the worker,
                // so any Sonos group it formed is split before we exit.
                self.supervisor = None;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// Persist the routing table and settings on a background thread.
    fn spawn_save(&self, path: PathBuf, config: Config, routes: Vec<RouteConfig>) {
        let actions = self.actions_tx.clone();
        let spawned = thread::Builder::new()
            .name("config-save".to_owned())
            .spawn(move || {
                let action = match config_io::save(&path, &config, &routes) {
                    Ok(()) => Action::Saved,
                    Err(err) => Action::SaveFailed(err.to_string()),
                };
                let _ = actions.send(action);
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "failed to spawn the config-save thread");
        }
    }

    /// Enumerate render endpoints on a background thread. The enumeration
    /// initialises its own COM apartment, so it is never run on the UI thread.
    ///
    /// The same thread also asks the Service Control Manager about the
    /// `sonosvad` service, because the answer is only meaningful next to the
    /// endpoint list: endpoints say whether the driver is loaded, the service
    /// says whether anything is driving them.
    fn spawn_enumerate(&self) {
        let actions = self.actions_tx.clone();
        let spawned = thread::Builder::new()
            .name("output-enumerate".to_owned())
            .spawn(move || {
                let action = match audio_capture::enumerate_render_endpoints() {
                    Ok(endpoints) => Action::OutputsEnumerated(
                        endpoints
                            .into_iter()
                            .map(|endpoint| {
                                Output::new(
                                    endpoint.id,
                                    endpoint.name,
                                    endpoint.adapter,
                                    endpoint.is_default,
                                )
                            })
                            .collect(),
                    ),
                    Err(err) => Action::OutputScanFailed(err.to_string()),
                };
                // Probed first so the service state is already in place when the
                // endpoint list lands and the advice is recomputed.
                let _ = actions.send(Action::VadServiceProbed(driver_status::probe_service()));
                let _ = actions.send(action);
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "failed to spawn the output-enumerate thread");
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

    /// Drain streaming lifecycle events into actions.
    fn pump_worker(&mut self, ctx: &Context) {
        let mut events = Vec::new();
        if let Some(supervisor) = &self.supervisor {
            while let Some(event) = supervisor.try_event() {
                events.push(event);
            }
        }
        for event in events {
            let terminal = matches!(event, WorkerEvent::Stopped);
            self.apply(ctx, Action::Worker(event));
            // Only release the handle if the reducer did *not* queue a restart;
            // if it did, `self.supervisor` is already the new worker.
            if terminal && self.state.stream == StreamStatus::Stopped {
                self.supervisor = None;
            }
        }
    }

    /// Drain actions posted by background threads (discovery, enumeration, save).
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

    /// Reflect the streaming state in the tray's Start/Stop items.
    fn sync_tray(&self) {
        if let Some(tray) = &self.tray {
            tray.set_active(self.state.stream.is_active());
        }
    }

    /// Paint the whole UI, collecting and then applying user intents.
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut pending = Vec::new();

        egui::Panel::top("header").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Sonos Router");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.status_badge(ui);
                });
            });
            ui.label("Send each Windows audio output to a Sonos room.");
            ui.add_space(4.0);
            self.toolbar(ui, &mut pending);
            ui.add_space(4.0);
        });

        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| ui.label(&self.state.status_line));
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("body").show(ui, |ui| {
                self.routing_table(ui, &mut pending);
                self.problems(ui);
                self.live_routes(ui);
                ui.add_space(8.0);
                self.settings(ui, &mut pending);
                ui.add_space(8.0);
                Self::footnote(ui);
            });
        });

        for action in pending {
            self.apply(&ctx, action);
        }
    }

    /// The coloured "what is happening right now" pill in the header.
    fn status_badge(&self, ui: &mut egui::Ui) {
        let (colour, text) = match &self.state.stream {
            StreamStatus::Stopped => (ui.visuals().weak_text_color(), "Not streaming".to_owned()),
            StreamStatus::Starting | StreamStatus::Stopping => (
                ui.visuals().warn_fg_color,
                format!("{}…", self.state.stream.label()),
            ),
            StreamStatus::Streaming => {
                let routes = self.state.live.len();
                (
                    egui::Color32::from_rgb(0x2e, 0xa0, 0x43),
                    format!(
                        "Streaming {routes} route{}",
                        if routes == 1 { "" } else { "s" }
                    ),
                )
            }
            StreamStatus::Retrying(_) => (ui.visuals().error_fg_color, "Retrying…".to_owned()),
        };
        ui.colored_label(colour, text);
    }

    /// Start/Stop plus the two refresh buttons.
    fn toolbar(&self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            let active = self.state.stream.is_active();
            let (label, enabled) = if !active {
                ("▶  Start", self.state.can_start())
            } else if self.state.unapplied {
                ("⟳  Apply changes", self.state.can_start())
            } else {
                ("▶  Streaming", false)
            };
            if ui
                .add_enabled(enabled, egui::Button::new(label))
                .on_disabled_hover_text(self.start_hint())
                .clicked()
            {
                pending.push(Action::Apply);
            }

            if ui
                .add_enabled(active, egui::Button::new("■  Stop"))
                .clicked()
            {
                pending.push(Action::Stop);
            }

            ui.separator();

            if ui
                .add_enabled(
                    !self.state.discovery.is_running(),
                    egui::Button::new("Find rooms"),
                )
                .on_hover_text("Search the network for Sonos rooms")
                .clicked()
            {
                pending.push(Action::RefreshRooms);
            }
            if ui
                .add_enabled(
                    !self.state.outputs.is_running(),
                    egui::Button::new("Refresh outputs"),
                )
                .on_hover_text("Re-read the Windows playback devices")
                .clicked()
            {
                pending.push(Action::RefreshOutputs);
            }

            ui.separator();

            if ui
                .add_enabled(
                    !self.state.table.is_idle(),
                    egui::Button::new("Clear routing"),
                )
                .clicked()
            {
                pending.push(Action::ClearRouting);
            }
            if ui
                .button("Save")
                .on_hover_text(format!(
                    "Write the routing to {}",
                    self.state.config_path.display()
                ))
                .clicked()
            {
                pending.push(Action::Save);
            }
        });
    }

    /// Why the Start button is disabled, shown on hover.
    fn start_hint(&self) -> String {
        if self.state.stream.is_active() && !self.state.unapplied {
            return "Already streaming. Change a room to enable Apply.".to_owned();
        }
        if self.state.table.is_idle() {
            return "Pick a room next to at least one output first.".to_owned();
        }
        self.state
            .table
            .conflicts()
            .first()
            .map_or_else(|| "Ready.".to_owned(), |conflict| conflict.message.clone())
    }

    /// The routing table: every output, and where it plays.
    fn routing_table(&self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        let offline = self.state.table.offline_rows(&self.state.rooms);
        let conflicting = self.conflicting_rows();

        egui::Grid::new("routing")
            .num_columns(2)
            .striped(true)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_width(NAME_COLUMN_WIDTH);
                    ui.strong("Windows output");
                });
                ui.strong("Plays on");
                ui.end_row();

                for (index, row) in self.state.table.rows().iter().enumerate() {
                    ui.vertical(|ui| {
                        // Device names are long and the description is a fixed
                        // short phrase, so reserve real width here: left to the
                        // grid's auto-sizing both wrap to a few characters.
                        ui.set_min_width(NAME_COLUMN_WIDTH);
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        if conflicting.contains(&index) {
                            ui.colored_label(ui.visuals().error_fg_color, &row.output.name)
                                .on_hover_text(&row.output.name);
                        } else {
                            ui.label(&row.output.name).on_hover_text(&row.output.name);
                        }
                        let description = row.output.description();
                        if !description.is_empty() {
                            ui.weak(description);
                        }
                    });

                    ui.vertical(|ui| {
                        egui::ComboBox::from_id_salt(("destination", index))
                            .selected_text(row.destination.label())
                            .width(DESTINATION_WIDTH)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(!row.destination.is_routed(), "Not routed")
                                    .clicked()
                                {
                                    pending.push(Action::Assign {
                                        row: index,
                                        destination: Destination::Off,
                                    });
                                }
                                if ui
                                    .selectable_label(
                                        row.destination == Destination::AllRooms,
                                        "All rooms",
                                    )
                                    .on_hover_text(
                                        "Every room, joined into one synchronised Sonos group",
                                    )
                                    .clicked()
                                {
                                    pending.push(Action::Assign {
                                        row: index,
                                        destination: Destination::AllRooms,
                                    });
                                }
                                if self.state.rooms.is_empty() {
                                    ui.separator();
                                    ui.weak("No rooms found yet");
                                    return;
                                }
                                ui.separator();
                                for room in &self.state.rooms {
                                    let selected =
                                        row.destination.uuid() == Some(room.uuid.as_str());
                                    if ui.selectable_label(selected, &room.name).clicked() {
                                        pending.push(Action::Assign {
                                            row: index,
                                            destination: Destination::Room {
                                                uuid: room.uuid.clone(),
                                                name: room.name.clone(),
                                            },
                                        });
                                    }
                                }
                            });

                        if offline.contains(&index) {
                            ui.colored_label(ui.visuals().warn_fg_color, "offline — skipped")
                                .on_hover_text(
                                    "This room did not answer discovery. Switch the speaker on \
                                     and press Find rooms.",
                                );
                        }
                    });
                    ui.end_row();
                }
            });

        if self.state.table.rows().len() == 1 {
            ui.add_space(6.0);
            ui.weak(match self.state.outputs.error() {
                Some(reason) => format!("Could not list the Windows outputs: {reason}"),
                None => "Looking for Windows outputs…".to_owned(),
            });
        }
    }

    /// Rows the table considers to be in conflict, for highlighting.
    fn conflicting_rows(&self) -> Vec<usize> {
        self.state
            .table
            .conflicts()
            .into_iter()
            .flat_map(|conflict| conflict.rows)
            .collect()
    }

    /// Conflicts (blocking) and warnings (not), each with the fix.
    fn problems(&self, ui: &mut egui::Ui) {
        let conflicts = self.state.table.conflicts();
        let warnings = self.state.table.warnings(self.state.driver.service);
        let discovery_error = self.state.discovery.error();
        let driver_advice = self.state.driver.advice();
        if conflicts.is_empty()
            && warnings.is_empty()
            && discovery_error.is_none()
            && driver_advice.is_none()
        {
            return;
        }

        ui.add_space(8.0);
        for conflict in &conflicts {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, "✖");
                ui.label(&conflict.message);
            });
        }
        for warning in &warnings {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, "⚠");
                ui.label(&warning.message);
            });
        }
        // The kernel-driver mode is optional and this app never needs it, so its
        // state is reported as information rather than as a warning — and only
        // when its two halves disagree. See `DriverStatus::advice`.
        if let Some(advice) = driver_advice {
            ui.horizontal_wrapped(|ui| {
                ui.weak("ℹ");
                ui.weak(advice);
            });
        }
        if let Some(reason) = discovery_error {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, "⚠");
                ui.label(format!(
                    "Room search failed: {reason}. If this network blocks multicast, put any \
                     one speaker's IP address into the Seed host box in Settings."
                ));
            });
        }
    }

    /// What is actually streaming right now, straight from the worker.
    fn live_routes(&self, ui: &mut egui::Ui) {
        if self.state.live.is_empty() {
            return;
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Live routes ({})", self.state.live.len()))
            .default_open(true)
            .show(ui, |ui| {
                for report in &self.state.live {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(report.label());
                        ui.weak(format!(
                            "· port {}{}",
                            report.port,
                            if report.grouped { " · grouped" } else { "" }
                        ));
                    });
                }
                ui.add_space(4.0);
                ui.weak(
                    "Sonos buffers roughly 0.4–1 second before it plays, so the speakers stay \
                     behind this PC by about that much. That delay is fixed by the speakers, \
                     not by this app.",
                );
            });
    }

    /// The audio settings, collapsed by default because the defaults are right.
    fn settings(&mut self, ui: &mut egui::Ui, pending: &mut Vec<Action>) {
        let profile_text = self.state.settings.profile.name().to_owned();
        let current_profile = self.state.settings.profile;
        let settings = &mut self.state.settings;

        egui::CollapsingHeader::new("Settings")
            .id_salt("settings")
            .show(ui, |ui| {
                egui::Grid::new("settings-grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Quality")
                            .on_hover_text("How the audio is encoded on the way to the speakers.");
                        ui.vertical(|ui| {
                            egui::ComboBox::from_id_salt("profile")
                                .selected_text(profile_text)
                                .show_ui(ui, |ui| {
                                    for profile in ALL_PROFILES {
                                        if ui
                                            .selectable_label(
                                                profile == current_profile,
                                                profile.name(),
                                            )
                                            .on_hover_text(profile_hint(profile))
                                            .clicked()
                                        {
                                            settings.profile = profile;
                                        }
                                    }
                                });
                            ui.weak(profile_hint(current_profile));
                        });
                        ui.end_row();

                        ui.label("Seed host").on_hover_text(
                            "The IP address of any one Sonos speaker. Use this when the network \
                             blocks SSDP multicast — a NAT'd VM, a guest network, or an access \
                             point without working IGMP. One speaker reports the whole household.",
                        );
                        ui.text_edit_singleline(&mut settings.seed_host);
                        ui.end_row();

                        ui.label("Release a room after").on_hover_text(
                            "Seconds of silence after which the room is handed back, so \
                                 the Sonos app can use it again. 0 keeps it for as long as the \
                                 route runs.",
                        );
                        ui.text_edit_singleline(&mut settings.idle_stop_after_secs);
                        ui.end_row();

                        ui.label("First port").on_hover_text(
                            "Each route serves its audio on its own TCP port, counting up from \
                             here. Allow these through the Windows firewall on Private networks.",
                        );
                        ui.text_edit_singleline(&mut settings.port);
                        ui.end_row();

                        ui.label("Bind address");
                        ui.text_edit_singleline(&mut settings.bind_address);
                        ui.end_row();

                        ui.label("Buffer (ms)");
                        ui.text_edit_singleline(&mut settings.buffer_ms);
                        ui.end_row();

                        ui.label("Resample to (Hz)")
                            .on_hover_text("Leave empty to keep whatever rate Windows captures.");
                        ui.text_edit_singleline(&mut settings.target_sample_rate);
                        ui.end_row();

                        ui.label("Room search timeout (s)");
                        ui.text_edit_singleline(&mut settings.discovery_timeout_secs);
                        ui.end_row();
                    });

                ui.add_space(6.0);
                if ui.button("Save settings and routing").clicked() {
                    pending.push(Action::Save);
                }
                ui.weak(format!("Saved to {}", self.state.config_path.display()));
            });
    }

    /// The one thing worth knowing that the UI cannot show.
    fn footnote(ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("How this works")
            .id_salt("how")
            .show(ui, |ui| {
                ui.label(
                    "Audio keeps playing on the PC as well as on Sonos. Windows has no \
                     user-mode way to silence an output, so if you do not want to hear both, \
                     mute the local output in the volume mixer — Sonos keeps its own volume.",
                );
                ui.add_space(4.0);
                ui.label(
                    "Nothing is installed and nothing survives closing this app. If you want \
                     the rooms to appear in the Windows sound picker itself, or audio to keep \
                     running after you log out, the repository's driver mode and headless \
                     service do that instead.",
                );
            });
    }
}

impl App for GuiApp {
    /// Runs before each paint, and also while the window is hidden.
    ///
    /// Everything that must keep working from the tray alone lives here: with
    /// the window closed to tray there is no `ui` pass, so draining the tray,
    /// the worker and the discovery channel cannot be done during painting.
    fn logic(&mut self, ctx: &Context, _frame: &mut Frame) {
        if !self.started {
            self.started = true;
            self.apply(ctx, Action::RefreshOutputs);
            self.apply(ctx, Action::RefreshRooms);
        }

        self.pump_tray(ctx);
        self.pump_worker(ctx);
        self.pump_channel(ctx);
        self.handle_close(ctx);
        self.sync_tray();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        self.render(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Drop the supervisor here rather than relying on process teardown:
        // its `Drop` stops the worker and joins it, which is what ungroups the
        // Sonos rooms. Skipping it would leave the household grouped.
        self.supervisor = None;
    }
}

/// Nudge a repaint on a fixed interval.
///
/// `egui` only repaints on input by default, so a hidden window would otherwise
/// stop servicing the tray, the discovery channel and the worker's events.
fn spawn_repaint_pump(ctx: Context) {
    let spawned = thread::Builder::new()
        .name("repaint-pump".to_owned())
        .spawn(move || loop {
            thread::sleep(REPAINT_INTERVAL);
            ctx.request_repaint();
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "failed to spawn the repaint pump; the tray may feel unresponsive");
    }
}
