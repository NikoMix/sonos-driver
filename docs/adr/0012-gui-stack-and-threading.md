# 0012 — GUI stack (egui/eframe + tray-icon) and the Bridge threading model

- Status: Accepted
- Date: 2026-06-25

## Context

`sonos-driver` shipped v0.1.0 as a headless binary plus a `clap` CLI. We want an
optional desktop front-end — a system-tray app with a settings/status window — so a
non-technical user can pick a Sonos room, start/stop the bridge, and watch live
telemetry without the terminal. The GUI must drive the **existing** public API of
`sonos-driver-core` (`Bridge`, `Bridge::stop_handle()`, `StopHandle`, `BridgeMetrics`)
and `audio-core` (`Config`, `CodecProfile`); it must not fork or modify any shipping
crate.

Two sets of forces are in tension:

1. **License posture.** The project is permissive-only (no copyleft, no
   commercial-license dependencies; see ADR-0009). That rules out Slint (its free
   tier is GPL/royalty). The chosen UI stack and every transitive dependency must
   clear `cargo-deny` and `cargo-audit`.
2. **The audio path must never stall.** `Bridge::run(&mut self)` is a blocking call
   that owns `&mut self` for the entire session, and `BridgeMetrics` is read via
   `&self`. A naive "call `run()` on the UI thread" would freeze the window; a naive
   "poll `metrics()` while `run()` holds `&mut self`" does not type-check in safe Rust.

## Decision

Add a **new** crate `crates/sonos-driver-gui`, split into a headless, unit-tested
library and a thin rendering binary, and run the bridge on its own thread.

**Stack (all permissive; pinned via the workspace):**

- `egui` 0.34 + `eframe` 0.34 (immediate-mode UI on the `glow` renderer,
  `default-features = false`), `tray-icon` 0.24 + `muda` 0.19 for the tray and its
  menu, and `image` 0.25 (PNG only) to decode the embedded window/tray icon. `eframe`
  wraps `winit`, so its Win32 message pump is the same thread the tray/menu events
  arrive on; we forward those events into the egui loop and call
  `ctx.request_repaint()`.

**Architecture — an Elm-style core the shell executes:**

- The library is pure and headless: `reduce(&mut AppState, Action) -> Vec<Effect>`.
  All decisions (room-list state, target selection, start/stop gating, config
  validation and round-tripping) live here and are unit-tested with no window and no
  network. The binary (`app`/`tray`/`icon` + `main`) only renders state, pumps OS
  events into `Action`s, and performs the returned `Effect`s.

**Threading — live metrics over the `run_with` DI seam:**

- A worker thread resolves the Sonos target (mirroring core's documented
  `resolve_target` precedence), opens loopback capture wrapped in a `CountingSource`
  decorator that bumps an `Arc<AtomicU64>` per non-empty frame, and calls
  `bridge.run_with(source, control)`. The UI reads that atomic once per frame for a
  lock-free **live** `frames_captured` signal and never touches the audio path. On
  stop (via `StopHandle`) the worker reads the authoritative `bridge.metrics()` plus
  `bridge.controllable_latency()` and pushes a final snapshot to the UI over a
  channel.
- The new crate keeps `#![forbid(unsafe_code)]` and inherits the workspace pedantic
  lints (`-D warnings`).
- Other potentially-blocking calls follow the same off-thread pattern: the
  capture-device picker calls `audio_capture::enumerate_render_endpoints()` (which
  initialises its own COM apartment) on a short-lived worker thread and reports the
  result back as an `Action`, so device enumeration never stalls the UI either.

**License-gate exceptions.** The blessed `eframe`/`egui` 0.34 stack pulls three
permissive-but-non-enumerated licenses that cannot be feature-gated away: `eframe`
hard-codes `egui-winit`'s `clipboard` feature (→ `arboard` → `clipboard-win` /
`error-code`, both `BSL-1.0`), and `default_fonts` bundles `epaint_default_fonts`
(`OFL-1.1` + `Ubuntu-font-1.0`). All three are OSI-approved / FSF-libre with no
copyleft or commercial restriction. Rather than widen the global allow-list, each is
added as a **per-crate** `exceptions` entry in `deny.toml`, scoped to exactly the crate
that carries it.

## Consequences

- The UI stays responsive: the bridge runs off-thread, control is a `StopHandle`, and
  telemetry flows one-way (atomic + channel) so the audio path is never blocked by
  rendering.
- The decision logic is testable without a display or hardware; the rendering layer is
  intentionally thin. This keeps the pedantic clippy gate and `cargo test` meaningful.
- We accept a **known contract gap**: because `Bridge::run` holds `&mut self` for the
  whole session, the GUI cannot poll the *authoritative* `BridgeMetrics` live — it
  shows the `CountingSource` capture counter until the session ends. A future
  `Bridge::metrics_handle() -> impl Send + Sync` (or an `Arc<…>` snapshot) in
  `sonos-driver-core` would let the GUI surface streamed-frames/underruns/reconnects
  live; that change is out of scope here (it would modify a shipping crate).
- The three `deny.toml` exceptions are narrowly scoped and commented; if a future
  version of any of those crates relicenses, the gate will flag it rather than silently
  accept it.
- The GUI is an **additional** binary; the headless bridge and CLI are unchanged, so
  users who do not want a desktop app pay nothing.
