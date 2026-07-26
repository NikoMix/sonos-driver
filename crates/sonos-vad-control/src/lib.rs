//! Stable Sonos room identity and Windows device-name derivation.
//!
//! This crate is the pure, platform-independent half of the Sonos virtual-audio
//! driver's user-mode control plane. It has no kernel, FFI, or OS-specific code,
//! so it builds and unit-tests on any platform and keeps the workspace's
//! `#![forbid(unsafe_code)]` guarantee.
//!
//! It answers one question: *what should the Windows audio endpoint for this
//! Sonos room be called?* — see [`RoomKey`] and [`device_name_for`].
//!
//! # Why this is all that is left here
//!
//! ADR-0017 originally had the kernel driver **create and destroy** render
//! endpoints on demand, driven by a serialisable command/event protocol and a
//! reconcile planner that lived in this crate. ADR-0018 replaced that with a
//! **fixed** pool of endpoints declared in the driver INF, which user mode
//! simply *renames*: dynamic `PcRegisterSubdevice` churn was the single riskiest
//! thing in the design, and renaming is exactly what the Sound control panel
//! already does.
//!
//! The command/event protocol and the create/destroy registry were therefore
//! deleted rather than left to rot. Slot planning now lives in
//! [`sonos-vad-endpoint`](https://docs.rs/sonos-vad-endpoint), next to the
//! Windows APIs that apply it.
//!
//! ```
//! use std::collections::BTreeSet;
//! use sonos_vad_control::{device_name_for, RoomKey};
//!
//! let living_room = RoomKey::new("RINCON_1", "Living Room");
//! assert_eq!(device_name_for(&living_room, &BTreeSet::new()), "Sonos Living Room");
//! ```
#![forbid(unsafe_code)]

mod room;

pub use room::{device_name_for, RoomKey, DEVICE_NAME_PREFIX};
