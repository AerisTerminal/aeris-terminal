#ifndef AppVersion
  #error AppVersion must be supplied by the clean-break installer builder
#endif
#ifndef DesktopPath
  #error DesktopPath must be supplied by the clean-break installer builder
#endif
#ifndef IconPath
  #error IconPath must be supplied by the clean-break installer builder
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by the clean-break installer builder
#endif

[Setup]
AppId={{08131BC4-8BBC-48B4-A67A-A032EE62FBD4}
AppName=Axiusflow
AppVersion={#AppVersion}
AppPublisher=Axiusflow
AppPublisherURL=https://axiusflow.com
AppSupportURL=https://axiusflow.com
DefaultDirName={localappdata}\Programs\Axiusflow
DefaultGroupName=Axiusflow
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile={#IconPath}
UninstallDisplayIcon={app}\axiusflow_desktop.exe
UninstallDisplayName=Axiusflow
UninstallFilesDir={localappdata}\Programs\Axiusflow-Uninstall
OutputDir={#OutputDir}
OutputBaseFilename=Axiusflow-Setup
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
UsePreviousAppDir=yes
VersionInfoCompany=Axiusflow
VersionInfoDescription=Axiusflow Installer
VersionInfoProductName=Axiusflow

[InstallDelete]
; This is an explicit clean-break migration. Delete only the owned program
; directory; user data lives separately under {localappdata}\Axiusflow.
Type: filesandordirs; Name: "{app}\*"

[Files]
Source: "{#DesktopPath}"; DestDir: "{app}"; DestName: "axiusflow_desktop.exe"; Flags: ignoreversion

[Registry]
; Retire the legacy resident-engine autostart left by IPC-era installers.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Axiusflow Engine"; Flags: deletevalue

[Icons]
Name: "{autoprograms}\Axiusflow\Axiusflow"; Filename: "{app}\axiusflow_desktop.exe"; WorkingDir: "{app}"; IconFilename: "{app}\axiusflow_desktop.exe"; AppUserModelID: "com.axiusflow.desktop"; Comment: "Axiusflow trading terminal"

[Run]
Filename: "{app}\axiusflow_desktop.exe"; Description: "Launch Axiusflow"; Flags: nowait postinstall skipifsilent

[Code]
procedure RegisterCloseResource(const Filename: String);
begin
#if Ver >= EncodeVer(7, 0)
  RegisterExtraCloseApplicationsResource(Filename);
#else
  RegisterExtraCloseApplicationsResource(False, Filename);
#endif
end;

procedure RegisterExtraCloseApplicationsResources;
var
  FindRec: TFindRec;
  VersionsRoot: String;
  VersionRoot: String;
begin
  RegisterCloseResource(ExpandConstant('{app}\axiusflow_desktop.exe'));
  RegisterCloseResource(ExpandConstant('{app}\axiusflow_launcher.exe'));
  RegisterCloseResource(ExpandConstant('{app}\axiusflow_engine.exe'));

  VersionsRoot := ExpandConstant('{app}\versions');
  if not DirExists(VersionsRoot) then
    exit;

  if FindFirst(VersionsRoot + '\*', FindRec) then
  begin
    try
      repeat
        if ((FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) and
           (FindRec.Name <> '.') and (FindRec.Name <> '..') then
        begin
          VersionRoot := VersionsRoot + '\' + FindRec.Name;
          RegisterCloseResource(VersionRoot + '\axiusflow_desktop.exe');
          RegisterCloseResource(VersionRoot + '\axiusflow_engine.exe');
          RegisterCloseResource(VersionRoot + '\axiusflow_launcher.exe');
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;
