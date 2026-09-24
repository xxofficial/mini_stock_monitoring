#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName "微行情"
#define MyAppExeName "微行情.exe"
#define MyReleaseDir "..\dist\MiniStockMonitor"

[Setup]
AppId={{F80D6D2B-153C-42C4-889C-42787350376B}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=xxofficial
AppPublisherURL=https://github.com/xxofficial/mini_stock_monitoring
AppSupportURL=https://github.com/xxofficial/mini_stock_monitoring/issues
AppUpdatesURL=https://github.com/xxofficial/mini_stock_monitoring/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename={#MyAppName}-{#MyAppVersion}-windows-x64-setup
SetupIconFile=..\assets\mini-stock-monitor.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
AppMutex=MiniStockMonitor.Desktop.1
VersionInfoDescription={#MyAppName}安装程序
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}

[Languages]
Name: "chinesesimp"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加选项："; Flags: checkedonce

[Files]
Source: "{#MyReleaseDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyReleaseDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyReleaseDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "启动{#MyAppName}"; Flags: nowait postinstall skipifsilent
