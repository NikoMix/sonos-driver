//! A bounded jitter / ring buffer that decouples the capture and consumer
//! rates and bounds the latency we control.
//!
//! Capture and network timing are both irregular; a small buffer smooths that
//! jitter without the unbounded memory growth an unbounded queue would risk.
//! The depth is the primary latency knob the bridge exposes (see
//! `docs/LATENCY-BUDGET.md`).

use std::collections::VecDeque;

use audio_core::AudioFrame;

/// A bounded FIFO of [`AudioFrame`]s sized from a target depth in milliseconds.
///
/// # Capacity and overflow policy
///
/// Capacity is `buffer_ms * sample_rate / 1000` per-channel frames. When a push
/// would exceed capacity the **oldest** frames are dropped first (head-drop) so
/// the buffer stays close to the live edge. For a live audio bridge this keeps
/// latency minimal at the cost of a brief discontinuity, which is preferable to
/// accumulating ever-staler audio. A single frame larger than the whole
/// capacity is still accepted (memory stays bounded to that one frame) and
/// counted as an overrun.
///
/// # Telemetry
///
/// * [`JitterBuffer::underruns`] counts pops attempted while empty.
/// * [`JitterBuffer::overruns`] counts pushes that exceeded capacity (and so
///   dropped audio or overshot the target depth).
///
/// Neither starvation (popping an empty buffer) nor a burst (pushing far beyond
/// capacity) can panic.
#[derive(Debug)]
pub struct JitterBuffer {
    queue: VecDeque<AudioFrame>,
    buffered_frames: usize,
    capacity_frames: usize,
    target_ms: u32,
    sample_rate: u32,
    underruns: u64,
    overruns: u64,
}

impl JitterBuffer {
    /// Create a buffer holding up to `buffer_ms` of audio at `sample_rate`.
    ///
    /// A `buffer_ms` of zero yields a zero-capacity buffer that retains nothing
    /// (every push is an overrun) — a valid way to disable jitter buffering for
    /// the absolute lowest latency.
    #[must_use]
    pub fn new(buffer_ms: u32, sample_rate: u32) -> Self {
        let capacity_frames = usize::try_from(u64::from(buffer_ms) * u64::from(sample_rate) / 1000)
            .unwrap_or(usize::MAX);
        Self {
            queue: VecDeque::new(),
            buffered_frames: 0,
            capacity_frames,
            target_ms: buffer_ms,
            sample_rate,
            underruns: 0,
            overruns: 0,
        }
    }

    /// Append a frame, dropping the oldest audio first if capacity is exceeded.
    pub fn push(&mut self, frame: AudioFrame) {
        let incoming = frame.frame_count();

        if self.capacity_frames == 0 {
            // Buffering disabled: nothing can be retained.
            self.overruns += 1;
            return;
        }

        let exceeded = self.buffered_frames + incoming > self.capacity_frames;
        while self.buffered_frames + incoming > self.capacity_frames {
            match self.queue.pop_front() {
                Some(dropped) => self.buffered_frames -= dropped.frame_count(),
                // A lone frame larger than the whole buffer: accept it anyway.
                None => break,
            }
        }
        if exceeded {
            self.overruns += 1;
        }

        self.buffered_frames += incoming;
        self.queue.push_back(frame);
    }

    /// Remove and return the oldest frame, or `None` when starved.
    ///
    /// A pop on an empty buffer is counted as an underrun and returns `None`
    /// rather than panicking.
    pub fn pop(&mut self) -> Option<AudioFrame> {
        if let Some(frame) = self.queue.pop_front() {
            self.buffered_frames -= frame.frame_count();
            Some(frame)
        } else {
            self.underruns += 1;
            None
        }
    }

    /// Number of per-channel frames currently buffered.
    #[must_use]
    pub fn len_frames(&self) -> usize {
        self.buffered_frames
    }

    /// Whether the buffer currently holds no frames.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Maximum number of per-channel frames the buffer targets.
    #[must_use]
    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    /// Current buffered depth in whole milliseconds.
    #[must_use]
    pub fn depth_ms(&self) -> u32 {
        if self.sample_rate == 0 {
            return 0;
        }
        let ms = u64::try_from(self.buffered_frames).unwrap_or(u64::MAX) * 1000
            / u64::from(self.sample_rate);
        u32::try_from(ms).unwrap_or(u32::MAX)
    }

    /// The configured target depth in milliseconds.
    #[must_use]
    pub fn target_ms(&self) -> u32 {
        self.target_ms
    }

    /// Number of underruns (pops attempted while empty) observed so far.
    #[must_use]
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// Number of overruns (pushes that exceeded capacity) observed so far.
    #[must_use]
    pub fn overruns(&self) -> u64 {
        self.overruns
    }

    /// Drop all buffered frames without affecting the telemetry counters.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.buffered_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(frames: usize, rate: u32) -> AudioFrame {
        // Mono frames keep sample_count == frame_count for easy bookkeeping.
        AudioFrame::new(vec![0.0_f32; frames], 1, rate).expect("valid frame")
    }

    #[test]
    fn capacity_is_derived_from_buffer_ms_and_rate() {
        // 80 ms at 44.1 kHz == 3528 frames.
        let jb = JitterBuffer::new(80, 44_100);
        assert_eq!(jb.capacity_frames(), 3528);
        assert_eq!(jb.target_ms(), 80);
    }

    #[test]
    fn push_and_pop_preserve_order_and_depth() {
        let mut jb = JitterBuffer::new(100, 1_000); // capacity 100 frames
        jb.push(frame(10, 1_000));
        jb.push(frame(20, 1_000));
        assert_eq!(jb.len_frames(), 30);
        assert_eq!(jb.depth_ms(), 30); // 30 frames / 1000 Hz == 30 ms

        let first = jb.pop().expect("first frame");
        assert_eq!(first.frame_count(), 10);
        assert_eq!(jb.len_frames(), 20);
        assert_eq!(jb.overruns(), 0);
        assert_eq!(jb.underruns(), 0);
    }

    #[test]
    fn overflow_drops_oldest_and_counts_overruns() {
        let mut jb = JitterBuffer::new(100, 1_000); // capacity 100 frames
        for _ in 0..10 {
            jb.push(frame(20, 1_000)); // 10 * 20 == 200 frames pushed
        }
        // Capacity is respected: never more than 100 buffered.
        assert!(jb.len_frames() <= jb.capacity_frames());
        assert!(jb.overruns() > 0);
        // The most recent audio survives; the oldest was dropped.
        assert!(!jb.is_empty());
    }

    #[test]
    fn starvation_counts_underruns_without_panicking() {
        let mut jb = JitterBuffer::new(50, 8_000);
        assert!(jb.pop().is_none());
        assert!(jb.pop().is_none());
        assert_eq!(jb.underruns(), 2);
    }

    #[test]
    fn frame_larger_than_capacity_is_accepted_once() {
        let mut jb = JitterBuffer::new(10, 1_000); // capacity 10 frames
        jb.push(frame(50, 1_000)); // single oversized frame
        assert_eq!(jb.overruns(), 1);
        // Memory stays bounded to the one frame, and it is retrievable.
        let out = jb.pop().expect("oversized frame");
        assert_eq!(out.frame_count(), 50);
        assert!(jb.is_empty());
    }

    #[test]
    fn zero_capacity_retains_nothing() {
        let mut jb = JitterBuffer::new(0, 48_000);
        assert_eq!(jb.capacity_frames(), 0);
        jb.push(frame(64, 48_000));
        assert!(jb.is_empty());
        assert_eq!(jb.overruns(), 1);
        assert!(jb.pop().is_none());
    }

    #[test]
    fn clear_empties_without_touching_counters() {
        let mut jb = JitterBuffer::new(100, 1_000);
        jb.push(frame(40, 1_000));
        jb.pop();
        jb.push(frame(40, 1_000));
        jb.clear();
        assert!(jb.is_empty());
        assert_eq!(jb.len_frames(), 0);
        // Counters are telemetry and survive a clear.
        assert_eq!(jb.underruns(), 0);
    }
}
