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


[InstallDelete]
Type: files; Name: "{app}\aeris_desktop.exe"
Type: files; Name: "{app}\aeris_engine.exe"
Type: filesandordirs; Name: "{app}\.release-downloads"

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
Source: "{#LauncherPath}"; DestDir: "{app}"; DestName: "aeris_launcher.exe"; Flags: ignoreversion
Source: "{#ManifestPath}"; DestDir: "{tmp}\AerisRelease"; DestName: "manifest.json"; Flags: deleteafterinstall
Source: "{#LauncherPath}"; DestDir: "{tmp}\AerisRelease\bundle"; DestName: "aeris_launcher.exe"; Flags: deleteafterinstall
Source: "{#DesktopPath}"; DestDir: "{tmp}\AerisRelease\bundle"; DestName: "aeris_desktop.exe"; Flags: deleteafterinstall
Source: "{#RollbackCompatibilityPath}"; DestDir: "{tmp}\AerisRelease\bundle"; DestName: "rollback-compatibility.json"; Flags: deleteafterinstall

[Icons]
Name: "{autoprograms}\Aeris\Aeris"; Filename: "{app}\aeris_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\aeris_launcher.exe"; AppUserModelID: "com.aeris.desktop"; Comment: "Aeris Terminal trading terminal"

[Run]
Filename: "{app}\aeris_launcher.exe"; Description: "Launch Aeris Terminal"; Flags: nowait postinstall skipifsilent

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

var
  RemoveUserData: Boolean;

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
  Result := True;
  if UninstallSilent then
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

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Manifest: String;
  Bundle: String;
begin
  if CurStep <> ssPostInstall then
    exit;

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
  begin
    RaiseException('Aeris Terminal could not verify and install the bundled release. Setup has stopped without activating an unverified application.');
  end;
end;
