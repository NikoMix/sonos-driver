//! Core types and trait contracts for the `sonos-driver` workspace.
//!
//! This crate is the dependency hub: every other crate builds against the
//! stable abstractions defined here, which lets the capture, codec, pipeline,
//! transport and control workstreams be developed independently and in
//! parallel.
//!
//! # Architecture at a glance
//!
//! ```text
//! AudioSource  ->  pipeline  ->  Encoder  ->  StreamSink (Transport)  ->  Sonos
//! ```
//!
//! * [`AudioSource`] yields [`AudioFrame`]s of interleaved `f32` samples.
//! * [`Encoder`] turns frames into a byte stream for a given [`CodecProfile`].
//! * [`StreamSink`]/[`Transport`] expose those bytes over the network.
//!
//! All public items are runtime-agnostic (no async runtime, no OS APIs) so the
//! contract layer stays portable and trivially testable. The bundled
//! [`SyntheticSource`] provides deterministic audio for hardware-free CI.

#![forbid(unsafe_code)]

mod codec;
mod config;
mod error;
mod format;
mod frame;
mod source;
mod transport;

pub use codec::{CodecProfile, Encoder, LatencyClass};
pub use config::{
    CaptureConfig, Config, PipelineConfig, SonosConfig, StreamConfig, DEFAULT_STREAM_PORT,
};
pub use error::{Error, Result};
pub use format::{AudioFormat, SampleFormat};
pub use frame::AudioFrame;
pub use source::{AudioSource, SyntheticSource};
pub use transport::{StreamSink, Transport};

/// Commonly used exports, intended to be glob-imported by downstream crates:
/// `use audio_core::prelude::*;`.
pub mod prelude {
    pub use crate::{
        AudioFormat, AudioFrame, AudioSource, CodecProfile, Encoder, Error, LatencyClass, Result,
        SampleFormat, StreamSink, Transport,
    };
}
