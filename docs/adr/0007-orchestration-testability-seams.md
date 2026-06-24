# 0007 — Dependency-injected orchestration seams for hardware-free testing

- Status: Accepted
- Date: 2026-06-25

## Context

`sonos-driver-core` owns the run loop that wires capture → pipeline → codec →
stream-server while driving a Sonos player over UPnP (`Bridge::run`). Two forces
make this stage hard to verify:

- **No hardware or LAN in CI.** The production source is WASAPI loopback capture
  (Windows-only, no audio device in CI) and the production control target is a
  real Sonos player discovered over SSDP. Neither exists on the build agents, yet
  the orchestration logic — server start, URL hand-off, the encode loop, stop /
  teardown ordering, and metrics — is exactly the part most worth testing.
- **Latency attribution.** `docs/LATENCY-BUDGET.md` calls for measuring the
  *controllable* software latency. But `Pipeline::process` rebuilds frames during
  resampling/channel-conversion via `AudioFrame::new`, which does not carry the
  capture timestamp through, and the dominant latency contributor — the Sonos
  device buffer — is fixed and outside our control. A naive end-to-end timestamp
  would conflate stages we cannot influence with ones we can.

The building-block crates already expose the right abstractions
(`audio_core::AudioSource`, `Encoder`, `Transport`/`StreamSink`), so the gap is
purely at the orchestration boundary: the source and the player are constructed
concretely inside `run`, leaving no seam for a test to inject fakes.

## Decision

Add **additive** dependency-injection seams to `sonos-driver-core`, and define a
precise, narrow latency metric:

1. **`PlayerControl` trait** — the minimal AVTransport surface the loop needs
   (`set_stream_uri`, `play`, `stop`), implemented for
   `sonos_control::SonosController` in production and by an in-process mock in
   tests. `Send` is required so the supervising/teardown steps can own it.
2. **`Bridge::run_with(source, control)`** — the public seam that `run`
   delegates to after resolving the real capture source and Sonos controller.
   Tests drive `run_with` with a paced `SyntheticSource` and a mock
   `PlayerControl`, exercising the entire lifecycle over loopback HTTP with no
   hardware and no Sonos on the LAN.
3. **`ControllableLatency` + `Bridge::controllable_latency()`** — a live snapshot
   (samples / last / mean / max) of the **controllable** path only: the time
   from a source read to the encoded bytes being handed to the transport
   (resample + channel-convert + jitter-drain + encode + publish). It explicitly
   excludes the fixed Sonos buffer.
4. **Jitter handling: drain-fully, capture-paced.** The encode loop reads one
   block, processes it, pushes it into the pipeline jitter buffer and then drains
   the buffer fully (guarded by `is_empty` so an empty pop is never counted as a
   spurious underrun) before the next read. Because the loop is paced by capture,
   the encoder is never starved, so `underruns` honestly stays at ~0 — matching
   the `BridgeMetrics::underruns` contract ("capture starved the encoder"). Real
   jitter absorption is delegated to the stream-server broadcast ring buffer and
   the Sonos device buffer; the pipeline buffer here bounds bursts and surfaces
   overrun telemetry. `buffer_ms == 0` bypasses the buffer entirely (a
   zero-capacity buffer retains nothing) for the lowest latency.

The private `Bridge` representation changes to shared atomics
(`Arc<SharedMetrics>`, `Arc<LatencyAccumulator>`) so `metrics()` /
`controllable_latency()` return live snapshots while the encode thread runs. The
public signatures of `new`, `config`, `metrics`, `stop_handle`,
`latency_estimate` and `run` are unchanged.

## Consequences

- **Easier:** the run loop is covered by a hardware-free end-to-end harness in CI
  (`tests/e2e_latency.rs`) that asserts bytes are pulled by a loopback client,
  metrics advance, the controllable latency is measured, `StopHandle::stop()`
  terminates the loop, and the player receives `SetAVTransportURI`/`Play`/`Stop`.
  `run_with` is also a clean integration point for future front-ends (e.g. a
  `--dry-run` mode or alternative control backends).
- **Trade-off:** the bridge's public surface grows by one trait, one method and
  one type. They are additive and documented, but they are now part of the
  crate's contract. The `PlayerControl` abstraction is deliberately narrow
  (three methods) to keep that surface small; volume and topology remain on
  `sonos_control::SonosController` and are out of scope for the run loop.
- **Trade-off:** `controllable_latency()` measures only the software stages, not
  true acoustic latency. This is intentional and documented, but consumers must
  not read it as end-to-end delay — `LatencyEstimate`/`docs/LATENCY-BUDGET.md`
  remain the source of truth for the full, Sonos-buffer-inclusive figure.
- **Harder:** the capture-paced, drain-fully design means the pipeline jitter
  buffer is not the primary jitter defence; if a future change makes the loop
  free-running (decoupling capture from encode), the buffering and underrun
  accounting will need to be revisited.
