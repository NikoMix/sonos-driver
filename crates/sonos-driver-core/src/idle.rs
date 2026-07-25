//! Silence tracking, so an idle Windows machine hands the speakers back.
//!
//! Without this the bridge is antisocial: every room it serves shows "Windows"
//! as its source around the clock, and playing anything from the Sonos app
//! means kicking us off first. With eight rooms bound, that is the entire
//! household permanently occupied by a PC that is not making a sound.
//!
//! [`IdleTracker`] is written by the encode thread (once per captured block)
//! and read by the session supervisor, communicating only through atomics so
//! neither side can block the other. It reports *state changes* rather than
//! state, because the supervisor's job is to issue a transport command exactly
//! once per transition, not on every poll.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use audio_core::AudioFrame;

/// How long to wait before retrying a resume that the player rejected.
const RESUME_RETRY: Duration = Duration::from_secs(2);

/// Shared silence state for one room session.
#[derive(Debug)]
pub(crate) struct IdleTracker {
    /// Monotonic baseline; all stored instants are offsets from it, so they fit
    /// in an atomic and need no lock.
    epoch: Instant,
    /// Milliseconds since `epoch` at the last block that was not silent.
    last_audible_ms: AtomicU64,
    /// Peak magnitude at or above which a block counts as audible.
    threshold: f32,
    /// How long silence must last before the room is released. `None` disables
    /// idle release entirely.
    stop_after: Option<Duration>,
    /// Whether the room is currently released. Only the supervisor writes it.
    released: AtomicBool,
    /// Monotonic count of audible blocks. Compared against
    /// [`IdleTracker::released_seq`] to decide that sound is genuinely *back*,
    /// rather than comparing timestamps: at 20 Hz polling a release and the
    /// audible block that ends it routinely land in the same millisecond.
    audible_seq: AtomicU64,
    /// `audible_seq` as it stood when the room was released.
    released_seq: AtomicU64,
    /// Earliest time, in milliseconds since `epoch`, at which a resume may be
    /// issued. Non-zero only while backing off from a failed resume.
    resume_not_before_ms: AtomicU64,
}

/// What the supervisor should do about the room, this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdleAction {
    /// Nothing changed.
    None,
    /// Silence has lasted long enough; stop the transport.
    Release,
    /// Sound is back; point the player at the stream and play again.
    Resume,
}

impl IdleTracker {
    /// Create a tracker that releases the room after `stop_after` of silence.
    pub(crate) fn new(stop_after: Option<Duration>, threshold: f32) -> Self {
        Self {
            epoch: Instant::now(),
            last_audible_ms: AtomicU64::new(0),
            threshold,
            stop_after,
            released: AtomicBool::new(false),
            audible_seq: AtomicU64::new(0),
            released_seq: AtomicU64::new(0),
            resume_not_before_ms: AtomicU64::new(0),
        }
    }

    /// Whether idle release is switched on at all.
    pub(crate) fn is_enabled(&self) -> bool {
        self.stop_after.is_some()
    }

    /// Record one captured block.
    ///
    /// Cheap enough to run on every block: a peak scan is a linear pass over
    /// ~4 k samples with no allocation, perhaps 2 µs, against a block period of
    /// 10-20 ms.
    pub(crate) fn observe(&self, frame: &AudioFrame) {
        if self.stop_after.is_none() || frame.sample_count() == 0 {
            return;
        }
        let audible = frame
            .samples()
            .iter()
            .any(|sample| sample.abs() >= self.threshold);
        if audible {
            self.mark_audible();
        }
    }

    /// Note that audio is present right now.
    fn mark_audible(&self) {
        let now = self.millis_since_epoch();
        // Monotonic max rather than a plain store: `observe` runs on the encode
        // thread while `poll` runs on the supervisor, and a stale value must
        // never move the clock backwards.
        self.last_audible_ms.fetch_max(now, Ordering::Relaxed);
        self.audible_seq.fetch_add(1, Ordering::Relaxed);
    }

    /// How long the stream has been silent.
    pub(crate) fn silent_for(&self) -> Duration {
        let last = self.last_audible_ms.load(Ordering::Relaxed);
        Duration::from_millis(self.millis_since_epoch().saturating_sub(last))
    }

    /// Whether the room is currently released.
    pub(crate) fn is_released(&self) -> bool {
        self.released.load(Ordering::Relaxed)
    }

    /// Decide what the supervisor should do, and record the decision.
    ///
    /// Returns a transition at most once per state change, so a caller that
    /// polls at 20 Hz still issues exactly one `Stop` and one `Play` per quiet
    /// spell.
    pub(crate) fn poll(&self) -> IdleAction {
        let Some(stop_after) = self.stop_after else {
            return IdleAction::None;
        };

        if self.released.load(Ordering::Relaxed) {
            // Resume on the first audible block recorded after the release --
            // an event, not a time window, so there is no debounce to clip the
            // start of a notification sound and no way to mistake the silence
            // that caused the release for the sound that ends it.
            let heard_since_release = self.audible_seq.load(Ordering::Relaxed)
                > self.released_seq.load(Ordering::Relaxed);
            let backoff_elapsed =
                self.millis_since_epoch() >= self.resume_not_before_ms.load(Ordering::Relaxed);
            if heard_since_release && backoff_elapsed {
                self.released.store(false, Ordering::Relaxed);
                return IdleAction::Resume;
            }
        } else if self.silent_for() >= stop_after {
            self.released_seq
                .store(self.audible_seq.load(Ordering::Relaxed), Ordering::Relaxed);
            self.resume_not_before_ms.store(0, Ordering::Relaxed);
            self.released.store(true, Ordering::Relaxed);
            return IdleAction::Release;
        }

        IdleAction::None
    }

    /// Force the tracker back into the "playing" state.
    ///
    /// Used when the supervisor re-points the player for its own reasons (a
    /// dropped client), so the two mechanisms cannot disagree about whether the
    /// transport is running.
    pub(crate) fn mark_resumed(&self) {
        self.released.store(false, Ordering::Relaxed);
        self.mark_audible();
    }

    /// Put the tracker back into the released state after a failed resume, so
    /// the next audible poll tries again.
    ///
    /// Without this a single failed `Play` — a speaker briefly off the network,
    /// say — would leave the room silently released until the service restarts.
    /// The retry point is pushed [`RESUME_RETRY`] into the future so a room that
    /// is unreachable while music plays is retried every couple of seconds, not
    /// on every 50 ms poll.
    pub(crate) fn mark_released_for_retry(&self) {
        let retry_at = self
            .millis_since_epoch()
            .saturating_add(u64::try_from(RESUME_RETRY.as_millis()).unwrap_or(u64::MAX));
        self.released_seq
            .store(self.audible_seq.load(Ordering::Relaxed), Ordering::Relaxed);
        self.resume_not_before_ms.store(retry_at, Ordering::Relaxed);
        self.released.store(true, Ordering::Relaxed);
    }

    fn millis_since_epoch(&self) -> u64 {
        // Saturating: a session running for 584 million years is not a case
        // worth a panic.
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(peak: f32) -> AudioFrame {
        AudioFrame::new(vec![peak, -peak, peak, -peak], 2, 44_100).expect("valid frame")
    }

    fn tracker(stop_after_ms: u64) -> IdleTracker {
        IdleTracker::new(Some(Duration::from_millis(stop_after_ms)), 1.0e-6)
    }

    #[test]
    fn a_disabled_tracker_never_releases() {
        let tracker = IdleTracker::new(None, 1.0e-6);
        assert!(!tracker.is_enabled());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(tracker.poll(), IdleAction::None);
        assert!(!tracker.is_released());
    }

    #[test]
    fn silence_releases_the_room_once() {
        let tracker = tracker(30);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);
        assert!(tracker.is_released());
        // Still silent: no second Stop.
        assert_eq!(tracker.poll(), IdleAction::None);
    }

    #[test]
    fn audio_resumes_the_room_once() {
        let tracker = tracker(30);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);

        tracker.observe(&frame(0.5));
        assert_eq!(tracker.poll(), IdleAction::Resume);
        assert!(!tracker.is_released());
        assert_eq!(tracker.poll(), IdleAction::None);
    }

    #[test]
    fn audio_below_the_threshold_does_not_count_as_sound() {
        let tracker = tracker(30);
        tracker.observe(&frame(1.0e-9));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);
    }

    #[test]
    fn continuous_audio_never_releases() {
        let tracker = tracker(30);
        for _ in 0..6 {
            tracker.observe(&frame(0.25));
            std::thread::sleep(Duration::from_millis(10));
            assert_eq!(tracker.poll(), IdleAction::None);
        }
        assert!(!tracker.is_released());
    }

    #[test]
    fn empty_frames_do_not_count_as_audio() {
        let tracker = tracker(30);
        let empty = AudioFrame::new(Vec::new(), 2, 44_100).expect("valid empty frame");
        tracker.observe(&empty);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);
    }

    #[test]
    fn mark_resumed_clears_a_release() {
        let tracker = tracker(30);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);
        tracker.mark_resumed();
        assert!(!tracker.is_released());
        assert_eq!(tracker.poll(), IdleAction::None);
    }

    #[test]
    fn a_failed_resume_is_retried_but_not_on_every_poll() {
        let tracker = tracker(30);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);

        tracker.observe(&frame(0.5));
        assert_eq!(tracker.poll(), IdleAction::Resume);

        // The supervisor's Play failed; go back to released.
        tracker.mark_released_for_retry();
        assert!(tracker.is_released());

        // Audio is still playing, but the retry gap has not elapsed, so the
        // player is not hammered once per poll.
        for _ in 0..5 {
            tracker.observe(&frame(0.5));
            assert_eq!(tracker.poll(), IdleAction::None);
        }
        assert!(tracker.is_released());
    }

    #[test]
    fn a_release_is_not_immediately_undone_by_its_own_silence() {
        // Regression: inferring "audio is back" from a short silent_for window
        // made the tracker resume on the very next poll after releasing,
        // because the silence that triggered the release was itself recent.
        let tracker = tracker(30);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(tracker.poll(), IdleAction::Release);
        for _ in 0..5 {
            assert_eq!(tracker.poll(), IdleAction::None);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(tracker.is_released());
    }
}
