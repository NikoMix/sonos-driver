# Driver setup — optional kernel virtual-audio driver (personal, test-signed)

> **Scope.** This runbook is for the **optional** kernel virtual-audio driver
> described in [ADR-0017](adr/0017-kernel-virtual-audio-driver.md): it makes
> **each Sonos room appear as its own Windows output device**. It is **owner-built
> and personal-use only**, using free test-signing. The default `sonos-driver`
> product (the user-mode bridge — CLI + GUI) needs **none** of this: no driver, no
> signing, no administrator install. Only follow this guide if you specifically
> want OS-level per-room output devices and accept the test-signing trade-offs.
>
> **None of the steps below run in CI** (they need the full WDK, an elevated
> shell, and a reboot). The kernel driver is therefore not compiled, signed, or
> installed by this repository's pipeline — it is a documented local step.

## What you get, and the trade-offs

- ✅ A *Sound → Output* entry per discovered Sonos room (e.g. `Sonos Living
  Room`). Route any app — or all of Windows — to a room by picking its device.
- ⚠️ **Test-signing** puts Windows in a watermarked state and weakens the
  machine's driver-security posture for the duration. Reversible (see
  [Disable test-signing](#5-disable-test-signing-revert)).
- ⚠️ **Secure Boot** must usually be **off** to boot with test-signing enabled;
  on a machine with Secure Boot on, `bcdedit /set testsigning on` is silently
  ignored until you disable Secure Boot in firmware.
- ⚠️ Distribution to *other* machines is **out of scope** — that needs a paid EV
  certificate + Microsoft Partner Center attestation (ADR-0014 §3).

## Prerequisites

| Requirement | Notes |
| --- | --- |
| Windows 10/11 **x64** | The owner's own machine. |
| **Administrator** PowerShell | Every step below is elevated. |
| **Windows Driver Kit (WDK)** + matching SDK, or the **EWDK** | C/C++ KMDF/ACX toolchain. Not bundled; install from Microsoft. |
| **Visual Studio Build Tools** (MSVC) | Same toolset the Rust build already uses. |
| **Microsoft `simpleaudiosample`** sample source | **MS-PL** base, already vendored under `driver/sonosvad/Source/` — see [`../driver/DERIVATION.md`](../driver/DERIVATION.md). |
| A free afternoon and a **reboot** | Test-signing requires a restart to take effect. |

The helper scripts referenced below live in [`../driver/scripts/`](../driver/scripts/).
Run them from an elevated PowerShell. Each script prints what it is about to do
and stops on the first error.

## 1. Enable test-signing

```powershell
# Elevated PowerShell
./driver/scripts/Enable-TestSigning.ps1
# -> runs: bcdedit /set testsigning on
# Reboot when prompted. After reboot you will see a "Test Mode" desktop watermark.
```

If the setting does not "stick" after reboot, **Secure Boot is on** — disable it
in your firmware (UEFI) setup and retry. (`Enable-TestSigning.ps1` warns about
this.)

## 2. Create a test code-signing certificate

```powershell
./driver/scripts/New-TestCert.ps1 -Subject "CN=SonosVAD Test" -OutDir ./driver/_local
# Creates a self-signed cert in the local cert store, exports SonosVADTest.cer,
# and installs it into Local Machine \ Trusted Root and \ Trusted Publishers so
# the test-signed driver is trusted on THIS machine only.
```

This certificate is for your machine only; never distribute it. The `_local`
output folder is git-ignored.

## 3. Build the driver

```powershell
./driver/scripts/Build-Driver.ps1 -Configuration Release -Platform x64
# Wraps msbuild against the vendored driver project (see driver/DERIVATION.md).
# Requires the WDK/EWDK on PATH. Produces sonosvad.sys + sonosvad.inf.
```

> The driver **source is vendored** under `driver/sonosvad/Source/` and builds as
> it stands — there is nothing to assemble by hand.
> [`../driver/DERIVATION.md`](../driver/DERIVATION.md) records what was changed in
> Microsoft's `simpleaudiosample` (MS-PL) to produce it, and how to re-derive
> against a newer upstream sample.
>
> **Build output must be on a local volume.** `stampinf` rewrites the INF through
> a memory-mapped view; on a network or VM-shared folder that write is silently
> discarded while `stampinf` still reports success, leaving `$ARCH$` unresolved
> and InfVerif failing with `error 1274`.

## 4. Sign and install

```powershell
./driver/scripts/Sign-Driver.ps1   -CertSubject "CN=SonosVAD Test" -SysPath ./driver/_local/sonosvad.sys
./driver/scripts/Install-Driver.ps1 -InfPath ./driver/sonosvad.inf
# Sign-Driver: signtool sign /v /s PrivateCertStore /n "CN=SonosVAD Test" ...
# Install-Driver: pnputil /add-driver sonosvad.inf /install
```

After install, open *Settings → System → Sound* (or `mmsys.cpl`) — one virtual
output device should appear. Run the `sonos-driver` user-mode service to map and
stream it to a room.

## 5. Disable test-signing (revert)

```powershell
./driver/scripts/Uninstall-Driver.ps1     # pnputil /delete-driver ... /uninstall
./driver/scripts/Disable-TestSigning.ps1   # bcdedit /set testsigning off  (reboot)
```

After the reboot the watermark is gone and the machine is back to enforcing
signed drivers. Re-enable Secure Boot in firmware if you turned it off in step 1.

## How this maps to the Rust control plane

The user-mode side is modelled and unit-tested today in the
[`sonos-vad-control`](../crates/sonos-vad-control) crate — it plans the
create/destroy commands (one endpoint per room) and folds the driver's replies
back into its state, with no kernel dependency. The driver phases (PCM bridge,
end-to-end, dynamic per-room endpoints, GUI integration) are tracked in GitHub
issues #46–#49 under epic #44.

## Troubleshooting

| Symptom | Likely cause / fix |
| --- | --- |
| No watermark after reboot | Secure Boot is on — disable it in firmware, re-run step 1. |
| `pnputil` "driver package … not imported" | Driver not signed, or the test cert is not in *Trusted Root* + *Trusted Publishers* (step 2). |
| Device appears but no audio | Nothing is routed to it yet — pick it as the app/default output, and run the `sonos-driver` service to stream it. |
| Driver fails to load (Code 52) | Test-signing not active, or the signature/cert chain is wrong — re-check steps 1–4. |
