<#
.SYNOPSIS
    Build the sonos-driver Windows installer (.exe) with Inno Setup.

.DESCRIPTION
    Produces a single-file Windows setup executable that installs both shipped
    binaries (the CLI and the tray GUI), the docs, both licences, and a generated
    example config, plus Start-menu shortcuts and an uninstaller.

    To guarantee the installer payload is byte-for-byte identical to the portable
    zip, this script reuses scripts/package.ps1: it builds (or reuses) the zip,
    expands it to a temporary stage, and compiles installer/sonos-driver.iss
    against that stage with the Inno Setup command-line compiler (ISCC).

    The setup executable is named:

      sonos-driver-<version>-x86_64-setup.exe

    where <version> is read from [workspace.package] in the workspace Cargo.toml.

    Inno Setup is free for commercial use with no paid licence; see
    docs/adr/0015-release-pipeline.md.

.PARAMETER OutputDir
    Directory to write the setup .exe (and to find/create the zip). Defaults to
    "dist" at the repository root.

.PARAMETER SkipBuild
    Forwarded to package.ps1: reuse already-built release binaries instead of
    rebuilding. The zip is still (re)assembled from those binaries.

.PARAMETER IncludeDriver
    Forwarded to package.ps1: also bundle the signed sonosvad driver and its install
    scripts, and offer the kernel-driver mode as an optional component in the wizard.
    The driver must already be built and signed; see package.ps1's help.

.PARAMETER RequireInstaller
    Fail if the Inno Setup compiler (ISCC.exe) cannot be found. Without this flag
    a missing ISCC is a warning and the script exits 0 (so a developer without
    Inno Setup installed can still build the portable zip via package.ps1).

.PARAMETER Iscc
    Explicit path to ISCC.exe. If omitted the script searches PATH and the
    default Inno Setup 6 install locations.

.EXAMPLE
    pwsh -File scripts/build-installer.ps1
    Builds the zip and, if ISCC is available, the matching setup .exe in dist\.

.NOTES
    Windows / PowerShell 5.1+ or 7+. Requires Inno Setup 6 (ISCC.exe) to emit the
    installer; install it with `choco install innosetup` or from jrsoftware.org.
#>
[CmdletBinding()]
param(
    [string] $OutputDir,
    [switch] $SkipBuild,
    [switch] $IncludeDriver,
    [switch] $RequireInstaller,
    [string] $Iscc
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Resolve paths -----------------------------------------------------------
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot  = Resolve-Path (Join-Path $scriptDir '..')
$issPath   = Join-Path $repoRoot 'installer\sonos-driver.iss'
$target    = 'x86_64-pc-windows-msvc'

if (-not $OutputDir) {
    $OutputDir = Join-Path $repoRoot 'dist'
}
if (-not (Test-Path $issPath)) {
    throw "Inno Setup script not found at '$issPath'."
}

# --- Read the workspace version (same source of truth as package.ps1) --------
$cargoToml = Join-Path $repoRoot 'Cargo.toml'
$tomlText  = Get-Content $cargoToml -Raw
$wpMatch = [regex]::Match($tomlText, '(?ms)^\[workspace\.package\]\s*(.*?)(^\[|\z)')
if (-not $wpMatch.Success) {
    throw "Could not locate a [workspace.package] section in $cargoToml."
}
$verMatch = [regex]::Match($wpMatch.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
if (-not $verMatch.Success) {
    throw "Could not read 'version' from [workspace.package] in $cargoToml."
}
$version = $verMatch.Groups[1].Value
Write-Host "sonos-driver version: $version" -ForegroundColor Cyan

# --- Locate the Inno Setup compiler -----------------------------------------
function Find-Iscc {
    param([string] $Explicit)
    if ($Explicit) {
        if (Test-Path $Explicit) { return (Resolve-Path $Explicit).Path }
        throw "ISCC.exe not found at the supplied -Iscc path '$Explicit'."
    }
    $cmd = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path $c)) { return $c }
    }
    return $null
}

$isccPath = Find-Iscc -Explicit $Iscc
if (-not $isccPath) {
    $msg = 'Inno Setup compiler (ISCC.exe) not found. Install it with ' +
           '`choco install innosetup` or from https://jrsoftware.org/isdl.php.'
    if ($RequireInstaller) {
        throw $msg
    }
    Write-Warning $msg
    Write-Warning 'Skipping installer build (the portable zip is unaffected). Pass -RequireInstaller to make this fatal.'
    return
}
Write-Host "Using ISCC: $isccPath" -ForegroundColor Cyan

# --- Build / reuse the portable zip, then stage from it ----------------------
$zipName = "sonos-driver-$version-$target.zip"
$zipPath = Join-Path $OutputDir $zipName

$packageArgs = @{ OutputDir = $OutputDir }
if ($SkipBuild)     { $packageArgs['SkipBuild']     = $true }
if ($IncludeDriver) { $packageArgs['IncludeDriver'] = $true }
Write-Host 'Assembling portable zip via package.ps1' -ForegroundColor Cyan
& (Join-Path $scriptDir 'package.ps1') @packageArgs | Out-Null
if (-not (Test-Path $zipPath)) {
    throw "package.ps1 did not produce the expected archive '$zipPath'."
}

$stageRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sonos-driver-iss-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stageRoot -Force | Out-Null
try {
    Expand-Archive -Path $zipPath -DestinationPath $stageRoot -Force
    # package.ps1 nests everything under a single "<name>-<version>-<target>" folder.
    $payloadDir = Join-Path $stageRoot "sonos-driver-$version-$target"
    if (-not (Test-Path $payloadDir)) {
        throw "Expanded archive did not contain the expected payload folder '$payloadDir'."
    }

    if (-not (Test-Path $OutputDir)) {
        New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
    }

    Write-Host 'Compiling installer with ISCC' -ForegroundColor Cyan
    & $isccPath `
        "/DAppVersion=$version" `
        "/DPayloadDir=$payloadDir" `
        "/DOutputDir=$OutputDir" `
        $issPath
    if ($LASTEXITCODE -ne 0) {
        throw "ISCC failed with exit code $LASTEXITCODE."
    }

    $setupPath = Join-Path $OutputDir "sonos-driver-$version-x86_64-setup.exe"
    if (-not (Test-Path $setupPath)) {
        throw "ISCC reported success but '$setupPath' is missing."
    }
    $sizeKb = [math]::Round((Get-Item $setupPath).Length / 1KB, 1)
    Write-Host ""
    Write-Host "Built installer sonos-driver $version" -ForegroundColor Green
    Write-Host "  -> $setupPath ($sizeKb KB)" -ForegroundColor Green
    Write-Output $setupPath
} finally {
    if (Test-Path $stageRoot) {
        Remove-Item $stageRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
