; ---------------------------------------------------------------------------
;  VoicePrompt — non-Store, per-user, x64 installer (Inno Setup 6)
;
;  Build:
;      iscc /DSourceDir="..\publish\win-x64" VoicePrompt.iss
;
;  Design notes
;  * PrivilegesRequired=lowest  -> installs under %LOCALAPPDATA%\Programs, no UAC prompt.
;  * The app is framework-dependent: the .NET 8 Desktop Runtime must be present. The
;    installer checks for it and points the user at the official download instead of
;    silently producing a broken install.
;  * Only the Desktop OAuth client JSON is bundled — never the Android or Web client, and
;    never the .secrets folder. It is staged locally by scripts/Configure-OAuth.ps1 and is
;    optional, so an unconfigured build still produces a working (sign-in disabled) app.
;  * User data in %LOCALAPPDATA%\VoicePrompt (tokens, history, settings) is preserved on
;    upgrade and left in place on uninstall.
; ---------------------------------------------------------------------------

#define AppName        "VoicePrompt"
#define AppVersion     "1.4.3"
#define AppPublisher   "tomaskubica"
#define AppExeName     "VoicePrompt.exe"
#define AppId          "{{9C1E7A54-3E77-4B27-9E4A-3A5C1E2C0B41}"

#ifndef SourceDir
  #define SourceDir "..\publish\win-x64"
#endif

#ifndef OutputDir
  #define OutputDir "output"
#endif

; The Desktop OAuth client JSON is optional at build time.
#define DesktopClientJson "staging\google-desktop-client.json"
#ifndef PublicRelease
#if FileExists(AddBackslash(SourcePath) + DesktopClientJson)
  #define HaveDesktopClient
#endif
#endif

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=no
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#AppExeName}
OutputDir={#OutputDir}
OutputBaseFilename=VoicePrompt-Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Per-user install: no elevation, no machine-wide state.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; x64 only.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=yes
RestartApplications=no
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startupicon"; Description: "Start {#AppName} automatically when I sign in to Windows"; GroupDescription: "Startup:"; Flags: checkedonce
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\*.dll";         DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\*.json";        DestDir: "{app}"; Excludes: "google-desktop-client.json,*client_secret*.json"; Flags: ignoreversion
Source: "{#SourceDir}\Assets\*";      DestDir: "{app}\Assets"; Flags: ignoreversion recursesubdirs createallsubdirs
#ifdef HaveDesktopClient
; Sensitive operational config: the Desktop OAuth client only. Never the Android/Web
; clients and never the repository's .secrets folder.
Source: "{#DesktopClientJson}";       DestDir: "{app}"; DestName: "google-desktop-client.json"; Flags: ignoreversion
#endif

[Icons]
Name: "{group}\{#AppName}";                     Filename: "{app}\{#AppExeName}"
Name: "{group}\Uninstall {#AppName}";           Filename: "{uninstallexe}"
Name: "{userdesktop}\{#AppName}";               Filename: "{app}\{#AppExeName}"; Tasks: desktopicon
; NOTE: deliberately no {userstartup} shortcut. Auto-start is owned by exactly one
; mechanism — the HKCU ...\Run value below — which the app's Settings toggle reads and
; writes (VoicePrompt.App RegistryAutoStartManager). A Startup-folder shortcut would be
; a second, app-invisible mechanism and would launch the tray app twice at sign-in.

[Registry]
; Per-user auto-start: the single source of truth. The app itself manages this value from
; its Settings tab; the installer only seeds it, and always removes it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
    ValueName: "VoicePrompt"; ValueData: """{app}\{#AppExeName}"""; \
    Flags: uninsdeletevalue; Tasks: startupicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "Start {#AppName} now"; \
    Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{app}\Assets"
Type: files;          Name: "{app}\google-desktop-client.json"

[InstallDelete]
; Upgrade hygiene: installers up to 1.0.0 also dropped a Startup-folder shortcut. Remove
; it so upgraded machines are left with exactly one auto-start mechanism (HKCU Run) and
; do not launch the tray app twice at sign-in.
Type: files; Name: "{userstartup}\{#AppName}.lnk"

[Messages]
; Unsigned build: warn honestly rather than hide it.
WelcomeLabel2=This will install [name/ver] on your computer.%n%nThis build is not code-signed, so Windows SmartScreen may warn you the first time you run the installer. Choose "More info" and then "Run anyway" if you trust this build.%n%nVoicePrompt installs for the current user only and does not require administrator rights.

[Code]

const
  DotNetDownloadUrl = 'https://dotnet.microsoft.com/download/dotnet/8.0';

{ The app is framework-dependent, so verify the .NET 8 Desktop Runtime is installed. }
function DesktopRuntimeInstalled(): Boolean;
var
  Base: String;
  Find: TFindRec;
begin
  Result := False;
  Base := ExpandConstant('{commonpf64}\dotnet\shared\Microsoft.WindowsDesktop.App');
  if not DirExists(Base) then
    Exit;

  if FindFirst(Base + '\8.*', Find) then
  begin
    try
      repeat
        if (Find.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0 then
        begin
          Result := True;
          Exit;
        end;
      until not FindNext(Find);
    finally
      FindClose(Find);
    end;
  end;
end;

{ Locate a previously installed VoicePrompt.exe via its uninstall registration. }
function PreviousAppExe(): String;
var
  Location: String;
begin
  Result := '';
  if RegQueryStringValue(HKCU,
       ExpandConstant('Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppId}_is1'),
       'InstallLocation', Location) then
  begin
    Location := RemoveBackslashUnlessRoot(Location);
    if FileExists(Location + '\{#AppExeName}') then
      Result := Location + '\{#AppExeName}';
  end;
end;

{ Ask a running tray instance to shut down cleanly so an upgrade never force-kills it and
  never blocks a silent install. `--quit` returns only once the app has actually exited. }
procedure QuitRunningInstance(const Exe: String);
var
  ResultCode: Integer;
begin
  if (Exe <> '') and FileExists(Exe) then
  begin
    Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(750);
  end;
end;

function InitializeSetup(): Boolean;
var
  Answer: Integer;
begin
  { Do this first: a running instance holds the executable and would fail a silent upgrade. }
  QuitRunningInstance(PreviousAppExe());

  Result := True;
  if DesktopRuntimeInstalled() then
    Exit;

  Answer := MsgBox(
    'VoicePrompt needs the .NET 8 Desktop Runtime (x64), which was not found.' + #13#10#13#10 +
    'Open the download page now?' + #13#10#13#10 +
    'Choose No to continue installing anyway (the app will not start until the runtime is present).',
    mbConfirmation, MB_YESNOCANCEL);

  if Answer = IDYES then
  begin
    ShellExecAsOriginalUser('open', DotNetDownloadUrl, '', '', SW_SHOWNORMAL, ewNoWait, Answer);
    Result := False;
  end
  else if Answer = IDCANCEL then
    Result := False;
end;

{ Belt and braces: the target directory may differ from the recorded install location. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  QuitRunningInstance(ExpandConstant('{app}\{#AppExeName}'));
end;

{ Never delete user data (tokens/history/settings) — an uninstall may precede a reinstall. }
function InitializeUninstall(): Boolean;
begin
  { Before any file-in-use check: ask the tray app to exit on its own. }
  QuitRunningInstance(ExpandConstant('{app}\{#AppExeName}'));
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    QuitRunningInstance(ExpandConstant('{app}\{#AppExeName}'));

  if CurUninstallStep = usPostUninstall then
    Log('User data under %LOCALAPPDATA%\VoicePrompt was intentionally preserved.');
end;
