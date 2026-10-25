; Per-user installer for Aeris Terminal on Windows, built by the Windows preview
; lane with Inno Setup. It installs the desktop directly, without the signed
; launcher lifecycle in tools/windows/aeris_setup.iss, and shares that
; installer's AppId and folder so a later lifecycle install upgrades it in place.
; Uninstall removes the program only: workspaces, the trading journal and vault
; entries under %LOCALAPPDATA%\Aeris stay with the user.
#ifndef AppVersion
  #error AppVersion must be supplied by the packaging step
#endif
#ifndef DesktopPath
  #error DesktopPath must be supplied by the packaging step
#endif
#ifndef IconPath
  #error IconPath must be supplied by the packaging step
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by the packaging step
#endif

[Setup]
AppId={{08131BC4-8BBC-48B4-A67A-A032EE62FBD4}
AppName=Aeris Terminal
AppVersion={#AppVersion}
AppVerName=Aeris Terminal {#AppVersion}
AppPublisher=Aeris Terminal
AppPublisherURL=https://aeristerminal.com/
AppSupportURL=https://aeristerminal.com/contact/
AppUpdatesURL=https://aeristerminal.com/download/
DefaultDirName={localappdata}\Programs\Aeris
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
SetupIconFile={#IconPath}
UninstallDisplayIcon={app}\aeris_desktop.exe
UninstallDisplayName=Aeris Terminal
OutputDir={#OutputDir}
OutputBaseFilename=Aeris-Terminal-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoCompany=Aeris Terminal
VersionInfoDescription=Aeris Terminal Setup
VersionInfoProductName=Aeris Terminal
VersionInfoProductVersion={#AppVersion}

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#DesktopPath}"; DestDir: "{app}"; DestName: "aeris_desktop.exe"; Flags: ignoreversion

[Registry]
; The desktop registers its notification identity under this key; uninstall removes it.
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\com.aeris.desktop"; Flags: uninsdeletekey

[Icons]
Name: "{autoprograms}\Aeris Terminal"; Filename: "{app}\aeris_desktop.exe"; WorkingDir: "{app}"; AppUserModelID: "com.aeris.desktop"; Comment: "Aeris Terminal trading terminal"
Name: "{autodesktop}\Aeris Terminal"; Filename: "{app}\aeris_desktop.exe"; WorkingDir: "{app}"; AppUserModelID: "com.aeris.desktop"; Comment: "Aeris Terminal trading terminal"; Tasks: desktopicon

[Run]
Filename: "{app}\aeris_desktop.exe"; Description: "{cm:LaunchProgram,Aeris Terminal}"; Flags: nowait postinstall skipifsilent
