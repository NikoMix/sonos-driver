# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Default streaming profile is now FLAC** (`audio-core`). The bridge previously
  defaulted to raw L16 PCM, which real Sonos players reject ("unable to play
  'stream' — the file is in an unsupported format"); RIFF/WAVE PCM is likewise
  rejected ("the song is not encoded correctly"). The HTTP stream server fans
  audio out at the *live edge*, so the one-time container header never reaches a
  player that joins the running stream — only FLAC works there, because its
  frames are self-describing and a decoder can resync mid-stream. FLAC is now the
  default for both the CLI and the GUI; it is lossless at roughly half the
  bandwidth of PCM. `l16-pcm` (lowest latency) and `wav` remain selectable for
  renderers that accept them. See
  [ADR-0016](docs/adr/0016-default-profile-flac.md).
  ([#42](https://github.com/NikoMix/sonos-driver/issues/42))

### Fixed

- **Multi-homed room discovery** (`sonos-control`): SSDP `M-SEARCH` discovery now
  sends from every usable local IPv4 interface, explicitly selecting each as the
  multicast egress interface (`IP_MULTICAST_IF`), instead of relying on the OS
  default route. On hosts with more than one active network — Ethernet *and*
  Wi-Fi, or virtual adapters from Hyper-V/WSL/Docker/VPNs (common on Windows) —
  the default route is frequently not the speakers' network, so "Discover rooms"
  silently returned nothing. Discovery now fans out across all interfaces (with
  the default route kept as a safety net) and de-duplicates replies. The unicast
  path used by the integration tests is unchanged. ([#38](https://github.com/NikoMix/sonos-driver/issues/38))
- **Multi-homed streaming playback** (`stream-server`, `sonos-driver-core`): the
  HTTP stream URL advertised to a Sonos player is now resolved *per target* — the
  bridge picks the local interface that actually routes to *that* speaker instead
  of resolving the OS default-route IP once at startup. On a multi-homed host
  whose Sonos sits on a different NIC than the default route, the speaker was
  handed an unreachable URL: `SetAVTransportURI`/`Play` still succeeded (so no
  error surfaced), but the speaker could never fetch the stream — it stayed
  silent with no transport activity. `StreamServer::stream_url_for(peer)` and a
  defaulted `PlayerControl::peer_ip()` thread the speaker's own IP through both
  the initial play and reconnect; loopback/integration paths advertise verbatim
  and are unchanged. ([#40](https://github.com/NikoMix/sonos-driver/issues/40))

## [0.2.0] - 2026-06-25

Adds a desktop experience and hardens the streaming path, packaging, and release
tooling on top of v0.1.0. The bridge remains pure-Rust and user-mode; the default
capture → pipeline → encode → serve → control path is unchanged.

### Added

- **System-tray desktop GUI** (`sonos-driver-gui`): an `egui`/`eframe` tray +
  settings/status application that drives the existing public API of
  `sonos-driver-core` and `audio-core` (no changes to those crates' behaviour).
  The Bridge runs on its own worker thread, controlled via `StopHandle`, with a
  live captured-frames counter pushed to the UI over a channel so the UI never
  blocks on the audio path. Controller/state logic is split out and unit-tested
  headlessly; the crate keeps `#![forbid(unsafe_code)]`. See
  [ADR-0012](docs/adr/0012-gui-stack-and-threading.md).
- **Per-application capture** (`audio-capture`): optional WASAPI *process
  loopback* that captures — or, inversely, *excludes* — a single application's
  audio tree instead of the whole render endpoint, selected by process name
  (`"spotify.exe"`, the `.exe` is optional) or numeric PID. Still pure user-mode
  (no kernel driver), brought up via `ActivateAudioInterfaceAsync` on the
  process-loopback virtual device and drained by the same pump/decode path as the
  default-device loopback. Requires Windows 11 / Windows Server 2022 (build
  20348+); older builds fail fast with a clear typed error via a runtime OS gate.
  The classic default-device loopback remains the default. New
  `capture.process` / `capture.exclude-process` config fields and
  `--process <name|pid>` / `--exclude-process` CLI flags (precedence:
  flags > file > defaults; a configured `process` takes precedence over
  `device`). All new `unsafe` stays inside the audited `audio_capture::wasapi`
  module; every other crate remains `#![forbid(unsafe_code)]`. See
  [ADR-0011](docs/adr/0011-process-loopback-capture.md).
- **Codec `unsafe`-forbid parity and AAC re-evaluation** (`audio-codec`): the
  codec crate is hardened in line with the rest of the workspace, and the
  AAC/MP3 deferral is re-confirmed against the current licence-clean pure-Rust
  ecosystem. See
  [ADR-0013](docs/adr/0013-codec-hardening-and-aac-reeval.md).
- **Driver-reality documentation and release engineering** clarifying why the
  project ships as a user-mode bridge rather than a kernel virtual-audio device.
  See [ADR-0014](docs/adr/0014-virtual-audio-driver-reality.md).
- **Dual-binary packaging**: `scripts/package.ps1` now bundles **both**
  `sonos-driver.exe` (CLI) and `sonos-driver-gui.exe` (tray GUI) into the
  portable zip alongside the docs, both licences, and a generated example config.
- **Windows installer**: `installer/sonos-driver.iss` plus
  `scripts/build-installer.ps1` compile a single-file Inno Setup installer
  (`sonos-driver-<version>-x86_64-setup.exe`) whose payload is expanded from the
  portable zip, so the two artifacts are identical by construction. Inno Setup is
  free for commercial use (no paid licence). See
  [ADR-0015](docs/adr/0015-release-pipeline.md).
- **Mature release pipeline**: `.github/workflows/release.yml` gates the
  artifacts behind the full quality suite (fmt, clippy `-D warnings`, tests,
  release build, `cargo-deny`, `cargo-audit`), then builds the zip and installer,
  emits a `SHA256SUMS.txt`, and publishes a GitHub Release. See
  [ADR-0015](docs/adr/0015-release-pipeline.md).

### Changed

- **L16 content type** now advertises `audio/L16;rate=44100;channels=2` (the
  RFC 2586 rate/channels parameters) instead of a bare `audio/L16`, so players do
  not fall back to the 8 kHz mono default and instead decode stereo at the
  streamed sample rate.
- **UPnP targeting** now resolves a UUID target to its zone-group coordinator
  before binding (matching the existing room-name behaviour), so streaming to a
  stereo pair or a grouped player routes transport commands correctly.
- **Codec selection** is restricted and validated to the profiles that actually
  ship an encoder (L16 PCM, WAV, FLAC) in both the GUI and the CLI; selecting AAC
  or MP3 is now rejected up front with a clear error that references
  [ADR-0004](docs/adr/0004-aac-mp3-codec-evaluation.md) instead of failing partway
  into a session.
- **Dependencies** refreshed to their latest compatible versions; the workspace
  version is bumped to `0.2.0`.
- **CI and release workflows** pin every third-party GitHub Action to a full
  commit SHA and run `actions/checkout` with `persist-credentials: false`.

### Fixed

- The GUI no longer panics if the Bridge worker thread fails to spawn; the
  failure is surfaced to the UI as a session-ended event.
- The GUI now distinguishes a **missing** configuration (first run → silent
  defaults) from a **malformed** one (surfaced in the status line), so a later
  save can no longer silently overwrite an unreadable config file.

### Security

- **Permissive licences only**, enforced by `cargo-deny`; zero known advisories,
  enforced by `cargo-audit`. The release pipeline runs both as merge gates and
  pins its CI actions by commit SHA. The bridge still speaks only plain HTTP to
  Sonos on the local network and has no cloud dependency.

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

[Unreleased]: https://github.com/NikoMix/sonos-driver/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/NikoMix/sonos-driver/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/NikoMix/sonos-driver/releases/tag/v0.1.0
