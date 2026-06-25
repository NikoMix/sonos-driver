# `DERIVATION.md` — how to build `sonosvad` from Microsoft's Sysvad sample

This document is the **honest substitute for a vendored kernel driver**. Rather
than ship a half-written or untested `.sys`, it specifies the exact, reviewable
changes to apply to Microsoft's **Sysvad** sample to produce the `sonosvad`
virtual-audio driver that backs ADR-0017. An owner with the WDK can follow it to
assemble, build, and test-sign the driver on their own machine.

> **Licensing.** Sysvad ships under the **MIT License** as part of
> [`microsoft/Windows-driver-samples`](https://github.com/microsoft/Windows-driver-samples).
> MIT permits derivation and redistribution; it requires that Microsoft's
> copyright notice and the MIT permission notice be preserved in any substantial
> portion of the source. **Keep the original Sysvad file header on every file you
> copy or adapt, and add a short "Modified for sonos-driver" note beneath it.**

## 0. Get the base

```text
git clone https://github.com/microsoft/Windows-driver-samples.git
# Base path used below:
#   Windows-driver-samples/audio/sysvad/
```

Sysvad already implements a PortCls/WaveRT virtual audio adapter with render and
capture endpoints and a topology miniport. We reuse that scaffolding and change
four things: identity, endpoint set, a loopback-capture bridge per render
endpoint, and a user-mode control interface.

## 1. Identity & INF

- Copy `audio/sysvad/` to this repo's `driver/src/` (owner-local; not committed
  unless you choose to vendor it under MIT).
- Rename the driver/binary to `sonosvad` (`sonosvad.sys`).
- Replace the sample INF with [`../sonosvad.inf`](../sonosvad.inf): a unique
  device hardware id (`Root\sonosvad`), a friendly name, the `MEDIA` class, and
  the install/coinstaller sections. Generate a fresh INF `ClassGuid` device
  instance and a new interface GUID — do **not** reuse Sysvad's sample GUIDs.

## 2. Endpoint topology — start with ONE static render endpoint (Phase 0/2)

In the Sysvad endpoint tables (`endpoints.h` / the `*Topology`/`*Wave` miniport
registration), **reduce to a single render endpoint** named generically
(`Sonos Virtual Output`). This is the Phase 0/2 milestone: prove one signed,
selectable endpoint appears in *Sound → Output* and accepts audio.

Key files to touch (names as in the current Sysvad tree):

- `minwavert.cpp` / `minwavert.h` — the WaveRT render miniport.
- `mintopo.cpp` / `mintopo.h` — the topology miniport (jack/endpoint metadata).
- `endpoints.h` — endpoint descriptors and pin tables.
- `adapter.cpp` — adapter init / subdevice registration.

## 3. Kernel→user PCM bridge — paired loopback-capture pin (Phase 1)

Preferred bridge per ADR-0017 §3: expose each render endpoint's mixed PCM via a
**paired loopback-capture pin**, so the existing Rust `audio-capture` WASAPI
loopback path can read it with minimal new glue.

- Add a **capture pin** to the render miniport that mirrors the render stream's
  format and is fed from the render data path (Sysvad's "looped" buffer mechanism
  is the starting point — it already maintains a DPC-driven ring for its synthetic
  capture; repoint it from the tone generator to the render mix).
- Mark the capture endpoint with the loopback role so user-mode WASAPI opens it in
  loopback mode.
- **Fallback (only if the pin approach proves too constraining):** a custom IOCTL
  shared-ring (`METHOD_OUT_DIRECT`) the user-mode service polls. More code, more
  control. Decide on real hardware.

Validate Phase 1 by reading real samples from the endpoint with the existing Rust
capture before wiring the rest of the pipeline.

## 4. User-mode control interface — dynamic endpoints (Phase 3)

This is the most advanced part and is sequenced last.

- Add a **control device interface** (a separate symbolic-linked device, e.g.
  `\\.\SonosVadControl`) that user mode opens and sends commands to.
- Implement two IOCTLs mapping 1:1 onto the Rust
  [`ControlCommand`](../../crates/sonos-vad-control/src/protocol.rs) enum:
  - `IOCTL_SONOSVAD_CREATE_ENDPOINT` — input: room UUID + UTF-16 friendly name;
    output: the assigned `EndpointId` (`u32`). Dynamically registers a new render
    subdevice (+ its paired loopback pin) and returns its id.
  - `IOCTL_SONOSVAD_DESTROY_ENDPOINT` — input: `EndpointId`. Tears the subdevice
    down.
- Surface completion/asynchrony as the Rust
  [`DriverEvent`](../../crates/sonos-vad-control/src/protocol.rs) enum
  (`EndpointCreated` / `EndpointDestroyed` / `CreateFailed` / `DestroyFailed`),
  either as IOCTL return status or via an inverted-call notification IOCTL the
  service keeps pending.
- The serialisation/agreed field layout is defined in Rust today (serde JSON for
  tests; a fixed C struct layout for the IOCTL buffers at implementation time).
  The Rust `EndpointRegistry` already plans and sequences these commands and folds
  the events back — the driver only has to execute one command at a time.

Dynamic subdevice add/remove (`IoRegisterDeviceInterface` /
`PcRegisterSubdevice` lifetimes, PnP state) is the riskiest area and is sparsely
documented publicly — budget accordingly.

## 5. Build, sign, install

Use the scripts in [`./scripts`](./scripts) and the runbook in
[`../../docs/DRIVER-SETUP.md`](../../docs/DRIVER-SETUP.md):
`Enable-TestSigning` → `New-TestCert` → `Build-Driver` → `Sign-Driver` →
`Install-Driver` (and the matching uninstall/disable pair to revert).

## Mapping summary (driver ⇄ Rust control plane)

| Kernel (C/C++, this folder) | User-mode (Rust, `sonos-vad-control`) |
| --- | --- |
| `IOCTL_SONOSVAD_CREATE_ENDPOINT` | `ControlCommand::CreateEndpoint { room, device_name }` |
| `IOCTL_SONOSVAD_DESTROY_ENDPOINT` | `ControlCommand::DestroyEndpoint { id }` |
| create result / notification | `DriverEvent::EndpointCreated` / `CreateFailed` |
| destroy result / notification | `DriverEvent::EndpointDestroyed` / `DestroyFailed` |
| render endpoint friendly name | `device_name_for(&RoomKey, &taken)` |
| endpoint id (`u32`) | `EndpointId(u32)` |

Because the Rust side is already implemented and unit-tested, the kernel work can
be validated incrementally against a known-good control plane.
