# 0004 — `windows`(-rs) bindings with self-managed WASAPI FFI for `audio-capture`

- Status: Accepted
- Date: 2026-06-24

## Context

WS-A (`audio-capture`, issue #5) must capture the Windows system render endpoint
with **event-driven WASAPI loopback** and expose it as an `audio_core::AudioSource`.
Two binding strategies were considered:

1. The third-party **`wasapi`** crate — a higher-level safe-ish wrapper over the
   Core Audio APIs.
2. The Microsoft-published **`windows`** crate (windows-rs) with only the
   `Win32_Media_Audio` / `Win32_System_Com` (and a few supporting) features,
   writing the COM/FFI ourselves.

Forces:

- ADR/issue policy makes `audio-capture` the **single** crate permitted `unsafe`,
  and requires every `unsafe` block to be isolated and documented so `cargo-geiger`
  shows unsafe **confined to this crate**.
- Event-driven loopback only signals while real audio flows, so we need precise
  control over the audio-client init flags, the event handle, an always-on
  **silence keep-alive** render client, and the `AUDCLNT_E_DEVICE_INVALIDATED`
  reinitialise path.
- Supply-chain policy: permissive licenses only, minimal dependency surface,
  pinned to the latest release.

## Decision

Use the **`windows` crate (windows-rs), pinned to the latest release**, and write
the WASAPI COM/FFI ourselves inside a single internal module
(`audio_capture::wasapi`). The wrapper `wasapi` crate is **not** taken as a
dependency.

Rationale:

- **Unsafe stays ours and visible.** With the wrapper crate the `unsafe` lives in
  a dependency, so "geiger shows unsafe confined to `audio-capture`" would not hold.
  Owning the FFI lets us isolate it behind `#[allow(unsafe_code)] mod wasapi;` with a
  `// SAFETY:` invariant on every block.
- **Control granularity.** Direct bindings give exact control over
  `AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK`, the event
  handle, the keep-alive render client, format-change/device-change handling, and
  thread priority — the parts the wrapper hides or smooths over.
- **First-party + permissive.** windows-rs is MIT/Apache-2.0, maintained by
  Microsoft, and already transitively present in the lock file.
- **Tight surface.** Only the handful of Win32 features the capture path needs are
  enabled.

The safe/unsafe boundary is a hard rule: the FFI module hands the rest of the crate
only plain, owned, `Send` data (`EndpointInfo`, `WaveFormat`, `AudioFrame`). No COM
interface pointer crosses a thread or leaves the module.

## Consequences

- ✅ `unsafe` is auditable and confined to `audio_capture::wasapi`; the public
  surface and the rest of the workspace stay `forbid(unsafe_code)`-clean.
- ✅ Full control over the event-driven loopback + keep-alive + reconnect design
  needed for a continuous Sonos stream.
- ✅ One fewer third-party crate to vet; smaller advisory surface.
- ⚠️ More code to write and maintain than calling the wrapper crate; mitigated by
  keeping the FFI module small and pushing all testable logic (device matching,
  sample→`f32` conversion) into pure, platform-independent modules.
- ⚠️ Tied to windows-rs API churn across releases; pinned + `cargo update`-managed.
