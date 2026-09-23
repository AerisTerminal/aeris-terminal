#ifndef AppVersion
  #error AppVersion must be supplied by the packaging tool
#endif
#ifndef LauncherPath
  #error LauncherPath must be supplied by the packaging tool
#endif
#ifndef ManifestPath
  #error ManifestPath must be supplied by the packaging tool
#endif
#ifndef DesktopPath
  #error DesktopPath must be supplied by the packaging tool
#endif
#ifndef RollbackCompatibilityPath
  #error RollbackCompatibilityPath must be supplied by the packaging tool
#endif
#ifndef IconPath
  #error IconPath must be supplied by the packaging tool
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by the packaging tool
#endif

[Setup]
AppId={{08131BC4-8BBC-48B4-A67A-A032EE62FBD4}
AppName=Asceify
AppVersion={#AppVersion}
AppPublisher=Asceify
DefaultDirName={localappdata}\Programs\Asceify
DefaultGroupName=Asceify
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile={#IconPath}
UninstallDisplayIcon={app}\asceify_launcher.exe
UninstallDisplayName=Asceify
UninstallFilesDir={localappdata}\Programs\Asceify-Uninstall
OutputDir={#OutputDir}
OutputBaseFilename=Asceify-Setup
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
UsePreviousAppDir=no
VersionInfoCompany=Asceify
VersionInfoDescription=Asceify Installer
VersionInfoProductName=Asceify
VersionInfoProductVersion={#AppVersion}


[InstallDelete]
Type: files; Name: "{app}\asceify_desktop.exe"
Type: files; Name: "{app}\asceify_engine.exe"
Type: filesandordirs; Name: "{app}\.release-downloads"
Type: filesandordirs; Name: "{localappdata}\Programs\Axiusflow"
Type: filesandordirs; Name: "{localappdata}\Programs\Axiusflow-Uninstall"
Type: filesandordirs; Name: "{localappdata}\Programs\.Axiusflow-lifecycle"
Type: files; Name: "{localappdata}\Programs\.Axiusflow-lifecycle.lock"
Type: filesandordirs; Name: "{autoprograms}\Axiusflow"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Asceify Engine"; Flags: deletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Axiusflow Engine"; Flags: deletevalue

[Files]
Source: "{#LauncherPath}"; DestDir: "{app}"; DestName: "asceify_launcher.exe"; Flags: ignoreversion
Source: "{#ManifestPath}"; DestDir: "{tmp}\AsceifyRelease"; DestName: "manifest.json"; Flags: deleteafterinstall
Source: "{#LauncherPath}"; DestDir: "{tmp}\AsceifyRelease\bundle"; DestName: "asceify_launcher.exe"; Flags: deleteafterinstall
Source: "{#DesktopPath}"; DestDir: "{tmp}\AsceifyRelease\bundle"; DestName: "asceify_desktop.exe"; Flags: deleteafterinstall
Source: "{#RollbackCompatibilityPath}"; DestDir: "{tmp}\AsceifyRelease\bundle"; DestName: "rollback-compatibility.json"; Flags: deleteafterinstall

[Icons]
Name: "{autoprograms}\Asceify\Asceify"; Filename: "{app}\asceify_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\asceify_launcher.exe"; AppUserModelID: "com.asceify.desktop"; Comment: "Asceify trading terminal"

[Run]
Filename: "{app}\asceify_launcher.exe"; Description: "Launch Asceify"; Flags: nowait postinstall skipifsilent

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
  if DirExists(VersionsRoot) and FindFirst(VersionsRoot + '\*', FindRec) then
  begin
    try
      repeat
        if ((FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) and
           (FindRec.Name <> '.') and (FindRec.Name <> '..') then
        begin
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\asceify_desktop.exe';
          if FileExists(DesktopPath) then
            RegisterCloseResource(DesktopPath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;

  VersionsRoot := ExpandConstant('{localappdata}\Programs\Axiusflow\versions');
  if DirExists(VersionsRoot) and FindFirst(VersionsRoot + '\*', FindRec) then
  begin
    try
      repeat
        if ((FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) and
           (FindRec.Name <> '.') and (FindRec.Name <> '..') then
        begin
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\axiusflow_desktop.exe';
          if FileExists(DesktopPath) then
            RegisterCloseResource(DesktopPath);
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\axiusflow_launcher.exe';
          if FileExists(DesktopPath) then
            RegisterCloseResource(DesktopPath);
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\axiusflow_engine.exe';
          if FileExists(DesktopPath) then
            RegisterCloseResource(DesktopPath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;

  DesktopPath := ExpandConstant('{localappdata}\Programs\Axiusflow\axiusflow_launcher.exe');
  if FileExists(DesktopPath) then
    RegisterCloseResource(DesktopPath);
  DesktopPath := ExpandConstant('{localappdata}\Programs\Axiusflow\axiusflow_desktop.exe');
  if FileExists(DesktopPath) then
    RegisterCloseResource(DesktopPath);
  DesktopPath := ExpandConstant('{localappdata}\Programs\Axiusflow\axiusflow_engine.exe');
  if FileExists(DesktopPath) then
    RegisterCloseResource(DesktopPath);
end;

function FinishPendingUninstall(
  const Marker: String;
  const Launcher: String;
  const Root: String;
  const StatusCaption: String;
  const FailureMessage: String): String;
var
  ResultCode: Integer;
begin
  Result := '';
  if not FileExists(Marker) then
    exit;

  WizardForm.StatusLabel.Caption := StatusCaption;
  if (not FileExists(Launcher)) or
     (not Exec(
       Launcher,
       '--remove-all-local-data',
       Root,
       SW_HIDE,
       ewWaitUntilTerminated,
       ResultCode)) or
     (ResultCode <> 0) then
  begin
    Result := FailureMessage;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  { A pending uninstall is an explicit request to remove local state. Complete
    it before [InstallDelete] can replace either recovery launcher. }
  Result := FinishPendingUninstall(
    ExpandConstant('{localappdata}\Programs\.Asceify-lifecycle\uninstall.json'),
    ExpandConstant('{localappdata}\Programs\Asceify\asceify_launcher.exe'),
    ExpandConstant('{localappdata}\Programs\Asceify'),
    'Finishing the previous Asceify uninstall...',
    'Asceify could not finish the pending uninstall cleanup. Setup has not replaced the recovery launcher; retry or cancel Setup.');
  if Result <> '' then
    exit;

  Result := FinishPendingUninstall(
    ExpandConstant('{localappdata}\Programs\.Axiusflow-lifecycle\uninstall.json'),
    ExpandConstant('{localappdata}\Programs\Axiusflow\axiusflow_launcher.exe'),
    ExpandConstant('{localappdata}\Programs\Axiusflow'),
    'Finishing the previous Axiusflow uninstall...',
    'Axiusflow could not finish the pending uninstall cleanup. Setup has not replaced the recovery launcher; retry or cancel Setup.');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
  Launcher: String;
begin
  if CurUninstallStep <> usUninstall then
    exit;

  Launcher := ExpandConstant('{app}\asceify_launcher.exe');
  if (not FileExists(Launcher)) or
     (not Exec(
       Launcher,
       '--remove-all-local-data',
       ExpandConstant('{app}'),
       SW_HIDE,
       ewWaitUntilTerminated,
       ResultCode)) or
     (ResultCode <> 0) then
  begin
    MsgBox(
      'Asceify could not remove all local application data. Uninstall has stopped so cleanup can be retried safely.',
      mbError,
      MB_OK);
    Abort;
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

  Manifest := ExpandConstant('{tmp}\AsceifyRelease\manifest.json');
  Bundle := ExpandConstant('{tmp}\AsceifyRelease\bundle');
  WizardForm.StatusLabel.Caption := 'Verifying and installing the signed Asceify release...';
  if (not Exec(
      ExpandConstant('{app}\asceify_launcher.exe'),
      '--install "' + Manifest + '" "' + Bundle + '"',
      ExpandConstant('{app}'),
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode)) or (ResultCode <> 0) then
  begin
    RaiseException('Asceify could not verify and install the bundled release. Setup has stopped without activating an unverified application.');
  end;
end;
