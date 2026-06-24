# sonos-driver

A **pure-Rust, user-mode Windows → Sonos audio bridge**. It captures your PC's
system audio output (WASAPI loopback) and streams it to Sonos players on your LAN,
with selectable latency/quality profiles.

> [!IMPORTANT]
> **Latency reality.** Sonos players enforce a non-configurable internal buffer
> (~300–800 ms) on every network source, so **true sub-100 ms streaming to Sonos is
> impossible in software** — by anyone, not just this project. `sonos-driver` reaches
> the *lowest achievable* network latency (~350–850 ms with uncompressed L16 PCM on a
> good LAN) and is honest about the floor. See
> [docs/LATENCY-BUDGET.md](docs/LATENCY-BUDGET.md).

## Status

**v0.1.0 — feature-complete pipeline.** The full capture → pipeline → encode →
serve → control path is implemented across all eight crates and wired together by
[`Bridge::run()`](crates/sonos-driver-core/src/lib.rs): user-mode WASAPI loopback
capture, resample/convert/jitter pipeline, L16/WAV/FLAC encoders, a chunked HTTP
stream server, and SSDP + UPnP Sonos discovery/control, all behind a complete `clap`
CLI (`list` / `stream` / `stop` / `config`).

Everything except the Windows audio hardware path is exercised by automated tests
(including a hardware-free end-to-end latency harness). WASAPI loopback cannot run in
CI, so the capture path is verified with a
[manual checklist](docs/AUDIO-CAPTURE-MANUAL-CHECKLIST.md). See the
[changelog](CHANGELOG.md) for the full v1 capability set and
[docs/QUALITY.md](docs/QUALITY.md) for the code-quality/telemetry snapshot.

## Features

- 🎧 **System audio capture** via event-driven, user-mode WASAPI loopback — no driver
  install, with a silence keep-alive so frames keep flowing even when nothing is playing.
- 📡 **Sonos discovery & control** over SSDP + UPnP SOAP (`SetAVTransportURI` / `Play` /
  `Stop`, volume, and zone-group topology so a stereo pair or group is driven by its
  coordinator).
- 🔀 **Selectable lossless profiles:** `low-latency` (L16 PCM), `wav`, and `flac` — all
  pure-Rust. (`aac`/`mp3` are reserved but **not** implemented; see
  [Profiles](#profiles).)
- 🧩 **Modular crates** built on stable `audio-core` trait contracts — most of the system
  is testable without hardware or a real Sonos on the network.
- 📊 **Telemetry:** structured `tracing` logs plus live `frames-captured` /
  `frames-streamed` / `underruns` / `reconnects` metrics and a controllable-latency
  snapshot.
- 🛡️ **Permissive licensing only**, enforced by `cargo-deny`; zero known
  vulnerabilities, enforced by `cargo-audit`; `unsafe` confined to one audited WASAPI
  FFI island.

## Profiles

`sonos-driver` ships three lossless codecs. List them — with their honest end-to-end
latency (the **fixed Sonos buffer is included**) — at any time:

```text
$ sonos-driver profiles
Available streaming profiles (latency includes the fixed Sonos buffer):

  L16Pcm   ~350-850 ms    lossless
           Uncompressed 16-bit PCM (audio/L16) — lowest latency
  Wav      ~400-1000 ms   lossless
           WAVE-framed 16-bit PCM — lossless, widely compatible
  Flac     ~400-1000 ms   lossless
           FLAC lossless compression — quality with moderate latency
  Aac      ~1200-3000 ms  lossy
           AAC lossy compression — compatibility, higher latency
  Mp3      ~1200-3000 ms  lossy
           MP3 lossy compression — compatibility, higher latency
```

| Profile | CLI name | Codec | Status |
|---------|----------|-------|--------|
| `L16Pcm` | `low-latency` | Big-endian 16-bit PCM (`audio/L16`, RFC 2586) | ✅ Implemented — default, lowest latency |
| `Wav` | `wav` | RIFF/WAVE 16-bit PCM (`audio/wav`) | ✅ Implemented — lossless, broad compatibility |
| `Flac` | `flac` | FLAC lossless (`audio/flac`, pure-Rust `flacenc`) | ✅ Implemented — ~½ the bandwidth |
| `Aac` | `aac` | AAC | ⛔ Not implemented — no licence-clean pure-Rust encoder ([ADR-0004](docs/adr/0004-aac-mp3-codec-evaluation.md)) |
| `Mp3` | `mp3` | MP3 | ⛔ Not implemented — same as AAC |

> [!NOTE]
> `aac` and `mp3` are listed for completeness but currently fail fast with a clear
> `NotImplemented` error when selected: no MIT/Apache-licensed, pure-Rust encoder for
> them exists, and the project's permissive-licence-only rule rules out the C-based
> alternatives. See [docs/LATENCY-BUDGET.md](docs/LATENCY-BUDGET.md) and
> [ADR-0004](docs/adr/0004-aac-mp3-codec-evaluation.md).

## Quickstart

```powershell
# 1. Build the release binary (see Building for the MSVC prerequisite).
cargo build --release -p sonos-driver-cli

# 2. Find your Sonos rooms.
.\target\release\sonos-driver.exe list

# 3. Stream this PC's audio to a room at the lowest latency. Ctrl-C to stop.
.\target\release\sonos-driver.exe stream --room "Living Room" --profile low-latency
```

That's it — start playing audio on your PC and it comes out of the Sonos. If discovery
finds nothing, jump to [Troubleshooting](#troubleshooting).

> Prefer a portable build with no Rust toolchain on the target machine? See
> [Packaging a release](#packaging-a-release).

## Building

### Prerequisites

- **Rust** (stable; the repo pins a toolchain via [`rust-toolchain.toml`](rust-toolchain.toml)).
- **The MSVC toolchain**: Visual Studio 2022/Build Tools with the
  *"Desktop development with C++"* workload **and** the *Windows 10/11 SDK*. This
  provides `link.exe` and the CRT/SDK import libraries (`msvcrt.lib`, `kernel32.lib`, …).

> [!TIP]
> Cargo must run in an environment where the MSVC libraries are on the linker path.
> The simplest way is to build from a **"Developer PowerShell for VS"** (or Developer
> Command Prompt). From a plain shell you can initialise it first:
> ```powershell
> cmd /c '"C:\Program Files\Microsoft Visual Studio\<edition>\VC\Auxiliary\Build\vcvars64.bat" && powershell'
> ```
> If you see `LINK : fatal error LNK1104: cannot open file 'msvcrt.lib'`, the MSVC
> environment is not initialised — this is the fix.

### Commands

```powershell
cargo build --workspace            # debug build
cargo test  --workspace            # run all tests (no hardware required)
cargo run -p sonos-driver-cli -- profiles
cargo build --release --workspace  # optimised build
```

## Usage

The binary is `sonos-driver`. Global flags work with every subcommand: `-v`/`-vv`
increase log verbosity (debug/trace), `-q` silences everything but errors, and
`-c`/`--config <PATH>` loads a TOML config file.

### `list` — discover players

```powershell
# Discover Sonos rooms (table view): ROOM, MODEL, IP, ROLE, GROUP.
sonos-driver list

# Wait up to 5 s and emit machine-readable JSON for scripting.
sonos-driver list --json --timeout 5
```

The role/group columns come from the zone-group topology, so you can see which device
is a group **coordinator** (the one that must receive transport commands), a **member**,
or **standalone**.

### `stream` — bridge audio to a room

```powershell
# Stream to a room with the lowest-latency profile. Press Ctrl-C to stop cleanly.
sonos-driver stream --room "Living Room" --profile low-latency

# Target a specific device by UUID (takes precedence over --room).
sonos-driver stream --uuid RINCON_XXXXXXXXXXXX01400 --profile flac

# Configure a whole session from flags (no file needed). --bind and --target-rate
# are aliases for --bind-address and --target-sample-rate.
sonos-driver stream --room "Office" --profile flac --bind 0.0.0.0 --port 8080 `
  --target-rate 44100 --buffer-ms 80 --discovery-timeout 3

# Capture a non-default endpoint by id or friendly-name substring.
sonos-driver stream --room "Office" --device "Speakers"

# Preview the effective configuration and exit without streaming or touching the LAN.
sonos-driver stream --room "Office" --dry-run
```

`stream` flags (all optional; unset flags fall through to the config file, then the
built-in defaults):

| Flag | Meaning | Default |
|------|---------|---------|
| `--room <NAME>` | Target room/zone name | — |
| `--uuid <UUID>` | Target device UUID (wins over `--room`) | — |
| `--profile <P>` | `low-latency`, `wav`, `flac`, `aac`, `mp3` | `low-latency` |
| `--device <ID\|NAME>` | Capture endpoint (id or name substring) | default render device |
| `--port <PORT>` | Local stream-server TCP port | `8080` |
| `--bind-address <IP>` (`--bind`) | Address the stream server binds to | `0.0.0.0` |
| `--target-sample-rate <HZ>` (`--target-rate`) | Resample rate; `0` keeps the capture rate | `44100` |
| `--buffer-ms <MS>` | Jitter-buffer depth | `80` |
| `--discovery-timeout <SECS>` | SSDP discovery wait | `3` |
| `--dry-run` | Print effective config and exit | off |

### `stop` — halt playback

```powershell
# Exactly one target is required.
sonos-driver stop --room "Living Room"
sonos-driver stop --uuid RINCON_XXXXXXXXXXXX01400 --timeout 5
```

### `profiles` — list codecs

```powershell
sonos-driver profiles
```

See [Profiles](#profiles) for the output and the codec status table.

## Configuration files

A session can be driven entirely from a TOML file. Precedence is strict —
**command-line flags override file values, which override built-in defaults**:

```powershell
# Generate a fully-commented config at its defaults, then edit it.
sonos-driver config example > sonos-driver.toml

# Validate a file and print the effective configuration (uses the global --config flag).
sonos-driver --config sonos-driver.toml config check

# Use the file, overriding individual values with flags as needed.
sonos-driver --config sonos-driver.toml stream --port 9000
```

The example file (`config example`) documents every key:

```toml
[capture]
# Windows audio endpoint to capture: a device id or a substring of the friendly
# name. Leave unset to follow the current default render (playback) device.
# device = "Speakers"

[pipeline]
# Resample captured audio to this rate (Hz) before encoding. Sonos commonly
# expects 44100. Set to 0 to keep the native capture rate.
target-sample-rate = 44100
# Jitter-buffer depth in milliseconds: lower is less latency but more prone to
# underruns on a busy network.
buffer-ms = 80

[stream]
# Codec / latency profile: "l16-pcm" (lowest latency, default), "wav", "flac",
# "aac" or "mp3". See `sonos-driver profiles` and docs/LATENCY-BUDGET.md.
profile = "l16-pcm"
# Address the local HTTP stream server binds to. 0.0.0.0 exposes it on the LAN
# so the Sonos player can reach it.
bind-address = "0.0.0.0"
# TCP port for the local HTTP stream server.
port = 8080

[sonos]
# Target room / zone name, e.g. "Living Room". Required for streaming unless a
# uuid is set (either here or with --room on the command line).
# room = "Living Room"
# Target device UUID; takes precedence over `room` when set.
# uuid = "RINCON_XXXXXXXXXXXX01400"
# Seconds to wait for SSDP discovery responses.
discovery-timeout-secs = 3
```

> Selecting an unimplemented profile (`aac`/`mp3`) fails fast with a clear
> `NotImplemented` error before any socket is bound or the player is contacted.

## Troubleshooting

| Symptom | Cause & fix |
|---------|-------------|
| `LINK : fatal error LNK1104: cannot open file 'msvcrt.lib'` | The MSVC environment isn't initialised. Build from a *Developer PowerShell for VS*, or run `vcvars64.bat` first (see [Building](#building)). |
| `list` / `stream` finds **no devices** | Discovery is SSDP multicast on UDP 1900. Make sure the PC and Sonos are on the **same subnet/VLAN**, allow the app through the Windows Firewall, and raise `--timeout`/`--discovery-timeout`. Some Wi-Fi APs block multicast between clients. |
| Sonos plays but you hear **nothing / silence** | `0.0.0.0` must be reachable from the speaker; if you bound to a specific IP, use the LAN-facing one. Check that audio is actually playing on the PC — the keep-alive prevents stalls but you still need a source. |
| Audio **drops out / underruns** climb | Increase `--buffer-ms` (e.g. 120–200) to absorb network jitter, or switch from `flac` to `low-latency`/`wav` to cut encode cost on a busy machine. |
| Latency feels **high (~0.5–1 s)** | Expected. Sonos imposes a fixed ~300–800 ms playback buffer that no software can remove — see [docs/LATENCY-BUDGET.md](docs/LATENCY-BUDGET.md). |
| `error: ... NotImplemented` on `--profile aac`/`mp3` | Those codecs aren't implemented (no licence-clean pure-Rust encoder). Use `low-latency`, `wav`, or `flac`. |

## Packaging a release

[`scripts/package.ps1`](scripts/package.ps1) produces a portable, self-contained release
zip — no Rust toolchain needed on the target machine:

```powershell
# Builds --release and writes dist\sonos-driver-<version>-x86_64-pc-windows-msvc.zip
pwsh -File scripts/package.ps1
```

The zip bundles the `sonos-driver.exe` binary, `README.md`, both licence files, and an
example `sonos-driver.toml`. Run `Get-Help scripts/package.ps1 -Detailed` for options
(skip the build, change the output directory, etc.). The script must run from a shell
with the MSVC environment initialised, just like a normal build.

## Architecture

A Cargo workspace of small, single-responsibility crates built against the trait
contracts in `audio-core`:

```text
audio-core ──── traits & types (AudioFrame, Encoder, Transport, AudioSource, Config)
audio-capture ─ WASAPI loopback capture          (the sole unsafe/FFI island)
audio-pipeline ─ resample / channel-convert / jitter buffer
audio-codec ─── L16 PCM / WAV / FLAC encoders    (AAC/MP3 not implemented)
stream-server ─ chunked HTTP server (axum + broadcast ring buffer)
sonos-control ─ SSDP discovery + UPnP AVTransport/RenderingControl SOAP
sonos-driver-core ─ Bridge orchestration, session lifecycle, metrics, latency
sonos-driver-cli ── clap command-line UX (list / stream / stop / config)
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the data flow and threading
model, and [docs/adr/](docs/adr/) for the key decisions.

## Security & quality

- **Permissive licences only**, enforced by `cargo-deny`; zero known vulnerabilities,
  enforced by `cargo-audit`.
- **`unsafe` is confined** to one audited WASAPI FFI island in `audio-capture`; every
  other crate either declares `#![forbid(unsafe_code)]` or contains no `unsafe` at all
  (confirmed by `cargo-geiger`).
- **`clippy` pedantic** runs as `-D warnings`, and every public item is documented.
- No TLS or cloud dependency: the bridge only speaks plain HTTP to Sonos on your LAN.

A reproducible snapshot of these numbers (`cargo-geiger`, `cargo-llvm-cov`,
`cargo-audit`, `cargo-deny`) lives in [docs/QUALITY.md](docs/QUALITY.md). To report a
vulnerability, see [SECURITY.md](SECURITY.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). In short: `cargo fmt`, keep `cargo clippy
--all-targets -- -D warnings` clean, add tests, and only add **permissively-licensed**
dependencies (the build fails otherwise via `cargo-deny`).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at
your option.