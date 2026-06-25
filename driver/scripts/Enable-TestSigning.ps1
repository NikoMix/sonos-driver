#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Enables Windows test-signing so the personal, self-signed sonosvad driver can
    load. Personal-use only. See docs/DRIVER-SETUP.md and ADR-0017.
.DESCRIPTION
    Runs `bcdedit /set testsigning on`. A reboot is required for the change to
    take effect, after which Windows shows a "Test Mode" desktop watermark and
    weakens its driver-security posture until reverted with Disable-TestSigning.ps1.

    If Secure Boot is enabled, Windows silently ignores test-signing; this script
    detects that and warns you to disable Secure Boot in firmware first.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [switch] $NoPrompt
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Enable test-signing (personal use) ==' -ForegroundColor Cyan

try {
    $secureBoot = Confirm-SecureBootUEFI -ErrorAction Stop
} catch {
    $secureBoot = $null  # Legacy BIOS or cmdlet unavailable.
}

if ($secureBoot -eq $true) {
    Write-Warning 'Secure Boot is ON. Windows will IGNORE test-signing until you disable Secure Boot in your firmware (UEFI) setup. Disable it, then re-run this script.'
}

if (-not $NoPrompt) {
    $answer = Read-Host 'This puts Windows in a watermarked test state and weakens driver security. Continue? [y/N]'
    if ($answer -notmatch '^[Yy]') {
        Write-Host 'Aborted; no changes made.' -ForegroundColor Yellow
        return
    }
}

Write-Host 'Running: bcdedit /set testsigning on'
bcdedit /set testsigning on
if ($LASTEXITCODE -ne 0) {
    throw "bcdedit failed with exit code $LASTEXITCODE."
}

Write-Host 'Test-signing enabled. REBOOT for it to take effect.' -ForegroundColor Green
Write-Host 'Revert later with Disable-TestSigning.ps1.'
