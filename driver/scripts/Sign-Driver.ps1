<#
.SYNOPSIS
    Test-signs the built sonosvad driver package with the personal test cert
    created by New-TestCert.ps1. See docs/DRIVER-SETUP.md and ADR-0018.
.DESCRIPTION
    Signs BOTH sonosvad.sys and sonosvad.cat. Signing only the .sys is the
    classic mistake: `pnputil` validates the *catalog* signature against Trusted
    Publishers, so a package with a signed binary and an unsigned (or
    differently-signed) catalog is rejected at install with a bare "third-party
    INF does not contain digital signature information".

    Order matters, and only in one direction. The .sys is signed first because
    the catalog stores each file's Authenticode hash, which deliberately excludes
    the certificate table -- so embedding a signature afterwards does not
    invalidate the catalog. Signing the catalog first and the binary second is
    equally fine for that reason; what is not fine is leaving either unsigned.

    The build already test-signs both files with a WDK-generated certificate
    ("WDKTestCert <user>,<serial>"), which is trusted nowhere by default. Running
    this replaces those signatures with your own cert, which New-TestCert.ps1 has
    put in Trusted Root and Trusted Publishers.
.PARAMETER CertSubject
    Subject (CN) of the test cert. Must match New-TestCert.ps1's -Subject.
.PARAMETER Configuration
    Build configuration whose artifacts to sign. Release (default) or Debug.
.PARAMETER Platform
    Build platform whose artifacts to sign. x64 (default) or ARM64.
.PARAMETER SysPath
    Explicit path to sonosvad.sys. Defaults to the build's artifact directory.
.PARAMETER CatPath
    Explicit path to sonosvad.cat. Defaults to the build's artifact directory.
.PARAMETER TimestampUrl
    RFC-3161 timestamp server. Default: http://timestamp.digicert.com.
.EXAMPLE
    powershell -File driver\scripts\Sign-Driver.ps1
.NOTES
    Not run by CI. Requires the Windows SDK (signtool) on the machine.
#>
[CmdletBinding()]
param(
    [string] $CertSubject = 'CN=SonosVAD Test',

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string] $Platform = 'x64',

    [string] $SysPath,
    [string] $CatPath,
    [string] $TimestampUrl = 'http://timestamp.digicert.com'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'DriverCommon.ps1')

Write-Host '== Test-sign sonosvad driver package ==' -ForegroundColor Cyan

$artifactDir = Get-SonosVadArtifactDir -Configuration $Configuration -Platform $Platform
if (-not $SysPath) { $SysPath = Join-Path $artifactDir 'sonosvad.sys' }
if (-not $CatPath) { $CatPath = Join-Path $artifactDir 'sonosvad.cat' }

foreach ($path in @($SysPath, $CatPath)) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "Not found: $path. Run Build-Driver.ps1 first."
    }
}

$signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
if ($signtool) {
    $signtoolPath = $signtool.Path
}
else {
    $signtoolPath = Get-KitTool -Name 'signtool.exe'
}
Write-Host "Using signtool: $signtoolPath"

$cert = Get-ChildItem Cert:\LocalMachine\My -ErrorAction SilentlyContinue |
    Where-Object { $_.Subject -eq $CertSubject } |
    Select-Object -First 1
# signtool searches the *user* store unless told otherwise, so which store the
# certificate lives in decides whether /sm is required. New-TestCert.ps1 creates
# it in LocalMachine\My; a cert imported by hand is often in CurrentUser\My.
$machineStore = $true
if (-not $cert) {
    $machineStore = $false
    $cert = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $CertSubject } |
        Select-Object -First 1
}
if (-not $cert) {
    throw "No certificate with subject '$CertSubject' in LocalMachine\My or CurrentUser\My. Run New-TestCert.ps1 first."
}
Write-Host "Signing with $CertSubject (thumbprint $($cert.Thumbprint))"

foreach ($path in @($SysPath, $CatPath)) {
    Write-Host "-- $(Split-Path -Leaf $path)"
    $arguments = @('sign', '/v', '/fd', 'SHA256')
    if ($machineStore) { $arguments += '/sm' }
    $arguments += @('/sha1', $cert.Thumbprint, '/tr', $TimestampUrl, '/td', 'SHA256', $path)

    $sign = Invoke-NativeCommand -Exe $signtoolPath -Arguments $arguments
    if ($sign.ExitCode -ne 0) {
        Write-Host $sign.Output
        throw "signtool sign failed for $path with exit code $($sign.ExitCode)."
    }
}

Write-Host 'Verifying...'
foreach ($path in @($SysPath, $CatPath)) {
    # /pa uses the Authenticode policy; a self-signed test cert does not chain to
    # a Microsoft root, so a non-zero result here is expected and not a failure
    # while test signing is enabled. The signer line is the useful output.
    $verify = Invoke-NativeCommand -Exe $signtoolPath -Arguments @('verify', '/v', '/pa', $path)
    $signer = ($verify.Output -split "`n" | Select-String -Pattern 'Issued to:' | Select-Object -First 1)
    Write-Host ("  {0,-14} {1}" -f (Split-Path -Leaf $path), ($signer -replace '\s+', ' ').Trim())
}

Write-Host ''
Write-Host 'Driver package test-signed. Next: Install-Driver.ps1.' -ForegroundColor Green
