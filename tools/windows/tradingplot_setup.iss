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
AppName=TradingPlot
AppVersion={#AppVersion}
AppPublisher=TradingPlot
AppPublisherURL=https://axiusflow.com
AppSupportURL=https://axiusflow.com
DefaultDirName={localappdata}\Programs\TradingPlot
DefaultGroupName=TradingPlot
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile={#IconPath}
UninstallDisplayIcon={app}\tradingplot_launcher.exe
UninstallDisplayName=TradingPlot
UninstallFilesDir={localappdata}\Programs\TradingPlot-Uninstall
OutputDir={#OutputDir}
OutputBaseFilename=TradingPlot-Setup
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
UsePreviousAppDir=no
VersionInfoCompany=TradingPlot
VersionInfoDescription=TradingPlot Installer
VersionInfoProductName=TradingPlot
VersionInfoProductVersion={#AppVersion}


[InstallDelete]
Type: files; Name: "{app}\tradingplot_desktop.exe"
Type: files; Name: "{app}\tradingplot_engine.exe"
Type: filesandordirs; Name: "{app}\.release-downloads"
Type: filesandordirs; Name: "{localappdata}\Programs\Axiusflow"
Type: filesandordirs; Name: "{localappdata}\Programs\Axiusflow-Uninstall"
Type: filesandordirs; Name: "{localappdata}\Programs\.Axiusflow-lifecycle"
Type: files; Name: "{localappdata}\Programs\.Axiusflow-lifecycle.lock"
Type: filesandordirs; Name: "{autoprograms}\Axiusflow"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "TradingPlot Engine"; Flags: deletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Axiusflow Engine"; Flags: deletevalue

[Files]
Source: "{#LauncherPath}"; DestDir: "{app}"; DestName: "tradingplot_launcher.exe"; Flags: ignoreversion
Source: "{#ManifestPath}"; DestDir: "{tmp}\TradingPlotRelease"; DestName: "manifest.json"; Flags: deleteafterinstall
Source: "{#LauncherPath}"; DestDir: "{tmp}\TradingPlotRelease\bundle"; DestName: "tradingplot_launcher.exe"; Flags: deleteafterinstall
Source: "{#DesktopPath}"; DestDir: "{tmp}\TradingPlotRelease\bundle"; DestName: "tradingplot_desktop.exe"; Flags: deleteafterinstall
Source: "{#RollbackCompatibilityPath}"; DestDir: "{tmp}\TradingPlotRelease\bundle"; DestName: "rollback-compatibility.json"; Flags: deleteafterinstall

[Icons]
Name: "{autoprograms}\TradingPlot\TradingPlot"; Filename: "{app}\tradingplot_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\tradingplot_launcher.exe"; AppUserModelID: "com.tradingplot.desktop"; Comment: "TradingPlot trading terminal"

[Run]
Filename: "{app}\tradingplot_launcher.exe"; Description: "Launch TradingPlot"; Flags: nowait postinstall skipifsilent

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
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\tradingplot_desktop.exe';
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
    ExpandConstant('{localappdata}\Programs\.TradingPlot-lifecycle\uninstall.json'),
    ExpandConstant('{localappdata}\Programs\TradingPlot\tradingplot_launcher.exe'),
    ExpandConstant('{localappdata}\Programs\TradingPlot'),
    'Finishing the previous TradingPlot uninstall...',
    'TradingPlot could not finish the pending uninstall cleanup. Setup has not replaced the recovery launcher; retry or cancel Setup.');
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

  Launcher := ExpandConstant('{app}\tradingplot_launcher.exe');
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
      'TradingPlot could not remove all local application data. Uninstall has stopped so cleanup can be retried safely.',
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

  Manifest := ExpandConstant('{tmp}\TradingPlotRelease\manifest.json');
  Bundle := ExpandConstant('{tmp}\TradingPlotRelease\bundle');
  WizardForm.StatusLabel.Caption := 'Verifying and installing the signed TradingPlot release...';
  if (not Exec(
      ExpandConstant('{app}\tradingplot_launcher.exe'),
      '--install "' + Manifest + '" "' + Bundle + '"',
      ExpandConstant('{app}'),
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode)) or (ResultCode <> 0) then
  begin
    RaiseException('TradingPlot could not verify and install the bundled release. Setup has stopped without activating an unverified application.');
  end;
end;
