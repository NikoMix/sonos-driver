//! Route planning for the driverless mode ([ADR-0019]).
//!
//! Windows has no user-mode way to create an audio endpoint — endpoints are a
//! projection of kernel streaming filters, and the modern audio framework (ACX)
//! is deliberately kernel-only. So the driverless mode cannot put a room in the
//! sound picker. What it *can* do is everything else: capture system audio, or
//! one application's audio via WASAPI process loopback, and stream it to any
//! room, or to the whole household as one synchronised Sonos group.
//!
//! With no endpoint to select, the routing has to be written down instead. A
//! [`Route`] is one sentence — *"send this [`Source`] to these rooms"* — and
//! [`plan`] resolves a set of them against a discovered household into
//! [`PlannedRoute`]s the supervisor can run.
//!
//! This crate is deliberately free of Windows APIs, FFI and I/O, so the part
//! most likely to be misconfigured is also the part that is fully unit-tested
//! on any platform.
//!
//! ```
//! use sonos_routing::{plan, RoomSelector, Route, Source};
//! use sonos_vad_control::RoomKey;
//!
//! let household = vec![
//!     RoomKey::new("RINCON_1", "Küche"),
//!     RoomKey::new("RINCON_2", "Wohnzimmer"),
//! ];
//! let routes = [
//!     // Spotify plays in the kitchen...
//!     Route::new(
//!         Source::Process("spotify.exe".into()),
//!         RoomSelector::Named(vec!["Küche".into()]),
//!     ),
//!     // ...and everything else in the living room.
//!     Route::new(
//!         Source::ExceptProcess("spotify.exe".into()),
//!         RoomSelector::Named(vec!["Wohnzimmer".into()]),
//!     ),
//! ];
//!
//! let planned = plan(&routes, &household, 8080)?;
//! assert_eq!(planned[0].label(), "App \"spotify.exe\" -> Küche");
//! assert_eq!(planned[1].port, 8081);
//! # Ok::<(), sonos_routing::PlanError>(())
//! ```
//!
//! [ADR-0019]: https://github.com/NikoMix/sonos-driver/blob/main/docs/adr/0019-driverless-routing-mode.md

#![forbid(unsafe_code)]

mod error;
mod plan;
mod route;
mod source;

pub use error::PlanError;
pub use plan::{plan, PlannedRoute};
pub use route::{RoomSelector, RoomsField, Route, RouteConfig};
pub use source::Source;

// Re-exported because it appears in this crate's public signatures, so callers
// do not have to depend on `sonos-vad-control` just to name a room.
pub use sonos_vad_control::RoomKey;
