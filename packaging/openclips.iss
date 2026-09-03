; Inno Setup script for OpenClips.
;
; Build the staged folder first:
;   powershell -File scripts\package.ps1 -BundleRuntime
; then compile this script with Inno Setup 6 (ISCC.exe packaging\openclips.iss).
; The installer ships the executable together with the GStreamer runtime
; staged by the packaging script, so nothing else has to be installed.

#define AppName "OpenClips"
#define AppVersion "0.1.0"
#define AppExe "openclips.exe"
#define StageDir "..\dist\OpenClips-" + AppVersion + "-win64"

[Setup]
AppId={{7F1C1E58-2A40-4C0D-9B1F-0C6B4B7D3E21}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=OpenClips contributors
AppPublisherURL=https://github.com/openclips/openclips
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
UninstallDisplayIcon={app}\{#AppExe}
OutputDir=..\dist
OutputBaseFilename=OpenClips-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
LicenseFile=..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "startup"; Description: "Launch OpenClips when Windows starts (minimized to the tray)"; GroupDescription: "Startup:"; Flags: unchecked

[Files]
Source: "{#StageDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\gstreamer\*"; DestDir: "{app}\gstreamer"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "OpenClips"; ValueData: """{app}\{#AppExe}"" --minimized"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start OpenClips now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "taskkill.exe"; Parameters: "/IM {#AppExe} /F"; Flags: runhidden; RunOnceId: "StopOpenClips"
