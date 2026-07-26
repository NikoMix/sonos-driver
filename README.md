# sonos-driver

**Your Sonos rooms as Windows audio destinations.** Send this PC's audio to the
kitchen, or to every room at once in sync, and change your mind while it plays.

There are three ways to do it, covered below: a small **routing app** that needs
no install, a **CLI/service** for headless and per-application setups, and a
**virtual audio driver** that puts each room in the Windows sound picker itself.

Underneath all three sits a **pure-Rust user-mode bridge** that captures a
Windows output (WASAPI loopback) and streams it to the matching Sonos room, with
selectable latency/quality profiles.

> [!IMPORTANT]
> **Latency reality.** Sonos players enforce a non-configurable internal buffer
> (~300–800 ms) on every network source, so **true sub-100 ms streaming to Sonos is
> impossible in software** — by anyone, not just this project. `sonos-driver` reaches
> the *lowest achievable* network latency (~350–850 ms with uncompressed L16 PCM on a
> good LAN) and is honest about the floor. See
> [docs/LATENCY-BUDGET.md](docs/LATENCY-BUDGET.md).

> [!NOTE]
> **Start with the app.** [Sonos Router](#sonos-router-the-app) needs no install,
> no reboot and no administrator rights: launch it, pick a room next to each
> output, press Start. The **[virtual audio driver](#rooms-as-windows-outputs)**
> is the only way to get rooms into the Windows sound picker itself, but it is
> personal-use and test-signed rather than WHQL-certified, so loading it needs
> test signing enabled — see [docs/DRIVER-SETUP.md](docs/DRIVER-SETUP.md).

## Three ways to run it

|  | **Sonos Router** (app) | **Driverless CLI** | **Driver mode** |
| --- | --- | --- | --- |
| Rooms in the Windows sound picker | ❌ | ❌ | ✅ |
| Change routing while it plays | ✅ | ❌ (edit + restart) | ✅ (sound picker) |
| Per-app routing (Spotify → Küche) | ❌ | ✅ | ❌ |
| Several rooms as one synced group | ✅ | ✅ | ✅ |
| Needs an install + reboot | ❌ | ❌ | ✅ |
| Needs administrator rights | ❌ | ❌ | ✅ |
| Works with Secure Boot on | ✅ | ✅ | ❌ (until [attestation-signed](docs/adr/0019-driverless-routing-mode.md)) |
| Runs without a logged-in session | ❌ (use `sonoscast`) | ✅ (as a service) | ✅ |
| Route chosen by | the app's table | `cast.toml` or CLI flags | the Windows sound UI |

The app and the CLI are the **same driverless engine** with different front
ends, and they share one config file — configure it in the app, then let the
`sonoscast` service run the very same routes headless at boot if you want them
always on.

The driver is not a competitor to either: Windows routes audio *by endpoint*, so
only the driver can put a room in the sound picker, and only the driverless mode
can split by application. All three drive the same streaming pipeline and all
can be installed at once. The reasoning, including why no user-mode API can
create an audio endpoint, is in
[ADR-0019](docs/adr/0019-driverless-routing-mode.md); the app's design is in
[ADR-0020](docs/adr/0020-runtime-routing-app.md).

## Status

**Feature complete in all three modes; the driver awaits host validation.**

**Sonos Router** (the app) is complete and validated against a live household —
output enumeration, discovery, assignment, apply, retry and clean shutdown are
exercised end to end by `cargo run -p sonos-driver-gui --example smoke`.

The **driverless CLI/service is complete and needs no host validation** —
nothing to install means nothing that can fail to load. Its planner has been
exercised against a live household with `sonos-driver cast --dry-run`.

`sonosvad`, the **driver**, builds, signs and passes every static check
(InfVerif, ApiValidator, PE/import and catalog/signature). Load-dependent
verification — installing the driver, Driver Verifier, the audio round-trip, and
the grouping calls against real speakers — has to run on a machine where test
signing can be enabled, via
[`driver/scripts/Test-DriverOnHost.ps1`](driver/scripts/Test-DriverOnHost.ps1).

The Rust workspace is green across **407 tests** with `clippy -D warnings` clean
at pedantic level.

The user-mode bridge is unchanged and complete: capture → pipeline → encode →
serve → control, wired by
[`Bridge::run()`](crates/sonos-driver-core/src/lib.rs). Drive it from the app,
from the `clap` CLI (`list` / `stream` / `cast` / `stop` / `config` / `vad`), or
let one of the two headless services run it — `sonosvad-svc` for the driver
mode, `sonos-cast-svc` for the driverless one.

Everything except the Windows audio hardware path is exercised by automated tests
(including a hardware-free end-to-end latency harness). WASAPI loopback cannot run in
CI, so the capture path is verified with a
[manual checklist](docs/AUDIO-CAPTURE-MANUAL-CHECKLIST.md). See the
[changelog](CHANGELOG.md) for the full capability set and
[docs/QUALITY.md](docs/QUALITY.md) for the code-quality/telemetry snapshot.

## Features

- 🎛️ **Sonos Router** — a small app that lists every Windows output and every
  Sonos room and lets you point one at the other, live, with no install and no
  administrator rights. Assignments survive replugging a USB DAC, and it keeps
  streaming when a speaker is switched off.
- 🔊 **One Windows output per Sonos room**, named after the room, via the
  `sonosvad` virtual audio driver plus a headless service that binds them.
  Unused outputs are hidden, so the picker shows your rooms and nothing else.
- 🏠 **"All Sonos" / All rooms** — plays to the whole household as a real,
  synchronised Sonos group (not parallel streams, which drift audibly).
  Available both as a driver endpoint and as a destination in the app.
- 🎚️ **Last-audio-wins.** Start audio on a room while "All Sonos" is playing and
  that room leaves the party; when it falls silent, the party takes it back.
- 💤 **Idle release.** A silent output hands its room back to the Sonos app
  rather than holding it hostage, and reclaims it the moment audio returns.
- 🚫 **Driverless mode** for machines where installing a driver is not on the
  table: `sonos-driver cast` streams to any rooms with nothing installed, no
  reboot and Secure Boot left on.
- 🎯 **Per-application routing** via WASAPI process loopback — send Spotify to
  the kitchen and everything else to the living room, at the same time.
  Impossible through the driver, because Windows routes by endpoint.
- 🎧 **System audio capture** via event-driven, user-mode WASAPI loopback — the
  bridge alone needs no driver install, with a silence keep-alive so frames keep
  flowing even when nothing is playing.
- 📡 **Sonos discovery & control** over SSDP + UPnP SOAP (`SetAVTransportURI` / `Play` /
  `Stop`, volume, and zone-group topology so a stereo pair or group is driven by its
  coordinator), plus **seed-host discovery** for networks where multicast is
  blocked (NAT'd VMs, guest wifi, APs without IGMP).
- 🔀 **Selectable lossless profiles:** `flac` (default — the format real Sonos players
  reliably accept over the live stream), `wav`, and `low-latency` (L16 PCM) — all
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
| `L16Pcm` | `low-latency` | Big-endian 16-bit PCM (`audio/L16`, RFC 2586) | ✅ Implemented — lowest latency (many Sonos players reject it; see [ADR-0016](docs/adr/0016-default-profile-flac.md)) |
| `Wav` | `wav` | RIFF/WAVE 16-bit PCM (`audio/wav`) | ✅ Implemented — lossless, broad compatibility |
| `Flac` | `flac` | FLAC lossless (`audio/flac`, pure-Rust `flacenc`) | ✅ Implemented — **default**, ~½ the bandwidth, best Sonos compatibility |
| `Aac` | `aac` | AAC | ⛔ Not implemented — no licence-clean pure-Rust encoder ([ADR-0004](docs/adr/0004-aac-mp3-codec-evaluation.md)) |
| `Mp3` | `mp3` | MP3 | ⛔ Not implemented — same as AAC |

> [!NOTE]
> `aac` and `mp3` are listed for completeness but currently fail fast with a clear
> `NotImplemented` error when selected: no MIT/Apache-licensed, pure-Rust encoder for
> them exists, and the project's permissive-licence-only rule rules out the C-based
> alternatives. See [docs/LATENCY-BUDGET.md](docs/LATENCY-BUDGET.md) and
> [ADR-0004](docs/adr/0004-aac-mp3-codec-evaluation.md).

## Quickstart

### Sonos Router (the app)

The fastest route to sound in another room. No install, no reboot, no
administrator rights.

```powershell
# 1. Build it (see Building for the MSVC prerequisite).
cargo build --release -p sonos-driver-gui

# 2. Run it.
.\target\release\sonos-router.exe
```

The window lists every audio output this PC has, one per row, with **Default
output** at the top — that row follows whatever Windows is currently set to, so
it keeps working when you switch headphones. Pick a room next to an output (or
**All rooms**), press **Start**, and audio playing on that output appears on the
speakers.

- **Change your mind while it plays.** Reassign a row and press **Apply**.
- **Closing the window keeps it running** in the notification area; audio stops
  only when you quit, because the app *is* the streamer.
- **Your routes are remembered** in `%APPDATA%\sonos-driver\routes.toml`, and
  that file is the same format the `sonoscast` service reads — configure it in
  the app, then run it headless at boot if you want it always on.
- **If no rooms appear**, your network is blocking multicast. Open **Settings**,
  enter the IP of any one Sonos player as the seed host, and rescan; it will
  find the rest from there.

### Rooms as Windows outputs (the driver)

This installs the `sonosvad` driver and the headless service, after which every
room shows up in the Windows sound picker itself — no app running, nothing to
launch. It needs an **elevated** shell and a one-time reboot to enable test
signing.

```powershell
# 1. Enable test signing (reboots), then create a test certificate.
.\driver\scripts\Enable-TestSigning.ps1
.\driver\scripts\New-TestCert.ps1

# 2. Build and sign the driver.
.\driver\scripts\Build-Driver.ps1
.\driver\scripts\Sign-Driver.ps1

# 3. Build the service, then install both. Install-Driver.ps1 stages the
#    driver, creates its device node, and registers the service for you.
cargo build --release -p sonos-vad-service
.\driver\scripts\Install-Driver.ps1
Start-Service sonosvad
```

Open **Settings → System → Sound** and your rooms are there, plus **All Sonos**.
Pick one and play something. To undo it all, run
`.\driver\scripts\Uninstall-Driver.ps1` followed by
`.\driver\scripts\Disable-TestSigning.ps1`.

If discovery finds nothing — common on networks that block multicast — point the
service at any one player and it will find the rest:
`.\driver\scripts\Install-Driver.ps1 -SeedHost 192.168.1.50`. Full runbook,
including verification and rollback, in
[docs/DRIVER-SETUP.md](docs/DRIVER-SETUP.md).

### Driverless CLI (scripting and per-app routing)

The same engine as the app, driven from a terminal — for headless machines,
scripts, and the one thing neither the app nor the driver can do: routing a
single application.

```powershell
# 1. Build the release binary (see Building for the MSVC prerequisite).
cargo build --release -p sonos-driver-cli

# 2. Find your Sonos rooms.
.\target\release\sonos-driver.exe list

# 3. Send this PC's audio to every room, as one synced group. Ctrl-C to stop.
.\target\release\sonos-driver.exe cast --all

# ...or to specific rooms; the first one named coordinates the group.
.\target\release\sonos-driver.exe cast --room "Wohnzimmer" --room "Küche"

# Check what a command or config resolves to before it plays anything.
.\target\release\sonos-driver.exe cast --all --dry-run
```

**Per-application routing** is the thing the driver cannot do — Windows routes
audio by endpoint, not by app. Write the routes to
`%ProgramData%\sonos-driver\cast.toml` and run a bare `sonos-driver cast`:

```toml
[[route]]
process = "spotify.exe"
rooms = ["Küche"]

[[route]]
except-process = "spotify.exe"
rooms = ["Wohnzimmer", "Schlafzimmer"]
```

Both routes run at once. A room may appear in only one route — overlapping rooms
are rejected at startup with the conflicting route numbers, rather than fighting
over the speaker at runtime. Process loopback needs Windows 11 / Server 2022
(build 20348+).

To keep it running across reboots, install it as a service:

```powershell
cargo build --release -p sonos-cast-service
.\target\release\sonos-cast-svc.exe install   # also writes a starter cast.toml
Start-Service sonoscast
```

### Streaming to a single room

`stream` predates `cast` and remains the simplest one-room command:

```powershell
.\target\release\sonos-driver.exe stream --room "Living Room" --profile low-latency
```

That's it — start playing audio on your PC and it comes out of the Sonos. If discovery
finds nothing, jump to [Troubleshooting](#troubleshooting).

> **Just want to run it?** Grab a prebuilt **portable zip** or **installer** from the
> [Releases](https://github.com/NikoMix/sonos-driver/releases) page — no Rust toolchain
> needed. To build a portable zip yourself, see
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

### `cast` — driverless streaming to one or more rooms

Same pipeline as `stream`, but it can target several rooms as one group and can
capture a single application. Nothing is installed and nothing needs elevation.

```powershell
# Every room, as one synchronised Sonos group.
sonos-driver cast --all

# Specific rooms; the first one named coordinates the group.
sonos-driver cast --room "Wohnzimmer" --room "Küche"

# Only Spotify's audio, or everything except it.
sonos-driver cast --all --process spotify.exe
sonos-driver cast --all --except-process spotify.exe

# Run every route in the config file (the way to route several apps at once).
sonos-driver cast

# Resolve rooms and ports and print the plan, without playing anything.
sonos-driver cast --all --dry-run
```

| Flag | Meaning | Default |
|------|---------|---------|
| `--room <NAME\|UUID>` | Play here; repeat for several rooms | — |
| `--all` | Play in every discovered room | off |
| `--process <NAME\|PID>` | Capture only this app (build 20348+) | — |
| `--except-process <NAME\|PID>` | Capture everything but this app | — |
| `--device <ID\|NAME>` | Capture this endpoint instead of the default | default render device |
| `--seed-host <HOST>` | Discover via one known player instead of SSDP | — |
| `--routes <PATH>` | Read routes from this file | `%ProgramData%\sonos-driver\cast.toml` |
| `--dry-run` | Print the resolved plan and exit | off |

Naming a room (or `--all`) builds a one-off route and ignores the file; without
one, the file's routes are used. Each route gets its own stream-server port,
allocated upwards from `stream.port`.

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

### `vad` — drive the virtual audio driver

These operate on the `sonosvad` endpoints. `bind`, `release` and `ungroup` write
to endpoint property stores or the speakers, so they need an **elevated** shell.
In normal use the service does all of this for you; reach for these when
something looks wrong.

```powershell
sonos-driver vad status                   # list endpoints and what they're bound to
sonos-driver vad bind --seed-host 1.2.3.4 # discover rooms, rename endpoints to match
sonos-driver vad probe --device "Küche"    # does the endpoint clock run at real time?
sonos-driver vad serve                    # bind, then stream every output to its room
sonos-driver vad ungroup                  # split rooms left grouped by "All Sonos"
sonos-driver vad release                  # restore factory names and visibility
```

`bind` takes `--keep-spare-endpoints` to leave unused outputs visible in the
sound picker, and `--no-all-sonos` to skip the aggregate output; `serve` accepts
both as well.

`ungroup` matters after a crash: grouping lives in the speakers, not in Windows,
so a service killed mid-party would otherwise leave the household grouped with
no UI to undo it. The driver journals exactly which rooms it absorbed, replays
that on service start, and `Uninstall-Driver.ps1` runs it before removing the
driver — so you should rarely need it by hand.

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
# Codec / latency profile: "flac" (lossless, default — most compatible with
# Sonos), "wav", "l16-pcm" (lowest latency), "aac" or "mp3". See
# `sonos-driver profiles` and docs/LATENCY-BUDGET.md.
profile = "flac"
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

The zip bundles the three user-mode binaries (`sonos-driver.exe`,
`sonos-router.exe` and `sonos-cast-svc.exe`), `README.md`, `CHANGELOG.md`, both
licence files, and an example `sonos-driver.toml`. Run
`Get-Help scripts/package.ps1 -Detailed` for options (skip the build, change the output
directory, etc.). The script must run from a shell with the MSVC environment initialised,
just like a normal build.

To ship the kernel driver too, build and sign it first, then add `-IncludeDriver`:

```powershell
.\driver\scripts\Build-Driver.ps1
.\driver\scripts\Sign-Driver.ps1
pwsh -File scripts/package.ps1 -IncludeDriver
```

That adds `sonosvad-svc.exe` and a `driver\` folder holding the signed
`sonosvad.sys`/`.inf`/`.cat`, every install script, and an `INSTALL.md` with the
exact commands for the payload's flat layout. A missing or unsigned driver is a
hard error rather than a silent omission, so a release can never claim to
contain a driver it does not. You can re-verify a downloaded package without the
source tree: `.\driver\scripts\Test-Driver.ps1 -ArtifactDir .\driver` (the one
check that compares the INF against the C++ headers reports SKIP).

### Building the installer

[`scripts/build-installer.ps1`](scripts/build-installer.ps1) compiles a single-file
Windows installer (`sonos-driver-<version>-x86_64-setup.exe`) from the **same payload** as
the zip, using [Inno Setup](https://jrsoftware.org/isinfo.php) (free for commercial use, no
paid licence):

```powershell
# Requires the Inno Setup compiler: choco install innosetup
pwsh -File scripts/build-installer.ps1

# ...with the driver offered as an optional component
pwsh -File scripts/build-installer.ps1 -IncludeDriver
```

With `-IncludeDriver` the wizard gains a **Full** setup type that lays down the
driver package. It deliberately does **not** install the driver for you: that
needs test signing, which needs Secure Boot off and a reboot, and it should be a
conscious decision rather than a consequence of clicking Next. The installer
places the files and points at `driver\INSTALL.md`.

If the Inno Setup compiler isn't installed, the script warns and skips the installer (the
portable zip is unaffected). The release workflow installs it automatically and publishes
the zip, the installer, and a `SHA256SUMS.txt` to the GitHub Release. See
[ADR-0015](docs/adr/0015-release-pipeline.md) and [docs/RELEASING.md](docs/RELEASING.md).

## Architecture

A Cargo workspace of small, single-responsibility crates built against the trait
contracts in `audio-core`:

```text
audio-core ──── traits & types (AudioFrame, Encoder, Transport, AudioSource, Config)
audio-capture ─ WASAPI loopback capture          (the sole unsafe/FFI island)
audio-pipeline ─ resample / channel-convert / jitter buffer
audio-codec ─── L16 PCM / WAV / FLAC encoders    (AAC/MP3 not implemented)
stream-server ─ chunked HTTP server (axum + broadcast ring buffer)
sonos-control ─ SSDP discovery + UPnP AVTransport/RenderingControl SOAP + grouping
sonos-driver-core ─ Bridge orchestration, session lifecycle, arbitration, metrics
sonos-vad-endpoint ─ enumerate / rename / hide the sonosvad render endpoints
sonos-vad-service ─ headless LocalSystem service: one bridge per room  (sonosvad-svc)
sonos-routing ───── driverless planner: routes -> rooms, conflicts, ports (no I/O)
sonos-cast-service ─ driverless host: library + service                 (sonos-cast-svc)
sonos-driver-cli ── clap command-line UX (list / stream / cast / stop / config / vad)
sonos-driver-gui ── Sonos Router: the routing-table desktop app        (sonos-router)
driver/sonosvad ─── C++ PortCls/WaveRT virtual audio driver (MS-PL derivation)
```

The two service crates are the only things that differ between the driver and
driverless modes, and they share everything below `sonos-driver-core`.
`sonos-cast-service` depends on no `sonos-vad-*` crate and the driver depends on
neither, so the driverless path cannot destabilise the kernel path — the
property that makes it a useful diagnostic when something misbehaves. The app
sits on top of `sonos-cast-service` as a third front end to that same engine, so
a route behaves identically however it was configured.

The driver is a **null sink**: it accepts a render stream and advances its
position at exactly real time. Audio never crosses the kernel boundary, because
WASAPI loopback capture — implemented by the user-mode audio engine — already
provides it. That is what keeps the kernel component small enough to trust. See
[ADR-0018](docs/adr/0018-loopback-rename-and-service-hosting.md).

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