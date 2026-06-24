//! Channel-layout conversion on the canonical interleaved-`f32` frame.
//!
//! Sonos targets stereo, while a capture endpoint may be mono, stereo or a
//! surround layout. [`convert_channels`] maps between layouts while keeping the
//! interleaved-`f32` [`AudioFrame`] invariant intact.

use audio_core::{AudioFrame, Error, Result};

/// Convert `frame` to `target_channels`, preserving the interleaved-`f32`
/// representation and the per-channel frame count.
///
/// The down/up-mix policy is intentionally simple and well defined:
///
/// * **Equal counts** — the frame is returned unchanged.
/// * **To mono** (`target_channels == 1`) — the source channels are averaged
///   (an equal-weight downmix), avoiding the clipping a plain sum can cause.
/// * **From mono** (source is mono) — the single channel is broadcast to every
///   output channel.
/// * **To stereo from a wider layout** (`target_channels == 2`, source ≥ 3) —
///   the first two channels (front left / front right) are taken. This is the
///   conventional, low-cost stereo fold-down for surround content.
/// * **Any other change** — a best-effort map that copies the first
///   `min(source, target)` channels through and fills any additional output
///   channels with silence; surplus input channels are dropped.
///
/// # Errors
///
/// Returns [`Error::Pipeline`] if `target_channels` is zero or the rebuilt
/// frame fails [`AudioFrame`] validation.
pub fn convert_channels(frame: &AudioFrame, target_channels: u16) -> Result<AudioFrame> {
    if target_channels == 0 {
        return Err(Error::pipeline("target channel count must be non-zero"));
    }

    let source_channels = frame.channels();
    let sample_rate = frame.sample_rate();
    if source_channels == target_channels {
        return AudioFrame::new(frame.samples().to_vec(), source_channels, sample_rate);
    }

    let src = usize::from(source_channels);
    let dst = usize::from(target_channels);
    let frames = frame.frame_count();
    let input = frame.samples();
    let mut output = vec![0.0_f32; frames * dst];

    match (source_channels, target_channels) {
        // Downmix every source channel to a single averaged mono channel.
        (_, 1) => {
            let scale = 1.0 / f32::from(source_channels);
            for (f, slot) in output.iter_mut().enumerate() {
                let base = f * src;
                let sum: f32 = input[base..base + src].iter().sum();
                *slot = sum * scale;
            }
        }
        // Broadcast the single mono channel to all output channels.
        (1, _) => {
            for (f, sample) in input.iter().enumerate() {
                let base = f * dst;
                for slot in &mut output[base..base + dst] {
                    *slot = *sample;
                }
            }
        }
        // Fold a wider layout down to the front stereo pair.
        (_, 2) => {
            for f in 0..frames {
                let in_base = f * src;
                let out_base = f * 2;
                output[out_base] = input[in_base];
                output[out_base + 1] = input[in_base + 1];
            }
        }
        // General best-effort: pass the common channels through, silence extras.
        _ => {
            let common = src.min(dst);
            for f in 0..frames {
                let in_base = f * src;
                let out_base = f * dst;
                output[out_base..out_base + common]
                    .copy_from_slice(&input[in_base..in_base + common]);
            }
        }
    }

    AudioFrame::new(output, target_channels, sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_layout_is_unchanged() {
        let frame = AudioFrame::new(vec![0.1, -0.2, 0.3, -0.4], 2, 48_000).expect("frame");
        let out = convert_channels(&frame, 2).expect("converted");
        assert_eq!(out.channels(), 2);
        assert_eq!(out.samples(), frame.samples());
    }

    #[test]
    fn mono_broadcasts_to_stereo() {
        let frame = AudioFrame::new(vec![0.5, -0.5, 0.25], 1, 44_100).expect("frame");
        let out = convert_channels(&frame, 2).expect("converted");
        assert_eq!(out.channels(), 2);
        assert_eq!(out.frame_count(), 3);
        assert_eq!(out.samples(), &[0.5, 0.5, -0.5, -0.5, 0.25, 0.25]);
    }

    #[test]
    fn stereo_downmixes_to_mono_by_averaging() {
        // Frame 0: (1.0, 0.0) -> 0.5; Frame 1: (-0.4, 0.4) -> 0.0.
        let frame = AudioFrame::new(vec![1.0, 0.0, -0.4, 0.4], 2, 48_000).expect("frame");
        let out = convert_channels(&frame, 1).expect("converted");
        assert_eq!(out.channels(), 1);
        assert_eq!(out.frame_count(), 2);
        assert_eq!(out.samples(), &[0.5, 0.0]);
    }

    #[test]
    fn surround_folds_to_front_stereo_pair() {
        // One 5.1-style frame: L,R,C,LFE,Ls,Rs -> keep L,R.
        let frame =
            AudioFrame::new(vec![0.9, -0.9, 0.1, 0.0, 0.2, -0.2], 6, 48_000).expect("frame");
        let out = convert_channels(&frame, 2).expect("converted");
        assert_eq!(out.channels(), 2);
        assert_eq!(out.frame_count(), 1);
        assert_eq!(out.samples(), &[0.9, -0.9]);
    }

    #[test]
    fn general_upmix_copies_common_and_silences_extras() {
        // Stereo -> quad: front pair carries data, rear pair is silent.
        let frame = AudioFrame::new(vec![0.3, -0.3], 2, 48_000).expect("frame");
        let out = convert_channels(&frame, 4).expect("converted");
        assert_eq!(out.channels(), 4);
        assert_eq!(out.frame_count(), 1);
        assert_eq!(out.samples(), &[0.3, -0.3, 0.0, 0.0]);
    }

    #[test]
    fn zero_target_is_rejected() {
        let frame = AudioFrame::new(vec![0.0, 0.0], 2, 48_000).expect("frame");
        assert!(matches!(
            convert_channels(&frame, 0),
            Err(Error::Pipeline(_))
        ));
    }
}
