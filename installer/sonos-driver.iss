; Inno Setup script for sonos-driver.
;
; Builds a Windows installer that bundles the shipped binaries (the CLI, the
; Sonos Router app and the driverless background service), the documentation,
; both licence files, and a generated example configuration. Inno Setup is free
; for commercial use with no paid licence (its own permissive licence), so it
; satisfies the project's "no paid vendor licence" constraint
; (see docs/adr/0015-release-pipeline.md).
;
; If the staged payload also contains the driver (package.ps1 -IncludeDriver),
; the kernel-driver mode is offered as an optional component. Installing the
; driver itself is deliberately NOT automated here: it needs test signing, which
; needs Secure Boot off and a reboot, and that is a decision the user has to make
; consciously rather than by clicking Next. The installer therefore lays the
; files down and points at driver\INSTALL.md.
;
; This script is parameter-driven so the same file serves local and CI builds.
; Compile it with the Inno Setup command-line compiler (ISCC), passing the
; version and the staged payload directory:
;
;   ISCC /DAppVersion=0.2.0 /DPayloadDir=<stage> /DOutputDir=<dist> installer\sonos-driver.iss
;
; scripts\build-installer.ps1 resolves those values and invokes ISCC; it is what
; the release workflow runs after staging the portable payload.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef PayloadDir
  #define PayloadDir "..\dist\stage"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

[Setup]
; A stable AppId keeps upgrades and uninstall entries coherent across versions.
; Never change this GUID once released.
AppId={{B5C6E3F2-7A41-4E8B-9C2D-3F6A1E0D7B94}
AppName=sonos-driver
AppVersion={#AppVersion}
AppVerName=sonos-driver {#AppVersion}
AppPublisher=NikoMix
AppPublisherURL=https://github.com/NikoMix/sonos-driver
AppSupportURL=https://github.com/NikoMix/sonos-driver/issues
AppUpdatesURL=https://github.com/NikoMix/sonos-driver/releases
DefaultDirName={autopf}\sonos-driver
DefaultGroupName=sonos-driver
DisableProgramGroupPage=yes
LicenseFile={#PayloadDir}\LICENSE-MIT
OutputDir={#OutputDir}
OutputBaseFilename=sonos-driver-{#AppVersion}-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
; x64-only: the shipped binaries are x86_64-pc-windows-msvc, and the optional
; kernel driver is x64-only and cannot be emulated. "x64os" means "64-bit x64
; Windows" — the modern spelling of the deprecated "x64" identifier, with
; identical semantics. Deliberately not "x64compatible", which would also allow
; ARM64 machines running x64 code under emulation, where the driver cannot load.
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0
WizardStyle=modern
UninstallDisplayName=sonos-driver {#AppVersion}
UninstallDisplayIcon={app}\sonos-router.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

; The driver is only present when the payload was staged with -IncludeDriver, so
; its component is declared conditionally at compile time.
#define DriverPayload PayloadDir + "\driver\sonosvad.sys"
#if FileExists(DriverPayload)
  #define WithDriver
#endif

[Types]
Name: "standard"; Description: "Sonos Router, the CLI and the background service"
#ifdef WithDriver
Name: "full";     Description: "Everything, including the sonosvad kernel driver"
#endif
Name: "custom";   Description: "Custom installation"; Flags: iscustom

[Components]
#ifdef WithDriver
Name: "core";   Description: "Sonos Router app, CLI and driverless service"; Types: standard full custom; Flags: fixed
Name: "driver"; Description: "sonosvad kernel driver (needs test signing + a reboot)"; Types: full
#else
Name: "core";   Description: "Sonos Router app, CLI and driverless service"; Types: standard custom; Flags: fixed
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut for Sonos Router"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#PayloadDir}\sonos-driver.exe";     DestDir: "{app}"; Components: core; Flags: ignoreversion
Source: "{#PayloadDir}\sonos-router.exe";     DestDir: "{app}"; Components: core; Flags: ignoreversion
Source: "{#PayloadDir}\sonos-cast-svc.exe";   DestDir: "{app}"; Components: core; Flags: ignoreversion
Source: "{#PayloadDir}\README.md";            DestDir: "{app}"; Components: core; Flags: ignoreversion isreadme
Source: "{#PayloadDir}\CHANGELOG.md";         DestDir: "{app}"; Components: core; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE-MIT";          DestDir: "{app}"; Components: core; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE-APACHE";       DestDir: "{app}"; Components: core; Flags: ignoreversion
; Ship the generated example config under an .example name so an upgrade never
; clobbers a user's own %APPDATA%\sonos-driver\sonos-driver.toml.
Source: "{#PayloadDir}\sonos-driver.toml";    DestDir: "{app}"; DestName: "sonos-driver.example.toml"; Components: core; Flags: ignoreversion
#ifdef WithDriver
Source: "{#PayloadDir}\sonosvad-svc.exe";     DestDir: "{app}";               Components: driver; Flags: ignoreversion
Source: "{#PayloadDir}\driver\*";             DestDir: "{app}\driver";        Components: driver; Flags: ignoreversion
Source: "{#PayloadDir}\driver\scripts\*.ps1"; DestDir: "{app}\driver\scripts"; Components: driver; Flags: ignoreversion
#endif

[Icons]
Name: "{group}\Sonos Router"; Filename: "{app}\sonos-router.exe"; WorkingDir: "{app}"; Components: core
Name: "{group}\Example configuration";   Filename: "{app}\sonos-driver.example.toml"; Components: core
#ifdef WithDriver
Name: "{group}\How to install the driver"; Filename: "{app}\driver\INSTALL.md"; Components: driver
#endif
Name: "{group}\{cm:UninstallProgram,sonos-driver}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Sonos Router";     Filename: "{app}\sonos-router.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\sonos-router.exe"; Description: "{cm:LaunchProgram,Sonos Router}"; Flags: nowait postinstall skipifsilent
