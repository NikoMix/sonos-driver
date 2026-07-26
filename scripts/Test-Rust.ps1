<#
.SYNOPSIS
    Formats, lints and tests the Rust workspace.

.DESCRIPTION
    The canonical Rust validation entry point. It exists because two properties
    of the Windows Sandbox development environment break a plain `cargo test`:

    1. `target/` MUST live on a real local volume.
       The repo is checked out under a `<MappedFolder>` share. Cargo writes
       build-script executables into `target/` and then executes them; running
       an image off the sandbox's host-mapped filesystem fails with
       "The file or directory is corrupted and unreadable" (os error 1392).
       Setting CARGO_TARGET_DIR to a local path is the fix. (This is the same
       class of failure as the stampinf memory-mapped-write trap documented in
       driver/sonosvad/Directory.Build.props.)

    2. Cargo's default job count OOMs the sandbox.
       The VM reports 32 logical CPUs but only ~4 GB of RAM. Cargo therefore
       spawns 32 rustc processes, and the build dies with
       "rustc-LLVM ERROR: out of memory" or STATUS_STACK_BUFFER_OVERRUN
       (0xc0000409) - often surfacing as spurious "internal compiler error:
       no resolution for an import" from the poisoned artefacts. Capping jobs
       and dropping debuginfo keeps peak RSS well inside the budget.

    On a normal developer machine none of this is harmful, so the script is
    safe to run anywhere; pass -Jobs 0 to let Cargo choose.

.PARAMETER TargetDir
    Where to put Cargo's build output. Must be on a local volume.

.PARAMETER Jobs
    Parallel Cargo jobs. 0 means "let Cargo decide".

.PARAMETER SkipFormat
    Skip the `cargo fmt --check` gate.

.PARAMETER Debuginfo
    Keep debug information (uses considerably more RAM and disk).

.EXAMPLE
    .\driver\..\scripts\Test-Rust.ps1
    Runs fmt, clippy and the full test suite with sandbox-safe settings.
#>
[CmdletBinding()]
param(
    [string]$TargetDir = 'C:\build\sonos-target',
    [int]$Jobs = 2,
    [switch]$SkipFormat,
    [switch]$Debuginfo
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot

function Assert-LocalVolume {
    param([string]$Path)

    $qualifier = Split-Path -Qualifier $Path
    if (-not $qualifier) {
        throw "TargetDir '$Path' must be an absolute path on a local drive."
    }
    # A sandbox <MappedFolder> surfaces as a network-backed drive; anything the
    # build both writes and later executes has to be on a real local volume.
    $drive = Get-PSDrive -Name $qualifier.TrimEnd(':') -ErrorAction SilentlyContinue
    if ($null -ne $drive -and $drive.DisplayRoot) {
        throw "TargetDir '$Path' is on mapped drive '$($drive.DisplayRoot)'. Build outputs must be on a local volume."
    }
}

function Invoke-Cargo {
    param(
        [Parameter(Mandatory)][string]$Title,
        [Parameter(Mandatory)][string[]]$CargoArgs
    )

    Write-Host ''
    Write-Host "==> $Title" -ForegroundColor Cyan
    Write-Host "    cargo $($CargoArgs -join ' ')" -ForegroundColor DarkGray

    # cargo writes its progress to stderr. Under $ErrorActionPreference =
    # 'Stop', Windows PowerShell turns every native stderr line into a
    # terminating NativeCommandError, so the preference has to be relaxed for
    # the duration of the call. Success is decided by $LASTEXITCODE alone.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & cargo @CargoArgs 2>&1 | ForEach-Object { Write-Host "$_" }
    }
    finally {
        $ErrorActionPreference = $previous
    }
    if ($LASTEXITCODE -ne 0) {
        throw "$Title failed (exit $LASTEXITCODE)."
    }
    Write-Host "    OK" -ForegroundColor Green
}

Assert-LocalVolume -Path $TargetDir
New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null

# rustup was installed with --no-modify-path, so PATH may not have cargo yet.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ((Test-Path $cargoBin) -and ($env:PATH -notlike "*$cargoBin*")) {
    $env:PATH = "$cargoBin;$env:PATH"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found. Install Rust with rustup (see docs/DRIVER-SETUP.md)."
}

$env:CARGO_TARGET_DIR = $TargetDir
if (-not $Debuginfo) {
    $env:CARGO_PROFILE_DEV_DEBUG = '0'
    $env:CARGO_PROFILE_TEST_DEBUG = '0'
    $env:CARGO_INCREMENTAL = '0'
}

$jobArgs = @()
if ($Jobs -gt 0) { $jobArgs = @('-j', "$Jobs") }

Push-Location $repoRoot
try {
    Write-Host "Repo        : $repoRoot"
    Write-Host "Target dir  : $TargetDir"
    Write-Host "Cargo jobs  : $(if ($Jobs -gt 0) { $Jobs } else { 'auto' })"
    Write-Host "Toolchain   : $(cargo --version)"

    if (-not $SkipFormat) {
        Invoke-Cargo -Title 'cargo fmt --check' -CargoArgs @('fmt', '--all', '--check')
    }
    Invoke-Cargo -Title 'cargo clippy (warnings are errors)' `
        -CargoArgs (@('clippy', '--workspace', '--all-targets') + $jobArgs + @('--', '-D', 'warnings'))
    Invoke-Cargo -Title 'cargo test' -CargoArgs (@('test', '--workspace') + $jobArgs)

    Write-Host ''
    Write-Host 'Rust workspace validation passed.' -ForegroundColor Green
}
finally {
    Pop-Location
}
