# `driver/` — optional kernel virtual-audio driver (owner-built, NOT CI-built)

This folder holds the **scaffold** for the optional Windows kernel virtual-audio
driver described in [ADR-0017](../docs/adr/0017-kernel-virtual-audio-driver.md).
Its purpose is to make **each Sonos room appear as its own Windows output
device**, so any application — or the whole system — can be routed to a room from
the OS mixer.

## Read this first

- **This is not part of the shipped product.** The default `sonos-driver` (CLI +
  GUI user-mode bridge) needs none of it — no driver, no signing, no admin. The
  driver is an **optional, personal-use, test-signed add-on** (ADR-0017).
- **Nothing here is compiled, signed, or installed by CI.** A Windows kernel
  driver requires the full **WDK/EWDK**, an elevated shell, and a reboot for
  test-signing. The repository's pipeline cannot do any of that, so the driver
  build is a **documented local, owner-side step**.
- **This folder intentionally does not contain a ready-to-compile `.sys` or a
  hand-written full miniport.** Shipping a half-written kernel driver would be
  dishonest and a licensing hazard. Instead it provides:
  - [`DERIVATION.md`](./DERIVATION.md) — the exact, reviewable set of changes to
    apply to Microsoft's **Sysvad** sample (MIT-licensed) to produce this driver.
  - [`sonosvad.inf`](./sonosvad.inf) — the installation manifest (device class,
    install sections, endpoint topology hooks).
  - [`scripts/`](./scripts) — PowerShell helpers for the whole lifecycle
    (test-signing on/off, test cert, build, sign, install, uninstall).

## Provenance & licensing

The driver is **derived from Microsoft's Sysvad sample**
(`Windows-driver-samples`, **MIT-licensed** — redistributable, no purchase). The
MIT terms require keeping Microsoft's copyright notice with any derived source;
record it at the top of every Sysvad-derived file you create under this folder.
No paid component is introduced: the WDK is a free Microsoft SDK, and personal-use
**test-signing is free**. See [ADR-0017](../docs/adr/0017-kernel-virtual-audio-driver.md)
§5 and ADR-0014 §3 for the signing rationale and the distribution boundary.

## How it fits together

```text
Windows audio engine (kernel)
  app / default output ──▶ virtual render endpoint "Sonos Living Room"  ┐
                          virtual render endpoint "Sonos Kitchen"       │  Component A
                                   │  paired loopback-capture pin       │  (this folder,
                                   ▼                                    │   C/C++, owner-built)
                          per-endpoint PCM ──▶ user mode  ◀── IOCTL/pipe ┘
                                   │                         (add/remove endpoints)
                                   ▼
  Rust user-mode service:  read PCM ▶ FLAC ▶ live-edge HTTP ▶ UPnP play ▶ Sonos room
        (Component B — crates/, mostly EXISTS; control plane = crates/sonos-vad-control)
```

- **Component A** (this folder) is the kernel driver: dynamic per-room render
  endpoints + a kernel→user PCM bridge.
- **Component B** is the existing Rust pipeline. The OS-independent control plane
  that drives endpoint create/destroy lives in
  [`crates/sonos-vad-control`](../crates/sonos-vad-control) and is fully
  unit-tested today, with **no** kernel dependency.

## Build & install

Follow [`docs/DRIVER-SETUP.md`](../docs/DRIVER-SETUP.md) end-to-end. In short:

1. `scripts/Enable-TestSigning.ps1` (reboot).
2. `scripts/New-TestCert.ps1`.
3. `scripts/Build-Driver.ps1` (needs the WDK; builds the Sysvad-derived sources).
4. `scripts/Sign-Driver.ps1` then `scripts/Install-Driver.ps1`.
5. To revert: `scripts/Uninstall-Driver.ps1` then `scripts/Disable-TestSigning.ps1`.

## Status / roadmap

Tracked under GitHub epic **#44**:

- **#45 — Phase 0 (this PR):** ADR-0017, this scaffold, the setup runbook, and the
  pure-Rust `sonos-vad-control` control plane. ✅ verifiable here.
- **#46 — Phase 1:** kernel→user PCM bridge (owner-side WDK).
- **#47 — Phase 2:** end-to-end one device → one room.
- **#48 — Phase 3:** dynamic per-room endpoints driven by SSDP discovery.
- **#49 — Phase 4:** GUI integration, packaging, test-sign automation.
- **#50 — Phase 5 (optional):** production signing (EV cert + Partner Center).

`_local/` (build/sign scratch output) is git-ignored.
