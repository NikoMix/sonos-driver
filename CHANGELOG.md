# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Sonos Router — a routing app that maps each Windows output to a Sonos
  room, live.** Launch `sonos-router.exe`, see every audio output this PC has
  and every room the household has, point one at the other and press Start. No
  install, no reboot, no administrator rights. See
  [ADR-0020](docs/adr/0020-runtime-routing-app.md).
  - **One row per Windows output**, not a rule builder — Windows already decides
    which output an app plays to, so the only missing fact is where each output
    should end up. A row left at *Off* behaves exactly as it does today.
  - **A "Default output" row** that follows whatever Windows is currently set
    to, resolved at the moment streaming starts and on every retry, so switching
    headphones does not break the route.
  - **All rooms** as a destination, forming a real synchronised Sonos group
    (the same path the driver's "All Sonos" endpoint uses).
  - **Assignments are keyed by endpoint id**, so unplugging and replugging a USB
    DAC — or Windows renaming a device — never moves a mapping onto a different
    output.
  - **A powered-off speaker does not stop the house.** Its route is reported as
    offline and skipped; every other route still plays.
  - **Apply while streaming** restarts the routes atomically rather than running
    two workers that would fight over ports and rooms.
  - **Routes persist** to `%APPDATA%\sonos-driver\routes.toml` in the exact
    format `sonos-driver cast --config` and the `sonoscast` service already
    read — configure in the app, run headless at boot if you want it always on.
  - Closing the window keeps it running in the notification area; the app is the
    streamer, so quitting stops the audio.
  - The routing table, validation and state machine are pure and unit-tested
    (68 tests) with no egui, threads or I/O, and the whole app is exercised
    end to end against real hardware by `--example smoke`.
- **`WorkerEvent` / `Observer` in `sonos-cast-service`** so the streaming worker
  reports `Discovering` / `Streaming(routes)` / `Failed` / `Stopped` instead of
  only logging it. The services pass a silent observer and are unchanged.
- **Every Sonos room is now a native Windows audio output**, plus an **All
  Sonos** output that plays to the whole household at once. Pick a room in the
  Windows sound picker and system audio plays there; there is no GUI, by design.
  See [ADR-0018](docs/adr/0018-loopback-rename-and-service-hosting.md).
  - `sonosvad.sys` — a PortCls/WaveRT virtual audio driver (C++, derived from
    Microsoft's MS-PL `simpleaudiosample`) exposing nine render endpoints: eight
    rooms and one for the aggregate. It is a null sink that advances its stream
    position at exactly real time; the audio never crosses the kernel boundary,
    because WASAPI loopback capture already provides it in user mode.
  - `sonosvad-svc` — a headless LocalSystem service that discovers the
    household, renames each endpoint to its room, loopback-captures it, and
    streams it to that room through the existing FLAC/HTTP/UPnP pipeline.
  - **All Sonos** forms a real Sonos group rather than streaming to each room in
    parallel, which drifts audibly. Absorbing whole zone groups keeps bonded
    satellites and existing user groups intact and makes the party trivially
    reversible.
  - **Last-audio-wins arbitration.** When All Sonos and a room's own output both
    have audio, whichever started most recently gets the speaker; when it falls
    silent, the other picks the room back up.
  - **`vad ungroup`** splits any rooms left grouped by a run that was killed
    mid-party. Grouping lives in the speakers, so the absorbed rooms are
    journalled to `%ProgramData%`; the service replays it on startup and
    `Uninstall-Driver.ps1` runs it before removing the driver.
  - **Idle release.** A silent output stops its player and hands the room back,
    so the driver never holds speakers hostage from the Sonos app, and reclaims
    them the moment audio returns.
  - **Spare endpoints are hidden** from the sound picker, so it shows your rooms
    rather than a run of "Sonos Output 7" placeholders. `vad release` restores
    both the names and the visibility.
  - **Seed-host discovery** (`--seed-host`): any one player reports the whole
    household over unicast HTTP, which works on networks where SSDP multicast
    does not (NAT'd VMs, guest wifi, APs without IGMP).
  - `vad status | bind | release | ungroup | probe | serve` CLI, and
    `driver/scripts/Test-Driver.ps1` (static validation: InfVerif, ApiValidator,
    PE/import checks, catalog/signature) plus `Test-DriverOnHost.ps1` for the
    load-dependent checks a sandbox cannot run.

- **A driverless mode**, for machines where installing a driver is not on the
  table. It does everything the driver does except put rooms in the sound
  picker — Windows offers no user-mode way to create an audio endpoint, so that
  one capability is genuinely kernel-only. See
  [ADR-0019](docs/adr/0019-driverless-routing-mode.md).
  - **`sonos-driver cast`** streams to one room, several rooms as a synchronised
    group, or `--all`, with nothing installed, no reboot and Secure Boot left on.
  - **Per-application routing** via WASAPI process loopback: send Spotify to the
    kitchen and everything else to the living room, concurrently. This is not
    possible through the driver at all, because Windows routes by endpoint — so
    the two modes are complements, not a fallback.
  - **`sonos-cast-svc`** — a second headless service (`sonoscast`) running the
    identical worker from `%ProgramData%\sonos-driver\cast.toml`, written for you
    on install. Both services can be installed at once.
  - New `sonos-routing` crate: a hardware-free planner that resolves routes
    against a discovered household, matching rooms by UUID or case-insensitive
    name (including non-ASCII), allocating a stream port per route, and rejecting
    two routes that claim the same room with both route numbers named.
  - **`cast --dry-run`** resolves and prints the plan — rooms, capture mode,
    ports and group coordinators — without touching a speaker.

- **Foundation for an optional kernel virtual-audio driver** that would enumerate
  **each Sonos room as its own Windows output device** (personal-use, free
  test-signed; the user-mode bridge remains the default product). This phase lands
  the verifiable, pure-Rust pieces only:
  - New `sonos-vad-control` crate — the OS-independent user-mode control plane for
    the future driver: a `RoomKey`/`device_name_for` room-identity and
    device-naming model, the `serde`-serialisable `ControlCommand`/`DriverEvent`
    message contract, and an `EndpointRegistry` reconcile planner that converges
    live render endpoints to the discovered rooms (idempotent; create/destroy with
    retry-on-failure). 15 unit tests + 2 doctests, `#![forbid(unsafe_code)]`, no
    new third-party dependencies.
  - [ADR-0017](docs/adr/0017-kernel-virtual-audio-driver.md) — architecture,
    polyglot boundary, kernel→user PCM bridge, and the test-signing decision,
    framed as a scoped, personal-use revisit of ADR-0014 (whose distribution
    conclusion stands).
  - `docs/DRIVER-SETUP.md` — owner runbook for enabling test-signing, building the
    WDK driver, creating a test certificate, and installing/uninstalling.
  - `driver/` scaffold (owner-built, **not** CI-built): INF, derivation guide, and
    PowerShell helper scripts. The kernel driver itself cannot be compiled, signed,
    or installed in CI — it requires the full WDK and a reboot for test-signing.
    ([#44](https://github.com/NikoMix/sonos-driver/issues/44),
    [#45](https://github.com/NikoMix/sonos-driver/issues/45))

### Changed

- The desktop binary is now **`sonos-router.exe`** (was `sonos-driver-gui.exe`);
  the crate is still `sonos-driver-gui`. Installer shortcuts and the release
  payload follow the new name.
- README now presents **three** ways to run the project — the app, the
  driverless CLI/service, and the kernel driver — and leads with the app, since
  it is the one that needs nothing installed.
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

- **A failed route now actually retries instead of silently giving up.** Every
  multi-room session was handed the *caller's* cancellation flag, and session
  teardown sets that flag — so the first room to finish or fail latched the
  supervisor's stop and the worker's retry loop exited for good. A speaker being
  switched off, or one transient network error, permanently killed the stream.
  Sessions now get their own teardown flags and cancellation is relayed inward,
  one way. This affected `sonos-driver cast`, the `sonoscast` service and the
  app alike. See [ADR-0021](docs/adr/0021-cancellation-versus-teardown.md).
- **A `--seed-host` never reached the room bridges, so streaming to a named room
  failed on networks without SSDP multicast.** Both service hosts resolved the
  seed, used it for discovery, then dropped it: `MultiRoomBridge` clones a
  template `Config` per room, and a single-room bridge re-resolves its own player
  from `sonos.seed_host`, which was still unset. The bridge fell back to
  multicast and died with "no Sonos players responded to SSDP discovery" seconds
  after discovery had succeeded over unicast against that very seed. Only the
  aggregate path escaped, because `run_target` forwards the seed for
  `SessionTarget::AllRooms` but discards it for `SessionTarget::Room` — so
  `--all` worked while `--room` did not, which disguised a lost setting as a
  per-room quirk. The seed is now resolved through one shared `apply_seed_host`
  helper that folds it back into the template, so discovery and streaming cannot
  disagree about how to reach the household.

- **The driver install scripts could not complete an install or an uninstall.**
  Six defects, found by walking the runbook end to end rather than reading it:
  - `Sign-Driver.ps1` defaulted `-SysPath` to `driver/_local/sonosvad.sys` and
    `Install-Driver.ps1` defaulted `-InfPath` to `driver/sonosvad.inf`. Neither
    path is ever written — `Build-Driver.ps1` emits to
    `driver/sonosvad/artifacts/<Platform>/<Configuration>/`. Both now resolve
    through a shared `Get-SonosVadArtifactDir` and honour `-Configuration` /
    `-Platform`.
  - `Sign-Driver.ps1` signed only `sonosvad.sys`, leaving `sonosvad.cat` on the
    throwaway `WDKTestCert` the build generates. `pnputil` validates the
    **catalog** against Trusted Publishers, so every install was rejected with a
    bare "third-party INF does not contain digital signature information". It now
    signs both. (Re-signing the `.sys` does not invalidate the catalog: catalog
    entries store the Authenticode hash, which excludes the certificate table.)
  - `Sign-Driver.ps1` searched only the user certificate store, while
    `New-TestCert.ps1` creates the certificate in `LocalMachine\My`, so signing
    failed with "No certificates were found that met all the given criteria". It
    now finds the certificate in either store and passes `/sm` when required.
  - `$PSScriptRoot` is empty inside `param()` defaults when a script is run via
    `powershell -File` on PS 5.1, so `New-TestCert.ps1` aborted immediately on an
    empty `-OutDir`, and `Install-Driver.ps1` / `Uninstall-Driver.ps1` could not
    locate `sonosvad-svc.exe`. All three now resolve in the script body.
  - `Uninstall-Driver.ps1` dereferenced `.Source` on the `$null` that
    `Get-Command` returns when `sonos-driver.exe` is not on `PATH`. Under
    `Set-StrictMode` that threw and **aborted the uninstall before the device
    node and driver package were removed**, leaving the driver installed and any
    All Sonos group un-split. The same hazard in `Get-SonosVadDeviceNode` (which
    both install and uninstall depend on) and on `Test-DriverOnHost.ps1`'s
    device-failure path is guarded too.
- **`README.md` told you to start the wrong service.** The service is named
  `sonosvad`; `sonosvad-svc` is the executable. The quickstart also duplicated
  the service registration that `Install-Driver.ps1` already performs.

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