#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Creates a self-signed code-signing certificate for test-signing the sonosvad
    driver on THIS machine only. See docs/DRIVER-SETUP.md and ADR-0017.
.DESCRIPTION
    Generates a code-signing cert in the local store, exports the public .cer, and
    installs it into Local Machine \ Trusted Root and \ Trusted Publishers so the
    test-signed driver is trusted on this machine. The certificate is for personal
    use only and must never be distributed.
.PARAMETER Subject
    Certificate subject (CN). Must match the -CertSubject passed to Sign-Driver.ps1.
.PARAMETER OutDir
    Directory for the exported public certificate (.cer). Created if missing.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [string] $Subject = 'CN=SonosVAD Test',
    [string] $OutDir  = (Join-Path $PSScriptRoot '..\_local')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Create test code-signing certificate ==' -ForegroundColor Cyan

if (-not (Test-Path -LiteralPath $OutDir)) {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
}
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path
$cerPath = Join-Path $OutDir 'SonosVADTest.cer'

Write-Host "Creating self-signed cert: $Subject"
$cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject $Subject `
    -CertStoreLocation 'Cert:\LocalMachine\My' `
    -KeyUsage DigitalSignature `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears(3)

Write-Host "Thumbprint: $($cert.Thumbprint)"

Export-Certificate -Cert $cert -FilePath $cerPath -Force | Out-Null
Write-Host "Exported public cert -> $cerPath"

foreach ($store in @('Root', 'TrustedPublisher')) {
    Write-Host "Importing into LocalMachine\$store ..."
    Import-Certificate -FilePath $cerPath -CertStoreLocation "Cert:\LocalMachine\$store" | Out-Null
}

Write-Host 'Test certificate created and trusted on THIS machine.' -ForegroundColor Green
Write-Host "Pass -CertSubject '$Subject' to Sign-Driver.ps1."
