//! Unified error type for the workspace.

use std::borrow::Cow;

/// Convenience alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// The single error type returned by every fallible operation in the
/// `sonos-driver` crates.
///
/// Keeping one error enum at the contract layer means downstream crates can
/// propagate failures with `?` across module boundaries without bespoke
/// conversions, while still carrying enough structure for callers to branch on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Audio capture (e.g. WASAPI loopback) failed.
    #[error("audio capture error: {0}")]
    Capture(Cow<'static, str>),

    /// Encoding or decoding failed.
    #[error("codec error: {0}")]
    Codec(Cow<'static, str>),

    /// The audio pipeline (resampling, conversion, buffering) failed.
    #[error("pipeline error: {0}")]
    Pipeline(Cow<'static, str>),

    /// The streaming transport (HTTP server / sink) failed.
    #[error("transport error: {0}")]
    Transport(Cow<'static, str>),

    /// Sonos discovery or UPnP control failed.
    #[error("sonos control error: {0}")]
    Control(Cow<'static, str>),

    /// Configuration was missing or invalid.
    #[error("configuration error: {0}")]
    Config(Cow<'static, str>),

    /// An audio format was unsupported or inconsistent.
    #[error("invalid audio format: {0}")]
    InvalidFormat(Cow<'static, str>),

    /// A feature contract exists but its implementation is not yet wired up.
    /// Used by Phase-0 skeletons so callers get a typed, non-panicking result.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    /// An underlying I/O operation failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Build a [`Error::Capture`] from any string-like value.
    #[must_use]
    pub fn capture(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Capture(msg.into())
    }

    /// Build a [`Error::Codec`] from any string-like value.
    #[must_use]
    pub fn codec(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Codec(msg.into())
    }

    /// Build a [`Error::Pipeline`] from any string-like value.
    #[must_use]
    pub fn pipeline(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Pipeline(msg.into())
    }

    /// Build a [`Error::Transport`] from any string-like value.
    #[must_use]
    pub fn transport(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Transport(msg.into())
    }

    /// Build a [`Error::Control`] from any string-like value.
    #[must_use]
    pub fn control(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Control(msg.into())
    }

    /// Build a [`Error::Config`] from any string-like value.
    #[must_use]
    pub fn config(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Config(msg.into())
    }

    /// Build a [`Error::InvalidFormat`] from any string-like value.
    #[must_use]
    pub fn invalid_format(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::InvalidFormat(msg.into())
    }
}
