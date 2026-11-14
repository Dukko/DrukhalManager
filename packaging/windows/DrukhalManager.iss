; The Windows installer, built with Inno Setup 6 from the folder
; bundle.sh makes:
;
;     ISCC /DAppVersion=0.9.0 /DBundleDir=<bundle folder> /O<output folder> DrukhalManager.iss
;
; It installs for the current user only, so it needs no administrator
; rights. Installing a newer version over an older one updates it in place.

#ifndef AppVersion
  #error Pass the version: /DAppVersion=X.Y.Z
#endif
#ifndef BundleDir
  #error Pass the bundle folder: /DBundleDir=...
#endif

; The file version has to be numbers only: 0.10.0-beta.1 becomes 0.10.0.
#define NumericVersion Copy(AppVersion, 1, Pos("-", AppVersion + "-") - 1)

[Setup]
; Identifies the app to Windows across versions; never change it.
AppId={{5C247286-C456-498E-9D23-1CCBBE21CB07}
AppName=DrukhalManager
AppVersion={#AppVersion}
AppVerName=DrukhalManager {#AppVersion}
AppPublisher=Dukko
AppPublisherURL=https://github.com/Dukko/DrukhalManager
AppSupportURL=https://github.com/Dukko/DrukhalManager/issues
AppUpdatesURL=https://github.com/Dukko/DrukhalManager/releases
VersionInfoVersion={#NumericVersion}
PrivilegesRequired=lowest
; With PrivilegesRequired=lowest, {autopf} is %LOCALAPPDATA%\Programs.
DefaultDirName={autopf}\DrukhalManager
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=drukhal-manager.ico
UninstallDisplayIcon={app}\bin\DrukhalManager.exe
UninstallDisplayName=DrukhalManager
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputBaseFilename=DrukhalManager-setup-x64
LicenseFile={#BundleDir}\LICENSE

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; GTK's files from the version before, so none it no longer ships is left
; behind to load. Settings and data live elsewhere and stay.
Type: filesandordirs; Name: "{app}\bin"
Type: filesandordirs; Name: "{app}\lib"
Type: filesandordirs; Name: "{app}\share"

[Files]
Source: "{#BundleDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\DrukhalManager"; Filename: "{app}\bin\DrukhalManager.exe"
Name: "{autodesktop}\DrukhalManager"; Filename: "{app}\bin\DrukhalManager.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\bin\DrukhalManager.exe"; Description: "{cm:LaunchProgram,DrukhalManager}"; Flags: nowait postinstall skipifsilent
