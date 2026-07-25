#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Removes the sonosvad device node and deletes its driver package. See
    docs/DRIVER-SETUP.md and ADR-0018.
.DESCRIPTION
    The mirror image of Install-Driver.ps1, in the reverse order:

      1. Stop and deregister the streaming service. It has to go first: while it
         runs it holds loopback capture streams open on the endpoints, and it
         would immediately rename them again after step 2.
      2. Restore any endpoint names the user-mode service changed. Renames are
         stored per-endpoint in the MMDevices registry hive and survive driver
         removal, so skipping this leaves stale "Sonos Kitchen" entries behind
         forever.
      3. Remove the root device node with `devgen /remove`. Deleting the package
         first would leave an orphaned node reporting "driver not found".
      4. `pnputil /delete-driver ... /uninstall` to drop the package.

.PARAMETER OemInf
    Optional explicit published OEM INF name (e.g. oem23.inf). Located
    automatically when omitted.
.PARAMETER SonosDriverExe
    Path to sonos-driver.exe, used for step 2. Skipped when not found.
.PARAMETER ServiceExe
    Path to sonosvad-svc.exe, used for step 1. When the binary is gone the
    service is removed with `sc.exe delete` instead.
.PARAMETER KeepEndpointNames
    Leave renamed endpoints alone.
.PARAMETER KeepService
    Leave the service registered. Only useful when reinstalling the driver.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [string] $OemInf,
    [string] $SonosDriverExe,
    [string] $ServiceExe = (Join-Path $PSScriptRoot '..\..\target\release\sonosvad-svc.exe'),
    [switch] $KeepEndpointNames,
    [switch] $KeepService
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'DriverCommon.ps1')

Write-Host '== Uninstall sonosvad driver ==' -ForegroundColor Cyan

# ------------------------------------------------------------- 1. the service
if (-not $KeepService) {
    $svc = Get-Service -Name 'sonosvad' -ErrorAction SilentlyContinue
    if (-not $svc) {
        Write-Host 'Service sonosvad is not registered.'
    }
    elseif ($ServiceExe -and (Test-Path -LiteralPath $ServiceExe)) {
        Write-Host 'Stopping and deregistering the sonosvad service...'
        $rm = Invoke-NativeCommand -Exe $ServiceExe -Arguments @('uninstall')
        Write-Host $rm.Output
        if ($rm.ExitCode -ne 0) {
            Write-Warning "sonosvad-svc uninstall exited $($rm.ExitCode); falling back to sc.exe."
            $null = Invoke-NativeCommand -Exe 'sc.exe' -Arguments @('stop', 'sonosvad')
            $null = Invoke-NativeCommand -Exe 'sc.exe' -Arguments @('delete', 'sonosvad')
        }
    }
    else {
        # The binary was deleted before the uninstaller ran; the SCM entry is
        # still there and would keep failing to start on every boot.
        Write-Warning "sonosvad-svc.exe not found at '$ServiceExe'; removing the service with sc.exe."
        $null = Invoke-NativeCommand -Exe 'sc.exe' -Arguments @('stop', 'sonosvad')
        $null = Invoke-NativeCommand -Exe 'sc.exe' -Arguments @('delete', 'sonosvad')
    }

    # Deleting a service is asynchronous when a handle is still open; give the
    # SCM a moment so the following steps do not race a still-running worker.
    for ($i = 0; $i -lt 20; $i++) {
        if (-not (Get-Service -Name 'sonosvad' -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 250
    }
}

# ---------------------------------------------------------- 2. release renames
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

# ------------------------------------------------------------ 3. device node
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

# --------------------------------------------------------- 4. driver package
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
