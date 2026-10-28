; Inno Setup script for RC Manager on Windows.
; Built by CI: iscc /DAppVersion=<version> /DSrcDir=<folder with rc-manager.exe and clients\>
; Installs per user (no administrator rights, so the updater can update silently); the setup
; also offers an installation for all users.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SrcDir
  #define SrcDir "..\..\dist\RC-Manager-windows-x64"
#endif

[Setup]
AppId={{8F3E2C1A-5B7D-4E9A-9C21-6A0F4D2B7E13}
AppName=RC Manager
AppVersion={#AppVersion}
AppVerName=RC Manager {#AppVersion}
AppPublisher=StofflHD
AppPublisherURL=https://github.com/StofflHD/RC-Manager
AppSupportURL=https://github.com/StofflHD/RC-Manager/issues
AppUpdatesURL=https://github.com/StofflHD/RC-Manager/releases
AppCopyright=© StofflHD and Claude
DefaultDirName={autopf}\RC Manager
DefaultGroupName=RC Manager
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=rc-manager.ico
UninstallDisplayIcon={app}\rc-manager.exe
UninstallDisplayName=RC Manager
OutputDir=..\..\dist
OutputBaseFilename=RC-Manager-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SrcDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{group}\RC Manager"; Filename: "{app}\rc-manager.exe"
Name: "{autodesktop}\RC Manager"; Filename: "{app}\rc-manager.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\rc-manager.exe"; Description: "{cm:LaunchProgram,RC Manager}"; Flags: nowait postinstall skipifsilent
