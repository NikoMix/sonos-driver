# 0017 — Kernel virtual-audio driver: a scoped, personal-use revisit of ADR-0014

- Status: **Superseded in part** by [ADR-0018](./0018-loopback-rename-and-service-hosting.md)
- Date: 2026-06-26

> **Corrections to this record.** ADRs are historical documents and the body
> below is left as it was written, but two statements in it are wrong and are
> corrected here:
>
> 1. **Licensing.** This ADR repeatedly calls Microsoft's driver samples
>    *MIT-licensed*. They are licensed under the **Microsoft Public License
>    (MS-PL)**. See [`driver/DERIVATION.md`](../../driver/DERIVATION.md) for the
>    obligations that actually apply.
> 2. **Base sample.** §1 names **Sysvad** as the base. The driver was in fact
>    derived from **`simpleaudiosample`**, which is the same PortCls/WaveRT
>    scaffolding without the hardware-codec fiction.
>
> §3 (the kernel→user PCM bridge and dynamic subdevice creation) is superseded by
> ADR-0018: neither was built, and neither is needed.

## Context

[ADR-0014](./0014-virtual-audio-driver-reality.md) recorded, with verified
evidence, why this project **ships as a user-mode bridge** and does **not**
implement a kernel-mode virtual audio render endpoint: a selectable Windows
output device requires a kernel miniport, a pure-Rust audio miniport does not
exist in today's ecosystem, and loading a driver on stock Windows needs a paid
EV certificate plus Microsoft Partner Center attestation — none of which is
achievable or verifiable here, and the paid certificate conflicts with the
project's no-paid-vendor-licence rule.

The owner has since asked, explicitly, for the experience ADR-0014 ruled out for
*distribution*: **each Sonos room enumerated as its own Windows output device**,
with the system audio routed to that device streamed to the matching room. When
asked to scope it (`ask_user`), the owner chose **personal use on their own
machine(s) only, with free test-signing accepted**.

That single scoping choice dissolves the specific blocker ADR-0014 leaned on. The
EV-certificate / Partner Center pipeline is only required to load a driver on
*other people's* stock machines. For the owner's *own* machines, Windows already
provides a free, first-party path — `bcdedit /set testsigning on` plus a
self-signed test certificate — that costs nothing and installs nothing from a
vendor. ADR-0014's reasoning is still correct **for distribution**; this ADR
revisits only the **personal-use** sub-case it left open in its own consequences
("a separate, out-of-repo research effort … remains theoretically possible").

This is therefore **not a contradiction of ADR-0014** but a narrowing of it: the
user-mode bridge remains the default, supported, zero-install product; the kernel
driver is an **optional, owner-built, test-signed add-on** for the owner who
wants OS-level per-room output devices and accepts the test-signing trade-offs.

## Decision

**Pursue the kernel virtual-audio driver as an optional, personal-use,
test-signed component, built and signed by the owner out-of-CI, while keeping the
pure-Rust user-mode bridge as the default shipped path.** Concretely:

### 1. Two-component architecture

- **Component A — KMDF virtual-audio driver (NEW, C/C++, owner-built).** Derived
  from Microsoft's **Sysvad** sample (MIT-licensed — redistributable, no
  purchase). It exposes one or more virtual *render* endpoints (each appears in
  *Sound → Output*), buffers the PCM that applications render to each endpoint,
  surfaces that PCM to user mode, and supports **dynamic add/remove** of
  endpoints — one per discovered Sonos room — driven by a user-mode control
  channel. This component is **out-of-tree for CI**: it requires the full WDK and
  cannot be compiled, signed, or installed in this environment.
- **Component B — Rust user-mode service (mostly EXISTS, ~70 % reuse).** SSDP
  discovery (`sonos-control`), FLAC encode (`audio-codec`), the live-edge HTTP
  stream server (`stream-server`, FLAC-default per ADR-0016), and UPnP transport
  control (`sonos-driver-core`). It reads PCM from each active virtual endpoint
  and streams it to the mapped room.

### 2. Polyglot boundary — the Rust crates keep `#![forbid(unsafe_code)]`

An audio miniport in Rust today is heavy `unsafe` FFI against PortCls/ACX with no
safe binding surface (ADR-0014 §2), so the driver is realistically **C/C++**. The
repository therefore becomes **polyglot**: the driver component cannot keep
`#![forbid(unsafe_code)]`, but **every Rust crate continues to forbid unsafe**
(the only pre-existing exception is the documented WASAPI FFI island in
`audio-capture`). The new control-plane crate added in this phase,
`sonos-vad-control`, is pure-Rust and forbids unsafe.

### 3. Kernel→user PCM bridge — prefer a paired loopback-capture pin

Two options were considered for surfacing each endpoint's rendered PCM to user
mode: (a) a **paired loopback-capture pin** per virtual render endpoint, or (b) a
custom IOCTL shared-ring. We **prefer (a)**: a loopback-capture pin lets the
*existing* `audio-capture` WASAPI loopback path read the endpoint with minimal new
glue, maximising reuse of Component B. Option (b) is the fallback if the pin
approach proves too constraining. The final choice is deferred to the
implementation phase, where it can be measured on real hardware.

### 4. Control plane — modelled now, in safe Rust (`sonos-vad-control`)

The user-mode service must converge the set of live render endpoints to the set
of currently discovered rooms, and fold the driver's asynchronous replies back
into its state. That reconcile logic is OS-independent and fully unit-testable
**without a driver**, so it is implemented now as `crates/sonos-vad-control`:

- `RoomKey` + `device_name_for` — stable room identity (keyed on the durable
  Sonos UUID) and deterministic, de-duplicated Windows device-name derivation
  (for example `Sonos Living Room`, `Sonos Office (2)`).
- `ControlCommand` / `DriverEvent` — the `serde`-serialisable message contract
  for the eventual IOCTL / named-pipe transport, so the wire format is not
  re-invented later.
- `EndpointRegistry` — a reconcile planner: `reconcile(&desired_rooms)` returns
  the create/destroy commands to send the driver; `apply(&event)` advances state
  on each reply (created → active, destroyed → dropped, create-failed → retry,
  destroy-failed → revert-and-retry). Idempotent until events arrive.

This crate is the verifiable engineering deliverable of Phase 0: it compiles,
clippy-pedantic-cleanly, and unit-tests on any platform, and it is the seam the
future driver plugs into.

### 5. Signing — free test-signing for personal use; production deferred

Personal use → `bcdedit /set testsigning on` + a self-signed test certificate
(`makecert`/`MakeCert`/PowerShell `New-SelfSignedCertificate` + `signtool`).
Free, installs nothing from a vendor, satisfies the no-paid-licence rule. The
trade-offs (a desktop watermark and a weaker machine driver-security posture) are
owner-accepted and documented in `docs/DRIVER-SETUP.md`. Production signing (EV
certificate + Partner Center attestation, per ADR-0014 §3) remains **deferred and
optional** — required only if the owner later wants to install on machines other
than their own.

### 6. Latency floor is unchanged

Per ADR-0003 and ADR-0014 §4, the dominant latency term is the Sonos player's
fixed ~300–800 ms network buffer. The driver changes *how* audio is captured
(OS-level per-room device instead of app/system loopback), not *how fast Sonos
plays it*. It adds a small term (kernel render → loopback → capture). This ADR
makes **no** low-latency claim; the ADR-0003 floor stands.

### Honest UX nuance (designed for, not glossed)

A virtual output device only carries audio that applications **route** to it:

- *One app's audio to a room* — pick that room's device in Windows' per-app
  output settings (clean, natural).
- *All system audio to one room* — set that room's device as the Windows default
  output.
- *The same audio to several rooms at once* — **not free**; needs multiple apps
  targeting different devices, or in-driver / user-mode fan-out duplication.
  Native synchronised multi-room is a Sonos-group feature better mapped by
  grouping rooms over UPnP than by duplicating audio. This is flagged as a design
  item, not a silent gap.

## Consequences

- ✅ The owner gets the requested experience — **each Sonos room as a selectable
  Windows output device** — without any paid certificate or vendor licence, using
  free first-party test-signing.
- ✅ The reconcile control plane is **landed and verified now** (pure-Rust
  `sonos-vad-control`, all workspace gates green), de-risking the largest
  *software* unknown before any kernel work begins.
- ✅ The default product is **unchanged**: stock Windows, no driver, no signing,
  no admin. The driver is strictly opt-in.
- ✅ Every Rust crate keeps `#![forbid(unsafe_code)]`; the `unsafe` boundary is
  confined to the out-of-tree C/C++ driver (plus the pre-existing WASAPI island).
- ⚠️ The repository becomes **polyglot** (C/C++ WDK + Rust) and gains a non-cargo,
  WDK/msbuild build step that **cannot run in the billing-blocked CI** — the
  driver build is a documented local, owner-side step.
- ⚠️ Test-signing puts the owner's machine in a watermarked state and weakens its
  driver-security posture (Secure Boot interaction documented). Owner-accepted for
  personal use.
- ⚠️ Dynamic endpoint add/remove (one device per room) is advanced and sparsely
  documented publicly — it carries the most technical risk and is sequenced last
  among the build phases.
- ⚠️ Distribution to other machines is still gated on the paid EV/Partner Center
  pipeline of ADR-0014 §3 and remains explicitly out of scope here.

Supersession note: this ADR **revisits and narrows** ADR-0014 for the personal-use
case; it does **not** supersede it. ADR-0014's distribution conclusion stands. See
ADR-0001 (user-mode choice), ADR-0003 (latency floor), ADR-0011 (process
loopback), ADR-0012 (GUI), ADR-0014 (driver reality), ADR-0016 (FLAC default), and
`docs/DRIVER-SETUP.md` (owner setup runbook).
