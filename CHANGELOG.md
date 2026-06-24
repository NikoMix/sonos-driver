# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-25

First feature-complete release of the pure-Rust, user-mode Windows → Sonos audio
bridge. The full capture → pipeline → encode → serve → control path is implemented
and wired together by `Bridge::run()`.

### Added

- **System audio capture** (`audio-capture`): event-driven, user-mode WASAPI
  loopback capture of the default (or a selected) Windows render endpoint, exposed
  as an `AudioSource`. Includes a silence keep-alive thread so frames keep flowing
  when nothing is playing, plus overrun/device-change recovery telemetry. All
  `unsafe` is confined to one audited COM/FFI module; every other crate is
  `#![forbid(unsafe_code)]`.
- **DSP pipeline** (`audio-pipeline`): `rubato` resampling to a target sample rate,
  channel up-/down-mix to stereo, and a bounded jitter/ring buffer with overrun
  counters. A `buffer-ms` of `0` bypasses the jitter buffer for the lowest latency.
- **Lossless encoders** (`audio-codec`): L16 PCM (`audio/L16`, big-endian, RFC 2586),
  WAV (RIFF/WAVE 16-bit PCM), and FLAC (pure-Rust `flacenc`). AAC and MP3 are
  reserved in the profile enum but fail fast with `Error::NotImplemented` — no
  licence-clean pure-Rust encoder exists (see
  [ADR-0004](docs/adr/0004-aac-mp3-codec-evaluation.md)).
- **HTTP stream server** (`stream-server`): local chunked-HTTP server (`axum`/`hyper`
  on `tokio`) that Sonos players pull from, fanning encoded bytes out to every
  connected client through a bounded broadcast ring buffer with drop-oldest
  back-pressure. No TLS (LAN-only by design).
- **Sonos discovery & control** (`sonos-control`): SSDP `M-SEARCH` discovery and UPnP
  SOAP control (`AVTransport` `SetAVTransportURI`/`Play`/`Stop`, `RenderingControl`
  `SetVolume`) with zone-group topology resolution so transport commands are routed
  to the group coordinator. Built on `std::net` + `socket2` + `quick-xml`.
- **Orchestration & telemetry** (`sonos-driver-core`): `Bridge::run()` resolves the
  target (UUID wins over room), opens capture, and runs the session lifecycle —
  start server, encode worker, start playback, supervise with reconnect debouncing,
  and graceful teardown. Exposes the `run_with`/`PlayerControl` dependency-injection
  seam used by a hardware-free end-to-end latency harness. Live metrics:
  `frames-captured`, `frames-streamed`, `underruns`, `reconnects`, plus a
  controllable-latency snapshot (which deliberately excludes the fixed Sonos buffer).
- **Command-line interface** (`sonos-driver-cli`): a `clap` CLI with `list`, `stream`,
  `stop`, `config` (`example`/`check`), and `profiles` commands. Strict configuration
  precedence (`flags > file > defaults`), TOML config files, a `--dry-run` preview,
  verbosity/quiet flags, and graceful Ctrl-C shutdown.
- **Documentation**: README front door, `docs/ARCHITECTURE.md`,
  `docs/LATENCY-BUDGET.md` (the honest latency floor), `docs/QUALITY.md` (code-quality
  and telemetry snapshot), a manual audio-capture verification checklist, and ADRs
  0001–0010.
- **Packaging**: `scripts/package.ps1` produces a portable release zip (CLI binary +
  README + both licence files + an example `sonos-driver.toml`) with no extra crate
  or `xtask` (see [ADR-0010](docs/adr/0010-portable-release-packaging.md)).

### Security

- **Permissive licences only**, enforced by `cargo-deny`; zero known advisories,
  enforced by `cargo-audit`. The bridge speaks only plain HTTP to Sonos on the local
  network and has no cloud dependency.

[Unreleased]: https://github.com/NikoMix/sonos-driver/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/NikoMix/sonos-driver/releases/tag/v0.1.0
