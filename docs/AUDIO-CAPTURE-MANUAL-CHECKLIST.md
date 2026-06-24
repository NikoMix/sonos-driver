# `audio-capture` — Manual Hardware Verification Checklist

WASAPI loopback capture touches live Core Audio COM interfaces and real output
devices, so it **cannot run in CI** (GitHub-hosted runners have no audio
endpoint, and the loopback engine never raises buffer events without a render
device). The pure logic — device matching ([`device`]) and sample→`f32`
conversion ([`format`]) — is covered by automated unit tests; the FFI path below
is verified by hand on a Windows host before each release that touches this crate.

Run every section on a real Windows 10/11 machine with at least one active render
endpoint. Record the date, Windows build, and crate commit with the results.

- Host: _______________  Windows build: _______________
- Commit: _______________  Date: _______________  Tester: _______________

---

## 0. Build & smoke

- [ ] `cargo build -p audio-capture` succeeds in a shell where MSVC env is
      initialised (`vcvars64.bat`) and `~/.cargo/bin` is on `PATH`.
- [ ] `cargo test -p audio-capture --all-features` — all unit tests pass.
- [ ] `cargo clippy -p audio-capture --all-targets --all-features -- -D warnings`
      is clean.

## 1. Endpoint enumeration

Use a tiny harness (or `examples/`/`cargo test -- --ignored` if present) that calls
`audio_capture::enumerate_render_endpoints()`.

- [ ] Returns at least one endpoint and does not error.
- [ ] Exactly one endpoint reports `is_default == true`.
- [ ] Each entry has a non-empty `id` and a human-readable `name`
      (e.g. matches the device shown in Windows *Sound settings → Output*).
- [ ] Plugging in a second output device (HDMI/USB DAC/headset) and re-enumerating
      lists the new endpoint.

## 2. Default-device capture

Construct a `LoopbackCapture` from a `CaptureConfig` with `device: None`.

- [ ] `LoopbackCapture::open` succeeds.
- [ ] `format()` reports `SampleFormat::F32`, with `sample_rate` and `channels`
      matching the default endpoint's shared-mode mix format
      (cross-check against *Sound settings → Device properties → Advanced*).
- [ ] With music/video playing, repeated `read()` calls return `Some(frame)` with
      `frame.frame_count() > 0` and interleaved `f32` samples in `[-1.0, 1.0]`.
- [ ] Non-silent content yields non-zero samples; the RMS tracks loudness
      (louder playback → larger values).

## 3. Silence keep-alive (continuity)

- [ ] **Stop all audio playback.** `read()` keeps returning `Some(frame)` at a
      steady cadence (frames do not stall) — the silence keep-alive is holding the
      engine active.
- [ ] During silence, captured samples are at/near `0.0` (no audible artefacts; the
      keep-alive writes genuine zero buffers and must not inject tones/clicks).
- [ ] Listen on the output device: the keep-alive is **inaudible** (no hiss/pop).
- [ ] Resume playback → non-zero samples return within a few buffers.

## 4. Device selection

- [ ] `device: Some("<exact endpoint id from §1>")` opens **that** device
      (verify `format()` matches that endpoint, not the default).
- [ ] `device: Some("<friendly-name substring>")` (e.g. `"Speakers"` or a DAC
      model) resolves by case-insensitive substring match.
- [ ] An unknown selector (`device: Some("no-such-device")`) fails fast with a
      clear `Error::Capture` message naming the missing selector.

## 5. Format coverage

Repeat §2's capture sanity check after switching the endpoint's *Default Format*
in *Device properties → Advanced*:

- [ ] 16-bit, 44.1 kHz, stereo.
- [ ] 24-bit, 48 kHz, stereo.
- [ ] 32-bit float, 48 kHz, stereo (native engine format).
- [ ] A high-rate mode if available (e.g. 96 kHz) — `format()` follows the change
      and samples stay in range.

## 6. Device-change & invalidation resilience

While a capture is running and being read:

- [ ] Change the Windows **default output device** (Sound settings). With
      `device: None`, the stream re-resolves to the new default and keeps producing
      frames; `reconnect_count()` increments. No panic, no permanent end-of-stream.
- [ ] Disable/re-enable the active endpoint in *Device Manager* (or unplug/replug a
      USB DAC). The capture thread reinitialises (`reconnect_count()` increments)
      and resumes; if the device stays gone, `read()` eventually returns an error
      then `None` (graceful end), never a hang.
- [ ] Change the active endpoint's default **format** mid-stream → capture
      reinitialises and `format()` reflects the new layout.

## 7. Backpressure & telemetry

- [ ] Drive a slow consumer (sleep between `read()`s while loud audio plays).
      `overrun_count()` increases (newest blocks dropped) and the process stays
      responsive — no unbounded memory growth, no deadlock.
- [ ] A normal-speed consumer holds `overrun_count()` at/near 0.

## 8. Lifecycle & teardown

- [ ] Dropping the `LoopbackCapture` joins both worker threads promptly
      (≲ ~300 ms) with no hang.
- [ ] After drop, the silence keep-alive has stopped (the device is released; no
      lingering render client in *Volume Mixer*).
- [ ] Open → capture → drop in a loop (≥ 20 iterations) leaks no handles/threads
      (watch Task Manager / handle count stays flat).

## 9. Unsafe confinement (per release)

- [ ] `cargo geiger -p audio-capture` (if installed) shows `unsafe` usage **only**
      inside `audio_capture::wasapi`; `device`, `format`, `capture`, and `lib`
      report none.
- [ ] Every `unsafe` block in `src/wasapi/` carries a `// SAFETY:` comment stating
      the upheld invariant.

---

### Notes / observations

> Record anomalies, device-specific quirks, measured join/teardown timings, and the
> `overrun_count` / `reconnect_count` values seen during §6–§7 here.

[`device`]: ../crates/audio-capture/src/device.rs
[`format`]: ../crates/audio-capture/src/format.rs
