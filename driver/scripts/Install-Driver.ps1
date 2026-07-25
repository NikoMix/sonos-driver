#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Installs the test-signed sonosvad driver and creates its device node. See
    docs/DRIVER-SETUP.md and ADR-0018.
.DESCRIPTION
    Two steps are needed, and doing only the first is the classic mistake:

      1. `pnputil /add-driver ... /install` stages the package into the driver
         store. On its own this changes nothing visible, because sonosvad is a
         *root-enumerated* virtual device (`ROOT\sonosvad` in the INF): there is
         no physical hardware for PnP to match the package against.
      2. A device node has to be created for that hardware ID. `devgen /add
         /bus ROOT` (shipped in the WDK, the supported replacement for devcon)
         creates one that persists across reboots; PnP then matches the staged
         package and loads the driver.

    Test-signing must be on (Enable-TestSigning.ps1 + reboot) and the test cert
    trusted (New-TestCert.ps1), or step 1 is rejected.

    After a successful install the endpoints appear in Sound -> Output as
    "Sonos Output 1..N"; `sonos-driver vad bind` renames them to your rooms.
.PARAMETER InfPath
    Path to sonosvad.inf.
.PARAMETER HardwareId
    The root hardware ID declared in the INF.
.PARAMETER InstanceId
    Instance ID for the generated device node, so repeat installs are idempotent
    and Uninstall-Driver.ps1 can find it again.
.PARAMETER SkipDeviceNode
    Stage the package only. Use when the device node already exists and you are
    just updating the binary.
.PARAMETER SkipService
    Do not register the sonosvad streaming service. The endpoints will appear in
    Sound but stay silent until something binds and streams them, which is what
    you want when driving the pipeline by hand with `sonos-driver vad`.
.PARAMETER ServiceExe
    Path to sonosvad-svc.exe. The registered service command line points at this
    exact path, so put the binary where it will stay before installing.
.PARAMETER SeedHost
    IP address of any one Sonos player. Baked into the service command line, for
    networks where SSDP multicast is filtered and discovery would otherwise find
    nothing.
.NOTES
    Not run by CI. Requires an elevated PowerShell.
#>
[CmdletBinding()]
param(
    [string] $InfPath = (Join-Path $PSScriptRoot '..\sonosvad.inf'),
    [string] $HardwareId = 'ROOT\sonosvad',
    [string] $InstanceId = 'sonosvad',
    [switch] $SkipDeviceNode,
    [switch] $SkipService,
    [string] $ServiceExe = (Join-Path $PSScriptRoot '..\..\target\release\sonosvad-svc.exe'),
    [string] $SeedHost
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'DriverCommon.ps1')

Write-Host '== Install sonosvad driver ==' -ForegroundColor Cyan

if (-not (Test-Path -LiteralPath $InfPath)) {
    throw "INF not found: $InfPath"
}
$InfPath = (Resolve-Path -LiteralPath $InfPath).Path

Write-Host "pnputil /add-driver `"$InfPath`" /install"
$add = Invoke-NativeCommand -Exe 'pnputil.exe' -Arguments @('/add-driver', $InfPath, '/install')
Write-Host $add.Output
if ($add.ExitCode -ne 0) {
    throw "pnputil install failed with exit code $($add.ExitCode). Check that test-signing is ON (reboot applied) and the test cert is in Trusted Root + Trusted Publishers."
}

if ($SkipDeviceNode) {
    Write-Host 'Package staged; device node creation skipped as requested.' -ForegroundColor Green
    return
}

$existing = Get-SonosVadDeviceNode
if ($existing) {
    Write-Host "Device node already present: $($existing.InstanceId) ($($existing.Status))."
    Write-Host 'Restarting it so the freshly staged binary is loaded.'
    # A disable/enable cycle re-runs AddDevice + StartDevice against the newly
    # staged package, which is what makes a rebuild take effect without a reboot.
    Disable-PnpDevice -InstanceId $existing.InstanceId -Confirm:$false -ErrorAction SilentlyContinue
    Enable-PnpDevice -InstanceId $existing.InstanceId -Confirm:$false -ErrorAction SilentlyContinue
}
else {
    $devgen = Get-Devgen
    Write-Host "devgen /add /bus ROOT /hardwareid $HardwareId /instanceid $InstanceId"
    $gen = Invoke-NativeCommand -Exe $devgen -Arguments @(
        '/add', '/bus', 'ROOT', '/hardwareid', $HardwareId, '/instanceid', $InstanceId)
    Write-Host $gen.Output
    if ($gen.ExitCode -ne 0) {
        throw "devgen failed with exit code $($gen.ExitCode)."
    }
}

$node = Get-SonosVadDeviceNode
if (-not $node) {
    throw 'The sonosvad device node was not created. Check Device Manager -> View -> Show hidden devices, and C:\Windows\INF\setupapi.dev.log.'
}
if ($node.Status -ne 'OK') {
    Write-Warning "Device node status is '$($node.Status)' (problem $($node.Problem)). The package was staged but the driver did not start; see C:\Windows\INF\setupapi.dev.log."
}
else {
    Write-Host "Device node $($node.InstanceId) started." -ForegroundColor Green
}

# The service is registered only after the device node exists, so its first
# start finds endpoints to bind instead of entering its retry loop.
if (-not $SkipService) {
    Write-Host ''
    Write-Host '-- Registering the streaming service --' -ForegroundColor Cyan

    if (-not (Test-Path -LiteralPath $ServiceExe)) {
        Write-Warning "sonosvad-svc.exe not found at '$ServiceExe'. Build it with 'cargo build --release -p sonos-vad-service' and re-run with -ServiceExe, or register it later. The driver itself is installed."
    }
    else {
        $serviceArgs = @('install')
        if ($SeedHost) { $serviceArgs += @('--seed-host', $SeedHost) }
        $install = Invoke-NativeCommand -Exe $ServiceExe -Arguments $serviceArgs
        Write-Host $install.Output
        if ($install.ExitCode -ne 0) {
            Write-Warning "Service registration failed with exit code $($install.ExitCode). The driver is installed; register the service manually with: $ServiceExe install"
        }
        else {
            Write-Host 'Service sonosvad registered (auto-start, LocalSystem).' -ForegroundColor Green
            Write-Host "Start it now with: Start-Service sonosvad"
        }
    }
}

Write-Host ''
Write-Host 'Open mmsys.cpl to confirm the "Sonos Output N" endpoints appear.' -ForegroundColor Green
Write-Host 'The service renames them to your Sonos rooms once started; to do it'
Write-Host 'by hand instead, run (elevated): sonos-driver vad bind'
