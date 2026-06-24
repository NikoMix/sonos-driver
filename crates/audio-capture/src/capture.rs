//! The safe [`LoopbackCapture`] handle and its capture-thread plumbing.
//!
//! This module owns everything *except* the WASAPI FFI: the bounded channel that
//! carries captured frames, the shared stop flag / live-format / metrics, the
//! [`audio_core::AudioSource`] state machine, and clean thread shutdown. The
//! actual COM/FFI work runs on threads spawned by [`crate::wasapi`] (Windows
//! only); nothing here is `unsafe`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use audio_core::{AudioFormat, AudioFrame, AudioSource, CaptureConfig, Result};

/// Capacity, in blocks, of the capture → consumer channel. WASAPI signals roughly
/// every engine period (~10 ms), so ~64 blocks is well over half a second of
/// slack — ample for a healthy consumer while bounding memory. When it fills, the
/// capture thread drops the newest block and counts an overrun rather than
/// blocking the time-critical thread (which would cause glitches and clock drift).
pub(crate) const CHANNEL_CAPACITY: usize = 64;

/// Item carried over the capture channel: a captured frame or a terminal error.
pub(crate) type CaptureItem = Result<AudioFrame>;

/// State shared between the public handle and the capture / keep-alive threads.
pub(crate) struct SharedState {
    stop: AtomicBool,
    format: Mutex<AudioFormat>,
    overruns: AtomicU64,
    reconnects: AtomicU64,
}

impl SharedState {
    pub(crate) fn new(format: AudioFormat) -> Self {
        Self {
            stop: AtomicBool::new(false),
            format: Mutex::new(format),
            overruns: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
        }
    }

    /// Whether the consumer has asked the capture threads to wind down.
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Request that the capture threads stop at the next opportunity.
    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Publish the live endpoint format (updated on (re)initialisation).
    pub(crate) fn set_format(&self, format: AudioFormat) {
        match self.format.lock() {
            Ok(mut guard) => *guard = format,
            Err(poisoned) => *poisoned.into_inner() = format,
        }
    }

    /// Read the live endpoint format.
    pub(crate) fn format(&self) -> AudioFormat {
        match self.format.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// Record that a block was dropped because the channel was full.
    pub(crate) fn record_overrun(&self) {
        self.overruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that the capture client was reinitialised (device/format change).
    pub(crate) fn record_reconnect(&self) {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot of the dropped-block (overrun) counter.
    pub(crate) fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Snapshot of the reinitialisation (reconnect) counter.
    pub(crate) fn reconnects(&self) -> u64 {
        self.reconnects.load(Ordering::Relaxed)
    }
}

/// A handle to a WASAPI loopback capture session.
///
/// Construct one with [`LoopbackCapture::open`] and pull audio by repeatedly
/// calling [`AudioSource::read`]. Dropping the handle stops the capture and
/// keep-alive threads and joins them.
pub struct LoopbackCapture {
    receiver: Receiver<CaptureItem>,
    shared: Arc<SharedState>,
    workers: Vec<JoinHandle<()>>,
    ended: bool,
}

impl LoopbackCapture {
    /// Open a loopback capture session for the endpoint selected by `config`.
    ///
    /// The endpoint is chosen from [`CaptureConfig::device`] (an endpoint id or a
    /// friendly-name substring), falling back to the current default render
    /// device when it is `None`. Capture runs event-driven on a dedicated
    /// time-critical thread, and a silence keep-alive renderer keeps the engine
    /// active so frames keep flowing even while the system is otherwise idle.
    ///
    /// # Errors
    ///
    /// Returns [`audio_core::Error::Capture`] when the requested device cannot be
    /// found or the WASAPI client cannot be initialised, and
    /// [`audio_core::Error::InvalidFormat`] when the endpoint mix format cannot be
    /// converted to canonical `f32`. On non-Windows targets it always returns an
    /// [`audio_core::Error::Capture`] explaining the platform requirement.
    pub fn open(config: &CaptureConfig) -> Result<Self> {
        Self::open_impl(config.device.as_deref())
    }

    #[cfg(windows)]
    fn open_impl(selector: Option<&str>) -> Result<Self> {
        crate::wasapi::open_capture(selector)
    }

    #[cfg(not(windows))]
    fn open_impl(_selector: Option<&str>) -> Result<Self> {
        Err(audio_core::Error::capture(
            "WASAPI loopback capture is only available on Windows",
        ))
    }

    /// Assemble a handle from already-spawned worker threads and shared state.
    pub(crate) fn from_parts(
        receiver: Receiver<CaptureItem>,
        shared: Arc<SharedState>,
        workers: Vec<JoinHandle<()>>,
    ) -> Self {
        Self {
            receiver,
            shared,
            workers,
            ended: false,
        }
    }

    /// Number of blocks dropped so far because the consumer fell behind.
    ///
    /// A persistently rising value means the downstream pipeline is not draining
    /// frames fast enough; it is exposed for the orchestration layer's metrics.
    #[must_use]
    pub fn overrun_count(&self) -> u64 {
        self.shared.overruns()
    }

    /// Number of times the capture client was reinitialised after a device or
    /// format change (e.g. a default-device switch).
    #[must_use]
    pub fn reconnect_count(&self) -> u64 {
        self.shared.reconnects()
    }
}

impl AudioSource for LoopbackCapture {
    fn format(&self) -> AudioFormat {
        self.shared.format()
    }

    fn read(&mut self) -> Result<Option<AudioFrame>> {
        if self.ended {
            return Ok(None);
        }
        match self.receiver.recv() {
            Ok(Ok(frame)) => Ok(Some(frame)),
            Ok(Err(err)) => {
                self.ended = true;
                Err(err)
            }
            // Every sender dropped: the capture threads have finished, which is a
            // graceful end of stream.
            Err(_) => {
                self.ended = true;
                Ok(None)
            }
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        self.shared.request_stop();
        // The capture/keep-alive threads use timeout-bounded waits and never block
        // on the channel, so they observe the stop flag promptly; join to ensure
        // no COM work outlives this handle.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        tracing::debug!(
            overruns = self.shared.overruns(),
            reconnects = self.shared.reconnects(),
            "loopback capture stopped"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use audio_core::{Error, SampleFormat};

    use super::*;

    fn handle_with_channel() -> (std::sync::mpsc::SyncSender<CaptureItem>, LoopbackCapture) {
        let (tx, rx) = sync_channel::<CaptureItem>(4);
        let shared = Arc::new(SharedState::new(AudioFormat::studio_stereo()));
        let capture = LoopbackCapture::from_parts(rx, shared, Vec::new());
        (tx, capture)
    }

    fn frame() -> AudioFrame {
        AudioFrame::new(vec![0.0_f32; 4], 2, 48_000).expect("valid frame")
    }

    #[test]
    fn reports_seeded_format() {
        let (_tx, capture) = handle_with_channel();
        let fmt = capture.format();
        assert_eq!(fmt.sample_format, SampleFormat::F32);
        assert_eq!(fmt.sample_rate, 48_000);
        assert_eq!(fmt.channels, 2);
    }

    #[test]
    fn read_yields_sent_frames() {
        let (tx, mut capture) = handle_with_channel();
        tx.send(Ok(frame())).expect("send");
        let got = capture.read().expect("ok").expect("some");
        assert_eq!(got.frame_count(), 2);
    }

    #[test]
    fn error_is_surfaced_then_stream_ends() {
        let (tx, mut capture) = handle_with_channel();
        tx.send(Err(Error::capture("device exploded")))
            .expect("send");
        assert!(matches!(capture.read(), Err(Error::Capture(_))));
        // After a terminal error the source is permanently ended.
        assert!(capture.read().expect("ok").is_none());
    }

    #[test]
    fn dropping_all_senders_is_graceful_end_of_stream() {
        let (tx, mut capture) = handle_with_channel();
        drop(tx);
        assert!(capture.read().expect("ok").is_none());
    }

    #[test]
    fn metrics_start_at_zero_and_track_shared_state() {
        let (_tx, capture) = handle_with_channel();
        assert_eq!(capture.overrun_count(), 0);
        assert_eq!(capture.reconnect_count(), 0);
        capture.shared.record_overrun();
        capture.shared.record_reconnect();
        capture.shared.record_reconnect();
        assert_eq!(capture.overrun_count(), 1);
        assert_eq!(capture.reconnect_count(), 2);
    }
}
