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
#if Ver < EncodeVer(6, 6)
  #error Inno Setup 6.6 or newer is required for the uninstall data choice
#endif

[Setup]
AppId={{08131BC4-8BBC-48B4-A67A-A032EE62FBD4}
AppName=Aeris Terminal
AppVersion={#AppVersion}
AppPublisher=Aeris Terminal
DefaultDirName={localappdata}\Programs\Aeris
DefaultGroupName=Aeris Terminal
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile={#IconPath}
UninstallDisplayIcon={app}\aeris_launcher.exe
UninstallDisplayName=Aeris Terminal
UninstallFilesDir={localappdata}\Programs\Aeris-Uninstall
OutputDir={#OutputDir}
OutputBaseFilename=Aeris-Setup
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
UsePreviousAppDir=no
VersionInfoCompany=Aeris Terminal
VersionInfoDescription=Aeris Terminal Installer
VersionInfoProductName=Aeris Terminal
VersionInfoProductVersion={#AppVersion}

[Tasks]
; Same task name as the direct 1.x installer, so an upgrade keeps the user's desktop shortcut choice.
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[InstallDelete]
; The direct 1.x installer's aeris_desktop.exe is removed only after the bundled release is
; active (InstallBundledRelease), so a failed upgrade leaves that installation runnable.
Type: files; Name: "{app}\aeris_engine.exe"
Type: filesandordirs; Name: "{app}\.release-downloads"
Type: files; Name: "{autodesktop}\Aeris Terminal.lnk"; Tasks: not desktopicon

[UninstallDelete]
; Both launcher removal commands retire the running launcher outside {app}; it can be deleted once
; that launcher has exited. Any other file in the staging directory is left in place.
Type: files; Name: "{localappdata}\Programs\.Aeris-uninstall-stage\aeris_launcher.retired-*"
Type: dirifempty; Name: "{localappdata}\Programs\.Aeris-uninstall-stage"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Aeris Engine"; Flags: deletevalue
; The desktop registers its notification identity under this key; uninstall removes it.
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\com.aeris.desktop"; Flags: uninsdeletekey

[Files]
Source: "{#LauncherPath}"; DestDir: "{app}"; DestName: "aeris_launcher.exe"; Flags: ignoreversion; BeforeInstall: KeepPreviousLauncher
Source: "{#ManifestPath}"; DestDir: "{tmp}\AerisRelease"; DestName: "manifest.json"; Flags: deleteafterinstall
Source: "{#LauncherPath}"; DestDir: "{tmp}\AerisRelease\bundle"; DestName: "aeris_launcher.exe"; Flags: deleteafterinstall
Source: "{#DesktopPath}"; DestDir: "{tmp}\AerisRelease\bundle"; DestName: "aeris_desktop.exe"; Flags: deleteafterinstall
; The last file runs the launcher's verified install before any shortcut or registry change.
Source: "{#RollbackCompatibilityPath}"; DestDir: "{tmp}\AerisRelease\bundle"; DestName: "rollback-compatibility.json"; Flags: deleteafterinstall; AfterInstall: InstallBundledRelease

[Icons]
; The shortcut names match the direct 1.x installer's, so an upgrade repoints them at the launcher
; once its release is active; a failed upgrade keeps the 1.x shortcuts.
Name: "{autoprograms}\Aeris Terminal"; Filename: "{app}\aeris_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\aeris_launcher.exe"; AppUserModelID: "com.aeris.desktop"; Comment: "Aeris Terminal trading terminal"; Check: BundledReleaseActive
Name: "{autodesktop}\Aeris Terminal"; Filename: "{app}\aeris_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\aeris_launcher.exe"; AppUserModelID: "com.aeris.desktop"; Comment: "Aeris Terminal trading terminal"; Tasks: desktopicon; Check: BundledReleaseActive

[Run]
Filename: "{app}\aeris_launcher.exe"; Description: "Launch Aeris Terminal"; Flags: nowait postinstall skipifsilent; Check: BundledReleaseActive

[Code]
const
  { Marks the uninstall started from Windows' installed-apps list: /SILENT skips Inno's own
    confirmation box, and this switch still shows the one data-choice confirmation. }
  InteractiveRemovalSwitch = '/INTERACTIVE';

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
  { A direct 1.x installation runs the desktop from the install root. }
  RegisterCloseResource(ExpandConstant('{app}\aeris_desktop.exe'));

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
          DesktopPath := VersionsRoot + '\' + FindRec.Name + '\aeris_desktop.exe';
          if FileExists(DesktopPath) then
            RegisterCloseResource(DesktopPath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
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
    ExpandConstant('{localappdata}\Programs\.Aeris-lifecycle\uninstall.json'),
    ExpandConstant('{localappdata}\Programs\Aeris\aeris_launcher.exe'),
    ExpandConstant('{localappdata}\Programs\Aeris'),
    'Finishing the previous Aeris Terminal uninstall...',
    'Aeris Terminal could not finish the pending uninstall cleanup. Setup has not replaced the recovery launcher; retry or cancel Setup.');
end;

const
  { Setup's own codes stop at 8. Inno Setup discards exceptions raised by AfterInstall and
    cannot roll back from it, so a failed bundled install reports this code instead. }
  BundledReleaseFailedExitCode = 20;

var
  BundledReleaseFailure: String;

function PreviousLauncherCopy(): String;
begin
  Result := ExpandConstant('{tmp}\previous_aeris_launcher.exe');
end;

procedure KeepPreviousLauncher;
var
  Launcher: String;
begin
  { A failed bundled install restores the launcher this Setup replaced from this copy. }
  Launcher := ExpandConstant('{app}\aeris_launcher.exe');
  if FileExists(Launcher) and not CopyFile(Launcher, PreviousLauncherCopy(), False) then
    BundledReleaseFailure := 'Aeris Terminal could not keep a copy of the installed launcher, so Setup has stopped before installing the release.';
end;

procedure RestorePreviousLauncher;
begin
  if FileExists(PreviousLauncherCopy()) and
     not CopyFile(PreviousLauncherCopy(), ExpandConstant('{app}\aeris_launcher.exe'), False) then
    Log('The replaced launcher could not be restored.');
end;

procedure InstallBundledRelease;
var
  ResultCode: Integer;
  Manifest: String;
  Bundle: String;
  LegacyDesktop: String;
begin
  if BundledReleaseFailure = '' then
  begin
    Manifest := ExpandConstant('{tmp}\AerisRelease\manifest.json');
    Bundle := ExpandConstant('{tmp}\AerisRelease\bundle');
    WizardForm.StatusLabel.Caption := 'Verifying and installing the signed Aeris Terminal release...';
    if (not Exec(
        ExpandConstant('{app}\aeris_launcher.exe'),
        '--install "' + Manifest + '" "' + Bundle + '"',
        ExpandConstant('{app}'),
        SW_HIDE,
        ewWaitUntilTerminated,
        ResultCode)) or (ResultCode <> 0) then
      BundledReleaseFailure := 'Aeris Terminal could not verify and install the bundled release. Setup has stopped without activating an unverified application.';
  end;

  if BundledReleaseFailure <> '' then
  begin
    Log(BundledReleaseFailure);
    RestorePreviousLauncher;
    SuppressibleMsgBox(BundledReleaseFailure, mbCriticalError, MB_OK, IDOK);
    exit;
  end;

  LegacyDesktop := ExpandConstant('{app}\aeris_desktop.exe');
  if FileExists(LegacyDesktop) and not DeleteFile(LegacyDesktop) then
    Log('The direct 1.x desktop executable could not be removed: ' + LegacyDesktop);
end;

function BundledReleaseActive(): Boolean;
begin
  Result := BundledReleaseFailure = '';
end;

function GetCustomSetupExitCode(): Integer;
begin
  if BundledReleaseFailure <> '' then
    Result := BundledReleaseFailedExitCode
  else
    Result := 0;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and (BundledReleaseFailure <> '') then
  begin
    WizardForm.FinishedHeadingLabel.Caption := 'Aeris Terminal was not installed';
    WizardForm.FinishedLabel.Caption := BundledReleaseFailure;
  end;
end;

procedure RouteInstalledAppsUninstallThroughDataChoice;
var
  Key: String;
begin
  { Setup rewrites UninstallString after [Registry] runs, so it is replaced here. }
  Key := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\' +
    ExpandConstant('{#SetupSetting("AppId")}') + '_is1';
  if not RegWriteStringValue(
      HKA,
      Key,
      'UninstallString',
      '"' + ExpandConstant('{uninstallexe}') + '" /SILENT ' + InteractiveRemovalSwitch) then
    Log('The uninstall command could not be updated; uninstall will ask twice.');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    RouteInstalledAppsUninstallThroughDataChoice;
end;

var
  RemoveUserData: Boolean;
  InteractiveRemoval: Boolean;

function HasInteractiveRemovalSwitch(): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), InteractiveRemovalSwitch) = 0 then
      Result := True;
end;

function InitializeUninstall(): Boolean;
var
  Form: TSetupForm;
  Prompt: TNewStaticText;
  RemoveData: TNewCheckBox;
  UninstallButton, CancelButton: TNewButton;
  W: Integer;
begin
  { The user's data is kept unless they tick the box; a silent uninstall always keeps it. }
  RemoveUserData := False;
  InteractiveRemoval := HasInteractiveRemovalSwitch();
  Result := True;
  if UninstallSilent and not InteractiveRemoval then
    exit;

  Form := CreateCustomForm(ScaleX(440), ScaleY(140), False, True);
  try
    Form.Caption := 'Uninstall Aeris Terminal';

    Prompt := TNewStaticText.Create(Form);
    Prompt.Parent := Form;
    Prompt.AutoSize := False;
    Prompt.WordWrap := True;
    Prompt.Left := ScaleX(12);
    Prompt.Top := ScaleY(12);
    Prompt.Width := Form.ClientWidth - ScaleX(24);
    Prompt.Height := ScaleY(44);
    Prompt.Caption := 'Aeris Terminal will be removed from this computer. Your trading journal, workspaces, settings and saved sign-ins are kept unless you choose to remove them.';

    RemoveData := TNewCheckBox.Create(Form);
    RemoveData.Parent := Form;
    RemoveData.Left := ScaleX(12);
    RemoveData.Top := ScaleY(62);
    RemoveData.Width := Form.ClientWidth - ScaleX(24);
    RemoveData.Height := ScaleY(20);
    RemoveData.Caption := 'Remove my Aeris data';
    RemoveData.Checked := False;

    UninstallButton := TNewButton.Create(Form);
    UninstallButton.Parent := Form;
    UninstallButton.Caption := 'Uninstall';
    UninstallButton.ModalResult := mrOk;
    UninstallButton.Default := True;

    CancelButton := TNewButton.Create(Form);
    CancelButton.Parent := Form;
    CancelButton.Caption := 'Cancel';
    CancelButton.ModalResult := mrCancel;
    CancelButton.Cancel := True;

    W := Form.CalculateButtonWidth([UninstallButton.Caption, CancelButton.Caption]);
    UninstallButton.Width := W;
    CancelButton.Width := W;
    UninstallButton.Height := ScaleY(23);
    CancelButton.Height := ScaleY(23);
    UninstallButton.Top := Form.ClientHeight - ScaleY(23 + 12);
    CancelButton.Top := UninstallButton.Top;
    CancelButton.Left := Form.ClientWidth - ScaleX(12) - W;
    UninstallButton.Left := CancelButton.Left - ScaleX(6) - W;

    Form.ActiveControl := UninstallButton;
    if Form.ShowModal() = mrOk then
      RemoveUserData := RemoveData.Checked
    else
      Result := False;
  finally
    Form.Free();
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
  Launcher: String;
  Removal: String;
  FailureMessage: String;
begin
  { /SILENT also suppresses Inno's completion message, so the interactive removal shows it. }
  if (CurUninstallStep = usPostUninstall) and InteractiveRemoval then
  begin
    MsgBox(FmtMessage(SetupMessage(msgUninstalledAll), ['Aeris Terminal']), mbInformation, MB_OK);
    exit;
  end;

  if CurUninstallStep <> usUninstall then
    exit;

  if RemoveUserData then
  begin
    Removal := '--remove-all-local-data';
    FailureMessage := 'Aeris Terminal could not remove all local application data. Uninstall has stopped so cleanup can be retried safely.';
  end
  else
  begin
    Removal := '--remove-application';
    FailureMessage := 'Aeris Terminal could not be removed. Uninstall has stopped so it can be retried safely; your Aeris data was not removed.';
  end;

  Launcher := ExpandConstant('{app}\aeris_launcher.exe');
  if (not FileExists(Launcher)) or
     (not Exec(
       Launcher,
       Removal,
       ExpandConstant('{app}'),
       SW_HIDE,
       ewWaitUntilTerminated,
       ResultCode)) or
     (ResultCode <> 0) then
  begin
    MsgBox(FailureMessage, mbError, MB_OK);
    Abort;
  end;
end;
