# 0018 — Loopback capture, endpoint renaming, and a single service host

- Status: **Accepted**
- Date: 2026-07-26
- Supersedes: [ADR-0017](./0017-kernel-virtual-audio-driver.md) §3

## Context

[ADR-0017](./0017-kernel-virtual-audio-driver.md) established *that* a kernel
virtual audio device would be built, and sketched *how*: a paired
loopback-capture pin in the driver (with an IOCTL shared-ring fallback) to get
PCM from kernel to user mode, and `PcRegisterSubdevice` to create one render
endpoint per discovered room at run time.

Building it revealed that both mechanisms are unnecessary, and that the riskiest
kernel work ADR-0017 flagged does not have to exist at all. This record captures
the design that was actually implemented and validated, and the reasons the
alternatives were dropped.

The user-facing requirement is unchanged and is the yardstick for everything
below:

> Each Sonos room appears as its own selectable Windows audio output. Audio
> routed to it plays in that room. Plus one aggregate output, **All Sonos**,
> that plays to the whole household at once. **No GUI** — integration with the
> native Windows sound UI only.

## Decision

### 1. WASAPI loopback replaces the kernel→user PCM bridge

Loopback capture is implemented by the Windows *audio engine*, in user mode, and
works against **any** shared-mode render endpoint — including one backed by a
third-party driver. `IAudioClient::Initialize(AUDCLNT_STREAMFLAGS_LOOPBACK)` on
the endpoint's own `IMMDevice` yields exactly the mixed stream Windows routed
there.

So the driver does not have to transport audio anywhere. It is a **null sink**:
it accepts the render stream and advances the WaveRT position at exactly real
time. Everything above it — capture, resample, encode, serve, control — is the
existing user-mode pipeline, unchanged.

This deletes the paired capture pin, the IOCTL ring buffer, the kernel-side
locking that a shared ring implies, and the entire class of bugs that comes with
DMA-like buffer handoff across the kernel boundary. It is the single largest
risk reduction in the project.

The correctness of the null sink is therefore the *whole* kernel contract, and
it is directly testable without any Sonos hardware: `vad probe` loopback-captures
one endpoint and asserts that N seconds of wall clock yield N seconds of frames.
A virtual device that lies about its clock is the one way this driver can
silently corrupt audio, so that check is the driver's primary test.

### 2. Fixed endpoints + renaming replaces dynamic subdevice creation

The driver declares a **fixed** pool of render endpoints in its INF. The
user-mode service **renames** them to the discovered room names by opening each
endpoint's property store `STGM_READWRITE` and writing
`PKEY_Device_FriendlyName` — precisely what the Sound control panel does when a
user renames a device. It needs administrator rights and nothing else.

`PcRegisterSubdevice` at run time would mean managing PnP lifetime for devices
that appear and vanish as speakers come and go on the LAN, in kernel mode, in a
driver that cannot be debugged on the machine it runs on. Renaming moves that
entire problem into user mode, where a mistake is a wrong label rather than a
bugcheck.

The cost is a fixed ceiling on rooms. The pool is **nine** endpoints: eight
rooms plus one reserved for **All Sonos**. Raising it is a rebuild, not a
redesign, and the ceiling is visible — `vad bind` warns when rooms could not be
placed and names the constant to change.

Endpoints no room was bound to are **disabled** (`DeviceState = 2` under
`HKLM\…\MMDevices\Audio\Render\{guid}`) so the sound picker shows the
household's rooms and not a run of "Sonos Output 7" placeholders. Registry
rather than `IPolicyConfig`: the latter is undocumented and its vtable layout
changes between Windows releases, which is not a foundation for something meant
to survive updates. A *deliberately disabled* endpoint is tracked separately
from a merely inactive one, because forcing an unplugged or not-present endpoint
to ACTIVE would conjure a phantom entry in the picker.

### 3. One LocalSystem service hosts every room

A single Windows service (`sonosvad-svc`) runs one bridge per bound endpoint.
Not one service per room: the endpoint pool, the room-to-endpoint binding and
the arbitration map are all household-wide state, and splitting them across
processes would mean inventing an IPC protocol to keep them consistent.

LocalSystem because renaming endpoints and reading the driver's device
interfaces both need administrative rights, and because the service must run
with no user logged in. There is no UI, no tray icon and no window: the only
surface is the Windows sound picker, which is what was asked for.

### 4. Seed-host discovery alongside SSDP

SSDP is multicast, and multicast does not survive every network: NAT'd VMs,
VLAN-segmented or guest wifi, and access points without working IGMP all
silently swallow it. But **any single Sonos player will report the entire
household over unicast HTTP** (`/status/topology` and the ZoneGroupTopology
service).

So discovery takes an optional seed host. With it, one HTTP GET to a known
player is enough to enumerate every room. This is not a workaround for a test
environment — it is the difference between working and not working on a
segmented home network, and it is how the household used to develop this was
enumerated in the first place.

### 5. Idle release: the driver must not hold speakers hostage

An endpoint with nothing playing still produces frames — digital silence. Left
alone, every bound room would sit on this driver's stream forever and could not
be used from the Sonos app.

So a session that has been silent for a configurable period **stops the player
and releases the room**, and reclaims it when audio returns.

"Audio is back" has to be an **event, not a time window**. An audible-block
counter is snapshotted at release and compared afterwards; comparing timestamps
does not work, because at 20 Hz polling the release and the audible block that
should have cancelled it land in the same millisecond. The reconnect logic
explicitly stands down while released — otherwise it would treat the deliberate
client disconnect as a fault and immediately grab the speakers back, and idle
release would never hold.

### 6. All Sonos: grouping, not parallel streams

The obvious implementation of an aggregate output — point every room at the same
URL — does not work. Each player fetches, buffers and starts independently, so
rooms land tens to hundreds of milliseconds apart and *stay* there. In an open
plan flat that is an echo.

Sonos has one synchronisation mechanism and it is **grouping**: a member's
transport URI is set to `x-rincon:<coordinator uuid>` and it leaves via the
vendor AVTransport action `BecomeCoordinatorOfStandaloneGroup`. That is what the
app's Group button does, and the aggregate output drives exactly that, then
plays the bridge's stream on the coordinator alone.

Three consequences worth recording:

- **Whole zone groups are the unit.** The bridge's rooms are already zone-group
  coordinators, so absorbing one brings its bonded satellites and any grouping
  the user already made along for free. Undoing the party is then
  `leave_group()` per absorbed coordinator, and its own group reforms exactly as
  it was. This is why there is **no topology snapshot**: moving whole groups is
  inherently reversible, and a snapshot could only go stale.
- **The coordinator is the first room in binding order** — the same order shown
  in the sound picker — so the choice is stable across restarts instead of
  shuffling with discovery timing.
- **The aggregate implements the same `PlayerControl` trait as a single room**,
  so the entire session machinery (stream server, latency accounting, idle
  release, reconnect) drives it unchanged. The aggregate is just another
  endpoint whose "player" happens to be the whole household.

### 7. Arbitration: whoever started playing most recently wins

Windows lets several applications render to different endpoints simultaneously,
so All Sonos and a room's own output can both carry audio — but a speaker can
obey only one of them.

The rule is **most recent start wins**, chosen because starting audio is a
deliberate act: the newest one is what the user is currently listening for.
Implementation-wise this is exactly last-write-wins on a room→session map, so no
sequence counter is needed; an early one was written, found never to be read,
and removed.

"Started" means the **silent → audible transition**, not "is producing samples".
With the latter, two continuously playing sessions would pre-empt each other
several times a second forever. A session pre-empted mid-stream therefore stays
pre-empted until it falls silent and starts again.

A room nobody holds is free for the taking, so when the winner goes quiet its
rooms fall back to whatever is still playing instead of going silent until the
user intervenes.

### 8. The party is journalled, because grouping lives in the speakers

Every other piece of state this driver touches is on the Windows machine and
dies with it. Grouping does not: it lives in the speakers. A service killed
mid-party — or a bugcheck — leaves the household grouped, with nothing in
Windows that remembers doing it and no UI to undo it from.

So the absorbed coordinators are written to `%ProgramData%\sonos-driver\` as
they join and the file is deleted when the party disbands. `vad ungroup` replays
it, the service replays it on startup, and `Uninstall-Driver.ps1` runs it before
removing the driver. Only rooms this driver grouped are ever split, so a group
the user made themselves survives.

The journal is advisory: failing to record a party is never a reason to refuse
to play one.

## Consequences

**Good**

- The kernel component is small, static and does one thing, which is the only
  kind of kernel component worth trusting on a machine you care about.
- Every hard problem — discovery, binding, encoding, grouping, arbitration,
  recovery — is in user mode, in Rust, under test.
- No GUI to maintain, and no GUI to get in the way: the Windows sound picker is
  the entire interface.

**Bad**

- A fixed endpoint ceiling (8 rooms + All Sonos). Raising it is a driver
  rebuild.
- Renaming endpoints and hiding spares both mutate machine-wide state that
  outlives the process. Both are undone by `vad release`, and the uninstaller
  runs it.
- Loopback capture follows the *shared* mix, so an exclusive-mode application on
  a bound endpoint would bypass the bridge. Acceptable: nothing sane opens a
  virtual Sonos output in exclusive mode.

**Unverified**

The sandbox this was developed in has no BCD store, so test signing cannot be
enabled and the driver **cannot be loaded there**. Everything load-dependent —
installation, endpoint enumeration, Driver Verifier, audio round-trip, latency,
and every grouping call against real hardware — is deferred to
`driver/scripts/Test-DriverOnHost.ps1` on the owner's machine. Build, static
analysis (InfVerif, ApiValidator, PE checks), signing and the whole user-mode
test suite do run in the sandbox and are green.

## Alternatives considered

- **Paired loopback pin / IOCTL ring (ADR-0017 §3).** Superseded: WASAPI
  loopback already provides the stream, in user mode, for free.
- **Dynamic `PcRegisterSubdevice` per room.** Superseded by renaming: same
  result, none of the PnP lifetime risk, and a mistake costs a wrong label
  rather than a bugcheck.
- **`IPolicyConfig` to hide spare endpoints.** Rejected: undocumented, and its
  vtable changes between Windows releases.
- **Parallel independent streams for All Sonos.** Rejected: audibly drifts.
  Grouping is the only synchronised option Sonos offers.
- **Snapshotting the topology before forming the party.** Rejected: absorbing
  whole zone groups is already reversible, and a snapshot could only go stale.
- **One service per room.** Rejected: the endpoint pool and the arbitration map
  are household-wide, and splitting them would require inventing IPC to keep
  them consistent.
