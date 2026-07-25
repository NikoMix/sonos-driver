#Requires -Version 5.1
<#
.SYNOPSIS
    Builds the sonosvad kernel-mode virtual audio driver package.

.DESCRIPTION
    Locates the 64-bit MSBuild that ships with Visual Studio Build Tools 2022,
    builds driver/sonosvad/SonosVad.sln, and copies the resulting driver package
    (sonosvad.sys, sonosvad.inf, sonosvad.cat, sonosvad.pdb) into
    driver/sonosvad/artifacts/<Platform>/<Configuration>/.

    Build intermediates and outputs go to -BuildRoot, which MUST be on a real
    local volume. See driver/sonosvad/Directory.Build.props for why (short
    version: stampinf.exe rewrites INF files through a memory-mapped view, and
    those writes are silently discarded on Windows Sandbox mapped folders).

    Prerequisites are installed by driver/scripts/Install-BuildTools.ps1.

.PARAMETER Configuration
    Release (default) or Debug.

.PARAMETER Platform
    x64 (default) or ARM64.

.PARAMETER BuildRoot
    Directory for intermediates and linker output. Defaults to a local temp path.

.PARAMETER Clean
    Delete BuildRoot before building.

.EXAMPLE
    powershell -File driver\scripts\Build-Driver.ps1 -Configuration Release -Platform x64
#>
[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string] $Platform = 'x64',

    [string] $BuildRoot,

    [switch] $Clean
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# $PSScriptRoot is unreliable inside param() defaults under -File on PS 5.1.
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$driverDir = (Resolve-Path (Join-Path $scriptDir '..\sonosvad')).Path
$solution = Join-Path $driverDir 'SonosVad.sln'

if (-not $BuildRoot) {
    $BuildRoot = Join-Path $env:TEMP 'sonosvad-build'
}

function Find-MSBuild {
    # The 64-bit MSBuild is required: the 32-bit one cannot load the WDK's
    # InfVerif.dll, which the WDK ships only for x64 and arm64.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $roots = @()
    if (Test-Path $vswhere) {
        $roots += & $vswhere -latest -products * -property installationPath 2>$null
    }
    $roots += @(
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\2022\BuildTools'),
        (Join-Path ${env:ProgramFiles} 'Microsoft Visual Studio\2022\BuildTools')
    )
    foreach ($root in ($roots | Where-Object { $_ })) {
        foreach ($rel in 'MSBuild\Current\Bin\amd64\MSBuild.exe', 'MSBuild\Current\Bin\arm64\MSBuild.exe') {
            $candidate = Join-Path $root $rel
            if (Test-Path $candidate) { return $candidate }
        }
    }
    throw 'Could not find a 64-bit MSBuild.exe. Run driver\scripts\Install-BuildTools.ps1 first.'
}

function Assert-LocalVolume {
    param([string] $Path)
    $qualifier = (Split-Path -Qualifier $Path)
    $disk = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$qualifier'" -ErrorAction SilentlyContinue
    if ($disk -and $disk.DriveType -eq 4) {
        throw "BuildRoot '$Path' is on a network drive. stampinf cannot reliably write there; choose a local path."
    }
}

$msbuild = Find-MSBuild
Write-Host '== Build sonosvad driver ==' -ForegroundColor Cyan
Write-Host "MSBuild   : $msbuild"
Write-Host "Solution  : $solution"
Write-Host "BuildRoot : $BuildRoot"
Write-Host "Config    : $Configuration|$Platform"

Assert-LocalVolume -Path $BuildRoot

if ($Clean -and (Test-Path $BuildRoot)) {
    Write-Host 'Cleaning build root...'
    Remove-Item $BuildRoot -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $BuildRoot | Out-Null

# /m:4 rather than plain /m: this was developed in a 4 GB sandbox where
# unbounded parallelism makes cl.exe thrash.
& $msbuild $solution `
    "/p:Configuration=$Configuration" `
    "/p:Platform=$Platform" `
    "/p:SonosVadBuildRoot=$BuildRoot\" `
    '/m:4' '/nologo' '/v:minimal' '/consoleloggerparameters:Summary'

if ($LASTEXITCODE -ne 0) {
    throw "MSBuild failed with exit code $LASTEXITCODE."
}

$packageDir = Join-Path $BuildRoot "bin\package\$Platform\$Configuration\package"
if (-not (Test-Path $packageDir)) {
    throw "Driver package directory not found: $packageDir"
}

$artifactDir = Join-Path $driverDir "artifacts\$Platform\$Configuration"
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
Get-ChildItem $artifactDir -File | Remove-Item -Force

Copy-Item (Join-Path $packageDir '*') $artifactDir -Force
$driverPdb = Join-Path $BuildRoot "bin\sonosvad\$Platform\$Configuration\sonosvad.pdb"
if (Test-Path $driverPdb) { Copy-Item $driverPdb $artifactDir -Force }

Write-Host ''
Write-Host "Driver package -> $artifactDir" -ForegroundColor Green
Get-ChildItem $artifactDir -File |
    Select-Object Name, @{ n = 'Size'; e = { '{0:N0}' -f $_.Length } } |
    Format-Table -AutoSize | Out-String | Write-Host

foreach ($required in 'sonosvad.sys', 'sonosvad.inf', 'sonosvad.cat') {
    if (-not (Test-Path (Join-Path $artifactDir $required))) {
        throw "Expected artifact missing: $required"
    }
}

Write-Host 'Build OK. Next: driver\scripts\Test-Driver.ps1 (static validation).' -ForegroundColor Green
