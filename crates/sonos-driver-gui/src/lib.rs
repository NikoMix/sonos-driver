//! Headless, render-agnostic core of the Sonos Router desktop app.
//!
//! # What this is
//!
//! A routing table between the two lists Windows and Sonos each already have:
//! every audio **output** this PC offers on one side, every Sonos **room** in
//! the household on the other. Point an output at a room and whatever plays
//! through that output plays there, with `All rooms` available for the whole
//! house at once. Changes take effect while the app runs; nothing is installed
//! and no driver is involved.
//!
//! It is the third and most direct of the three ways this repository can get
//! audio to a Sonos speaker:
//!
//! | | How you choose the room | Needs a driver |
//! | --- | --- | --- |
//! | **This app** | Point an output at a room, here | no |
//! | `sonos-driver cast` / `sonoscast` | A `cast.toml` file | no |
//! | `sonosvad.sys` + `sonosvad-svc` | The Windows sound picker | yes |
//!
//! The two driver-based modes are unchanged and remain the right answer when
//! the rooms have to appear in the Windows sound picker itself, or when audio
//! must survive logging out. This app streams in its own process.
//!
//! # Design
//!
//! Everything that does not draw pixels lives here, so it can be unit-tested
//! without a display, a sound card or a Sonos player on the network. The
//! rendering shell (the `sonos-driver-gui` binary) wires these pieces to
//! `eframe`/`egui`, the system tray and the OS event loop.
//!
//! The design follows a small Elm-style loop:
//!
//! ```text
//! UI / system event  ->  Action  ->  reduce(&mut AppState)  ->  [Effect]
//!                                                                  |
//!                                          executed by the shell (I/O, threads)
//! ```
//!
//! * [`state`] holds the pure [`AppState`](state::AppState): outputs, rooms,
//!   the routing table, and what the worker is doing.
//! * [`routes`] is the routing table itself — assignment, validation, and
//!   conversion to the routes the streaming worker runs.
//! * [`action`] defines [`Action`]/[`Effect`] and the pure [`reduce`] function.
//! * [`config_form`] converts between the editable settings and the typed
//!   [`audio_core::Config`]; [`config_io`] persists both halves as one
//!   `cast.toml`-compatible document.
//! * [`cast`] supervises the multi-route streaming worker on its own thread.
//! * [`discovery`] resolves Sonos rooms in the background.

#![forbid(unsafe_code)]

pub mod action;
pub mod cast;
pub mod config_form;
pub mod config_io;
pub mod discovery;
pub mod routes;
pub mod state;

pub use action::{reduce, Action, Effect};
pub use cast::CastSupervisor;
pub use config_form::SettingsForm;
pub use routes::{Conflict, Destination, OutputRoute, RoutingTable, Warning};
pub use state::{AppState, Output, Room, Scan, StreamStatus};
