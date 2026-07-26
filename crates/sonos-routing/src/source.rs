//! Audio sources a driverless route can capture.
//!
//! On the driver path the Windows sound picker *is* the source selector: each
//! room has its own endpoint and you choose one. Without a driver there is no
//! endpoint to choose, so the source has to be named explicitly — which is the
//! one real ergonomic difference between the two modes (ADR-0019).

use serde::{Deserialize, Serialize};

/// What a route captures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Whatever the current default render endpoint is playing.
    ///
    /// Follows the default device, so changing your speakers in Windows
    /// changes what gets streamed without touching the configuration.
    Default,
    /// One named render endpoint: a device id, or a substring of its name.
    ///
    /// Lets a second sound card, or a third-party virtual cable, act as the
    /// selector when the default device is in use for something else.
    Device(String),
    /// Only the named application's audio, via WASAPI process loopback.
    ///
    /// This is what makes per-application routing possible with no driver at
    /// all — "Spotify in the kitchen" without anything installed.
    Process(String),
    /// Everything *except* the named application's audio.
    ///
    /// The other half of a split: pair it with a [`Source::Process`] route so
    /// one app goes to one room and the rest of the system goes elsewhere.
    ExceptProcess(String),
}

impl Source {
    /// A stable, machine-readable identifier for this source.
    ///
    /// Used where the supervisor expects an endpoint id. It never names a real
    /// WASAPI endpoint — the driverless host derives the capture settings from
    /// the [`Source`] itself — so this only has to be unique and stable.
    #[must_use]
    pub fn descriptor(&self) -> String {
        match self {
            Self::Default => "source:default".to_owned(),
            Self::Device(name) => format!("source:device:{name}"),
            Self::Process(name) => format!("source:process:{name}"),
            Self::ExceptProcess(name) => format!("source:except-process:{name}"),
        }
    }

    /// A human-facing description, for logs and `--dry-run` output.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Default => "System audio".to_owned(),
            Self::Device(name) => format!("Output \"{name}\""),
            Self::Process(name) => format!("App \"{name}\""),
            Self::ExceptProcess(name) => format!("Everything but \"{name}\""),
        }
    }

    /// Whether this source uses WASAPI process loopback.
    ///
    /// Process loopback needs Windows 11 / Server 2022 (build 20348+), so a
    /// host can warn about an unsupported configuration up front instead of
    /// failing once the stream is already meant to be playing.
    #[must_use]
    pub const fn is_process_loopback(&self) -> bool {
        matches!(self, Self::Process(_) | Self::ExceptProcess(_))
    }

    /// Whether this source captures everything *but* its target process.
    #[must_use]
    pub const fn excludes_process(&self) -> bool {
        matches!(self, Self::ExceptProcess(_))
    }

    /// The process name this source targets, if any.
    #[must_use]
    pub fn process(&self) -> Option<&str> {
        match self {
            Self::Process(name) | Self::ExceptProcess(name) => Some(name),
            Self::Default | Self::Device(_) => None,
        }
    }

    /// The render endpoint this source captures, if it names one.
    ///
    /// [`Source::Default`] returns `None`, which is exactly what the capture
    /// layer reads as "follow the default device".
    #[must_use]
    pub fn device(&self) -> Option<&str> {
        match self {
            Self::Device(name) => Some(name),
            // Process loopback still opens against the default endpoint; the
            // process filter, not the device, is what narrows it.
            Self::Default | Self::Process(_) | Self::ExceptProcess(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Source;

    #[test]
    fn descriptors_are_unique_per_source() {
        let sources = [
            Source::Default,
            Source::Device("Speakers".into()),
            Source::Process("spotify.exe".into()),
            Source::ExceptProcess("spotify.exe".into()),
        ];
        let mut seen: Vec<String> = sources.iter().map(Source::descriptor).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), sources.len());
    }

    #[test]
    fn process_sources_expose_their_target_and_direction() {
        let only = Source::Process("spotify.exe".into());
        let except = Source::ExceptProcess("spotify.exe".into());

        assert!(only.is_process_loopback() && except.is_process_loopback());
        assert_eq!(only.process(), Some("spotify.exe"));
        assert_eq!(except.process(), Some("spotify.exe"));
        assert!(!only.excludes_process() && except.excludes_process());
        // Same target, opposite direction: the descriptors must not collide or
        // one would silently overwrite the other in the supervisor's plan.
        assert_ne!(only.descriptor(), except.descriptor());
    }

    #[test]
    fn only_an_explicit_device_pins_the_endpoint() {
        assert_eq!(Source::Device("Realtek".into()).device(), Some("Realtek"));
        // Everything else follows the default device.
        assert_eq!(Source::Default.device(), None);
        assert_eq!(Source::Process("a.exe".into()).device(), None);
        assert!(!Source::Default.is_process_loopback());
    }
}
