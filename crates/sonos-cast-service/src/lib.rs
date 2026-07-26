//! The driverless host ([ADR-0019]) — routing and streaming, minus the hosting.
//!
//! Windows offers no user-mode way to create an audio endpoint: endpoints are a
//! projection of kernel streaming filters, and ACX — the modern audio driver
//! framework — is deliberately KMDF-only. So this host cannot put a room in the
//! sound picker; only `sonosvad.sys` can do that.
//!
//! What it can do is everything else, with nothing installed and no reboot:
//! capture system audio, one named output, or a single application via WASAPI
//! process loopback, and stream it to any room — or to several rooms as one
//! synchronised Sonos group. Several routes run at once, so "Spotify in the
//! kitchen, everything else in the living room" needs no driver at all.
//!
//! The difference between the two hosts is narrower than it looks: it is where
//! the routing is chosen. The driver path uses the Windows sound UI; this one
//! uses a config file. Both share `sonos-driver-core`, so a fault reproduced
//! here is known not to be a kernel fault — which matters when the alternative
//! diagnosis costs a bugcheck.
//!
//! This is a library so the two front ends — the `sonos-cast-svc` service and
//! `sonos-driver cast` — run byte-for-byte the same worker rather than two
//! implementations that drift apart.
//!
//! [ADR-0019]: https://github.com/NikoMix/sonos-driver/blob/main/docs/adr/0019-driverless-routing-mode.md

#![forbid(unsafe_code)]

pub mod config;
pub mod observer;
pub mod worker;

pub use observer::{Observer, RouteReport, WorkerEvent};
pub use worker::{dry_run, run, WorkerSettings};
