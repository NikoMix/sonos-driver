#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Removes the sonosvad device node and deletes its driver package. See
    docs/DRIVER-SETUP.md and ADR-0018.
.DESCRIPTION
    The mirror image of Install-Driver.ps1, in the reverse order:

      1. Restore any endpoint names the user-mode service changed. Renames are
         stored per-endpoint in the MMDevices registry hive and survive driver
         removal, so skipping this leaves stale "Sonos Kitchen" entries behind
         forever.
      2. Remove the root device node with `devgen /remove`. Deleting the package
         first would leave an orphaned node reporting "driver not found".
      3. `pnputil /delete-driver ... /uninstall` to drop the package.

.PARAMETER OemInf
    Optional explicit published OEM INF name (e.g. oem23.inf). Located
    automatically when omitted.
.PARAMETER SonosDriverExe
    Path to sonos-driver.exe, used for step 1. Skipped when not found.
.PARAMETER KeepEndpointNames
    Leave renamed endpoints alone.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [string] $OemInf,
    [string] $SonosDriverExe,
    [switch] $KeepEndpointNames
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'DriverCommon.ps1')

Write-Host '== Uninstall sonosvad driver ==' -ForegroundColor Cyan

# ---------------------------------------------------------- 1. release renames
if (-not $KeepEndpointNames) {
    if (-not $SonosDriverExe) {
        $SonosDriverExe = (Get-Command 'sonos-driver.exe' -ErrorAction SilentlyContinue).Source
    }
    if ($SonosDriverExe -and (Test-Path -LiteralPath $SonosDriverExe)) {
        Write-Host 'Restoring factory endpoint names...'
        $release = Invoke-NativeCommand -Exe $SonosDriverExe -Arguments @('vad', 'release')
        Write-Host $release.Output
        if ($release.ExitCode -ne 0) {
            Write-Warning "vad release exited $($release.ExitCode); endpoint names may be left renamed."
        }
    }
    else {
        Write-Warning 'sonos-driver.exe not found; skipping endpoint name restore. Pass -SonosDriverExe to enable it.'
    }
}

# ------------------------------------------------------------ 2. device node
$node = Get-SonosVadDeviceNode
if ($node) {
    $devgen = Get-Devgen
    Write-Host "devgen /remove $($node.InstanceId)"
    $remove = Invoke-NativeCommand -Exe $devgen -Arguments @('/remove', $node.InstanceId)
    Write-Host $remove.Output
    if ($remove.ExitCode -ne 0) {
        # devgen refuses nodes it did not create; pnputil can still force it out.
        Write-Warning "devgen /remove exited $($remove.ExitCode); falling back to pnputil /remove-device."
        $fallback = Invoke-NativeCommand -Exe 'pnputil.exe' -Arguments @('/remove-device', $node.InstanceId)
        Write-Host $fallback.Output
        if ($fallback.ExitCode -ne 0) {
            throw "Could not remove device node $($node.InstanceId)."
        }
    }
}
else {
    Write-Host 'No sonosvad device node present.'
}

# --------------------------------------------------------- 3. driver package
if (-not $OemInf) {
    Write-Host 'Locating the published sonosvad driver package...'
    $OemInf = Get-SonosVadOemInf
}

if (-not $OemInf) {
    Write-Host 'No sonosvad driver package is staged; nothing more to remove.' -ForegroundColor Green
    return
}

Write-Host "pnputil /delete-driver $OemInf /uninstall"
$delete = Invoke-NativeCommand -Exe 'pnputil.exe' -Arguments @('/delete-driver', $OemInf, '/uninstall')
Write-Host $delete.Output
if ($delete.ExitCode -ne 0) {
    throw "pnputil uninstall failed with exit code $($delete.ExitCode)."
}

Write-Host "Driver $OemInf removed. Run Disable-TestSigning.ps1 to revert test-signing." -ForegroundColor Green
