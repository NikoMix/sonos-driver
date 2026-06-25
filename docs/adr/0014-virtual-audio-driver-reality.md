# 0014 — Virtual audio "driver" reality: why this ships as a user-mode bridge

- Status: Accepted (distribution conclusion stands; personal-use sub-case revisited and narrowed by [ADR-0017](./0017-kernel-virtual-audio-driver.md))
- Date: 2026-06-25

## Context

The original request was a *"Rust Windows **driver** that streams native Windows audio
output to Sonos with ultra-low latency"* — i.e. a **virtual audio device**: a "Sonos"
entry in the Windows *Sound → Output* list that any application could select, with audio
routed transparently to the speakers. ADR-0001 already chose user-mode WASAPI loopback for
v1 and deferred the kernel question. Phase 4 ("complete the implementation") is the point
to record, with verified evidence, **why the kernel virtual-audio-device is not a
deliverable of this project** and what we ship instead. This keeps expectations honest and
stops a future contributor from re-opening a multi-month, unverifiable rabbit hole.

## Decision

**This project ships as a pure-Rust, user-mode bridge. It does not, and will not in this
repository, implement a kernel-mode virtual audio render endpoint.** The reasoning is
technical and verifiable, not a matter of effort budget:

### 1. A selectable Windows output endpoint *requires* a kernel-mode audio driver

The Windows audio engine only enumerates render endpoints that are backed by an audio
**miniport driver** loaded in kernel space — historically a PortCls **WaveRT** miniport,
or the modern **ACX** (Audio Class eXtension) / AVStream model. There is **no user-mode
API** to register a virtual render endpoint that other applications can select. So the
"Sonos appears as an output device" experience is, by construction, kernel-driver
territory (Microsoft Learn: *Introduction to WaveRT Miniport Drivers*; *ACX Audio Class
Extensions overview*).

### 2. "Pure-Rust Windows audio driver" is not achievable with today's ecosystem

Microsoft's Rust-for-drivers project, [`windows-drivers-rs`](https://github.com/microsoft/windows-drivers-rs)
(`wdk` crates, 0.x), is explicitly **pre-production**, currently targets **KMDF 1.33
only**, and ships **zero audio-driver examples**. Audio miniports (PortCls/ACX) are C/C++
WDK components; there is no Rust binding surface or sample for them. A genuinely *pure-Rust*
audio miniport therefore does not exist and cannot be assembled from the current crates.
Any attempt would be a large C/C++ WDK driver with, at best, a thin Rust shim — which
defeats the project's pure-Rust, memory-safety premise.

### 3. Driver signing is a paid, account-gated, unverifiable process

Even a working driver cannot load on a normal Windows machine without signing:

- **Development:** test-signing only — `bcdedit /set testsigning on`, which is per-machine,
  puts the OS in a watermarked test state, and is unsuitable for end users.
- **Production:** an **EV code-signing certificate** (paid, identity-vetted, issued on
  hardware tokens by a commercial CA) plus submission through **Microsoft Partner Center**
  for **attestation signing** (or full WHQL/HLK testing) to obtain a Microsoft signature
  that loads on stock Windows 10/11 (Microsoft Learn: *Driver Signing*; *Attestation
  signing a kernel driver for public release*).

That pipeline needs a paid CA certificate and a Partner Center account, and its output can
only be validated on real machines. It is **impossible to perform or verify in this
environment**, and the paid-certificate requirement conflicts with the project's
"permissive, no paid vendor licence" constraint.

### 4. A kernel driver would not move the latency floor anyway

The dominant latency term is the Sonos player's **fixed, non-configurable ~300–800 ms
network buffer** (ADR-0003), not the Windows capture path (WASAPI loopback adds only
~10–20 ms). A virtual audio driver would change *how* audio is captured, not *how fast
Sonos plays it*, so it cannot deliver the "ultra-low latency" the name implies. The honest
floor documented in ADR-0003 stands regardless of capture mechanism.

### What we ship instead (the user-mode substitute)

- **System-mix capture** via WASAPI loopback — no driver, no install, no admin
  (ADR-0001, ADR-0006).
- **Per-application capture** via Windows *process loopback*
  (`ActivateAudioInterfaceAsync` with `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`),
  added in Phase 4 (ADR-0011 / WS-K). This targets a single app's audio — the practical
  equivalent of "route this program to Sonos" — again entirely in user mode (requires
  Windows 11 / Server 2022 build 20348+, with a runtime OS-version gate).
- **A tray/GUI control surface** (ADR-0012 / WS-J) for selecting room, profile, and
  capture source.

## Consequences

- ✅ Runs on stock Windows with **no kernel driver, no signing, no test-signing boot
  flag, and no administrator install** — dramatically smaller attack surface and zero
  supply-chain/cert burden.
- ✅ Stays **pure-Rust and memory-safe** (the only `unsafe` is the documented WASAPI FFI
  island in `audio-capture`); no C/C++ WDK component is introduced.
- ✅ Expectations stay honest: the latency floor is a Sonos property (ADR-0003), so we do
  not pretend a driver would beat it.
- ⚠️ There is **no** "Sonos" entry in the Windows output-device list. Users select what to
  capture in the app (whole-system or a specific process) rather than at the OS mixer.
- ⚠️ A signed kernel virtual-audio-device remains *theoretically* possible as a separate,
  out-of-repo research effort, but only with a paid EV certificate, a Partner Center
  account, real-hardware HLK/attestation testing, and a C/C++ (not pure-Rust) miniport —
  explicitly **out of scope** here. See ADR-0001 (user-mode choice), ADR-0003 (latency
  floor), ADR-0011 (process loopback), ADR-0012 (GUI).
