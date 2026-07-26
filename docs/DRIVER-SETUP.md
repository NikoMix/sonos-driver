# Driver setup — installing sonosvad on your own machine

> **Scope.** This runbook installs the `sonosvad` kernel virtual-audio driver, so
> that **each Sonos room appears as its own Windows audio output** — plus an
> **All Sonos** output that plays to the whole household in sync. It is
> owner-built and test-signed, for personal use.
>
> **You may not need it.** The [driverless mode](adr/0019-driverless-routing-mode.md)
> streams to any room, groups rooms, and routes individual applications, with
> nothing installed, no reboot and Secure Boot left on:
>
> ```powershell
> sonos-driver cast --all
> ```
>
> The one thing it cannot do is put rooms in the Windows sound picker, because
> Windows has no user-mode way to create an audio endpoint. If that is what you
> want, read on.
>
> **None of this runs in CI** — it needs the full WDK, an elevated shell and a
> reboot.

## What you get, and what it costs

- ✅ A *Sound → Output* entry per Sonos room, named after the room, plus **All
  Sonos**. Route any app — or all of Windows — by picking a device.
- ⚠️ **Test signing** must be on: a desktop watermark, a weaker driver-security
  posture, and anti-cheat or DRM software may refuse to run. Reversible.
- ⚠️ **Secure Boot must be off.** With it on, `bcdedit /set testsigning on` is
  accepted and then ignored at boot.
- ➡️ Neither is inherent to the driver — both are consequences of *signing*, not
  of certification, and both go away with attestation signing. See
  [Getting rid of test signing](#getting-rid-of-test-signing).

## Prerequisites

| Requirement | Notes |
| --- | --- |
| Windows 10/11 **x64** | Your own machine — this cannot be done in a VM without a BCD store. |
| **Administrator** PowerShell | Every step is elevated. |
| **Windows Driver Kit** + matching SDK | Install the SDK *first*, then the WDK of the same version. |
| **Visual Studio Build Tools** (MSVC) | The C++ toolchain; the same one the Rust build uses. |
| **Rust** (stable) | For the user-mode service. |
| A reboot | Test signing needs one to take effect. |

The driver source is vendored under `driver/sonosvad/Source/` and builds as it
stands. [`../driver/DERIVATION.md`](../driver/DERIVATION.md) records what was
changed in Microsoft's MS-PL `simpleaudiosample` to produce it.

## The short version

From an **elevated** PowerShell at the repo root:

```powershell
./driver/scripts/Enable-TestSigning.ps1     # then REBOOT
./driver/scripts/New-TestCert.ps1
./driver/scripts/Build-Driver.ps1
./driver/scripts/Sign-Driver.ps1
./driver/scripts/Test-Driver.ps1            # optional: 6 static checks

cargo build --release -p sonos-vad-service
./driver/scripts/Install-Driver.ps1
Start-Service sonosvad
```

Every script defaults to the right paths; the arguments below are only for when
you want something other than the default. Each prints what it is about to do and
stops on the first error.

---

## 1. Enable test signing

```powershell
./driver/scripts/Enable-TestSigning.ps1
```

Runs `bcdedit /set testsigning on`. **Reboot.** You should then see a "Test Mode"
watermark in the bottom-right of the desktop.

No watermark after the reboot means Secure Boot is still on — turn it off in your
firmware setup and re-run.

## 2. Create a test certificate

```powershell
./driver/scripts/New-TestCert.ps1
```

Creates a self-signed code-signing certificate (`CN=SonosVAD Test`) in
`LocalMachine\My`, exports the public part to the git-ignored `driver/_local/`,
and installs it into **Trusted Root** and **Trusted Publishers** so this machine
will accept a driver signed with it.

This certificate is for your machine only. Never distribute it.

## 3. Build

```powershell
./driver/scripts/Build-Driver.ps1
```

Produces `sonosvad.sys`, `sonosvad.inf` and `sonosvad.cat` in
`driver/sonosvad/artifacts/x64/Release/`. Expect **0 errors and 0 warnings** —
the project turns on `/analyze`, the driver-recommended ruleset, Spectre
mitigations and warnings-as-errors, so any new warning fails the build.

> **Build output must be on a local volume.** `stampinf` rewrites the INF through
> a memory-mapped view; on a network or VM-shared folder that write is silently
> discarded while `stampinf` still reports success, leaving `$ARCH$` unresolved
> and InfVerif failing with `error 1274`. `-BuildRoot` defaults to `%TEMP%`,
> which is normally fine.

## 4. Sign

```powershell
./driver/scripts/Sign-Driver.ps1
```

Signs **both** `sonosvad.sys` and `sonosvad.cat`, replacing the throwaway
`WDKTestCert` signature the build applies.

Signing only the binary is the classic mistake: `pnputil` validates the
**catalog** against Trusted Publishers, so a package with a signed `.sys` and an
untrusted `.cat` is rejected at install with a bare "third-party INF does not
contain digital signature information". Re-signing the `.sys` does not invalidate
the catalog, because catalog entries store the Authenticode hash, which excludes
the certificate table.

Optionally verify the whole package before touching the machine's device tree:

```powershell
./driver/scripts/Test-Driver.ps1
```

Six static checks: artifacts present, InfVerif's universal-driver rules, the
endpoint count agreeing between `definitions.h` and the INF, the PE image being a
64-bit kernel driver with the expected mitigations, ApiValidator's universal-API
surface, and the signature/catalog pair. It should report
`sonosvad.sys : Valid / signer 'CN=SonosVAD Test'` — `UnknownError` there means
step 2 did not put the certificate in Trusted Root.

## 5. Install

Build the service first, and put it where it will stay: the registered service
command line points at that exact path.

```powershell
cargo build --release -p sonos-vad-service
./driver/scripts/Install-Driver.ps1
Start-Service sonosvad
```

If SSDP multicast does not work on your network — NAT'd VMs, VLAN-segmented or
guest wifi, access points without working IGMP — give it any one player's address
and it will find the rest from there:

```powershell
./driver/scripts/Install-Driver.ps1 -SeedHost 192.168.1.50
```

Two things happen, and doing only the first is the classic mistake:

1. `pnputil /add-driver ... /install` stages the package into the driver store.
   On its own this changes nothing visible, because `sonosvad` is
   **root-enumerated** (`ROOT\sonosvad`) — there is no physical hardware for PnP
   to match it against.
2. `devgen /add /bus ROOT` creates a device node for that hardware ID. PnP then
   matches the staged package and loads the driver.

The script then registers the streaming service — named **`sonosvad`**; the
binary is `sonosvad-svc.exe` — as auto-start under LocalSystem. It is not started
for you, so that you can inspect the endpoints before anything renames them.

Then open **Settings → System → Sound**. Your rooms should be listed, plus
**All Sonos**. Pick one and play something.

Useful flags: `-SkipService` stages the driver without registering the streaming
service (for driving the pipeline by hand with `sonos-driver vad`);
`-SkipDeviceNode` updates the binary when the node already exists.

## 6. Verify

```powershell
./driver/scripts/Test-DriverOnHost.ps1
```

The load-dependent suite that cannot run anywhere but a real machine. Eight
phases — `preflight`, `verifier`, `install`, `endpoints`, `rename`, `probe`,
`soak`, `teardown` — each independently runnable via `-Phase` so you can resume
across the reboots that Driver Verifier needs.

The default is the safe subset: `preflight, install, endpoints, rename, probe`.
Nothing there reboots or destroys state. Add the rest deliberately:

```powershell
# after a reboot, with Driver Verifier armed against sonosvad
./driver/scripts/Test-DriverOnHost.ps1 -Phase verifier
./driver/scripts/Test-DriverOnHost.ps1 -Phase install,endpoints,rename,probe,soak -SeedHost 192.168.1.50
./driver/scripts/Test-DriverOnHost.ps1 -Phase teardown
```

> `-Phase verifier` arms Driver Verifier against `sonosvad`. That is what it is
> for: a driver bug then bugchecks the machine on purpose, rather than corrupting
> something quietly. Do not run it on a machine you need in the next hour.

## Rolling back

```powershell
./driver/scripts/Uninstall-Driver.ps1
./driver/scripts/Disable-TestSigning.ps1    # then REBOOT
```

`Uninstall-Driver.ps1` runs `sonos-driver vad ungroup` first, so speakers are not
left grouped, then restores the endpoint names, removes the service, deletes the
device node and removes the package from the driver store.

Re-enable Secure Boot in firmware if you turned it off.

## Getting rid of test signing

Test signing is a development posture, not the destination. The usual assumption
is that going further means WHQL certification — it does not.

| | Test signing | Attestation signing |
| --- | --- | --- |
| Cost | free | EV code-signing certificate, ~€200–500/yr; no Microsoft fee |
| HLK test passes | none | **none** |
| Secure Boot | must be **off** | stays **on** |
| Watermark | yes | no |
| Anti-cheat / DRM | often breaks | fine |
| Changes to this code | — | **none — packaging only** |

Attestation signing is an automated Partner Center submission: upload a signed
CAB, get back a Microsoft-signed driver that loads on any machine with Secure
Boot enabled. The only real barrier is the EV certificate, which requires an
organisation identity check. Tracked in
[issue #50](https://github.com/NikoMix/sonos-driver/issues/50).

Validate under test signing first — there is no sense buying a certificate for a
driver that has never loaded.

## Troubleshooting

| Symptom | Likely cause / fix |
| --- | --- |
| No watermark after reboot | Secure Boot is on. Disable it in firmware, re-run step 1. |
| `pnputil`: "third-party INF does not contain digital signature information" | The **catalog** is unsigned or signed by an untrusted cert. Re-run step 4, and check step 2 put the cert in Trusted Root *and* Trusted Publishers. |
| `Sign-Driver.ps1`: "No certificates were found" | The cert subject does not match. Pass the same `-CertSubject` you gave `New-TestCert.ps1 -Subject`. |
| `Build-Driver.ps1`: InfVerif `error 1274` | Build output is on a shared/mapped folder. Pass `-BuildRoot` a path on a local disk. |
| `Install-Driver.ps1`: "INF not found" | The build did not run, or ran for a different configuration. Pass `-Configuration` / `-Platform` to match. |
| Driver fails to load, Code 52 | Test signing is not active, or the signature chain is wrong. Re-check steps 1–4. |
| Endpoints appear but are named "Sonos Output 1..9" | The service has not bound them yet — check it is running, and that discovery found the household (`sonos-driver list`, then `sonos-driver vad status`). |
| Endpoints appear but are silent | Nothing is routed to them; pick one as the output for an app or as the default device. |
| Discovery finds nothing | SSDP multicast is blocked. Reinstall with `-SeedHost <any player's IP>`. |
