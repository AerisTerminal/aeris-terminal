#ifndef AppVersion
  #error AppVersion must be supplied by the release publisher
#endif
#ifndef LauncherPath
  #error LauncherPath must be supplied by the release publisher
#endif
#ifndef ManifestPath
  #error ManifestPath must be supplied by the release publisher
#endif
#ifndef DesktopPath
  #error DesktopPath must be supplied by the release publisher
#endif
#ifndef RollbackCompatibilityPath
  #error RollbackCompatibilityPath must be supplied by the release publisher
#endif
#ifndef IconPath
  #error IconPath must be supplied by the release publisher
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by the release publisher
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
UninstallDisplayIcon={app}\axiusflow_launcher.exe
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
VersionInfoProductVersion={#AppVersion}


[InstallDelete]
Type: files; Name: "{app}\axiusflow_desktop.exe"
Type: files; Name: "{app}\axiusflow_engine.exe"
Type: filesandordirs; Name: "{app}\.release-downloads"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Axiusflow Engine"; Flags: deletevalue

[Files]
Source: "{#LauncherPath}"; DestDir: "{app}"; DestName: "axiusflow_launcher.exe"; Flags: ignoreversion
Source: "{#ManifestPath}"; DestDir: "{tmp}\AxiusflowRelease"; DestName: "manifest.json"; Flags: deleteafterinstall
Source: "{#LauncherPath}"; DestDir: "{tmp}\AxiusflowRelease\bundle"; DestName: "axiusflow_launcher.exe"; Flags: deleteafterinstall
Source: "{#DesktopPath}"; DestDir: "{tmp}\AxiusflowRelease\bundle"; DestName: "axiusflow_desktop.exe"; Flags: deleteafterinstall
Source: "{#RollbackCompatibilityPath}"; DestDir: "{tmp}\AxiusflowRelease\bundle"; DestName: "rollback-compatibility.json"; Flags: deleteafterinstall

[Icons]
Name: "{autoprograms}\Axiusflow\Axiusflow"; Filename: "{app}\axiusflow_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\axiusflow_launcher.exe"; AppUserModelID: "com.axiusflow.desktop"; Comment: "Axiusflow trading terminal"

[Run]
Filename: "{app}\axiusflow_launcher.exe"; Description: "Launch Axiusflow"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\axiusflow_launcher.exe"; Parameters: "--remove-all-local-data"; Flags: runhidden waituntilterminated skipifdoesntexist

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
  DesktopPath: String;
begin
  { Desktop binaries live in immutable version directories rather than in the
    installer's direct file list. Register them with Restart Manager so Setup closes the running
    desktop before the launcher's transactional activation. }
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
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\axiusflow_desktop.exe';
          if FileExists(DesktopPath) then
            RegisterCloseResource(DesktopPath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Manifest: String;
  Bundle: String;
begin
  if CurStep <> ssPostInstall then
    exit;

  Manifest := ExpandConstant('{tmp}\AxiusflowRelease\manifest.json');
  Bundle := ExpandConstant('{tmp}\AxiusflowRelease\bundle');
  WizardForm.StatusLabel.Caption := 'Verifying and installing the signed Axiusflow release...';
  if (not Exec(
      ExpandConstant('{app}\axiusflow_launcher.exe'),
      '--install "' + Manifest + '" "' + Bundle + '"',
      ExpandConstant('{app}'),
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode)) or (ResultCode <> 0) then
  begin
    RaiseException('Axiusflow could not verify and install the bundled release. Setup has stopped without activating an unverified application.');
  end;
end;
