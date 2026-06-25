; Inno Setup script for sonos-driver.
;
; Builds a Windows installer that bundles both shipped binaries (the CLI and the
; system-tray GUI), the documentation, both licence files, and a generated
; example configuration. Inno Setup is free for commercial use with no paid
; licence (its own permissive licence), so it satisfies the project's
; "no paid vendor licence" constraint (see docs/adr/0015-release-pipeline.md).
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
; x64-only: the shipped binaries are x86_64-pc-windows-msvc. "x64" is the
; broadly-compatible architecture identifier accepted across Inno Setup 6.x.
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
MinVersion=10.0
WizardStyle=modern
UninstallDisplayName=sonos-driver {#AppVersion}
UninstallDisplayIcon={app}\sonos-driver-gui.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut for the tray GUI"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#PayloadDir}\sonos-driver.exe";     DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\sonos-driver-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\README.md";            DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "{#PayloadDir}\CHANGELOG.md";         DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE-MIT";          DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\LICENSE-APACHE";       DestDir: "{app}"; Flags: ignoreversion
; Ship the generated example config under an .example name so an upgrade never
; clobbers a user's own %APPDATA%\sonos-driver\sonos-driver.toml.
Source: "{#PayloadDir}\sonos-driver.toml";    DestDir: "{app}"; DestName: "sonos-driver.example.toml"; Flags: ignoreversion

[Icons]
Name: "{group}\sonos-driver (tray GUI)"; Filename: "{app}\sonos-driver-gui.exe"; WorkingDir: "{app}"
Name: "{group}\Example configuration";   Filename: "{app}\sonos-driver.example.toml"
Name: "{group}\{cm:UninstallProgram,sonos-driver}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\sonos-driver";      Filename: "{app}\sonos-driver-gui.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\sonos-driver-gui.exe"; Description: "{cm:LaunchProgram,sonos-driver}"; Flags: nowait postinstall skipifsilent
