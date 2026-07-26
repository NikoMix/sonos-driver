//! Sample-rate conversion built on `rubato`'s FFT synchronous resampler.
//!
//! The capture and Sonos target rates are each fixed for the lifetime of a
//! session, so a *synchronous* (fixed-ratio) resampler is the ideal fit: it is
//! the highest-quality option `rubato` offers and is considerably faster than a
//! sinc resampler, keeping the controllable slice of the latency budget small
//! (see `docs/LATENCY-BUDGET.md`).
//!
//! [`ResampleStage`] keeps the resampler's filter state across `process` calls
//! and adapts arbitrary, variable-sized input blocks onto the resampler's fixed
//! input chunk by carrying a small accumulator between calls.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler, WindowFunction};

use audio_core::{Error, Result};

/// Target size of the resampler's fixed input chunk, in per-channel frames.
///
/// ~5 ms at 48 kHz: small enough to keep the latency we control low while
/// staying comfortably above `rubato`'s minimum FFT block size. The dominant
/// end-to-end latency term is the fixed Sonos buffer, not this chunk.
const CHUNK_FRAMES: usize = 256;

/// How many sub-chunks `rubato` divides each processing chunk into. One keeps
/// the internal FFT block — and therefore the added delay — as small as the
/// input/output rate pair allows.
const SUB_CHUNKS: usize = 1;

/// A stateful resampler converting one fixed input rate to one fixed output
/// rate while preserving the canonical interleaved-`f32` layout.
pub(crate) struct ResampleStage {
    resampler: Fft<f32>,
    input_rate: u32,
    output_rate: u32,
    channels: usize,
    /// Interleaved input samples awaiting a full chunk, carried across calls.
    pending: Vec<f32>,
    /// Reusable interleaved output scratch, sized to the per-chunk maximum so
    /// the hot path performs no per-call allocation for resampler output.
    scratch: Vec<f32>,
    scratch_frames: usize,
}

// `rubato::Fft` does not implement `Debug`; summarise the stage's
// configuration instead so containing types can still derive `Debug`.
impl core::fmt::Debug for ResampleStage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResampleStage")
            .field("input_rate", &self.input_rate)
            .field("output_rate", &self.output_rate)
            .field("channels", &self.channels)
            .field("pending_samples", &self.pending.len())
            .field("scratch_frames", &self.scratch_frames)
            .finish_non_exhaustive()
    }
}

impl ResampleStage {
    /// Build a resampler for `input_rate` -> `output_rate` with `channels`
    /// interleaved channels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Pipeline`] if `rubato` rejects the parameters (e.g. a
    /// zero sample rate or channel count, already excluded by [`audio_core`]).
    pub(crate) fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self> {
        let in_hz = usize::try_from(input_rate)
            .map_err(|_| Error::pipeline("input sample rate exceeds platform word size"))?;
        let out_hz = usize::try_from(output_rate)
            .map_err(|_| Error::pipeline("output sample rate exceeds platform word size"))?;
        // `new_custom` rather than `new`: rubato 4's simplified constructor
        // derives the sub-chunk count from the chunk size (`chunk/256`), which
        // for today's 256-frame chunk happens to give the same 1 we want. That
        // is a coincidence, not an agreement — raising CHUNK_FRAMES would
        // silently raise the sub-chunk count and with it the added delay, which
        // is exactly what SUB_CHUNKS exists to pin down. The window is stated
        // for the same reason: it is the default, but a default is not a
        // promise. Both values match what rubato 3 used, so the audio path is
        // unchanged by the upgrade.
        let resampler = Fft::<f32>::new_custom(
            in_hz,
            out_hz,
            CHUNK_FRAMES,
            SUB_CHUNKS,
            channels,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|e| Error::pipeline(format!("failed to construct resampler: {e}")))?;
        let scratch_frames = resampler.output_frames_max();
        Ok(Self {
            resampler,
            input_rate,
            output_rate,
            channels,
            pending: Vec::with_capacity(CHUNK_FRAMES * channels * 2),
            scratch: vec![0.0; scratch_frames * channels],
            scratch_frames,
        })
    }

    /// Whether this stage already targets the given input rate and channel
    /// count (so it can be reused rather than rebuilt).
    pub(crate) fn matches(&self, input_rate: u32, channels: usize) -> bool {
        self.input_rate == input_rate && self.channels == channels
    }

    /// The output (target) sample rate this stage produces.
    pub(crate) fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// The resampler's group delay expressed in output frames, useful for
    /// latency accounting and test alignment.
    pub(crate) fn output_delay(&self) -> usize {
        self.resampler.output_delay()
    }

    /// Feed one interleaved input block and return every output sample that
    /// could be produced from the accumulated input so far.
    ///
    /// The returned vector may be empty while the resampler fills its internal
    /// buffers, and may span several input chunks once enough has accumulated.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Pipeline`] if `rubato` reports a processing error.
    pub(crate) fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        self.pending.extend_from_slice(input);

        let chunk = self.resampler.input_frames_next();
        let available = self.pending.len() / self.channels;
        if available < chunk {
            return Ok(Vec::new());
        }

        // Upper bound on output frames: chunks to run * per-chunk maximum.
        let chunks = available / chunk;
        let mut out = Vec::with_capacity(chunks * self.scratch_frames * self.channels);
        let mut consumed = 0usize;
        while available - consumed >= chunk {
            let written = run_chunk(
                &mut self.resampler,
                &self.pending,
                self.channels,
                available,
                consumed,
                &mut self.scratch,
                self.scratch_frames,
            )?;
            out.extend_from_slice(&self.scratch[..written * self.channels]);
            consumed += chunk;
        }
        self.pending.drain(..consumed * self.channels);
        Ok(out)
    }
}

/// Resample a single fixed-size chunk at `input_offset` into `scratch`,
/// returning the number of output frames written.
fn run_chunk(
    resampler: &mut Fft<f32>,
    input: &[f32],
    channels: usize,
    input_frames: usize,
    input_offset: usize,
    scratch: &mut [f32],
    scratch_frames: usize,
) -> Result<usize> {
    let input_adapter = InterleavedSlice::new(input, channels, input_frames)
        .map_err(|e| Error::pipeline(format!("input adapter: {e}")))?;
    let mut output_adapter = InterleavedSlice::new_mut(scratch, channels, scratch_frames)
        .map_err(|e| Error::pipeline(format!("output adapter: {e}")))?;
    let indexing = Indexing {
        input_offset,
        output_offset: 0,
        active_channels_mask: None,
        partial_len: None,
    };
    let (_read, written) = resampler
        .process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing))
        .map_err(|e| Error::pipeline(format!("resample: {e}")))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    // DSP verification converts freely between integer sample indices/counts and
    // floating point. All counts here stay well under 2^24 (≤ a few seconds of
    // audio), so f32/f64 conversions are exact; the cast lints are pure noise in
    // this test-only analysis code.
    #![allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use super::*;
    use std::f64::consts::PI;

    fn make_sine(frames: usize, rate: u32, freq: f64) -> Vec<f32> {
        let step = 2.0 * PI * freq / f64::from(rate);
        (0..frames)
            .map(|n| (step * n as f64).sin() as f32)
            .collect()
    }

    /// Fraction of total signal energy concentrated at `freq` via the Goertzel
    /// algorithm. Returns ~1.0 for a clean tone and drops as distortion/noise
    /// (THD+N) rises, so it doubles as a fidelity bound.
    fn energy_fraction_at(signal: &[f32], rate: u32, freq: f64) -> f64 {
        let n = signal.len();
        assert!(n > 0, "empty signal");
        let w = 2.0 * PI * freq / f64::from(rate);
        let coeff = 2.0 * w.cos();
        let (mut s_prev, mut s_prev2) = (0.0_f64, 0.0_f64);
        let mut total = 0.0_f64;
        for &x in signal {
            let xf = f64::from(x);
            let s = xf + coeff * s_prev - s_prev2;
            s_prev2 = s_prev;
            s_prev = s;
            total += xf * xf;
        }
        let power = s_prev * s_prev + s_prev2 * s_prev2 - coeff * s_prev * s_prev2;
        if total == 0.0 {
            return 0.0;
        }
        // Both the +k and -k DFT bins carry the tone, hence the factor of two.
        2.0 * power / (n as f64 * total)
    }

    /// Drive `input` through the stage in variable-sized blocks (exercising the
    /// cross-call accumulator) and return the concatenated output.
    fn run_variable_blocks(stage: &mut ResampleStage, input: &[f32], channels: usize) -> Vec<f32> {
        let block_frames = [200usize, 256, 300, 100, 512];
        let mut out = Vec::new();
        let mut offset = 0usize;
        let total_frames = input.len() / channels;
        let mut i = 0usize;
        while offset < total_frames {
            let take = block_frames[i % block_frames.len()].min(total_frames - offset);
            let start = offset * channels;
            let end = (offset + take) * channels;
            out.extend(stage.process(&input[start..end]).expect("process"));
            offset += take;
            i += 1;
        }
        out
    }

    fn assert_tone_preserved(in_rate: u32, out_rate: u32) {
        let freq = 997.0; // A prime-ish tone that lands between FFT bins.
        let in_frames = in_rate as usize; // one second of audio.
        let input = make_sine(in_frames, in_rate, freq);

        let mut stage = ResampleStage::new(in_rate, out_rate, 1).expect("stage");
        let output = run_variable_blocks(&mut stage, &input, 1);

        // Frame count tracks the rate ratio within one chunk + group delay of
        // slack (FixedSync::Input holds back a partial chunk and the delay).
        let expected = in_frames as f64 * f64::from(out_rate) / f64::from(in_rate);
        let slack = (2 * CHUNK_FRAMES + stage.output_delay() + 16) as f64;
        let produced = output.len() as f64;
        assert!(
            (produced - expected).abs() <= slack,
            "{in_rate}->{out_rate}: expected ~{expected} frames, produced {produced}",
        );

        // Analyse a steady-state interior window, skipping the resampler's
        // startup transient and the trailing partial region.
        let skip = stage.output_delay() + CHUNK_FRAMES;
        let tail = CHUNK_FRAMES;
        assert!(
            output.len() > skip + tail + out_rate as usize / 8,
            "not enough output to analyse",
        );
        let window = &output[skip..output.len() - tail];
        let fraction = energy_fraction_at(window, out_rate, freq);
        assert!(
            fraction > 0.99,
            "{in_rate}->{out_rate}: tone energy fraction {fraction:.5} below bound (distortion too high)",
        );
    }

    #[test]
    fn downsample_48k_to_44k1_preserves_tone() {
        assert_tone_preserved(48_000, 44_100);
    }

    #[test]
    fn upsample_44k1_to_48k_preserves_tone() {
        assert_tone_preserved(44_100, 48_000);
    }

    #[test]
    fn stage_reports_matching_configuration() {
        let stage = ResampleStage::new(48_000, 44_100, 2).expect("stage");
        assert!(stage.matches(48_000, 2));
        assert!(!stage.matches(44_100, 2));
        assert!(!stage.matches(48_000, 1));
        assert_eq!(stage.output_rate(), 44_100);
    }

    #[test]
    fn small_blocks_buffer_until_a_chunk_is_available() {
        let mut stage = ResampleStage::new(48_000, 44_100, 1).expect("stage");
        // A block smaller than one chunk yields no output yet.
        let tiny = make_sine(64, 48_000, 440.0);
        assert!(stage.process(&tiny).expect("process").is_empty());
        // Once enough has accumulated, output appears.
        let more = make_sine(CHUNK_FRAMES * 2, 48_000, 440.0);
        assert!(!stage.process(&more).expect("process").is_empty());
    }

    #[test]
    fn stereo_is_resampled_per_channel() {
        let frames = 48_000usize;
        let left = make_sine(frames, 48_000, 600.0);
        let mut interleaved = Vec::with_capacity(frames * 2);
        for &s in &left {
            interleaved.push(s); // L
            interleaved.push(s); // R (identical channels)
        }
        let mut stage = ResampleStage::new(48_000, 44_100, 2).expect("stage");
        let output = run_variable_blocks(&mut stage, &interleaved, 2);
        assert_eq!(output.len() % 2, 0, "output stays interleaved stereo");

        // Deinterleave channel 0 and confirm the tone survived.
        let ch0: Vec<f32> = output.iter().step_by(2).copied().collect();
        let skip = stage.output_delay() + CHUNK_FRAMES;
        let window = &ch0[skip..ch0.len() - CHUNK_FRAMES];
        assert!(energy_fraction_at(window, 44_100, 600.0) > 0.99);
    }
}
