# `DERIVATION.md` — how `sonosvad` was derived from Microsoft's audio sample

This is a record of an operation that has already been performed, not a plan.
The derived source lives in [`sonosvad/Source/`](sonosvad/Source/) and is built
by [`scripts/Build-Driver.ps1`](scripts/Build-Driver.ps1). Read this to
understand *what was changed and why*, and to re-do the derivation against a
newer version of the upstream sample.

> **Licensing — MS-PL, not MIT.**
> [`microsoft/Windows-driver-samples`](https://github.com/microsoft/Windows-driver-samples)
> is licensed under the **Microsoft Public License (MS-PL)**. Earlier revisions
> of this document claimed MIT; that was wrong.
>
> MS-PL is a weak-copyleft, file-scoped licence. The obligations we are actually
> under:
>
> - **§3(D)** — if we distribute any portion of the sample **in source form**, it
>   must remain under MS-PL, and we must ship a complete copy of the licence.
>   Every derived file therefore carries an MS-PL header, and
>   [`sonosvad/LICENSE-MS-PL.txt`](sonosvad/LICENSE-MS-PL.txt) is committed
>   alongside it.
> - **§3(D)** — distributing only the *compiled* `sonosvad.sys` is permitted under
>   a licence of our choosing, provided it does not contradict MS-PL.
> - **§3(B)** — no patent grant survives if we sue over patents in it.
> - **§4** — no trademark rights: we must not imply Microsoft endorsement, which
>   is why nothing user-visible says "Microsoft" or "Sysvad".
>
> The rest of this repository is under its own licence; MS-PL applies to the
> derived kernel sources under `driver/sonosvad/Source/` only.

## 0. Which sample, and why not the other one

The obvious candidate is **`audio/sysvad`**, and earlier revisions of this
document specified it. We used **`audio/simpleaudiosample`** instead.

Sysvad is a *demonstration of breadth*: it models a fake hardware codec with
offloaded streaming, keyword-spotter burst capture, cellular/handset endpoints,
sideband tone generation and a USB sideband topology. Almost all of it exists to
exercise driver features we do not want. A null-sink virtual endpoint needs none
of it, and every unused subsystem is code we would have to read, keep compiling,
and defend in review.

`simpleaudiosample` is the same PortCls/WaveRT scaffolding with the hardware
fiction reduced to a timer. That is exactly our problem: we are a sink that must
consume a render stream at real time and nothing else.

```text
git clone --filter=blob:none --no-checkout --depth 1 \
    https://github.com/microsoft/Windows-driver-samples.git wds
cd wds && git sparse-checkout init --cone \
       && git sparse-checkout set audio && git checkout
# Base path used below: wds/audio/simpleaudiosample/
```

## 1. What "derivation" meant in practice

The sample was copied to `driver/sonosvad/Source/` preserving its layout
(`Inc/`, `Main/`, `Filters/`, `Utilities/`), then changed as follows.

### 1.1 Identity

Every `SIMPLEAUDIOSAMPLE`/`SimpleAudioSample` token became `SONOSVAD`/`SonosVad`,
including the `.sys` name, the service name, the `DriverEntry` tag pool tags, and
the resource-script strings in `sonosvad.rc`.

**All GUIDs were regenerated.** Reusing a sample's interface or device GUIDs is a
real bug, not a cosmetic one: two drivers claiming the same interface GUID on one
machine collide in `IoRegisterDeviceInterface`, and a machine that has ever had
the sample installed would resolve our endpoints against its INF.

### 1.2 Endpoint set

The sample exposes one render and one capture endpoint. `sonosvad` exposes
**eight render endpoints and no capture endpoints**.

Capture pins were removed rather than left unused. An exposed capture endpoint
would appear in the Windows Sound control panel as a phantom recording device,
and would be selectable as a microphone.

Eight is a fixed, INF-declared count. It is a deliberate ceiling, chosen because
it comfortably exceeds a typical household's room count while keeping the
`SubdeviceDescriptor` table reviewable. To change it, edit
[`sonosvad/tools/Generate-Inx.ps1`](sonosvad/tools/Generate-Inx.ps1) and the
miniport pair table together — they must agree, and
[`scripts/Test-Driver.ps1`](scripts/Test-Driver.ps1) asserts that they do.

Endpoints are named `Sonos Output 1`..`Sonos Output 8` in the INF. These are
*factory* names; the user-mode service renames them to room names at runtime and
restores them on uninstall. See ADR-0018.

### 1.3 The sample's tone generator was removed

`simpleaudiosample` synthesises a sine wave into the capture buffer. We deleted
it. `sonosvad` never *produces* audio, and a driver that can emit tones into a
stream is a driver that can emit tones into a stream by accident.

What is kept is the sample's **timer-driven position advance**: `WaveRT` requires
that the play position advance at real time so the audio engine's clock does not
stall. This is the single behaviour the driver must get exactly right, and it is
what `sonos-driver vad probe` measures from user mode.

## 2. What was *not* built, and why

Two subsystems specified by ADR-0017 were deliberately never implemented. Both
were the riskiest parts of the design, and both turned out to be unnecessary.
ADR-0018 supersedes ADR-0017 §3 with the reasoning; in summary:

| ADR-0017 called for | Status | Why not |
| --- | --- | --- |
| A paired **loopback-capture pin** per render endpoint, to get PCM back to user mode | **Not built** | WASAPI loopback (`AUDCLNT_STREAMFLAGS_LOOPBACK`) is implemented by the *audio engine in user mode* and works against any shared-mode render endpoint. The kernel does not need to participate at all. |
| An **IOCTL shared-ring** fallback for the same purpose | **Not built** | Same reason. It existed only as a hedge against the pin approach failing. |
| A **user-mode control interface** creating/destroying subdevices dynamically as rooms appear | **Not built** | Endpoints are static; the service *renames* them via `IMMDevice::OpenPropertyStore(STGM_READWRITE)` + `PKEY_Device_FriendlyName`. This is what the Sound control panel itself does. It needs only admin rights and removes all `PcRegisterSubdevice` PnP-lifetime risk. |

The net effect is that `sonosvad.sys` has **no user-mode interface of its own**:
no IOCTLs, no custom device interface, no shared memory. Its entire contract with
the rest of the system is "be a well-behaved render endpoint". That is the
smallest kernel attack surface this product can have, and it is why the kernel
component is ~49 KB and static analysis is clean.

## 3. Build, sign, install

See [`README.md`](README.md) and [`../docs/DRIVER-SETUP.md`](../docs/DRIVER-SETUP.md).
In short:

```powershell
driver\scripts\Build-Driver.ps1        # MSBuild via the WDK
driver\scripts\New-TestCert.ps1        # one-off self-signed test cert
driver\scripts\Sign-Driver.ps1         # sign .sys and .cat
driver\scripts\Test-Driver.ps1         # static validation (runs anywhere)
driver\scripts\Enable-TestSigning.ps1  # requires a reboot
driver\scripts\Install-Driver.ps1      # pnputil stage + devgen node creation
```

Two traps worth knowing, both of which cost real debugging time:

- **`pnputil /add-driver` alone does nothing visible.** It stages the package into
  the driver store. A root-enumerated device has no bus to enumerate it, so no
  device node ever appears and no endpoints are created. `devgen /add /bus ROOT`
  is what actually creates the node. `Install-Driver.ps1` does both.
- **`stampinf` rewrites the INF through a memory-mapped view.** On a share — such
  as a Windows Sandbox `<MappedFolder>` — that write is silently discarded and
  `stampinf` still exits 0, leaving `$ARCH$` unresolved and InfVerif failing with
  `error 1274`. Build output must be on a local volume.

## 4. Re-deriving against a newer upstream sample

1. Fetch the new `audio/simpleaudiosample` as in §0.
2. Diff it against the vendored copy's *original* upstream state — the MS-PL
   header block on each file records which sample it came from.
3. Re-apply §1.1–§1.3. The identity rename is mechanical; the endpoint-count and
   tone-generator changes are the parts that need judgement.
4. Run `driver\scripts\Test-Driver.ps1`. It checks INF/miniport endpoint-count
   agreement, absence of capture pins, GUID uniqueness against the sample, and
   the PE/import surface — i.e. exactly the invariants §1 establishes.
