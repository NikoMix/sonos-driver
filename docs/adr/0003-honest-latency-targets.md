# 0003 — Honest latency targets (no sub-100 ms promise)

- Status: Accepted
- Date: 2026-06-24

## Context

The original request asked for "ultra-low-latency" (implicitly sub-100 ms) streaming
to Sonos. Research and prior art establish that Sonos players impose a
**non-configurable internal buffer of ~300–800 ms** on every network source, and that
the only ~75 ms-class inputs are proprietary, hardware-only, and patent-protected with
no public protocol. AirPlay 2 (~2 s) and generic UPnP (~1–3 s) are worse, not better.

We therefore cannot truthfully promise sub-100 ms over the network, and pretending
otherwise would mislead users and invite unfixable bug reports.

## Decision

The project targets the **lowest *achievable* network latency** and documents the
floor explicitly:

- Default `low-latency` profile uses **uncompressed L16 PCM** (~0 ms encode) to reach
  the Sonos buffer floor (~350–850 ms total on a good LAN).
- Additional profiles (`wav`, `flac`, `aac`, `mp3`) trade latency for bandwidth or
  compatibility.
- [`docs/LATENCY-BUDGET.md`](../LATENCY-BUDGET.md) states the per-stage budget and the
  reason for the floor; the issue-template contact link points users there first.

## Consequences

- ✅ Expectations are set truthfully; "why isn't it <100 ms?" is answered by design.
- ✅ Engineering effort focuses where it pays off — the milliseconds we actually
  control (capture, jitter buffer, copies, encode) — not on an impossible goal.
- ✅ The latency harness measures only the controllable portion; the Sonos buffer is
  documented, not asserted in CI.
- ⚠️ Users seeking true live-monitoring latency must use Sonos's hardware inputs; this
  software bridge cannot serve that use case, and says so.
