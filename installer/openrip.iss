; Inno Setup script for the OpenRip Windows installer.
; Built in CI (see .github/workflows/build.yml) with:
;   iscc /DAppVersion=0.1.0 /DSourceDir=<folder holding all the files> installer\openrip.iss
; SourceDir must contain: openrip.exe, ffmpeg.exe, ffprobe.exe, the libbluray / libaacs
; DLLs (and their dependencies), a licenses\ folder, and README.md.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "dist"
#endif

[Setup]
AppId={{6F1C2D84-3B7A-4E0B-9C55-0A7E4D2B91F3}
AppName=OpenRip
AppVersion={#AppVersion}
AppPublisher=OpenRip
DefaultDirName={autopf}\OpenRip
DefaultGroupName=OpenRip
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\openrip.exe
SetupIconFile=..\packaging\windows\openrip.ico
OutputDir=output
OutputBaseFilename=OpenRip-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Install for all users when run as administrator, or just for the current user otherwise.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
WizardStyle=modern
InfoAfterFile=after-install.txt

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; The app, FFmpeg, and the Blu-ray libraries all sit in one folder, which is where OpenRip
; looks for them first.
Source: "{#SourceDir}\openrip.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ffprobe.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs skipifsourcedoesntexist
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "THIRD-PARTY.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Dirs]
; Where the AACS key file goes (see the notes shown after installing).
Name: "{userappdata}\aacs"

[Icons]
Name: "{group}\OpenRip"; Filename: "{app}\openrip.exe"
Name: "{autodesktop}\OpenRip"; Filename: "{app}\openrip.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\openrip.exe"; Description: "{cm:LaunchProgram,OpenRip}"; Flags: nowait postinstall skipifsilent
