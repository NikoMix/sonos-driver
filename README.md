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

🚧 **Early development.** Phase 0 (the workspace, the `audio-core` trait contracts,
the L16/WAV encoders, CLI skeleton, CI, and supply-chain gating) is complete and
green. The hardware-touching pieces (WASAPI capture, Sonos SSDP/UPnP control, the HTTP
server, FLAC, the DSP pipeline) are tracked as parallel workstreams — see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and the
[open issues](https://github.com/NikoMix/sonos-driver/issues).

## Features (planned & in progress)

- 🎧 **System audio capture** via user-mode WASAPI loopback — no driver install.
- 📡 **Sonos discovery & control** over SSDP + UPnP (`SetAVTransportURI` / `Play`).
- 🔀 **Selectable profiles:** `low-latency` (L16 PCM), `wav`, `flac` (lossless),
  `aac`/`mp3` (compatibility).
- 🧩 **Modular crates** built on stable trait contracts — testable without hardware.
- 📊 **Telemetry:** structured `tracing` logs and latency/underrun/reconnect metrics.
- 🛡️ **Permissive licensing only**, enforced by `cargo-deny`; zero known
  vulnerabilities, enforced by `cargo-audit`.

## Profiles

```text
$ sonos-driver profiles
  L16Pcm   ~350-850 ms  lossless   Uncompressed 16-bit PCM (audio/L16) — lowest latency
  Wav      ~400-1000 ms lossless   WAVE-framed 16-bit PCM — lossless, widely compatible
  Flac     ~400-1000 ms lossless   FLAC lossless compression — quality with moderate latency
  Aac      ~1200-3000 ms lossy     AAC lossy compression — compatibility, higher latency
  Mp3      ~1200-3000 ms lossy     MP3 lossy compression — compatibility, higher latency
```

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

```powershell
# List Sonos rooms on the network (WS-B)
sonos-driver list

# Stream system audio to a room with the lowest-latency profile (WS-A/C/F)
sonos-driver stream --room "Living Room" --profile low-latency
```

> Commands that depend on not-yet-implemented workstreams currently exit with a clear
> `NotImplemented` message and a non-zero status.

## Architecture

A Cargo workspace of small, single-responsibility crates built against the trait
contracts in `audio-core`:

```text
audio-core ── traits & types (AudioFrame, Encoder, Transport, AudioSource, Config)
audio-capture ── WASAPI loopback           (WS-A)
audio-pipeline ── resample/convert/jitter  (WS-E)
audio-codec ── L16/WAV done; FLAC/AAC      (WS-D)
stream-server ── chunked HTTP server       (WS-C)
sonos-control ── SSDP + UPnP control       (WS-B)
sonos-driver-core ── orchestration         (WS-F)
sonos-driver-cli ── command-line UX        (WS-G)
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the data flow and threading
model, and [docs/adr/](docs/adr/) for the key decisions.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). In short: `cargo fmt`, keep `cargo clippy
--all-targets -- -D warnings` clean, add tests, and only add **permissively-licensed**
dependencies (the build fails otherwise via `cargo-deny`).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at
your option.