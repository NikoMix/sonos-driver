# `driver/` — kernel virtual-audio driver (owner-built, NOT CI-built)

This folder holds the Windows kernel virtual-audio driver `sonosvad`, introduced
in [ADR-0017](../docs/adr/0017-kernel-virtual-audio-driver.md) and reshaped by
[ADR-0018](../docs/adr/0018-loopback-rename-and-service-hosting.md). Its purpose is to
make **each Sonos room appear as its own Windows output device** — plus an
**All Sonos** device that plays to the whole household in sync — so any
application, or the whole system, can be routed to a room from the OS mixer.

## Read this first

- **This is optional.** The [driverless mode](../docs/adr/0019-driverless-routing-mode.md)
  (`sonos-driver cast`) streams to any room, groups rooms, and routes individual
  applications with nothing installed, no reboot and Secure Boot left on. The
  driver exists for the one thing that mode cannot do: put rooms in the Windows
  sound picker. Windows has no user-mode API to create an audio endpoint.
- **Nothing here is compiled, signed, or installed by CI.** A Windows kernel
  driver requires the full **WDK/EWDK**, an elevated shell, and a reboot for
  test-signing. The repository's pipeline cannot do any of that, so the driver
  build is a **documented local, owner-side step**.
- **This folder contains the full derived driver source** under
  [`sonosvad/Source/`](./sonosvad/Source), which builds clean to a ~49 KB
  `sonosvad.sys`. It also provides:
  - [`DERIVATION.md`](./DERIVATION.md) — what was changed in Microsoft's
    **`simpleaudiosample`** sample (**MS-PL**) to produce this driver, and why.
  - [`sonosvad/Source/Main/sonosvad.inx`](./sonosvad/Source/Main/sonosvad.inx) —
    the installation manifest template (device class, install sections, the nine
    render endpoints), stamped into `sonosvad.inf` at build time. It is
    **generated** by [`sonosvad/tools/Generate-Inx.ps1`](./sonosvad/tools/Generate-Inx.ps1);
    never hand-edit it.
  - [`scripts/`](./scripts) — PowerShell helpers for the whole lifecycle
    (test-signing on/off, test cert, build, sign, static validation, install,
    uninstall, and the on-host test suite).

## Provenance & licensing

The driver is **derived from Microsoft's `simpleaudiosample` sample**
(`Windows-driver-samples`), which is licensed under the **Microsoft Public
License (MS-PL)** — redistributable, no purchase.

MS-PL §3(D) requires that any portion we distribute **in source form** stays
under MS-PL and ships with a complete copy of the licence, so every derived file
carries an MS-PL header and
[`sonosvad/LICENSE-MS-PL.txt`](./sonosvad/LICENSE-MS-PL.txt) is committed beside
them. §4 grants no trademark rights, so nothing user-visible references
Microsoft. Full detail in [`DERIVATION.md`](./DERIVATION.md).

No paid component is introduced: the WDK is a free Microsoft SDK, and personal-use
**test-signing is free**. See [ADR-0017](../docs/adr/0017-kernel-virtual-audio-driver.md)
§5 and ADR-0014 §3 for the signing rationale and the distribution boundary.

## How it fits together

```text
Windows audio engine
  app / default output ──▶ render endpoint "Wohnzimmer"   ┐
                           render endpoint "Küche"        │  sonosvad.sys
                           render endpoint "All Sonos"    │  (this folder, C++,
                           ...  9 fixed endpoints         │   owner-built)
                                    │                     ┘
                                    │  WASAPI loopback capture (user mode)
                                    ▼
  sonosvad-svc:  capture ▶ FLAC ▶ live-edge HTTP ▶ UPnP play ▶ Sonos room
        (crates/, pure Rust — renames endpoints, hides spares, streams)
```

The driver is a **null sink**: it accepts the render stream and advances the
WaveRT position at exactly real time. It never sees the audio data. Two ADR-0018
decisions removed the riskiest kernel work:

1. **No kernel→user PCM bridge.** WASAPI loopback capture is a *user-mode* audio
   engine feature and works against any shared-mode render endpoint, so no IOCTL
   path, no ring buffer and no custom pin were needed.
2. **No dynamic subdevice creation.** The endpoints are fixed in the INF; the
   service renames them to discovered room names via `IPropertyStore` +
   `PKEY_Device_FriendlyName` and hides the spares.

The endpoint count is **9** and must agree in three places —
`Source/Inc/definitions.h` (`SONOSVAD_ENDPOINT_COUNT`), `Source/Filters/minipairs.h`,
and the generated INF. `Test-Driver.ps1` checks that they do. Change it via
`SonosVadEndpointCount` in `sonosvad.vcxproj`, then regenerate the INX.

The user-mode side lives in [`crates/sonos-vad-control`](../crates/sonos-vad-control)
(OS-independent endpoint/room binding logic, fully unit-tested) and
[`crates/sonos-vad-service`](../crates/sonos-vad-service) (the Windows service).

## Build & install

Follow [`docs/DRIVER-SETUP.md`](../docs/DRIVER-SETUP.md) end-to-end. In short,
from an **elevated** shell at the repo root:

1. `driver/scripts/Enable-TestSigning.ps1` — then **reboot** (Secure Boot must be off).
2. `driver/scripts/New-TestCert.ps1`.
3. `driver/scripts/Build-Driver.ps1` — needs the WDK; output goes to
   `sonosvad/artifacts/<Platform>/<Configuration>/`.
4. `driver/scripts/Sign-Driver.ps1` — signs the `.sys` **and** the `.cat`;
   `pnputil` validates the catalog, so signing only the binary fails at install.
5. `driver/scripts/Test-Driver.ps1` — 6 static checks, no install required.
6. `cargo build --release -p sonos-vad-service`, then
   `driver/scripts/Install-Driver.ps1` (add `-SeedHost <ip>` if SSDP is blocked).
7. `driver/scripts/Test-DriverOnHost.ps1` — the load-dependent suite.
8. To revert: `driver/scripts/Uninstall-Driver.ps1` then
   `driver/scripts/Disable-TestSigning.ps1` (reboot).

## Status / roadmap

Tracked under GitHub epic **#44**:

- **#45 — Phase 0:** ADR-0017, the scaffold, the setup runbook, and the
  pure-Rust `sonos-vad-control` control plane. ✅ delivered.
- **#46 — Phase 1:** kernel→user PCM bridge. ❌ closed as **not needed** —
  superseded by WASAPI loopback capture (ADR-0018).
- **#47 — Phase 2:** end-to-end one device → one room. ✅ delivered.
- **#48 — Phase 3:** per-room endpoints driven by SSDP discovery. ✅ delivered,
  as fixed endpoints renamed at runtime rather than dynamic subdevices (ADR-0018).
- **#49 — Phase 4:** packaging and installer integration. Open; the GUI part was
  dropped — the Windows sound UI *is* the interface.
- **#50 — Phase 5 (optional):** attestation signing (EV cert + Partner Center),
  which would let the driver load with Secure Boot on. Open.
- **#60:** on-host validation — the remaining blocker; needs real hardware.
- **#61 — driverless mode** (ADR-0019). ✅ delivered.

`_local/` (build/sign scratch output, including the test certificate) is
git-ignored, as is `sonosvad/artifacts/`.
