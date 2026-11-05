; Weather (Dreamnail v4) — Windows installer (Inno Setup).
;
; v4 has NO PyInstaller launcher: the app is the Flutter Release folder itself
; (dreamnail_ui.exe + the Flutter DLLs + the native engine dreamnail_ffi.dll +
; the sibling xray.exe + data\). This installs that whole folder to Program
; Files, adds Start Menu + optional desktop shortcuts, and best-effort whitelists
; the app in Windows Defender at install time (an installer already has the admin
; rights Add-MpPreference needs).
;
; Built in CI by build-v4.yml, which passes paths + version as /D defines.

#ifndef MyVersion
  #define MyVersion "0.0.0"
#endif
#ifndef MyOutputBase
  #define MyOutputBase "Weather-Installer"
#endif
#ifndef MyOutputDir
  #define MyOutputDir "out"
#endif
#ifndef MySourceDir
  #define MySourceDir "release"
#endif

[Setup]
; Stable AppId (distinct from v2's) so v4 upgrades replace cleanly.
AppId={{D4A8F1C6-9B2E-4A73-8F1D-3C5E7A9B0D24}
AppName=Weather
AppVersion={#MyVersion}
AppPublisher=Weather
DefaultDirName={autopf}\Weather
DefaultGroupName=Weather
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\dreamnail_ui.exe
UninstallDisplayName=Weather
OutputDir={#MyOutputDir}
OutputBaseFilename={#MyOutputBase}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64
#ifdef MyIcon
SetupIconFile={#MyIcon}
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
; The entire Flutter Release folder (exe + flutter DLLs + dreamnail_ffi.dll +
; xray.exe + data\).
Source: "{#MySourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{group}\Weather"; Filename: "{app}\dreamnail_ui.exe"
Name: "{group}\Uninstall Weather"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Weather"; Filename: "{app}\dreamnail_ui.exe"; Tasks: desktopicon

[Run]
; Best-effort Defender allow: the install dir + the per-user data dir the engine
; writes (%APPDATA%\Dreamnail). SilentlyContinue so Tamper Protection / no
; Defender can't fail the install.
Filename: "powershell.exe"; Parameters: "-NoProfile -WindowStyle Hidden -Command ""Add-MpPreference -ExclusionPath '{app}','{userappdata}\Dreamnail' -ErrorAction SilentlyContinue"""; StatusMsg: "Configuring Windows Security..."; Flags: runhidden waituntilterminated
Filename: "{app}\dreamnail_ui.exe"; Description: "Launch Weather"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "powershell.exe"; Parameters: "-NoProfile -WindowStyle Hidden -Command ""Remove-MpPreference -ExclusionPath '{app}','{userappdata}\Dreamnail' -ErrorAction SilentlyContinue"""; Flags: runhidden; RunOnceId: "DelDefenderExcl"
