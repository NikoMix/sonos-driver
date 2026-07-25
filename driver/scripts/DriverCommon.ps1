#Requires -Version 5.1
<#
.SYNOPSIS
    Helpers shared by the sonosvad driver scripts. Dot-source, do not run.

.DESCRIPTION
    Kept in one place so Install/Uninstall/Test-DriverOnHost agree on how the
    device node is found, how kit tools are located, and how native command
    output is captured.
#>

Set-StrictMode -Version Latest

function Invoke-NativeCommand {
    <#
    .SYNOPSIS
        Runs a native executable and returns its combined output and exit code.
    .DESCRIPTION
        Native tools write to stderr routinely - signtool always does when a
        certificate chain is untrusted, pnputil does for benign warnings. With
        $ErrorActionPreference = 'Stop' PowerShell turns each of those lines
        into a terminating NativeCommandError, so the redirect has to happen
        inside a relaxed scope and success has to be judged by exit code alone.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string] $Exe,
        [string[]] $Arguments = @()
    )

    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & $Exe @Arguments 2>&1 | ForEach-Object { "$_" }
        return [pscustomobject]@{
            Output   = ($output -join "`n")
            ExitCode = $LASTEXITCODE
        }
    }
    finally {
        $ErrorActionPreference = $old
    }
}

function Get-KitTool {
    <#
    .SYNOPSIS
        Finds the newest copy of a Windows Kits tool by name.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string] $Name,
        [string[]] $SubPaths = @('Tools', 'bin')
    )

    $kit = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
    foreach ($sub in $SubPaths) {
        $root = Join-Path $kit $sub
        if (-not (Test-Path $root)) { continue }
        $hits = Get-ChildItem $root -Filter $Name -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\(x64|amd64)\\' -or $_.Directory.Name -eq 'x64' } |
            Sort-Object FullName -Descending
        if (-not $hits) {
            $hits = Get-ChildItem $root -Filter $Name -Recurse -ErrorAction SilentlyContinue |
                Sort-Object FullName -Descending
        }
        if ($hits) { return $hits[0].FullName }
    }
    throw "$Name was not found under $kit. Install the Windows Driver Kit (see docs/DRIVER-SETUP.md)."
}

function Get-Devgen {
    <#
    .SYNOPSIS
        Path to devgen.exe, the WDK tool that creates root-enumerated devices.
    .DESCRIPTION
        devgen is the supported replacement for devcon. sonosvad declares
        `ROOT\sonosvad` in its INF, so a device node must be generated for the
        staged package to bind to anything.
    #>
    [CmdletBinding()]
    param()
    Get-KitTool -Name 'devgen.exe' -SubPaths @('Tools')
}

function Get-SonosVadDeviceNode {
    <#
    .SYNOPSIS
        The sonosvad PnP device node, or $null when it does not exist.
    .DESCRIPTION
        Matches on the instance-id prefix rather than the friendly name, because
        the node keeps its identity even when the driver fails to start and has
        no name yet. Hidden/non-present devices are included so a half-removed
        node is still reported.
    #>
    [CmdletBinding()]
    param()

    Get-PnpDevice -PresentOnly:$false -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like 'ROOT\MEDIA\*' -or $_.InstanceId -like 'ROOT\SONOSVAD\*' } |
        Where-Object {
            $hwids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds' -ErrorAction SilentlyContinue).Data
            $hwids -and ($hwids -join ';') -match 'sonosvad'
        } |
        Select-Object -First 1
}

function Get-SonosVadOemInf {
    <#
    .SYNOPSIS
        The published OEM INF name (oemNN.inf) of the staged sonosvad package.
    #>
    [CmdletBinding()]
    param()

    $enum = (Invoke-NativeCommand -Exe 'pnputil.exe' -Arguments @('/enum-drivers')).Output -split "`n"
    $current = $null
    foreach ($line in $enum) {
        if ($line -match 'Published Name\s*:\s*(oem\d+\.inf)') { $current = $Matches[1] }
        if ($current -and $line -match 'sonosvad|Sonos Virtual Audio') { return $current }
    }
    return $null
}
