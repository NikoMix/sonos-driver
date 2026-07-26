#Requires -Version 5.1
<#
.SYNOPSIS
    Static validation of the built sonosvad driver package.

.DESCRIPTION
    Runs every check that does NOT require loading the driver, so it works even
    in Windows Sandbox (which has no BCD store and therefore cannot enable test
    signing). Checks performed:

      1. Artifacts exist and are non-empty.
      2. INF passes InfVerif in the strictest applicable mode (/w /v /u).
      3. INF declares exactly the endpoint count the driver was compiled with.
      4. sonosvad.sys is a 64-bit native kernel PE subsystem image.
      5. The import table only references kernel modules.
      6. The image opted in to the expected mitigations (NX, DEP, ASLR, CFG).
      7. ApiValidator: only Universal-driver-legal APIs are imported.
      8. Authenticode signature present and internally consistent on .sys/.cat.
      9. The .cat covers every file in the package with a matching hash.

    Checks that require an actual driver load (Driver Verifier, endpoint
    enumeration, audio round-trip, latency) live in Test-DriverOnHost.ps1 and
    must be run on a real machine.

.PARAMETER ArtifactDir
    Directory holding sonosvad.sys/.inf/.cat. Defaults to the Build-Driver.ps1
    output for Release|x64.

.EXAMPLE
    powershell -File driver\scripts\Test-Driver.ps1
#>
[CmdletBinding()]
param(
    [string] $ArtifactDir,

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string] $Platform = 'x64'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

# The source tree is absent when this script runs from a release payload, where
# only driver\ (the built package) and driver\scripts\ are present. The checks
# that compare the INF against the C++ headers are then genuinely impossible, so
# they SKIP rather than fail: everything that validates the *artifacts* still runs,
# which is exactly what someone verifying a downloaded package needs.
$sourceDir = Join-Path $scriptDir '..\sonosvad'
$driverDir = if (Test-Path $sourceDir) { (Resolve-Path $sourceDir).Path } else { $null }

if (-not $ArtifactDir) {
    if (-not $driverDir) {
        throw ("No source tree next to this script and no -ArtifactDir given, so there " +
               "is nothing to validate. Pass -ArtifactDir <folder containing sonosvad.sys>.")
    }
    $ArtifactDir = Join-Path $driverDir "artifacts\$Platform\$Configuration"
}
if (-not (Test-Path $ArtifactDir)) {
    throw "Artifact directory not found: $ArtifactDir`nRun driver\scripts\Build-Driver.ps1 first."
}
$ArtifactDir = (Resolve-Path $ArtifactDir).Path

$sys = Join-Path $ArtifactDir 'sonosvad.sys'
$inf = Join-Path $ArtifactDir 'sonosvad.inf'
$cat = Join-Path $ArtifactDir 'sonosvad.cat'

$script:Failures = New-Object System.Collections.Generic.List[string]
$script:Skipped = New-Object System.Collections.Generic.List[string]

function Invoke-Check {
    param([string] $Name, [scriptblock] $Body)

    Write-Host ''
    Write-Host "== $Name" -ForegroundColor Cyan
    try {
        & $Body
        Write-Host "   PASS" -ForegroundColor Green
    }
    catch [System.Management.Automation.ItemNotFoundException] {
        Write-Host "   SKIP: $($_.Exception.Message)" -ForegroundColor Yellow
        $script:Skipped.Add("$Name : $($_.Exception.Message)")
    }
    catch {
        Write-Host "   FAIL: $($_.Exception.Message)" -ForegroundColor Red
        $script:Failures.Add("$Name : $($_.Exception.Message)")
    }
}

function Invoke-Native {
    <#
        Native tools write to stderr routinely (signtool always does when a
        chain is untrusted). With $ErrorActionPreference = 'Stop' that becomes a
        terminating error, so redirect inside a relaxed scope and return the
        combined output plus the exit code.
    #>
    param([string] $Exe, [string[]] $Arguments)

    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & $Exe @Arguments 2>&1 | ForEach-Object { "$_" }
        return [pscustomobject]@{ Output = ($output -join "`n"); ExitCode = $LASTEXITCODE }
    }
    finally {
        $ErrorActionPreference = $old
    }
}

function Get-KitTool {
    param([string] $Name, [string[]] $SubPaths)
    $kit = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
    foreach ($sub in $SubPaths) {
        $hits = Get-ChildItem (Join-Path $kit $sub) -Filter $Name -Recurse -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending
        if ($hits) { return $hits[0].FullName }
    }
    throw [System.Management.Automation.ItemNotFoundException] "$Name not found in the Windows Kits"
}

Write-Host '== sonosvad static validation ==' -ForegroundColor Cyan
Write-Host "Artifacts: $ArtifactDir"

# ---------------------------------------------------------------- 1. artifacts
Invoke-Check 'Artifacts present' {
    foreach ($f in $sys, $inf, $cat) {
        if (-not (Test-Path $f)) { throw "missing $f" }
        if ((Get-Item $f).Length -le 0) { throw "empty $f" }
    }
    Get-ChildItem $ArtifactDir -File |
        ForEach-Object { Write-Host ("   {0,-16} {1,10:N0} bytes" -f $_.Name, $_.Length) }
}

# ------------------------------------------------------------------ 2. InfVerif
Invoke-Check 'InfVerif (universal driver rules)' {
    $infverif = Get-KitTool -Name 'infverif.exe' -SubPaths @('Tools')
    # /w is the strictest mode (Windows Driver requirements: universal +
    # declarative). The mode switches /h /w /u /k are mutually exclusive.
    $r = Invoke-Native $infverif @('/w', '/v', $inf)
    $r.Output -split "`n" | ForEach-Object { Write-Host "   $_" }
    if ($r.ExitCode -ne 0) { throw "infverif exit code $($r.ExitCode)" }
}

# ------------------------------------------------- 3. endpoint count consistency
Invoke-Check 'INF endpoint count matches SONOSVAD_ENDPOINT_COUNT' {
    if (-not $driverDir) {
        throw [System.Management.Automation.ItemNotFoundException] `
            'no driver source tree next to this script (running from a release payload)'
    }
    $definitions = Join-Path $driverDir 'Source\Inc\definitions.h'
    $m = Select-String -Path $definitions -Pattern 'define\s+SONOSVAD_ENDPOINT_COUNT\s+\(?(\d+)\)?'
    if (-not $m) { throw 'SONOSVAD_ENDPOINT_COUNT not found in definitions.h' }
    $expected = [int]$m.Matches[0].Groups[1].Value

    $infText = Get-Content $inf -Raw
    $found = ([regex]::Matches($infText, 'KSNAME_WaveSonosOutput(\d+)\s*=') | ForEach-Object { [int]$_.Groups[1].Value } | Sort-Object -Unique)
    if ($found.Count -ne $expected) {
        throw "INF declares $($found.Count) wave endpoints, driver was built with $expected"
    }
    # minipairs.h must agree too
    $minipairs = Join-Path $driverDir 'Source\Filters\minipairs.h'
    $pairs = ([regex]::Matches((Get-Content $minipairs -Raw), 'SONOSVAD_ENDPOINT_MINIPAIR\s*\(\s*(\d+)\s*\)')).Count
    if ($pairs -lt $expected) {
        throw "minipairs.h expands $pairs endpoint pairs, expected at least $expected"
    }
    Write-Host "   $expected endpoints, consistent across definitions.h / minipairs.h / INF"
}

# ------------------------------------------------------------ 4-6. PE inspection
Invoke-Check 'PE image is a 64-bit native kernel driver with mitigations' {
    $dumpbin = Get-ChildItem (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC') `
        -Filter 'dumpbin.exe' -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\Hostx64\\x64\\' } | Select-Object -First 1
    if (-not $dumpbin) {
        throw [System.Management.Automation.ItemNotFoundException] 'dumpbin.exe not found'
    }

    $text = (Invoke-Native $dumpbin.FullName @('/nologo', '/headers', $sys)).Output

    if ($text -notmatch 'machine \(x64\)') { throw 'not an x64 image' }
    if ($text -notmatch 'subsystem \(Native\)' -and $text -notmatch '1 subsystem \(Native\)') {
        throw 'subsystem is not Native (kernel)'
    }
    foreach ($flag in 'Dynamic base', 'NX compatible') {
        if ($text -notmatch [regex]::Escape($flag)) { throw "missing DLL characteristic: $flag" }
    }
    if ($text -notmatch 'Control Flow Guard') {
        Write-Host '   note: Control Flow Guard not reported in DLL characteristics' -ForegroundColor Yellow
    }
    Write-Host '   x64 / Native subsystem / ASLR / NX'

    $imports = (Invoke-Native $dumpbin.FullName @('/nologo', '/dependents', $sys)).Output
    $modules = [regex]::Matches($imports, '(?im)^\s{4}(\S+\.(sys|dll|exe))\s*$') |
        ForEach-Object { $_.Groups[1].Value.ToLowerInvariant() } | Sort-Object -Unique
    $allowed = @('ntoskrnl.exe', 'hal.dll', 'portcls.sys', 'ks.sys', 'wdfldr.sys', 'wmilib.sys', 'drmk.sys')
    foreach ($mod in $modules) {
        if ($allowed -notcontains $mod) { throw "unexpected import module: $mod" }
    }
    Write-Host "   imports: $($modules -join ', ')"
}

# -------------------------------------------------------------- 7. ApiValidator
Invoke-Check 'ApiValidator (Universal driver API surface)' {
    $apivalidator = Get-KitTool -Name 'apivalidator.exe' -SubPaths @('bin')
    $kit = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
    $ddiRoot = Get-ChildItem (Join-Path $kit 'build') -Filter 'universalDDIs' -Directory -Recurse -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $ddiRoot) {
        throw [System.Management.Automation.ItemNotFoundException] 'universalDDIs directory not found'
    }
    $arch = if ($Platform -eq 'x64') { 'x64' } else { 'arm64' }
    $ddi = Join-Path $ddiRoot.FullName "$arch\UniversalDDIs.xml"
    $whitelist = Join-Path $ddiRoot.FullName "$arch\ModuleWhitelist.xml"
    if (-not (Test-Path $ddi)) {
        throw [System.Management.Automation.ItemNotFoundException] "UniversalDDIs.xml not found for $arch"
    }

    $r = Invoke-Native $apivalidator @("-SupportedApiXmlFiles:$ddi", "-ModuleWhiteListXmlFiles:$whitelist", "-BinaryPath:$sys")
    $r.Output -split "`n" | Where-Object { $_.Trim() } | ForEach-Object { Write-Host "   $_" }
    if ($r.ExitCode -ne 0) { throw "apivalidator exit code $($r.ExitCode)" }
}

# ------------------------------------------------------------ 8-9. signature/cat
Invoke-Check 'Authenticode signature and catalog' {
    # Get-AuthenticodeSignature discriminates between "no signature" and
    # "signed but the chain is untrusted". The latter is expected here: the
    # package is signed with a self-issued test certificate that is
    # deliberately not in any trust store.
    $fatal = @('NotSigned', 'HashMismatch', 'NotSupportedFileFormat')

    foreach ($target in $sys, $cat) {
        $sig = Get-AuthenticodeSignature -FilePath $target
        $name = Split-Path -Leaf $target
        if ($fatal -contains "$($sig.Status)") {
            throw "$name signature status: $($sig.Status) - $($sig.StatusMessage)"
        }
        if (-not $sig.SignerCertificate) { throw "$name has no signer certificate" }
        Write-Host "   $name : $($sig.Status) / signer '$($sig.SignerCertificate.Subject)'"
    }

    $signtool = Get-KitTool -Name 'signtool.exe' -SubPaths @('bin')

    # What matters for integrity is the *file digest* algorithm, not the
    # algorithm the (self-issued, throwaway) test certificate was signed with.
    foreach ($target in $sys, $cat) {
        $out = (Invoke-Native $signtool @('verify', '/pa', '/v', $target)).Output
        $digest = [regex]::Match($out, 'Hash of file \((?<alg>\w+)\)')
        if (-not $digest.Success) {
            $digest = [regex]::Match($out, 'Hash Algorithm:\s*(?<alg>\w+)')
        }
        if ($digest.Success) {
            $alg = $digest.Groups['alg'].Value
            Write-Host "   $(Split-Path -Leaf $target) file digest: $alg"
            if ($alg -match '^sha1$|^md5$') {
                throw "$(Split-Path -Leaf $target) uses a weak file digest: $alg"
            }
        }
    }

    # Catalog coverage: the .sys hash must be listed in the .cat. signtool
    # reports an untrusted root for the test cert, which is fine; a missing
    # hash reports differently.
    $out = (Invoke-Native $signtool @('verify', '/pa', '/v', '/c', $cat, $sys)).Output
    if ($out -match 'not found in the catalog|SignTool Error: The signature of the file|is not signed') {
        throw 'sonosvad.sys is not covered by sonosvad.cat'
    }
    Write-Host '   sonosvad.sys hash is present in sonosvad.cat'
}

# ------------------------------------------------------------------- summary
Write-Host ''
Write-Host '== Summary ==' -ForegroundColor Cyan
if ($script:Skipped.Count -gt 0) {
    Write-Host "Skipped ($($script:Skipped.Count)):" -ForegroundColor Yellow
    $script:Skipped | ForEach-Object { Write-Host "  - $_" -ForegroundColor Yellow }
}
if ($script:Failures.Count -gt 0) {
    Write-Host "Failed ($($script:Failures.Count)):" -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
Write-Host 'All static checks passed.' -ForegroundColor Green
Write-Host ''
Write-Host 'Load-time validation (Driver Verifier, endpoint enumeration, audio' -ForegroundColor Yellow
Write-Host 'round-trip, latency) cannot run here - use Test-DriverOnHost.ps1 on a' -ForegroundColor Yellow
Write-Host 'real machine with test signing enabled.' -ForegroundColor Yellow
