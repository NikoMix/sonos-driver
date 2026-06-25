<#
.SYNOPSIS
    Test-signs the built sonosvad.sys (and catalog) with the personal test cert
    created by New-TestCert.ps1. See docs/DRIVER-SETUP.md and ADR-0017.
.DESCRIPTION
    Locates signtool.exe in the Windows SDK and signs the driver binary with the
    self-signed test certificate, using a SHA-256 file digest and an RFC-3161
    timestamp. Personal use only; the resulting signature is trusted only on a
    machine where the test cert is installed and test-signing is enabled.
.PARAMETER CertSubject
    Subject (CN) of the test cert. Must match New-TestCert.ps1's -Subject.
.PARAMETER SysPath
    Path to the built sonosvad.sys.
.PARAMETER TimestampUrl
    RFC-3161 timestamp server. Default: http://timestamp.digicert.com.
.NOTES
    Not run by CI. Requires the Windows SDK (signtool) on the machine.
#>
[CmdletBinding()]
param(
    [string] $CertSubject  = 'CN=SonosVAD Test',
    [string] $SysPath      = (Join-Path $PSScriptRoot '..\_local\sonosvad.sys'),
    [string] $TimestampUrl = 'http://timestamp.digicert.com'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Test-sign sonosvad driver ==' -ForegroundColor Cyan

if (-not (Test-Path -LiteralPath $SysPath)) {
    throw "Driver binary not found: $SysPath. Run Build-Driver.ps1 first."
}

$signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
if (-not $signtool) {
    $candidates = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } |
        Sort-Object FullName -Descending
    if (-not $candidates) {
        throw 'signtool.exe not found. Install the Windows SDK (it ships with the WDK).'
    }
    $signtool = [pscustomobject]@{ Path = $candidates[0].FullName }
}

Write-Host "Using signtool: $($signtool.Path)"
Write-Host "Signing $SysPath with $CertSubject"
& $signtool.Path sign /v /fd SHA256 /a /n $CertSubject /tr $TimestampUrl /td SHA256 $SysPath
if ($LASTEXITCODE -ne 0) {
    throw "signtool sign failed with exit code $LASTEXITCODE."
}

Write-Host 'Verifying signature...'
& $signtool.Path verify /v /pa $SysPath
if ($LASTEXITCODE -ne 0) {
    Write-Warning "signtool verify returned $LASTEXITCODE (expected for a test cert not chained to a Microsoft root; OK while test-signing is enabled)."
}

Write-Host 'Driver test-signed. Next: Install-Driver.ps1.' -ForegroundColor Green
