//! Headless, render-agnostic core of the `sonos-driver-gui` system-tray app.
//!
//! # Workstream WS-J
//!
//! This library holds every piece of application logic that does *not* draw
//! pixels, so it can be unit-tested without a display, a GPU, or a Sonos player
//! on the network. The rendering shell (the `sonos-driver-gui` binary) wires
//! these pieces to `eframe`/`egui`, the system tray and the OS event loop.
//!
//! The design follows a small Elm-style loop:
//!
//! ```text
//! UI / system event  ->  Action  ->  reduce(&mut AppState)  ->  [Effect]
//!                                                                  |
//!                                          executed by the shell (I/O, threads)
//! ```
//!
//! * [`state`] holds the pure [`AppState`](state::AppState) (room list, bridge
//!   status, settings form, telemetry).
//! * [`action`] defines [`Action`]/[`Effect`] and the pure [`reduce`] function
//!   that advances the state and emits side effects for the shell to run.
//! * [`config_form`] converts between the editable settings form and the typed
//!   [`audio_core::Config`]; [`config_io`] persists it as TOML.
//! * [`bridge`] supervises a [`sonos_driver_core::Bridge`] on its own thread and
//!   reports lifecycle + telemetry without ever blocking the UI.
//! * [`discovery`] runs Sonos discovery in the background.
//! * [`metrics`] carries the lock-free live counter and the displayed snapshot.

#![forbid(unsafe_code)]

pub mod action;
pub mod bridge;
pub mod config_form;
pub mod config_io;
pub mod discovery;
pub mod metrics;
pub mod state;

pub use action::{reduce, Action, Effect};
pub use bridge::{BridgeEvent, BridgeSupervisor, CountingSource, SessionOutcome};
pub use config_form::SettingsForm;
pub use metrics::{LiveCounter, MetricsSnapshot};
pub use state::{AppState, BridgeStatus, CaptureDevice, DeviceScan, Discovery, Room, RoomList};
