# 0002 — Modular `Encoder`/`Transport`/`AudioSource` trait contracts

- Status: Accepted
- Date: 2026-06-24

## Context

The bridge has several independently complex concerns — WASAPI capture, DSP,
encoding (multiple codecs), an HTTP server, and Sonos SSDP/UPnP control. We want
these to be developed and tested in parallel (including as an autopilot fleet), to be
swappable (e.g. `tiny_http` vs `axum`; L16 vs FLAC), and to be testable without
hardware or a real Sonos on the LAN.

## Decision

`audio-core` defines small, stable trait contracts that every other crate builds
against rather than depending on each other's concrete types:

- **`AudioSource`** — pull-based audio input (real WASAPI capture or `SyntheticSource`).
- **`Encoder`** — frame-to-bytes encoding with an optional streaming `prelude()` and a
  `content_type()`; one implementation per `CodecProfile`.
- **`Transport` / `StreamSink`** — publishing encoded bytes to a player, decoupled
  from the specific HTTP implementation.

The canonical inter-stage representation is **interleaved `f32`** (`AudioFrame`).

## Consequences

- ✅ Phase-1 workstreams (capture, control, server, codec, pipeline) proceed in
  parallel against frozen contracts; integration risk drops.
- ✅ A `SyntheticSource` + in-memory sink make the pipeline, codecs, and server
  unit-testable in CI with no audio hardware and no Sonos device.
- ✅ Codecs and the server are pluggable; choosing/adding one is a trait impl, not a
  rewrite.
- ⚠️ A shared canonical `f32` format costs a little memory/throughput vs passing raw
  device buffers, accepted for simplicity and losslessness until the encode step.
- ⚠️ The contracts are a coordination point: changing them ripples to all crates, so
  they were designed up front in Phase 0 and are changed deliberately.
