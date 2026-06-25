#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Installs the test-signed sonosvad driver via pnputil. See docs/DRIVER-SETUP.md
    and ADR-0017.
.DESCRIPTION
    Adds and installs the driver package from its INF. Test-signing must be enabled
    (Enable-TestSigning.ps1 + reboot) and the test cert trusted (New-TestCert.ps1),
    or the install will be rejected. After install, a virtual output device appears
    in Sound -> Output.
.PARAMETER InfPath
    Path to sonosvad.inf.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [string] $InfPath = (Join-Path $PSScriptRoot '..\sonosvad.inf')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Install sonosvad driver ==' -ForegroundColor Cyan

if (-not (Test-Path -LiteralPath $InfPath)) {
    throw "INF not found: $InfPath"
}
$InfPath = (Resolve-Path -LiteralPath $InfPath).Path

Write-Host "pnputil /add-driver `"$InfPath`" /install"
pnputil.exe /add-driver "$InfPath" /install
if ($LASTEXITCODE -ne 0) {
    throw "pnputil install failed with exit code $LASTEXITCODE. Check that test-signing is ON (reboot applied) and the test cert is in Trusted Root + Trusted Publishers."
}

Write-Host 'Driver installed. Open mmsys.cpl to confirm the virtual output device appears.' -ForegroundColor Green
Write-Host 'Then run the sonos-driver service to map and stream it to a room.'
