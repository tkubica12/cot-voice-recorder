; Source-only migration installer. Build is authorized; installation/upgrade is not.
#define AppName "VoicePrompt"
#define AppVersion "1.5.1"
#define AppExeName "VoicePrompt.exe"
#define AppId "{{9C1E7A54-3E77-4B27-9E4A-3A5C1E2C0B41}"
#ifndef SourceDir
  #define SourceDir "..\..\target\installer-staging"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\target\installer-output"
#endif

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion} Rust migration preview
AppPublisher=tomaskubica
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExeName}
SetupIconFile={#SourceDir}\app.ico
OutputDir={#OutputDir}
OutputBaseFilename=VoicePrompt-Rust-Setup
Compression=lzma2/max
SolidCompression=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=no
RestartApplications=no
SetupLogging=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startupicon"; Description: "Start VoicePrompt automatically when I sign in"; Flags: checkedonce
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\app.ico"; DestDir: "{app}"; Flags: ignoreversion
; No wildcard JSON, OAuth configuration, token, settings, history or recovery payload.

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Parameters: "--installed"; IconFilename: "{app}\app.ico"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"; IconFilename: "{app}\app.ico"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Parameters: "--installed"; IconFilename: "{app}\app.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "VoicePrompt"; ValueData: """{app}\{#AppExeName}"" --installed"; Flags: uninsdeletevalue; Tasks: startupicon

[Run]
Filename: "{app}\{#AppExeName}"; Parameters: "--installed"; Description: "Start VoicePrompt"; Flags: nowait postinstall skipifsilent

[InstallDelete]
Type: files; Name: "{userstartup}\{#AppName}.lnk"

[Code]
function GracefulQuit(const Exe: String): Boolean;
var
  Code: Integer;
begin
  Result := True;
  if (Exe <> '') and FileExists(Exe) then
    Result := Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
  if Result then
    Sleep(750);
end;

function PreviousAppExe(): String;
var
  Location: String;
begin
  Result := '';
  if RegQueryStringValue(HKCU,
    ExpandConstant('Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppId}_is1'),
    'InstallLocation', Location) then
    Result := RemoveBackslashUnlessRoot(Location) + '\{#AppExeName}';
end;

function InitializeSetup(): Boolean;
begin
  Result := GracefulQuit(PreviousAppExe());
  if not Result then
    MsgBox('VoicePrompt could not exit safely. Finish dictation and close it before upgrading. No application was force-killed.', mbError, MB_OK);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not GracefulQuit(ExpandConstant('{app}\{#AppExeName}')) then
    Result := 'VoicePrompt did not exit safely. Installation stopped; user data is unchanged.';
end;

function InitializeUninstall(): Boolean;
begin
  Result := GracefulQuit(ExpandConstant('{app}\{#AppExeName}'));
end;

procedure CurUninstallStepChanged(Step: TUninstallStep);
begin
  if Step = usPostUninstall then
    Log('Tokens, settings, history, OAuth configuration and recovery under LocalAppData\VoicePrompt were preserved.');
end;
