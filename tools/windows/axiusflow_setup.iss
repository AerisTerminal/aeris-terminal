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
#ifndef EnginePath
  #error EnginePath must be supplied by the release publisher
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

[Files]
Source: "{#LauncherPath}"; DestDir: "{app}"; DestName: "axiusflow_launcher.exe"; Flags: ignoreversion
Source: "{#ManifestPath}"; DestDir: "{tmp}\AxiusflowRelease"; DestName: "manifest.json"; Flags: deleteafterinstall
Source: "{#DesktopPath}"; DestDir: "{tmp}\AxiusflowRelease\bundle"; DestName: "axiusflow_desktop.exe"; Flags: deleteafterinstall
Source: "{#EnginePath}"; DestDir: "{tmp}\AxiusflowRelease\bundle"; DestName: "axiusflow_engine.exe"; Flags: deleteafterinstall

[Icons]
Name: "{autoprograms}\Axiusflow\Axiusflow"; Filename: "{app}\axiusflow_launcher.exe"; WorkingDir: "{app}"; IconFilename: "{app}\axiusflow_launcher.exe"; AppUserModelID: "com.axiusflow.desktop"; Comment: "Axiusflow trading terminal"

[Run]
Filename: "{app}\axiusflow_launcher.exe"; Description: "Launch Axiusflow"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\axiusflow_launcher.exe"; Parameters: "--remove-all-local-data"; Flags: runhidden waituntilterminated skipifdoesntexist

[Code]
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
