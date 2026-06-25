#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Disables Windows test-signing, reverting the change made by
    Enable-TestSigning.ps1. See docs/DRIVER-SETUP.md and ADR-0017.
.DESCRIPTION
    Runs `bcdedit /set testsigning off`. A reboot is required; afterwards the
    "Test Mode" watermark is gone and Windows again enforces signed drivers.
    Uninstall the driver first with Uninstall-Driver.ps1.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Disable test-signing (revert) ==' -ForegroundColor Cyan
Write-Host 'Running: bcdedit /set testsigning off'
bcdedit /set testsigning off
if ($LASTEXITCODE -ne 0) {
    throw "bcdedit failed with exit code $LASTEXITCODE."
}

Write-Host 'Test-signing disabled. REBOOT to return to normal driver enforcement.' -ForegroundColor Green
Write-Host 'If you turned Secure Boot off in firmware to enable test-signing, re-enable it now.'
