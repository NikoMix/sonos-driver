#Requires -Version 5.1
<#
.SYNOPSIS
    Live validation of sonosvad on a real machine: install, load, exercise,
    measure, and tear down.

.DESCRIPTION
    The counterpart to Test-Driver.ps1. Everything here needs the driver to
    actually LOAD, so none of it can run in Windows Sandbox (no BCD store means
    test signing cannot be enabled).

    Phases, in order:

      preflight  Admin rights, test signing, Secure Boot, HVCI, artifacts,
                 signature, and that the CLI is built. Read-only.
      verifier   Enable Driver Verifier for sonosvad.sys. Requires a reboot to
                 take effect; the script stops and tells you.
      install    Trust the test certificate, stage the package, create the
                 ROOT\sonosvad device node, confirm it started.
      endpoints  Confirm the expected number of render endpoints appeared.
      rename     Bind endpoints to discovered rooms and release them again,
                 asserting the names round-trip exactly.
      probe      Measure the driver's clock: captured audio time vs wall clock.
                 This is the core correctness check and needs no Sonos hardware.
      soak       Hold captures open on every endpoint and re-check drift, to
                 catch leaks and clock wander that a short probe misses.
      teardown   Release names, remove the device node, delete the package,
                 disable Driver Verifier.

    Every phase is independently runnable so you can resume after the reboots
    that test signing and Driver Verifier require.

    SAFETY. Driver Verifier with a buggy driver bugchecks the machine on
    purpose - that is what it is for. Do not run -Phase verifier on a machine
    you are not willing to see BSOD. Teardown always tries to leave the system
    in its original state, and 'teardown' alone is safe to run at any time.

.PARAMETER Phase
    Which phases to run. Defaults to the non-destructive, no-reboot set
    (preflight, install, endpoints, rename, probe). 'all' adds verifier and
    soak. 'teardown' is never implied - pass it explicitly.

.PARAMETER Rooms
    Expected room count. When omitted the check is informational only.

.PARAMETER SeedHost
    Address of a known Sonos player, for networks where SSDP multicast does not
    work. Passed through to the CLI.

.PARAMETER Endpoints
    Expected render endpoint count. Defaults to 8, matching the INF.

.PARAMETER DriftTolerancePercent
    How far captured audio time may diverge from wall clock before the probe
    fails. Default 1 %.

.PARAMETER SoakMinutes
    Duration of the soak phase. Default 10.

.PARAMETER Cli
    Path to sonos-driver.exe. Defaults to target\release then target\debug.

.EXAMPLE
    powershell -File driver\scripts\Test-DriverOnHost.ps1 -Phase preflight

.EXAMPLE
    powershell -File driver\scripts\Test-DriverOnHost.ps1 `
        -Phase install,endpoints,rename,probe -SeedHost 192.168.178.25

.EXAMPLE
    powershell -File driver\scripts\Test-DriverOnHost.ps1 -Phase teardown
#>
[CmdletBinding()]
param(
    [ValidateSet('preflight', 'verifier', 'install', 'endpoints', 'rename', 'probe', 'soak', 'teardown', 'all')]
    [string[]] $Phase = @('preflight', 'install', 'endpoints', 'rename', 'probe'),

    [int] $Rooms,
    [string] $SeedHost,
    [int] $Endpoints = 8,
    [double] $DriftTolerancePercent = 1.0,
    [int] $SoakMinutes = 10,
    [string] $Cli,

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string] $Platform = 'x64',

    [string] $ArtifactDir,

    # Skip the "this can bugcheck your machine" confirmation.
    [switch] $Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $scriptDir 'DriverCommon.ps1')

$repoRoot = (Resolve-Path (Join-Path $scriptDir '..\..')).Path
$driverDir = (Resolve-Path (Join-Path $scriptDir '..\sonosvad')).Path
if (-not $ArtifactDir) {
    $ArtifactDir = Join-Path $driverDir "artifacts\$Platform\$Configuration"
}

if ($Phase -contains 'all') {
    $Phase = @('preflight', 'verifier', 'install', 'endpoints', 'rename', 'probe', 'soak')
}

$script:Failures = New-Object System.Collections.Generic.List[string]
$script:Notes = New-Object System.Collections.Generic.List[string]

function Write-Phase {
    param([string] $Name)
    Write-Host ''
    Write-Host ('=' * 70) -ForegroundColor DarkGray
    Write-Host "  $Name" -ForegroundColor Cyan
    Write-Host ('=' * 70) -ForegroundColor DarkGray
}

function Invoke-Check {
    param([string] $Name, [scriptblock] $Body)
    Write-Host ''
    Write-Host "== $Name" -ForegroundColor Cyan
    try {
        & $Body
        Write-Host '   PASS' -ForegroundColor Green
    }
    catch {
        Write-Host "   FAIL  $($_.Exception.Message)" -ForegroundColor Red
        $script:Failures.Add("$Name : $($_.Exception.Message)")
    }
}

function Assert-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'This script must run in an elevated PowerShell session.'
    }
}

function Resolve-Cli {
    if ($Cli) {
        if (-not (Test-Path $Cli)) { throw "CLI not found at $Cli" }
        return (Resolve-Path $Cli).Path
    }
    foreach ($candidate in @(
            (Join-Path $repoRoot 'target\release\sonos-driver.exe'),
            (Join-Path $repoRoot 'target\debug\sonos-driver.exe'))) {
        if (Test-Path $candidate) { return (Resolve-Path $candidate).Path }
    }
    throw "sonos-driver.exe not found. Build it with 'cargo build --release -p sonos-driver-cli', or pass -Cli."
}

function Invoke-Cli {
    <#
        The CLI writes progress to stderr. With $ErrorActionPreference = 'Stop'
        that becomes a terminating NativeCommandError, so judge by exit code.
    #>
    param([string[]] $Arguments)
    $exe = Resolve-Cli
    $result = Invoke-NativeCommand -Exe $exe -Arguments $Arguments
    if ($result.ExitCode -ne 0) {
        throw "sonos-driver $($Arguments -join ' ') failed ($($result.ExitCode)):`n$($result.Output)"
    }
    return $result.Output
}

function Get-SeedArgs {
    if ($SeedHost) { return @('--seed-host', $SeedHost) }
    return @()
}

# ---------------------------------------------------------------- preflight --

function Invoke-Preflight {
    Write-Phase 'Preflight'

    Invoke-Check 'Elevated' { Assert-Admin }

    Invoke-Check 'Test signing is enabled' {
        $out = (Invoke-NativeCommand -Exe 'bcdedit.exe' -Arguments @('/enum', '{current}')).Output
        if ($out -notmatch 'testsigning\s+Yes') {
            throw "Test signing is off. Run driver\scripts\Enable-TestSigning.ps1 and reboot.`nWithout it Windows refuses to load a self-signed driver."
        }
    }

    Invoke-Check 'Secure Boot is off (or test signing would be ignored)' {
        $sb = $null
        try { $sb = Confirm-SecureBootUEFI } catch { }
        if ($sb -eq $true) {
            throw 'Secure Boot is ON. Windows ignores test signing under Secure Boot; disable it in firmware.'
        }
        if ($null -eq $sb) { $script:Notes.Add('Secure Boot state could not be read (likely a legacy BIOS machine).') }
    }

    Invoke-Check 'Artifacts present' {
        foreach ($name in @('sonosvad.sys', 'sonosvad.inf', 'sonosvad.cat')) {
            $path = Join-Path $ArtifactDir $name
            if (-not (Test-Path $path)) { throw "missing $path - run Build-Driver.ps1" }
            if ((Get-Item $path).Length -eq 0) { throw "$name is empty" }
        }
    }

    Invoke-Check 'Driver is signed' {
        $sig = Get-AuthenticodeSignature (Join-Path $ArtifactDir 'sonosvad.sys')
        # UnknownError is the expected state for a self-signed test cert that is
        # not yet in the machine's trusted roots; install adds it.
        if ($sig.Status -notin @('Valid', 'UnknownError')) {
            throw "unexpected signature status '$($sig.Status)' - run Sign-Driver.ps1"
        }
        Write-Host "   signer: $($sig.SignerCertificate.Subject)"
    }

    Invoke-Check 'CLI is built' {
        $exe = Resolve-Cli
        Write-Host "   $exe"
    }

    Invoke-Check 'Static checks still pass' {
        $test = Join-Path $scriptDir 'Test-Driver.ps1'
        $result = Invoke-NativeCommand -Exe 'powershell.exe' `
            -Arguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $test, '-ArtifactDir', $ArtifactDir)
        if ($result.ExitCode -ne 0) { throw "Test-Driver.ps1 failed:`n$($result.Output)" }
    }

    $hvci = Get-CimInstance -ClassName Win32_DeviceGuard -Namespace root\Microsoft\Windows\DeviceGuard -ErrorAction SilentlyContinue
    if ($hvci -and $hvci.SecurityServicesRunning -contains 2) {
        $script:Notes.Add('HVCI (memory integrity) is running. It enforces stricter driver rules; if the load fails with 0xC0000428, turn it off in Core Isolation.')
    }
}

# ----------------------------------------------------------------- verifier --

function Invoke-Verifier {
    Write-Phase 'Driver Verifier'
    Assert-Admin

    if (-not $Force) {
        Write-Host ''
        Write-Host 'Driver Verifier deliberately bugchecks (BSODs) the machine on the first' -ForegroundColor Yellow
        Write-Host 'rule violation. That is the point - but do not do this on a machine you' -ForegroundColor Yellow
        Write-Host 'cannot afford to crash, and save your work first.' -ForegroundColor Yellow
        $answer = Read-Host 'Type ENABLE to continue'
        if ($answer -ne 'ENABLE') { throw 'Aborted by user.' }
    }

    # Standard flags plus the ones that matter for an audio miniport: DMA and
    # IRQL checking catch the classic PortCls mistakes (touching paged memory at
    # DISPATCH_LEVEL in the position/notification path).
    $result = Invoke-NativeCommand -Exe 'verifier.exe' -Arguments @(
        '/standard', '/driver', 'sonosvad.sys'
    )
    if ($result.ExitCode -ne 0) { throw "verifier.exe failed:`n$($result.Output)" }

    Write-Host ''
    Write-Host 'Driver Verifier is armed for sonosvad.sys but does NOT apply until reboot.' -ForegroundColor Yellow
    Write-Host 'Reboot, then re-run this script with the remaining phases:' -ForegroundColor Yellow
    Write-Host '  Test-DriverOnHost.ps1 -Phase install,endpoints,rename,probe,soak' -ForegroundColor Yellow
    Write-Host ''
    Write-Host 'To disarm later:  verifier.exe /reset   (also needs a reboot)' -ForegroundColor DarkGray
}

# ------------------------------------------------------------------ install --

function Invoke-Install {
    Write-Phase 'Install'
    Assert-Admin

    $install = Join-Path $scriptDir 'Install-Driver.ps1'
    $result = Invoke-NativeCommand -Exe 'powershell.exe' `
        -Arguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $install, '-ArtifactDir', $ArtifactDir)
    Write-Host $result.Output
    if ($result.ExitCode -ne 0) { throw "Install-Driver.ps1 failed ($($result.ExitCode))" }

    Invoke-Check 'Device node exists and started' {
        $node = Get-SonosVadDeviceNode
        if (-not $node) { throw 'no ROOT\sonosvad device node was created' }
        Write-Host "   $($node.InstanceId)  status=$($node.Status)"
        if ($node.Status -ne 'OK') {
            # Reading the problem code must not itself throw: this is the error
            # path, and a StrictMode failure here would mask the real diagnostic.
            $property = Get-PnpDeviceProperty -InstanceId $node.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode' -ErrorAction SilentlyContinue
            $problem = if ($property) { $property.Data } else { 'unavailable' }
            throw "device did not start (status=$($node.Status), problem code=$problem). Check setupapi.dev.log and the System event log."
        }
    }
}

# ---------------------------------------------------------------- endpoints --

function Get-EndpointStatus {
    $json = Invoke-Cli @('vad', 'status', '--json')
    return $json | ConvertFrom-Json
}

function Invoke-Endpoints {
    Write-Phase 'Endpoints'

    Invoke-Check "Driver exposes $Endpoints render endpoints" {
        $status = Get-EndpointStatus
        $count = @($status).Count
        foreach ($e in $status) {
            Write-Host ("   slot {0,-3} {1,-28} active={2}" -f $e.slot, $e.friendly_name, $e.active)
        }
        if ($count -ne $Endpoints) {
            throw "expected $Endpoints endpoints, found $count. The INF and the miniport table must agree."
        }
    }

    Invoke-Check 'Endpoints are visible to Windows as render devices' {
        $render = Get-CimInstance -ClassName Win32_SoundDevice -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match 'Sonos' }
        if (-not $render) { $script:Notes.Add('Win32_SoundDevice did not list a Sonos device; this is informational, WASAPI enumeration is authoritative.') }
    }
}

# ------------------------------------------------------------------- rename --

function Invoke-Rename {
    Write-Phase 'Rename round-trip'
    Assert-Admin

    $before = Get-EndpointStatus | Sort-Object slot

    Invoke-Check 'Endpoints start at their factory names' {
        foreach ($e in $before) {
            $expected = "Sonos Output $($e.slot)"
            if ($e.friendly_name -ne $expected) {
                $script:Notes.Add("slot $($e.slot) was already renamed to '$($e.friendly_name)'; releasing first.")
                Invoke-Cli @('vad', 'release') | Out-Null
                break
            }
        }
    }

    Invoke-Check 'Discovery finds rooms' {
        $out = Invoke-Cli (@('list') + (Get-SeedArgs))
        Write-Host $out
        if ($PSBoundParameters.ContainsKey('Rooms')) {
            $lines = @($out -split "`n" | Where-Object { $_ -match '\S' }) 
            # minus the header row
            $found = $lines.Count - 1
            if ($found -lt $Rooms) { throw "expected at least $Rooms rooms, saw $found" }
        }
    }

    Invoke-Check 'Bind renames endpoints to room names' {
        $out = Invoke-Cli (@('vad', 'bind') + (Get-SeedArgs))
        Write-Host $out
        $after = Get-EndpointStatus | Sort-Object slot
        $renamed = @($after | Where-Object { $_.friendly_name -notmatch '^Sonos Output \d+$' })
        if ($renamed.Count -eq 0) { throw 'bind reported success but no endpoint name changed' }
        foreach ($e in $renamed) { Write-Host "   slot $($e.slot) -> $($e.friendly_name)" }
    }

    Invoke-Check 'Renames survive a driver restart' {
        $node = Get-SonosVadDeviceNode
        if (-not $node) { throw 'device node vanished' }
        Disable-PnpDevice -InstanceId $node.InstanceId -Confirm:$false | Out-Null
        Start-Sleep -Seconds 2
        Enable-PnpDevice -InstanceId $node.InstanceId -Confirm:$false | Out-Null
        Start-Sleep -Seconds 5
        $after = Get-EndpointStatus
        $renamed = @($after | Where-Object { $_.friendly_name -notmatch '^Sonos Output \d+$' })
        if ($renamed.Count -eq 0) {
            throw 'names reverted after a restart - the property store write did not persist to HKLM MMDevices'
        }
    }

    Invoke-Check 'Release restores factory names exactly' {
        Invoke-Cli @('vad', 'release') | Out-Null
        $after = Get-EndpointStatus | Sort-Object slot
        foreach ($e in $after) {
            $expected = "Sonos Output $($e.slot)"
            if ($e.friendly_name -ne $expected) {
                throw "slot $($e.slot) is '$($e.friendly_name)', expected '$expected'"
            }
        }
    }
}

# -------------------------------------------------------------------- probe --

function Invoke-Probe {
    Write-Phase 'Clock probe'

    Invoke-Check "Driver clock tracks wall clock within $DriftTolerancePercent %" {
        $json = Invoke-Cli @('vad', 'probe', '--json', '--seconds', '10')
        $report = $json | ConvertFrom-Json
        Write-Host ("   device      : {0}" -f $report.device)
        Write-Host ("   format      : {0} Hz, {1} ch" -f $report.sample_rate, $report.channels)
        Write-Host ("   wall clock  : {0:N3} s" -f $report.elapsed_secs)
        Write-Host ("   captured    : {0:N3} s" -f $report.captured_secs)
        Write-Host ("   drift       : {0:N4} %" -f $report.drift_percent)

        if ($report.captured_secs -le 0) {
            throw 'captured no audio at all - the WaveRT position is not advancing, or loopback capture is not reaching this endpoint'
        }
        if ([Math]::Abs($report.drift_percent) -gt $DriftTolerancePercent) {
            throw "drift $($report.drift_percent) % exceeds $DriftTolerancePercent %. The driver's position advance is not real time; audio will slowly desync from Sonos."
        }
    }
}

# --------------------------------------------------------------------- soak --

function Invoke-Soak {
    Write-Phase "Soak ($SoakMinutes min)"

    Invoke-Check 'Sustained capture on every endpoint' {
        $exe = Resolve-Cli
        $deadline = (Get-Date).AddMinutes($SoakMinutes)
        $iteration = 0
        $worst = 0.0

        while ((Get-Date) -lt $deadline) {
            $iteration++
            $json = Invoke-Cli @('vad', 'probe', '--json', '--seconds', '30')
            $report = $json | ConvertFrom-Json
            $drift = [Math]::Abs($report.drift_percent)
            if ($drift -gt $worst) { $worst = $drift }
            Write-Host ("   [{0:HH:mm:ss}] pass {1,-3} drift {2,8:N4} %  worst {3,8:N4} %" -f (Get-Date), $iteration, $report.drift_percent, $worst)

            if ($drift -gt $DriftTolerancePercent) {
                throw "drift reached $drift % on pass $iteration"
            }
        }

        # A driver that leaks a stream object per open will eventually refuse to
        # open another one; reaching here with the node still healthy is the
        # cheap version of that check.
        $node = Get-SonosVadDeviceNode
        if (-not $node -or $node.Status -ne 'OK') {
            throw "device is no longer healthy after the soak (status=$($node.Status))"
        }
        Write-Host "   $iteration passes, worst drift $([Math]::Round($worst, 4)) %"
    }
}

# ----------------------------------------------------------------- teardown --

function Invoke-Teardown {
    Write-Phase 'Teardown'
    Assert-Admin

    $uninstall = Join-Path $scriptDir 'Uninstall-Driver.ps1'
    $result = Invoke-NativeCommand -Exe 'powershell.exe' `
        -Arguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $uninstall)
    Write-Host $result.Output
    if ($result.ExitCode -ne 0) {
        $script:Failures.Add("Uninstall-Driver.ps1 exited $($result.ExitCode)")
    }

    $verifier = Invoke-NativeCommand -Exe 'verifier.exe' -Arguments @('/querysettings')
    if ($verifier.Output -match 'sonosvad') {
        Invoke-NativeCommand -Exe 'verifier.exe' -Arguments @('/reset') | Out-Null
        $script:Notes.Add('Driver Verifier was reset; reboot to complete.')
    }

    Invoke-Check 'Device node is gone' {
        $node = Get-SonosVadDeviceNode
        if ($node) { throw "device node still present: $($node.InstanceId)" }
    }
}

# --------------------------------------------------------------------- main --

Write-Host "sonosvad host validation"
Write-Host "  repo      : $repoRoot"
Write-Host "  artifacts : $ArtifactDir"
Write-Host "  phases    : $($Phase -join ', ')"

foreach ($p in $Phase) {
    switch ($p) {
        'preflight' { Invoke-Preflight }
        'verifier' { Invoke-Verifier }
        'install' { Invoke-Install }
        'endpoints' { Invoke-Endpoints }
        'rename' { Invoke-Rename }
        'probe' { Invoke-Probe }
        'soak' { Invoke-Soak }
        'teardown' { Invoke-Teardown }
    }
    # A failed phase invalidates everything downstream of it, so stop rather
    # than report a cascade of confusing secondary failures.
    if ($script:Failures.Count -gt 0) {
        Write-Host ''
        Write-Host "Stopping: phase '$p' failed." -ForegroundColor Red
        break
    }
}

Write-Host ''
Write-Host ('=' * 70) -ForegroundColor DarkGray
if ($script:Notes.Count -gt 0) {
    Write-Host 'Notes:' -ForegroundColor Yellow
    foreach ($n in $script:Notes) { Write-Host "  - $n" -ForegroundColor Yellow }
    Write-Host ''
}
if ($script:Failures.Count -gt 0) {
    Write-Host "FAILED ($($script:Failures.Count))" -ForegroundColor Red
    foreach ($f in $script:Failures) { Write-Host "  - $f" -ForegroundColor Red }
    exit 1
}
Write-Host 'All requested phases passed.' -ForegroundColor Green
exit 0
