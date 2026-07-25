//! Supervising a [`Bridge`] on its own thread without ever blocking the UI.
//!
//! [`Bridge::run`](sonos_driver_core::Bridge::run) borrows the bridge mutably
//! for the entire session, so the live [`BridgeMetrics`](sonos_driver_core::BridgeMetrics)
//! cannot be polled concurrently through the public API. Instead the UI gets a
//! *live* signal from a [`CountingSource`] decorator — a lock-free atomic the
//! decorator bumps per captured block — and the *authoritative*
//! [`MetricsSnapshot`] once the session ends.
//!
//! The worker resolves the Sonos target (mirroring the core's documented
//! `resolve_target` precedence: a device `UUID` wins over a room name, with a
//! three-second discovery floor), opens `WASAPI` loopback capture wrapped in the
//! counting decorator, runs the bridge, then reports the final metrics and
//! controllable latency over a channel.

use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use audio_core::{
    AudioFormat, AudioFrame, AudioSource, CaptureConfig, Config, Error, Result, SonosConfig,
};
use sonos_driver_core::control::{controller_for_room, controller_for_uuid, SonosController};
use sonos_driver_core::{Bridge, ControllableLatency, StopHandle};

use crate::metrics::{LiveCounter, MetricsSnapshot};

/// Discovery floor mirroring the core's `MIN_DISCOVERY`: an `SSDP` search needs
/// a few seconds to collect responses on a typical `LAN`.
const MIN_DISCOVERY: Duration = Duration::from_secs(3);

/// An [`AudioSource`] decorator that counts non-empty reads into a shared,
/// lock-free [`LiveCounter`] the UI samples each frame.
#[derive(Debug)]
pub struct CountingSource<S: AudioSource> {
    inner: S,
    counter: LiveCounter,
}

impl<S: AudioSource> CountingSource<S> {
    /// Wrap `inner`, recording each captured block into `counter`.
    #[must_use]
    pub fn new(inner: S, counter: LiveCounter) -> Self {
        Self { inner, counter }
    }
}

impl<S: AudioSource> AudioSource for CountingSource<S> {
    fn format(&self) -> AudioFormat {
        self.inner.format()
    }

    fn read(&mut self) -> Result<Option<AudioFrame>> {
        let frame = self.inner.read()?;
        if frame.is_some() {
            self.counter.record_captured();
        }
        Ok(frame)
    }
}

/// The result of a finished bridge session.
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    /// Authoritative end-of-session counters.
    pub metrics: MetricsSnapshot,
    /// Controllable-latency snapshot at end of session.
    pub latency: ControllableLatency,
    /// The failure reason, or `None` for a clean stop.
    pub error: Option<String>,
}

impl SessionOutcome {
    /// An outcome representing an early failure before streaming began.
    fn failed(error: &Error) -> Self {
        Self {
            metrics: MetricsSnapshot::default(),
            latency: ControllableLatency::default(),
            error: Some(error.to_string()),
        }
    }

    /// Whether the session ended cleanly.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }
}

/// A lifecycle event emitted by the bridge worker.
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    /// Target resolved and capture started; audio is now streaming.
    Running,
    /// The session ended, cleanly or with an error.
    Ended(SessionOutcome),
}

/// Owns the bridge worker thread and the UI-facing handles into it.
///
/// Dropping the supervisor requests a stop and joins the worker, so a session
/// can never outlive the UI.
#[derive(Debug)]
pub struct BridgeSupervisor {
    stop: StopHandle,
    live: LiveCounter,
    events: Receiver<BridgeEvent>,
    handle: Option<JoinHandle<()>>,
}

impl BridgeSupervisor {
    /// Spawn a worker that runs a [`Bridge`] for `config`.
    ///
    /// If the operating system refuses to create the worker thread, the failure
    /// is reported as an ordinary [`BridgeEvent::Ended`] terminal event (with no
    /// running handle) rather than panicking, so the UI returns cleanly to idle.
    #[must_use]
    pub fn spawn(config: Config) -> Self {
        let (sender, events) = mpsc::channel();
        let live = LiveCounter::new();
        let bridge = Bridge::new(config);
        let stop = bridge.stop_handle();
        let worker_live = live.clone();
        let sender_on_error = sender.clone();
        let handle = thread::Builder::new()
            .name("sonos-bridge".to_owned())
            .spawn(move || {
                let outcome = run_session(bridge, worker_live, &sender);
                // Always report a terminal event, even on a resolution failure.
                let _ = sender.send(BridgeEvent::Ended(outcome));
            })
            .map_or_else(
                |error| {
                    // The OS refused a new thread: report it as an ordinary
                    // terminal event so the UI returns to idle instead of the
                    // whole process aborting on a panic.
                    let outcome = SessionOutcome::failed(&Error::transport(format!(
                        "failed to spawn the bridge worker thread: {error}"
                    )));
                    let _ = sender_on_error.send(BridgeEvent::Ended(outcome));
                    None
                },
                Some,
            );
        Self {
            stop,
            live,
            events,
            handle,
        }
    }

    /// Request a graceful stop; the worker tears down at the next loop boundary.
    pub fn request_stop(&self) {
        self.stop.stop();
    }

    /// The live count of captured audio blocks for the running session.
    #[must_use]
    pub fn live_captured(&self) -> u64 {
        self.live.captured()
    }

    /// Poll for the next lifecycle event without blocking.
    #[must_use]
    pub fn try_event(&self) -> Option<BridgeEvent> {
        self.events.try_recv().ok()
    }

    /// Whether the worker thread has finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for BridgeSupervisor {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Resolve the target, open capture and run the session, returning the outcome.
///
/// Emits [`BridgeEvent::Running`] just before the blocking run begins; the
/// caller is responsible for sending the terminal [`BridgeEvent::Ended`].
fn run_session(
    mut bridge: Bridge,
    live: LiveCounter,
    sender: &mpsc::Sender<BridgeEvent>,
) -> SessionOutcome {
    let config = bridge.config().clone();

    let control = match resolve_controller(&config.sonos) {
        Ok(control) => control,
        Err(err) => {
            tracing::warn!(%err, "bridge target resolution failed");
            return SessionOutcome::failed(&err);
        }
    };

    let source = match open_source(&config.capture, live) {
        Ok(source) => source,
        Err(err) => {
            tracing::warn!(%err, "audio capture could not be opened");
            return SessionOutcome::failed(&err);
        }
    };

    tracing::info!("bridge session started");
    let _ = sender.send(BridgeEvent::Running);
    let result = bridge.run_with(source, Box::new(control));

    let metrics = MetricsSnapshot::from(bridge.metrics());
    let latency = bridge.controllable_latency();
    let error = result.err().map(|err| {
        tracing::warn!(%err, "bridge session ended with an error");
        err.to_string()
    });
    SessionOutcome {
        metrics,
        latency,
        error,
    }
}

/// Resolve the configured Sonos target into a controller, mirroring the core's
/// `resolve_target` precedence (a device `UUID` wins over a room name).
///
/// # Errors
///
/// Returns [`audio_core::Error::Config`] when no target is configured and
/// [`audio_core::Error::Control`] when the configured target cannot be found.
fn resolve_controller(config: &SonosConfig) -> Result<SonosController> {
    let timeout = Duration::from_secs(u64::from(config.discovery_timeout_secs)).max(MIN_DISCOVERY);
    let seed = config
        .seed_host
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(uuid) = config
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        // Resolve the UUID to its group coordinator: the GUI sets both `room`
        // and `uuid` when a room is picked, and a UUID can point at a
        // non-coordinator member (e.g. one half of a stereo pair) that ignores
        // transport commands.
        return controller_for_uuid(seed, timeout, uuid);
    }

    if let Some(room) = config
        .room
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return controller_for_room(seed, timeout, room);
    }

    Err(Error::config(
        "no Sonos target configured: set a room or device uuid",
    ))
}

/// Open the platform loopback capture for `config`, wrapped in a
/// [`CountingSource`] feeding `counter`.
///
/// # Errors
///
/// Returns [`audio_core::Error::Capture`] when the device cannot be opened.
fn open_source(config: &CaptureConfig, counter: LiveCounter) -> Result<Box<dyn AudioSource>> {
    let capture = sonos_driver_core::capture::LoopbackCapture::open(config)?;
    Ok(Box::new(CountingSource::new(capture, counter)))
}

#[cfg(test)]
mod tests {
    use super::{resolve_controller, CountingSource, SessionOutcome};
    use crate::metrics::LiveCounter;
    use audio_core::{AudioSource, Error, SonosConfig, SyntheticSource};

    #[test]
    fn counting_source_counts_only_non_empty_reads() {
        let counter = LiveCounter::new();
        // 250 frames at 100 frames/read => exactly 3 non-empty reads, then end.
        let inner = SyntheticSource::new(48_000, 1, 440.0, 0.2, 100).with_limit(250);
        let mut source = CountingSource::new(inner, counter.clone());

        let mut reads = 0_u64;
        while source.read().expect("read ok").is_some() {
            reads += 1;
        }
        // The terminal `Ok(None)` must not be counted.
        assert_eq!(counter.captured(), reads);
        assert_eq!(counter.captured(), 3);
    }

    #[test]
    fn resolve_controller_fast_fails_without_a_target() {
        // Default config names neither a uuid nor a room: this must fail with a
        // config error and never touch the network, so the test is hermetic.
        let config = SonosConfig::default();
        assert!(matches!(resolve_controller(&config), Err(Error::Config(_))));
    }

    #[test]
    fn resolve_controller_treats_blank_fields_as_unset() {
        let config = SonosConfig {
            room: Some("   ".to_owned()),
            uuid: Some(String::new()),
            seed_host: Some("  ".to_owned()),
            discovery_timeout_secs: 0,
        };
        assert!(matches!(resolve_controller(&config), Err(Error::Config(_))));
    }

    #[test]
    fn session_outcome_failure_is_unsuccessful() {
        let outcome = SessionOutcome::failed(&Error::config("nope"));
        assert!(!outcome.succeeded());
        assert_eq!(outcome.metrics.captured, 0);
        assert!(outcome.error.is_some());
    }
}
