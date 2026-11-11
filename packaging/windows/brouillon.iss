; Brouillon - Inno Setup 6 script. Per-user install, no admin.
; Authenticode signing is OPTIONAL: it only happens when build-release.ps1 passes /DSign=1 together
; with /Sbrouillon="<command>" (see packaging/README.md). Otherwise the installer is unsigned.
; Build via packaging/build-release.ps1 (passes the /D defines below).
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BuildNumber
  #define BuildNumber "0"
#endif
#ifndef SourceExe
  #error SourceExe must be defined (/DSourceExe=path\to\brouillon.exe)
#endif
#ifndef LicenseFile
  #define LicenseFile "..\..\LICENSE"
#endif
#ifndef OutDir
  #define OutDir "..\..\site\public\downloads"
#endif
#ifndef IconFile
  #define IconFile "..\..\assets\brand\brouillon.ico"
#endif
#define NumericVersion Copy(AppVersion, 1, Pos("-", AppVersion + "-") - 1)

[Setup]
AppId={{7B3E5C1A-4D2F-4E8B-9A61-2F0C8D5E7B14}
AppName=Brouillon
AppVersion={#AppVersion} (build {#BuildNumber})
AppVerName=Brouillon {#AppVersion}
AppPublisher=Laurent Rochetta
AppPublisherURL=https://brouillon.rochetta.fr
AppSupportURL=https://brouillon.rochetta.fr
AppUpdatesURL=https://brouillon.rochetta.fr
DefaultDirName={autopf}\Brouillon
DefaultGroupName=Brouillon
; Per user, ALWAYS: the automatic updater runs this setup silently and no elevation prompt may ever appear.
; (No PrivilegesRequiredOverridesAllowed: setup can never switch to an all-users install.)
PrivilegesRequired=lowest
CloseApplications=yes
DisableProgramGroupPage=yes
LicenseFile={#LicenseFile}
OutputDir={#OutDir}
OutputBaseFilename=Brouillon-{#AppVersion}-build{#BuildNumber}-win-x64-setup
Compression=lzma2/ultra
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\brouillon.exe
VersionInfoVersion={#NumericVersion}.{#BuildNumber}
VersionInfoProductName=Brouillon
VersionInfoProductVersion={#NumericVersion}.{#BuildNumber}
VersionInfoCompany=Laurent Rochetta
VersionInfoDescription=Brouillon setup (build {#BuildNumber})
VersionInfoCopyright=Copyright (c) 2026 Laurent Rochetta (MIT)
ChangesEnvironment=yes
; Tell Explorer to refresh when the optional 'Open with' entries are (un)registered.
ChangesAssociations=yes
#ifdef Sign
SignTool=brouillon
SignedUninstaller=yes
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "addtopath"; Description: "Add Brouillon to the user PATH"; GroupDescription: "Options:"; Flags: unchecked
Name: "assoc"; Description: "Add Brouillon to the 'Open with' list of common text files (.txt .md .log .json .toml .yaml .csv); never changes your default app"; GroupDescription: "File associations:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "brouillon.exe"; Flags: ignoreversion
Source: "{#LicenseFile}"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
#ifdef McpExe
Source: "{#McpExe}"; DestDir: "{app}"; DestName: "brouillon-mcp.exe"; Flags: ignoreversion
#endif
#ifdef HelperExe
Source: "{#HelperExe}"; DestDir: "{app}"; DestName: "brouillon-remote.exe"; Flags: ignoreversion
#endif

[Icons]
Name: "{autoprograms}\Brouillon"; Filename: "{app}\brouillon.exe"; IconFilename: "{app}\brouillon.exe"; IconIndex: 0

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

; Optional 'Open with' registration, per user (HKCU\Software\Classes). It only adds Brouillon to the
; list; it never takes over the default handler. Removed on uninstall.
Root: HKCU; Subkey: "Software\Classes\Brouillon.TextFile"; ValueType: string; ValueName: ""; ValueData: "Text file (Brouillon)"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\Brouillon.TextFile\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\brouillon.exe,0"; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\Brouillon.TextFile\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\brouillon.exe"" ""%1"""; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.txt\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.md\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.log\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.json\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.toml\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.yaml\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCU; Subkey: "Software\Classes\.csv\OpenWithProgids"; ValueType: none; ValueName: "Brouillon.TextFile"; Flags: uninsdeletevalue; Tasks: assoc

[Run]
Filename: "{app}\brouillon.exe"; Description: "Launch Brouillon"; Flags: nowait postinstall skipifsilent
; Silent update started by Brouillon itself (brouillon-update passes /RELAUNCH=1): reopen the new version.
Filename: "{app}\brouillon.exe"; Flags: nowait runasoriginaluser; Check: RelaunchRequested

[Code]
function RelaunchRequested: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:RELAUNCH|0}') = '1');
end;

function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  P, App: string;
  I: Integer;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    if RegQueryStringValue(HKCU, 'Environment', 'Path', P) then
    begin
      App := ExpandConstant('{app}');
      I := Pos(';' + Uppercase(App), Uppercase(P));
      if I > 0 then
      begin
        Delete(P, I, Length(App) + 1);
        RegWriteExpandStringValue(HKCU, 'Environment', 'Path', P);
      end;
    end;
  end;
end;
