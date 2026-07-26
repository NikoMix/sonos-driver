#Requires -Version 5.1
#Requires -RunAsAdministrator
<#
.SYNOPSIS
    One command that takes a clean machine to working Sonos room endpoints.
    See docs/DRIVER-SETUP.md and ADR-0017.

.DESCRIPTION
    Build -> certificate -> sign -> test-signing -> install -> verify, in the one
    order that works, skipping whatever is already done.

    Run it, reboot when it asks, run it again. It is convergent: every step
    checks the machine's actual state first, so re-running after an interruption
    resumes rather than repeats, and running it on an already-working machine is
    a no-op that just re-verifies.

    A reboot in the middle is unavoidable -- `bcdedit /set testsigning on` only
    takes effect at boot, and the driver cannot load before it does. Rather than
    pretend otherwise, this script stops at that boundary with the reason and the
    exact command to resume. It also remembers that it enabled test-signing, so
    on the next run it can tell "configured but not yet rebooted" from "active",
    which bcdedit alone cannot: bcdedit reports the value stored for the *next*
    boot, not the one the running kernel is enforcing.

    What it does NOT do: disable Secure Boot. That lives in firmware, no API can
    change it, and Windows silently ignores test-signing while it is on -- so the
    script detects it and stops rather than producing a driver that installs and
    then refuses to start for reasons nobody can see.

    Teardown is the separate Uninstall-Driver.ps1, which is equally idempotent.

.PARAMETER SeedHost
    IP address of any one Sonos player, baked into the service command line. Use
    on networks where SSDP multicast is filtered and discovery finds nothing.

.PARAMETER CertSubject
    Subject (CN) of the personal test certificate. Reused across runs.

.PARAMETER Configuration
    Build configuration. Release (default) or Debug.

.PARAMETER Platform
    Build platform. x64 (default) or ARM64.

.PARAMETER SkipBuild
    Do not rebuild the driver or the services; use whatever is already built.
    Fails if the artifacts are missing.

.PARAMETER SkipService
    Install the driver and its endpoints but do not register the streaming
    service. The endpoints appear in Sound and stay silent until something binds
    them, which is what you want when driving the pipeline by hand with
    `sonos-driver vad`.

.PARAMETER WhatIfOnly
    Report what each step would do and change nothing. Answers "how far along is
    this machine?" without touching it.

.EXAMPLE
    .\driver\scripts\Setup-Driver.ps1 -SeedHost 192.168.178.25

.EXAMPLE
    .\driver\scripts\Setup-Driver.ps1 -WhatIfOnly

.NOTES
    Not run by CI: needs the WDK, an elevated shell, and a machine it may reboot.
#>
[CmdletBinding()]
param(
    [string] $SeedHost,
    [string] $CertSubject = 'CN=SonosVAD Test',

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string] $Platform = 'x64',

    [switch] $SkipBuild,
    [switch] $SkipService,
    [switch] $WhatIfOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $scriptDir 'DriverCommon.ps1')

$repoRoot = (Resolve-Path (Join-Path $scriptDir '..\..')).Path
$localDir = Join-Path (Split-Path -Parent $scriptDir) '_local'
$rebootMarker = Join-Path $localDir '.testsigning-pending'

# This script builds and signs, so it needs the source tree. A release payload has
# a flat driver\ folder with an already-signed package and its own INSTALL.md;
# without this guard the first path resolution fails with something about
# Resolve-Path that tells the reader nothing about what to do instead.
if (-not (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $scriptDir) 'sonosvad'))) {
    throw ("Setup-Driver.ps1 has to run from the repository - it builds and signs the driver, " +
           "and the source tree is not here. If you unpacked a release, the package is already " +
           "signed: follow driver\INSTALL.md in that folder instead.")
}

$script:StepNumber = 0
$script:Actions = @()

function Write-Step {
    param([Parameter(Mandatory)][string] $Title)
    $script:StepNumber++
    Write-Host ''
    Write-Host "== $script:StepNumber. $Title ==" -ForegroundColor Cyan
}

function Write-Done {
    param([Parameter(Mandatory)][string] $Message)
    Write-Host "   $Message" -ForegroundColor Green
}

function Write-Skip {
    param([Parameter(Mandatory)][string] $Message)
    Write-Host "   already done - $Message" -ForegroundColor DarkGray
}

function Write-Plan {
    param([Parameter(Mandatory)][string] $Message)
    Write-Host "   would: $Message" -ForegroundColor Yellow
    $script:Actions += $Message
}

function Invoke-Child {
    <#
    .SYNOPSIS
        Runs a sibling script in-process so its Write-Host output interleaves.
    .DESCRIPTION
        `&` on a .ps1 keeps the current scope's $ErrorActionPreference, which is
        what we want: a child that throws must abort the whole setup rather than
        let the next step run against a half-built machine.
    #>
    param(
        [Parameter(Mandatory)][string] $Name,
        [hashtable] $Arguments = @{}
    )
    $path = Join-Path $scriptDir $Name
    if (-not (Test-Path -LiteralPath $path)) { throw "Missing script: $path" }
    & $path @Arguments
}

function Test-CertReady {
    <#
    .SYNOPSIS
        True when the test cert exists AND is trusted for driver installation.
    .DESCRIPTION
        All three stores matter and they fail differently: without My there is
        nothing to sign with, without Root the signature chains to an untrusted
        anchor, and without TrustedPublisher pnputil rejects the catalog with a
        message that mentions none of this.
    #>
    param([Parameter(Mandatory)][string] $Subject)

    foreach ($store in @('My', 'Root', 'TrustedPublisher')) {
        $hit = Get-ChildItem "Cert:\LocalMachine\$store" -ErrorAction SilentlyContinue |
            Where-Object { $_.Subject -eq $Subject }
        if (-not $hit) { return $false }
    }
    return $true
}

function Get-TestSigningState {
    <#
    .SYNOPSIS
        'active', 'pending-reboot', 'off', or 'unknown'.
    .DESCRIPTION
        bcdedit reports what the *next* boot will use, so "on" alone cannot tell
        an enabled-and-rebooted machine from one that was configured five seconds
        ago. The marker file, written when this script enables it, closes that
        gap: newer than the last boot means the reboot has not happened yet.

        Returns 'unknown' where there is no BCD store to read at all (Windows
        Sandbox, some VMs), because there the driver can never load and saying
        "off" would imply enabling it is worth trying.
    #>
    $enum = Invoke-NativeCommand -Exe 'bcdedit.exe' -Arguments @('/enum', '{current}')
    if ($enum.ExitCode -ne 0) { return 'unknown' }
    if ($enum.Output -notmatch 'testsigning\s+Yes') { return 'off' }

    if (Test-Path -LiteralPath $rebootMarker) {
        $lastBoot = (Get-CimInstance Win32_OperatingSystem).LastBootUpTime
        $marked = (Get-Item -LiteralPath $rebootMarker).LastWriteTime
        if ($marked -gt $lastBoot) { return 'pending-reboot' }
        Remove-Item -LiteralPath $rebootMarker -Force -ErrorAction SilentlyContinue
    }
    return 'active'
}

function Stop-WithResume {
    param(
        [Parameter(Mandatory)][string] $Reason,
        [Parameter(Mandatory)][string] $Next
    )
    Write-Host ''
    Write-Host '-------------------------------------------------------------' -ForegroundColor Yellow
    Write-Host $Reason -ForegroundColor Yellow
    Write-Host ''
    Write-Host $Next
    Write-Host '-------------------------------------------------------------' -ForegroundColor Yellow
    exit 2
}

$resumeCommand = "powershell -File `"$($MyInvocation.MyCommand.Path)`""
if ($SeedHost) { $resumeCommand += " -SeedHost $SeedHost" }

Write-Host ''
Write-Host 'sonosvad setup' -ForegroundColor Cyan
Write-Host "  repo          $repoRoot"
Write-Host "  configuration $Configuration / $Platform"
if ($WhatIfOnly) { Write-Host '  mode          report only, nothing will change' -ForegroundColor Yellow }

# --------------------------------------------------------------- 1. Secure Boot
Write-Step 'Check Secure Boot'
try {
    $secureBoot = Confirm-SecureBootUEFI -ErrorAction Stop
} catch {
    $secureBoot = $null  # Legacy BIOS, or the cmdlet is unavailable.
}
if ($secureBoot -eq $true) {
    Stop-WithResume -Reason 'Secure Boot is ON, and Windows silently ignores test-signing while it is. The driver would install and then never start.' `
        -Next "Disable Secure Boot in your firmware (UEFI) setup, then re-run:`n  $resumeCommand"
}
elseif ($null -eq $secureBoot) {
    Write-Done 'Secure Boot state unavailable (legacy BIOS or non-UEFI) - continuing.'
}
else {
    Write-Done 'Secure Boot is off.'
}

# ------------------------------------------------------------ 2. build the driver
Write-Step 'Build the driver package'
$artifactDir = Get-SonosVadArtifactDir -Configuration $Configuration -Platform $Platform
$sysPath = Join-Path $artifactDir 'sonosvad.sys'

if ($SkipBuild) {
    if (-not (Test-Path -LiteralPath $sysPath)) {
        throw "-SkipBuild was passed but $sysPath does not exist. Run without -SkipBuild."
    }
    Write-Skip "using the existing package in $artifactDir"
}
elseif ($WhatIfOnly) {
    if (Test-Path -LiteralPath $sysPath) { Write-Skip "package present in $artifactDir (would rebuild anyway)" }
    else { Write-Plan 'build the driver with Build-Driver.ps1 (needs the WDK)' }
}
else {
    # Always rebuild: a stale .sys next to fresh sources is the kind of thing
    # that costs an hour of debugging a bug that was fixed already.
    Invoke-Child -Name 'Build-Driver.ps1' -Arguments @{ Configuration = $Configuration; Platform = $Platform }
    Write-Done "built -> $artifactDir"
}

# -------------------------------------------------------------- 3. certificate
Write-Step 'Create and trust the test certificate'
if (Test-CertReady -Subject $CertSubject) {
    Write-Skip "'$CertSubject' is present in My, Root and TrustedPublisher"
}
elseif ($WhatIfOnly) {
    Write-Plan "create '$CertSubject' and trust it in Root + TrustedPublisher"
}
else {
    Invoke-Child -Name 'New-TestCert.ps1' -Arguments @{ Subject = $CertSubject }
    if (-not (Test-CertReady -Subject $CertSubject)) {
        throw "The certificate '$CertSubject' is still not in all three stores after New-TestCert.ps1."
    }
    Write-Done 'certificate created and trusted on this machine'
}

# --------------------------------------------------------------------- 4. sign
Write-Step 'Test-sign the package'
if ($WhatIfOnly) {
    Write-Plan 'sign sonosvad.sys and sonosvad.cat with the test certificate'
}
else {
    # Unconditional: the build re-signs with a WDK cert trusted nowhere, so this
    # has to run after every build, and it is cheap enough not to bother
    # detecting whether it already ran.
    Invoke-Child -Name 'Sign-Driver.ps1' -Arguments @{
        CertSubject   = $CertSubject
        Configuration = $Configuration
        Platform      = $Platform
    }
    Write-Done 'sonosvad.sys and sonosvad.cat signed with your certificate'
}

# ------------------------------------------------------------- 5. test-signing
Write-Step 'Enable test-signing'
$signingState = Get-TestSigningState
switch ($signingState) {
    'active' { Write-Skip 'test-signing is on and this machine has booted with it' }
    'pending-reboot' {
        Stop-WithResume -Reason 'Test-signing is enabled but this machine has not rebooted since. The kernel is still enforcing signature checks, so installing now would fail.' `
            -Next "Reboot, then re-run:`n  $resumeCommand"
    }
    'unknown' {
        Stop-WithResume -Reason 'No BCD store could be read, so test-signing cannot be enabled here. This is normal in Windows Sandbox and some VMs - the driver cannot be loaded on this machine at all.' `
            -Next "Run this on real hardware. Everything up to and including signing has completed, and`n  powershell -File `"$scriptDir\Test-Driver.ps1`"`nstill validates the package statically."
    }
    default {
        if ($WhatIfOnly) {
            Write-Plan 'run bcdedit /set testsigning on, then require a reboot'
        }
        else {
            Invoke-Child -Name 'Enable-TestSigning.ps1' -Arguments @{ NoPrompt = $true }
            if (-not (Test-Path -LiteralPath $localDir)) {
                New-Item -ItemType Directory -Force -Path $localDir | Out-Null
            }
            Set-Content -LiteralPath $rebootMarker -Value (Get-Date).ToString('o') -Encoding ASCII
            Stop-WithResume -Reason 'Test-signing has been enabled. It only takes effect at boot, so the driver cannot be installed until this machine restarts.' `
                -Next "Reboot, then re-run:`n  $resumeCommand"
        }
    }
}

# ----------------------------------------------------------- 6. build services
Write-Step 'Build the user-mode services'
$serviceExe = Join-Path $repoRoot 'target\release\sonosvad-svc.exe'
if ($SkipBuild) {
    Write-Skip 'skipped by -SkipBuild'
}
elseif ($WhatIfOnly) {
    Write-Plan 'cargo build --release -p sonos-vad-service -p sonos-driver-cli'
}
else {
    $cargo = Get-Command 'cargo.exe' -ErrorAction SilentlyContinue
    if (-not $cargo) {
        Write-Warning 'cargo is not on PATH; skipping the service build. Install Rust from https://rustup.rs, or pass -SkipService.'
    }
    else {
        Push-Location $repoRoot
        try {
            $build = Invoke-NativeCommand -Exe $cargo.Source -Arguments @(
                'build', '--release', '-p', 'sonos-vad-service', '-p', 'sonos-driver-cli')
            # cargo writes progress to stderr, so exit code is the only signal.
            if ($build.ExitCode -ne 0) {
                Write-Host $build.Output
                throw "cargo build failed with exit code $($build.ExitCode)."
            }
        }
        finally { Pop-Location }
        Write-Done "built -> $serviceExe"
    }
}

# ------------------------------------------------------------------ 7. install
Write-Step 'Install the driver and create its device node'
if ($WhatIfOnly) {
    $node = Get-SonosVadDeviceNode
    if ($node) { Write-Skip "device node $($node.InstanceId) exists ($($node.Status)); would restage and restart it" }
    else { Write-Plan 'pnputil /add-driver /install, then devgen /add to create ROOT\sonosvad' }
    if (-not $SkipService) { Write-Plan 'register the sonosvad service' }
}
else {
    $installArgs = @{
        Configuration = $Configuration
        Platform      = $Platform
        ServiceExe    = $serviceExe
    }
    if ($SkipService) { $installArgs['SkipService'] = $true }
    if ($SeedHost) { $installArgs['SeedHost'] = $SeedHost }
    Invoke-Child -Name 'Install-Driver.ps1' -Arguments $installArgs
}

# ------------------------------------------------------------------- 8. verify
Write-Step 'Verify the installed package'
if ($WhatIfOnly) {
    Write-Plan 'run Test-Driver.ps1 against the installed package'
}
else {
    Invoke-Child -Name 'Test-Driver.ps1' -Arguments @{ Configuration = $Configuration; Platform = $Platform }
}

# ------------------------------------------------------------------- summary
Write-Host ''
if ($WhatIfOnly) {
    Write-Host '-------------------------------------------------------------' -ForegroundColor Cyan
    if ($script:Actions.Count -eq 0) {
        Write-Host 'Nothing to do - this machine is already set up.' -ForegroundColor Green
    }
    else {
        Write-Host "$($script:Actions.Count) step(s) would run. Re-run without -WhatIfOnly to do it." -ForegroundColor Yellow
    }
    Write-Host '-------------------------------------------------------------' -ForegroundColor Cyan
    return
}

Write-Host '-------------------------------------------------------------' -ForegroundColor Green
Write-Host 'Setup complete.' -ForegroundColor Green
Write-Host ''
Write-Host 'Open mmsys.cpl: your Sonos rooms should be listed as output devices.'
if (-not $SkipService) {
    Write-Host 'Start streaming with:  Start-Service sonosvad'
}
else {
    Write-Host 'Bind the endpoints by hand with:  sonos-driver vad bind'
}
Write-Host ''
Write-Host "Undo everything with:  powershell -File `"$scriptDir\Uninstall-Driver.ps1`""
Write-Host "                       powershell -File `"$scriptDir\Disable-TestSigning.ps1`"  (then reboot)"
Write-Host '-------------------------------------------------------------' -ForegroundColor Green
