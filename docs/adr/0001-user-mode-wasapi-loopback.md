# 0001 — User-mode WASAPI loopback (no kernel driver for v1)

- Status: Accepted
- Date: 2026-06-24

## Context

The original request was a "Windows driver" to stream system audio to Sonos. There
are two broad ways to obtain the system audio stream on Windows:

1. **User-mode WASAPI loopback** — capture the existing default render endpoint's
   mix. No installation, no signing, runs as a normal process.
2. **Kernel-mode virtual audio device** — present a selectable "Sonos" output device
   to Windows. Requires the Windows Driver Kit, a kernel-mode audio driver, and a
   cross-signed/attested driver package, plus a far larger test/maintenance surface.

The user-facing goal (stream what is playing to Sonos, at the lowest achievable
latency) is fully served by (1). Option (2) only adds the ergonomic of "Sonos shows
up as a playback device" — at a very large engineering and distribution cost.

## Decision

v1 captures audio with **user-mode WASAPI loopback**. The capability is exposed as an
`audio_core::AudioSource` so the rest of the system is agnostic to how audio is
obtained. A kernel-mode virtual audio device is deferred to a **Phase 4 research**
item, not abandoned.

## Consequences

- ✅ No driver signing, no installer privilege, no kernel risk; fast iteration.
- ✅ Pure-Rust, user-mode, portable across Windows x64/arm64.
- ✅ The `AudioSource` seam means a future virtual device can drop in without changing
  the pipeline, codec, server, or control crates.
- ⚠️ Audio is captured *post-mix* (the default endpoint), not as a separate routable
  device. Per-app routing and "Sonos as an output device" are out of scope until
  Phase 4.
- ⚠️ Loopback follows the default endpoint's format; format-change handling lives in
  `audio-capture` (WS-A).
