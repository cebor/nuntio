; Inno Setup script for the Windows installer, compiled by `cargo xtask
; package`. It passes Version, Stage (the directory with the built files),
; OutputDir and OutputBase on the command line with /D.
;
; The install directory is not added to PATH: it also holds nuntio-config,
; which nuntio puts on PATH only inside its own panes.

#ifndef Version
  #error Pass the version with /DVersion=<version>
#endif

[Setup]
; Never change the AppId: upgrades and the uninstaller find nuntio by it.
AppId={{E2B8E168-55F3-4D4D-A5DE-529B82ADFD36}
AppName=nuntio
AppVersion={#Version}
AppVerName=nuntio {#Version}
AppPublisher=Felix Itzenplitz
AppPublisherURL=https://cebor.github.io/nuntio/
AppSupportURL=https://github.com/cebor/nuntio/issues
AppUpdatesURL=https://github.com/cebor/nuntio/releases
VersionInfoVersion={#Version}
DefaultDirName={autopf}\nuntio
DisableProgramGroupPage=yes
; Installs for the current user without admin rights; the dialog offers
; an installation for all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile={#Stage}\LICENSE-MIT
SetupIconFile=icons\nuntio.ico
UninstallDisplayIcon={app}\nuntio.exe
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Stage}\nuntio.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\nuntio-config.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\THIRD-PARTY-LICENSES.html"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\nuntio"; Filename: "{app}\nuntio.exe"
Name: "{autodesktop}\nuntio"; Filename: "{app}\nuntio.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\nuntio.exe"; Description: "{cm:LaunchProgram,nuntio}"; Flags: nowait postinstall skipifsilent
