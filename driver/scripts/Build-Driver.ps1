<#
.SYNOPSIS
    Builds the Sysvad-derived sonosvad driver with msbuild + the WDK. See
    docs/DRIVER-SETUP.md, driver/DERIVATION.md, and ADR-0017.
.DESCRIPTION
    Thin wrapper around msbuild for the owner-assembled C/C++ driver project. This
    repository ships the INF, derivation guide, and scripts — NOT the driver
    sources — so you must first assemble driver/src/ by applying DERIVATION.md to
    Microsoft's Sysvad sample. This script then compiles it.

    Requires the Windows Driver Kit (WDK) or EWDK and MSVC on PATH. Not run by CI.
.PARAMETER Project
    Path to the driver .vcxproj (assembled per DERIVATION.md).
.PARAMETER Configuration
    Build configuration (Debug | Release). Default: Release.
.PARAMETER Platform
    Target platform. Default: x64.
.PARAMETER OutDir
    Where to copy the built .sys/.inf for signing. Default: driver/_local.
#>
[CmdletBinding()]
param(
    [string] $Project       = (Join-Path $PSScriptRoot '..\src\sonosvad.vcxproj'),
    [ValidateSet('Debug', 'Release')]
    [string] $Configuration = 'Release',
    [ValidateSet('x64', 'ARM64')]
    [string] $Platform      = 'x64',
    [string] $OutDir        = (Join-Path $PSScriptRoot '..\_local')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '== Build sonosvad driver ==' -ForegroundColor Cyan

$msbuild = Get-Command msbuild.exe -ErrorAction SilentlyContinue
if (-not $msbuild) {
    throw 'msbuild.exe not found on PATH. Open a WDK/EWDK or Visual Studio "Developer Command Prompt", or run the EWDK LaunchBuildEnv, then retry.'
}

if (-not (Test-Path -LiteralPath $Project)) {
    throw "Driver project not found: $Project`nAssemble driver/src/ first by following driver/DERIVATION.md (derive from Microsoft's MIT-licensed Sysvad sample). This repo intentionally does not vendor a ready-to-compile .sys."
}

if (-not (Test-Path -LiteralPath $OutDir)) {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
}

Write-Host "msbuild $Project /p:Configuration=$Configuration /p:Platform=$Platform"
& $msbuild.Path $Project "/p:Configuration=$Configuration" "/p:Platform=$Platform" /nologo /verbosity:minimal
if ($LASTEXITCODE -ne 0) {
    throw "msbuild failed with exit code $LASTEXITCODE."
}

$built = Get-ChildItem -Path (Split-Path $Project) -Recurse -Include 'sonosvad.sys', 'sonosvad.inf' -ErrorAction SilentlyContinue
foreach ($file in $built) {
    Copy-Item -LiteralPath $file.FullName -Destination $OutDir -Force
    Write-Host "  -> $($file.Name) copied to $OutDir"
}

Write-Host 'Build complete. Next: Sign-Driver.ps1 then Install-Driver.ps1.' -ForegroundColor Green
