<#
.SYNOPSIS
    Generates driver/sonosvad/Source/Main/sonosvad.inx.

.DESCRIPTION
    sonosvad exposes a fixed number of identical virtual render endpoints. Each
    endpoint needs its own KS reference strings, interface sections and
    AddInterface directives in the INF. Rather than hand-maintaining ~10 lines
    per endpoint, this script generates the whole .inx from EndpointCount.

    The value passed here MUST match SONOSVAD_ENDPOINT_COUNT in
    Source/Inc/definitions.h and the SONOSVAD_ENDPOINT_MINIPAIR list in
    Source/Filters/minipairs.h. sonosvad.vcxproj runs this script as a
    pre-build step so the three can never drift.

    The endpoints are interchangeable; which one becomes a room and which one
    becomes the "All Sonos" aggregate is decided at bind time by the user-mode
    service, not here.

.NOTES
    INF/INX files consumed by stampinf must be UTF-16LE (or ASCII). This script
    writes UTF-16LE with a BOM, matching the WDK samples.
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 32)]
    [int]$EndpointCount = 9,

    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

if (-not $OutputPath) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $OutputPath = Join-Path $scriptDir '..\Source\Main\sonosvad.inx'
}

$sb = [System.Text.StringBuilder]::new()
function Add-Line([string]$Text = '') { [void]$sb.AppendLine($Text) }

Add-Line ';'
Add-Line '; sonosvad.inx - Sonos Virtual Audio Device'
Add-Line ';'
Add-Line '; GENERATED FILE - do not edit by hand.'
Add-Line "; Regenerate with: tools\Generate-Inx.ps1 -EndpointCount $EndpointCount"
Add-Line ';'
Add-Line '; Copyright (c) Microsoft Corporation. All rights reserved.'
Add-Line '; Copyright (c) NikoMix and the sonos-driver contributors.'
Add-Line '; Licensed under the Microsoft Public License (MS-PL).'
Add-Line '; Derived from the Windows-driver-samples "simpleaudiosample" sample.'
Add-Line ';'
Add-Line ''
Add-Line '[Version]'
Add-Line 'Signature   = "$Windows NT$"'
Add-Line 'Class       = MEDIA'
Add-Line 'Provider    = %ProviderName%'
Add-Line 'ClassGUID   = {4d36e96c-e325-11ce-bfc1-08002be10318}'
Add-Line 'DriverVer   = $DRIVERVER$'
Add-Line 'CatalogFile = sonosvad.cat'
Add-Line 'PnpLockDown = 1'
Add-Line ''
Add-Line '[SourceDisksNames]'
Add-Line '222="SONOSVAD Driver Disk","",222'
Add-Line ''
Add-Line '[SourceDisksFiles]'
Add-Line 'sonosvad.sys=222'
Add-Line ''
Add-Line '[SignatureAttributes]'
Add-Line 'sonosvad.sys=SignatureAttributes.PETrust'
Add-Line ''
Add-Line '[SignatureAttributes.PETrust]'
Add-Line 'PETrust=true'
Add-Line ''
Add-Line '[Manufacturer]'
Add-Line '%MfgName%=SONOSVAD,NT$ARCH$.10.0...22000'
Add-Line ''
Add-Line '[SONOSVAD.NT$ARCH$.10.0...22000]'
Add-Line '%SONOSVAD_SA.DeviceDesc%=SONOSVAD_SA, ROOT\sonosvad'
Add-Line ''
Add-Line '[DestinationDirs]'
Add-Line 'SONOSVAD_SA.CopyList=13 ; 13=Package''s DriverStore directory'
Add-Line ''
Add-Line ';======================================================'
Add-Line '; SONOSVAD_SA'
Add-Line ';======================================================'
Add-Line '[SONOSVAD_SA.CopyList]'
Add-Line 'sonosvad.sys'
Add-Line ''
Add-Line '[SONOSVAD_SA.AddReg]'
Add-Line 'HKR,,AssociatedFilters,,"wdmaud,swmidi,redbook"'
Add-Line 'HKR,,Driver,,sonosvad.sys'
Add-Line ''
Add-Line '; The device must never idle out: an idle transition would tear down the'
Add-Line '; WaveRT streams that the user-mode service is loopback-capturing.'
Add-Line 'HKR,PowerSettings,SingleComponentMultiFxStates,3,0,0,0,0'
Add-Line ''
Add-Line 'HKR,Drivers,SubClasses,,"wave,mixer"'
Add-Line ''
Add-Line 'HKR,Drivers\wave\wdmaud.drv,Driver,,wdmaud.drv'
Add-Line 'HKR,Drivers\mixer\wdmaud.drv,Driver,,wdmaud.drv'
Add-Line ''
Add-Line 'HKR,Drivers\wave\wdmaud.drv,Description,,%SONOSVAD_SA.DeviceDesc%'
Add-Line 'HKR,Drivers\mixer\wdmaud.drv,Description,,%SONOSVAD_SA.DeviceDesc%'
Add-Line ''

Add-Line ';======================================================'
Add-Line '; render interfaces: one per virtual Sonos output'
Add-Line ';======================================================'
foreach ($i in 1..$EndpointCount) {
    Add-Line "[SONOSVAD.I.WaveSonosOutput$i]"
    Add-Line "AddReg=SONOSVAD.I.WaveSonosOutput$i.AddReg"
    Add-Line ''
    Add-Line "[SONOSVAD.I.WaveSonosOutput$i.AddReg]"
    Add-Line 'HKR,,CLSID,,%Proxy.CLSID%'
    Add-Line "HKR,,FriendlyName,,%SONOSVAD.WaveSonosOutput$i.szPname%"
    Add-Line ''
    Add-Line "[SONOSVAD.I.TopologySonosOutput$i]"
    Add-Line "AddReg=SONOSVAD.I.TopologySonosOutput$i.AddReg"
    Add-Line ''
    Add-Line "[SONOSVAD.I.TopologySonosOutput$i.AddReg]"
    Add-Line 'HKR,,CLSID,,%Proxy.CLSID%'
    Add-Line "HKR,,FriendlyName,,%SONOSVAD.TopologySonosOutput$i.szPname%"
    Add-Line ''
    Add-Line 'HKR,EP\0,%PKEY_AudioEndpoint_Association%,,%KSNODETYPE_ANY%'
    Add-Line ''
    Add-Line '; Opt in to event-driven (pull) mode.'
    Add-Line 'HKR,EP\0,%PKEY_AudioEndpoint_Supports_EventDriven_Mode%,0x00010001,0x1'
    Add-Line ''
}

Add-Line ';======================================================'
Add-Line '; SONOSVAD_SA'
Add-Line ';======================================================'
Add-Line '[SONOSVAD_SA.NT]'
Add-Line 'Include=ks.inf,wdmaudio.inf'
Add-Line 'Needs=KS.Registration, WDMAUDIO.Registration'
Add-Line 'CopyFiles=SONOSVAD_SA.CopyList'
Add-Line 'AddReg=SONOSVAD_SA.AddReg'
Add-Line ''
Add-Line '[SONOSVAD_SA.NT.Interfaces]'
foreach ($i in 1..$EndpointCount) {
    Add-Line ';'
    Add-Line "; Interfaces for Sonos output $i."
    Add-Line ';'
    Add-Line "AddInterface=%KSCATEGORY_AUDIO%,    %KSNAME_WaveSonosOutput$i%, SONOSVAD.I.WaveSonosOutput$i"
    Add-Line "AddInterface=%KSCATEGORY_RENDER%,   %KSNAME_WaveSonosOutput$i%, SONOSVAD.I.WaveSonosOutput$i"
    Add-Line "AddInterface=%KSCATEGORY_REALTIME%, %KSNAME_WaveSonosOutput$i%, SONOSVAD.I.WaveSonosOutput$i"
    Add-Line "AddInterface=%KSCATEGORY_AUDIO%,    %KSNAME_TopologySonosOutput$i%, SONOSVAD.I.TopologySonosOutput$i"
    Add-Line "AddInterface=%KSCATEGORY_TOPOLOGY%, %KSNAME_TopologySonosOutput$i%, SONOSVAD.I.TopologySonosOutput$i"
    Add-Line ''
}

Add-Line '[SONOSVAD_SA.NT.Services]'
Add-Line 'AddService=sonosvad,0x00000002,sonosvad_Service_Inst'
Add-Line ''
Add-Line '[sonosvad_Service_Inst]'
Add-Line 'DisplayName=%sonosvad.SvcDesc%'
Add-Line 'ServiceType=1'
Add-Line 'StartType=3'
Add-Line 'ErrorControl=1'
Add-Line 'ServiceBinary=%13%\sonosvad.sys'
Add-Line ''
Add-Line '[SONOSVAD_SA.NT.HW]'
Add-Line 'AddReg = AUDIOHW.AddReg'
Add-Line ''
Add-Line '[AUDIOHW.AddReg]'
Add-Line '; FILE_DEVICE_SOUND'
Add-Line 'HKR,,DeviceType,0x10001,0x0000001D'
Add-Line '; SDDL_DEVOBJ_SYS_ALL_ADM_RWX_WORLD_RWX_RES_RWX'
Add-Line 'HKR,,Security,,"D:P(A;;GA;;;SY)(A;;GRGWGX;;;BA)(A;;GRGWGX;;;WD)(A;;GRGWGX;;;RC)"'
Add-Line ''
Add-Line ';======================================================'
Add-Line '; WDF'
Add-Line ';======================================================'
Add-Line '[SONOSVAD_SA.NT.Wdf]'
Add-Line 'KmdfService = sonosvad, SONOSVAD_SA_WdfSect'
Add-Line ''
Add-Line '[SONOSVAD_SA_WdfSect]'
Add-Line 'KmdfLibraryVersion = $KMDFVERSION$'
Add-Line ''
Add-Line ';======================================================'
Add-Line '; COMMON'
Add-Line ';======================================================'
Add-Line '[Strings]'
Add-Line ''
Add-Line ';Non-localizable'
Add-Line ''
foreach ($i in 1..$EndpointCount) {
    Add-Line "KSNAME_WaveSonosOutput$i=`"WaveSonosOutput$i`""
    Add-Line "KSNAME_TopologySonosOutput$i=`"TopologySonosOutput$i`""
}
Add-Line ''
Add-Line 'Proxy.CLSID="{17CCA71B-ECD7-11D0-B908-00A0C9223196}"'
Add-Line 'KSCATEGORY_AUDIO="{6994AD04-93EF-11D0-A3CC-00A0C9223196}"'
Add-Line 'KSCATEGORY_RENDER="{65E8773E-8F56-11D0-A3B9-00A0C9223196}"'
Add-Line 'KSCATEGORY_REALTIME ="{EB115FFC-10C8-4964-831D-6DCB02E6F23F}"'
Add-Line 'KSCATEGORY_TOPOLOGY = "{DDA54A40-1E4C-11D1-A050-405705C10000}"'
Add-Line ''
Add-Line 'KSNODETYPE_ANY   = "{00000000-0000-0000-0000-000000000000}"'
Add-Line ''
Add-Line 'PKEY_AudioEndpoint_Association               = "{1DA5D803-D492-4EDD-8C23-E0C0FFEE7F0E},2"'
Add-Line 'PKEY_AudioEndpoint_Supports_EventDriven_Mode = "{1DA5D803-D492-4EDD-8C23-E0C0FFEE7F0E},7"'
Add-Line ''
Add-Line ';Localizable'
Add-Line ''
Add-Line 'ProviderName = "sonos-driver project"'
Add-Line 'MfgName      = "sonos-driver project"'
Add-Line ''
Add-Line 'SONOSVAD_SA.DeviceDesc="Sonos Virtual Audio Device (WDM)"'
Add-Line 'sonosvad.SvcDesc="Sonos Virtual Audio Device Driver"'
Add-Line ''
Add-Line ';; friendly names'
Add-Line ';; The user-mode service renames the resulting endpoints to the discovered'
Add-Line ';; Sonos room names via PKEY_Device_FriendlyName; these are the fallbacks'
Add-Line ';; shown before a room has been bound.'
foreach ($i in 1..$EndpointCount) {
    Add-Line "SONOSVAD.WaveSonosOutput$i.szPname=`"Sonos Output $i`""
    Add-Line "SONOSVAD.TopologySonosOutput$i.szPname=`"Sonos Output $i Topology`""
}

$full = [System.IO.Path]::GetFullPath($OutputPath)
$dir = Split-Path -Parent $full
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }

# stampinf requires UTF-16LE (with BOM) or ASCII.
$encoding = [System.Text.UnicodeEncoding]::new($false, $true)
[System.IO.File]::WriteAllText($full, $sb.ToString(), $encoding)

Write-Host "Generated $full ($EndpointCount endpoints)"
