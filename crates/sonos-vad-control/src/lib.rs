//! User-mode control plane for the optional Sonos virtual-audio driver.
//!
//! This crate is the **user-mode half** of the kernel virtual-audio driver
//! explored in ADR-0017 (`docs/adr/0017-kernel-virtual-audio-driver.md`). It is
//! deliberately free of any kernel, FFI, or Windows-specific code, so it builds
//! and unit-tests on any platform and preserves the workspace's
//! `#![forbid(unsafe_code)]` guarantee. The kernel driver itself is an
//! out-of-tree, owner-built C/C++ component; this crate models everything the
//! Rust service needs to drive it.
//!
//! It provides three things:
//!
//! - [`RoomKey`] and [`device_name_for`] — stable room identity and a
//!   deterministic, de-duplicated Windows device-name derivation.
//! - [`ControlCommand`] and [`DriverEvent`] — the message contract between the
//!   user-mode service and the kernel driver (both `serde`-serialisable so the
//!   eventual IOCTL / named-pipe wire format is not re-invented).
//! - [`EndpointRegistry`] — the reconcile planner that converges the set of live
//!   render endpoints to the set of currently discovered rooms, and folds the
//!   driver's asynchronous replies back into its state.
//!
//! # Control loop
//!
//! Each discovery tick the service calls [`EndpointRegistry::reconcile`] with the
//! rooms currently discovered (e.g. by `sonos-control`), sends the returned
//! commands to the driver, and feeds every [`DriverEvent`] reply through
//! [`EndpointRegistry::apply`]. The whole loop is deterministic and testable
//! without a driver.
//!
//! ```
//! use sonos_vad_control::{ControlCommand, DriverEvent, EndpointId, EndpointRegistry, RoomKey};
//!
//! let mut registry = EndpointRegistry::new();
//!
//! // A room is discovered -> plan a create.
//! let rooms = vec![RoomKey::new("RINCON_1", "Living Room")];
//! let commands = registry.reconcile(&rooms);
//! assert_eq!(commands.len(), 1);
//!
//! // The driver confirms the endpoint -> it becomes active.
//! registry.apply(&DriverEvent::EndpointCreated { room_uuid: "RINCON_1".into(), id: EndpointId(1) });
//! assert_eq!(registry.active_endpoint("RINCON_1"), Some(EndpointId(1)));
//!
//! // Re-running with the same rooms is idempotent.
//! assert!(registry.reconcile(&rooms).is_empty());
//! ```
#![forbid(unsafe_code)]

mod protocol;
mod registry;
mod room;

pub use protocol::{ControlCommand, DriverEvent, EndpointId};
pub use registry::{EndpointPhase, EndpointRegistry, EndpointStatus};
pub use room::{device_name_for, RoomKey, DEVICE_NAME_PREFIX};
