# 0019 — A driverless routing mode alongside the kernel driver

- Status: **Accepted**
- Date: 2026-07-26
- Relates to: [ADR-0018](./0018-loopback-rename-and-service-hosting.md),
  [ADR-0014](./0014-virtual-audio-driver-reality.md),
  [ADR-0011](./0011-process-loopback-capture.md),
  [ADR-0017](./0017-kernel-virtual-audio-driver.md)

## Context

[ADR-0018](./0018-loopback-rename-and-service-hosting.md) describes a working
design in which every Sonos room is a selectable Windows audio output. It needs
`sonosvad.sys`, a kernel-mode driver, and therefore it needs a signing story.

That raised a fair question: **does putting a device in the Windows sound picker
really require kernel-mode code, or is there a user-mode way that avoids driver
signing entirely?**

The question is worth recording an answer to, because the intuition behind it is
correct for every *other* device class. Windows 11 added
[`MFCreateVirtualCamera`](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera),
which registers a fully functional virtual camera from user mode with no driver
at all. It is natural to assume audio has the same thing.

It does not.

### Why user-mode audio endpoints do not exist

1. **Endpoints are built from kernel streaming filters.** The Windows
   AudioEndpointBuilder service enumerates KS filters registered under the audio
   device interface classes and constructs one `MMDevice` per pin. `MMDevice`
   enumeration — what the sound picker shows — has no registration API; it is
   purely a projection of what the kernel exposes. There is no documented or
   undocumented user-mode call that adds an entry.
2. **There is no audio counterpart to `MFCreateVirtualCamera`.** Cameras got a
   user-mode registration path; audio did not.
3. **ACX, the modern audio driver framework, is deliberately kernel-only.**
   Microsoft's own documentation states that ACX is built on KMDF *and not* UMDF
   specifically to avoid the latency of task-switching between user and kernel
   mode while streaming. The newest audio framework therefore closes this door
   rather than opening it.
4. **APOs are user-mode but cannot create endpoints.** Audio Processing Objects
   are user-mode COM DSP plugins that attach to an endpoint that already exists,
   and they are registered by a driver's INF in the first place.
5. **Microsoft's own "software" endpoints are kernel drivers.** Bluetooth A2DP
   and Remote Desktop audio both appear in the sound picker and both ship as
   kernel-mode drivers. So does every third-party product in this category
   (VB-CABLE, Virtual Audio Cable, Steam Streaming Speakers).

So the kernel driver is not an implementation preference. For the specific
outcome "the room appears in the Windows sound picker", it is the only mechanism
Windows offers.

### Certification is not the constraint; signing is

The two are routinely conflated, and separating them changes the calculus:

| | Required? | Cost | Secure Boot | Watermark |
| --- | --- | --- | --- | --- |
| **WHQL / HLK certification** | **No** | HLK lab time | — | — |
| **Test signing** | free path | none | must be **off** | yes |
| **Attestation signing** | production path | EV certificate (~€200–500/yr); no Microsoft fee | stays **on** | no |

Attestation signing is an automated Partner Center submission. It runs no HLK
tests and confers no logo; you upload a CAB signed with an EV code-signing
certificate and receive a Microsoft-signed driver that loads on any Windows
10/11 *client* with Secure Boot enabled. It is not certification.

This matters because test signing — the free path this project uses today —
requires **Secure Boot to be disabled**, which is a real security regression on
a daily-driver machine, adds a desktop watermark, and breaks anti-cheat and some
DRM. It is fine for development and acceptable for a deliberate personal-use
install. It is not something to require of someone who just wants to hear Spotify
in the kitchen.

## Decision

**Ship a driverless routing mode as a second, independent front end over the same
streaming core. Keep the kernel driver unchanged and unconditional.**

### 1. Two front ends, one core

```text
                      ┌──────────────────────────┐
  sonosvad.sys  ─────▶ │                          │
  (endpoint per room)  │   sonos-driver-core      │ ──▶ FLAC ──▶ HTTP ──▶ Sonos
                       │   Bridge / GroupPlayer   │
  routes in TOML ─────▶ │   Arbiter / party journal│
  (no driver)          └──────────────────────────┘
```

Both paths converge on `Bridge::run_with(player)`. Everything downstream —
the pipeline, the encoders, the stream server, discovery, grouping, idle release,
the arbiter and the party journal — is shared verbatim.

### 2. The driverless mode routes *sources* to rooms, not endpoints to rooms

With no endpoint to select, routing has to be expressed somewhere else. A
**route** pairs an audio source with a set of rooms:

| Source | Captures |
| --- | --- |
| `default` | whatever the current default render endpoint is playing |
| `device = "…"` | one named render endpoint |
| `process = "spotify.exe"` | only that application's audio |
| `except-process = "teams.exe"` | everything *but* that application |

The last two are WASAPI **process loopback**, which `audio-capture` already
implements. That is the significant find: per-application routing — "Spotify to
the kitchen, everything else to the living room" — was assumed to be a
driver-only capability because on the driver path it is done through the Windows
Volume Mixer. It is not. It works with no driver at all, and several routes can
run concurrently because `MultiRoomBridge` already supervises N independent
sessions.

### 3. A route with several rooms groups them, exactly like All Sonos

A driverless route has one capture and therefore one stream. Pointing several
players at one URL makes them drift, because each buffers independently
(ADR-0018 §6). So a multi-room route resolves to a `GroupPlayer` over the same
`resolve_group`/`disband`/journal machinery. A single-room route stays a plain
`SonosController`.

### 4. It is a separate crate pair, not a mode flag on the driver path

- **`sonos-routing`** — the pure planner: the route model, deterministic port
  allocation, conflict detection when two routes claim the same room, and
  resolution of configured room names against discovered rooms. No Windows APIs,
  so it is fully unit-testable, like `sonos-vad-control`.
- **`sonos-cast-service`** — the headless host (`sonos-cast-svc`), the same
  `install`/`uninstall`/`run`/`console` shape as `sonos-vad-service`.

Keeping them separate means the driverless path cannot destabilise the driver
path: no `sonos-vad-*` crate and no line of `sonosvad` C++ changes to support it.
The cost is a little duplicated service scaffolding, which is a good trade for
being able to reason about the two independently.

### 5. Neither mode is "the fallback"

They answer different questions, and the honest difference is narrow:

| | Driver path | Driverless path |
| --- | --- | --- |
| Room in the Windows sound picker | yes | **no — impossible without a driver** |
| System audio to one room | yes | yes |
| System audio to all rooms, in sync | yes | yes |
| Per-application routing | yes, via Volume Mixer | yes, via process loopback |
| Several apps to several rooms at once | yes | yes |
| Where routing is chosen | Windows sound UI | TOML / CLI |
| Install, reboot, Secure Boot off | required | **none** |
| Blast radius of a bug | BSOD | a crashed service |

It reduces to *where you choose the routing*, plus who bears the install cost.
Someone who wants the room in the sound picker accepts a driver; someone who
wants audio in the kitchen this afternoon does not have to.

## Consequences

- The project is useful on any Windows machine with nothing installed, including
  every machine where test signing will never be acceptable — corporate,
  Secure Boot enforced, or simply cautious.
- The driverless mode is a genuine correctness backstop for the driver path.
  Both share the streaming core, so a bug reproduced without the driver loaded is
  immediately known not to be a kernel bug — which matters a great deal when the
  alternative diagnosis costs a bugcheck.
- `sonos-driver-gui` keeps a purpose. It is a room picker with start/stop, which
  is precisely a driverless front end, so it stays as an optional convenience
  rather than being retired. It is still not part of the driver product, which
  remains GUI-free by requirement.
- Two service binaries can now be installed. They must not both drive the same
  room; the party journal and the arbiter are per-process, so cross-process
  arbitration does **not** exist. This is documented rather than engineered
  around: running both hosts against one household is a misconfiguration, and
  the honest fix is to say so, not to build a cross-process lock for a case
  nobody should be in.
- The attestation-signing path is now the documented route to running the driver
  on a primary machine with Secure Boot on. Nothing in the code changes for it;
  it is a packaging and certificate exercise, deliberately deferred.

## Alternatives considered

**Replace the kernel driver with the driverless mode.** Rejected: it cannot put
a room in the sound picker, which is the requirement the driver exists to meet.

**Piggyback on an already-signed third-party virtual cable** (VB-CABLE, Virtual
Audio Cable) and just rename its endpoints. Tempting — `sonos-vad-endpoint`
already matches adapters generically, so it is nearly free — but the free
VB-CABLE provides a single cable, so a multi-room household needs a paid
product; redistribution terms are unclear; and it swaps our small, auditable
null sink for someone else's closed kernel driver. Kept in mind as a supported
configuration rather than a design.

**Wait for Microsoft to ship a user-mode audio endpoint API.** ACX shipping as
KMDF-only is a strong signal that this is not coming, and the requirement is
present tense.

**A cross-process arbiter so both hosts can share a household.** Rejected as
solving a self-inflicted problem; see Consequences.
