#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Uninstalls the sonosvad driver via pnputil. See docs/DRIVER-SETUP.md and
    ADR-0017.
.DESCRIPTION
    Finds the installed sonosvad OEM INF in the driver store and removes it with
    pnputil /delete-driver /uninstall. Run this before Disable-TestSigning.ps1.
.PARAMETER OemInf
    Optional explicit published OEM INF name (e.g. oem23.inf). If omitted, the
    script locates it by matching the original driver INF / device description.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [string] $OemInf
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Uninstall sonosvad driver ==' -ForegroundColor Cyan

if (-not $OemInf) {
    Write-Host 'Locating the published sonosvad driver package...'
    $enum = pnputil.exe /enum-drivers
    $current = $null
    foreach ($line in $enum) {
        if ($line -match 'Published Name\s*:\s*(oem\d+\.inf)') {
            $current = $Matches[1]
        }
        if ($line -match 'sonosvad|Sonos Virtual Audio' -and $current) {
            $OemInf = $current
            break
        }
    }
}

if (-not $OemInf) {
    throw 'Could not find an installed sonosvad driver package. Pass -OemInf explicitly after checking `pnputil /enum-drivers`.'
}

Write-Host "pnputil /delete-driver $OemInf /uninstall"
pnputil.exe /delete-driver $OemInf /uninstall
if ($LASTEXITCODE -ne 0) {
    throw "pnputil uninstall failed with exit code $LASTEXITCODE."
}

Write-Host "Driver $OemInf removed. Run Disable-TestSigning.ps1 to revert test-signing." -ForegroundColor Green
