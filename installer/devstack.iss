; devstack installer (Inno Setup 6). Build with scripts\build-installer.ps1, which passes
; /DAppVersion and /DSourceDir. Produces dist\devstack-setup-<version>.exe.
;
; What it does:
;   * asks for an install folder (default C:\devstack; must be writable, so not Program Files)
;   * installs only devstack.exe, ds.exe and the uninstaller
;   * lets the user pick which tools to download right away (runs `ds setup`)
;   * offers: desktop icon, add tools to PATH, run at Windows startup
;   * installs the Visual C++ runtime if it's missing (Apache/PHP/MySQL need it)
;   * uninstall stops services and asks before deleting projects and databases

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif

[Setup]
AppId={{8C3E5F7A-2B1D-4E6F-9A0C-DE57AC4B1F10}
AppName=devstack
AppVersion={#AppVersion}
AppPublisher=devstack
DefaultDirName={sd}\devstack
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist
OutputBaseFilename=devstack-setup-{#AppVersion}
SetupIconFile=..\assets\devstack.ico
UninstallDisplayIcon={app}\devstack.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
DirExistsWarning=no

[Types]
Name: "recommended"; Description: "Recommended (Apache, PHP 8.4, MySQL 8.4, Mailpit, Composer, phpMyAdmin)"
Name: "minimal"; Description: "Just devstack (add tools later with Quick add)"
Name: "custom"; Description: "Choose for myself"; Flags: iscustom

[Components]
Name: "web"; Description: "Web server"; Types: recommended
Name: "web\apache"; Description: "Apache"; Types: recommended; Flags: exclusive
Name: "web\nginx"; Description: "Nginx"; Flags: exclusive
Name: "php"; Description: "PHP"; Types: recommended
Name: "php\v85"; Description: "PHP 8.5"
Name: "php\v84"; Description: "PHP 8.4"; Types: recommended
Name: "php\v83"; Description: "PHP 8.3"
Name: "php\v82"; Description: "PHP 8.2"
Name: "db"; Description: "Databases"; Types: recommended
Name: "db\mysql84"; Description: "MySQL 8.4 LTS"; Types: recommended
Name: "db\mysql9"; Description: "MySQL 9.x (Innovation)"
Name: "db\pg17"; Description: "PostgreSQL 17"
Name: "db\pg18"; Description: "PostgreSQL 18"
Name: "db\mongo"; Description: "MongoDB"
Name: "cache"; Description: "Caches"
Name: "cache\redis"; Description: "Redis"
Name: "cache\memcached"; Description: "Memcached"
Name: "tools"; Description: "Tools"; Types: recommended
Name: "tools\mailpit"; Description: "Mailpit (catches PHP mail)"; Types: recommended
Name: "tools\composer"; Description: "Composer"; Types: recommended
Name: "tools\phpmyadmin"; Description: "phpMyAdmin"; Types: recommended
Name: "tools\heidisql"; Description: "HeidiSQL"
Name: "tools\node"; Description: "Node.js LTS"
Name: "tools\git"; Description: "Git (MinGit)"
Name: "tools\ngrok"; Description: "ngrok (for Share)"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"
Name: "addpath"; Description: "Add php, composer, node, mysql... to my PATH"; Flags: unchecked
Name: "autostart"; Description: "Run devstack when Windows starts"; Flags: unchecked

[Files]
Source: "{#SourceDir}\devstack.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ds.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\devstack"; Filename: "{app}\devstack.exe"
Name: "{autodesktop}\devstack"; Filename: "{app}\devstack.exe"; Tasks: desktopicon

[Run]
Filename: "{tmp}\vc_redist.x64.exe"; Parameters: "/install /quiet /norestart"; StatusMsg: "Installing the Visual C++ runtime..."; Check: VcRedistDownloaded; Flags: shellexec waituntilterminated
Filename: "{app}\ds.exe"; Parameters: "setup --pause-on-error --packages {code:SetupPackages}"; StatusMsg: "Downloading the selected tools (this can take a few minutes)..."; Check: HasPackages; Flags: waituntilterminated
Filename: "{app}\ds.exe"; Parameters: "path add"; Tasks: addpath; Flags: runhidden waituntilterminated
Filename: "{app}\ds.exe"; Parameters: "autostart on"; Tasks: autostart; Flags: runhidden waituntilterminated
Filename: "{app}\devstack.exe"; Description: "Open devstack now"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "{app}\ds.exe"; Parameters: "stop --force"; Flags: runhidden waituntilterminated; RunOnceId: "StopServices"
Filename: "{app}\ds.exe"; Parameters: "autostart off"; Flags: runhidden waituntilterminated; RunOnceId: "Autostart"
Filename: "{app}\ds.exe"; Parameters: "path remove"; Flags: runhidden waituntilterminated; RunOnceId: "Path"

[Code]
var
  VcPage: TDownloadWizardPage;
  VcDownloaded: Boolean;

function VcRedistInstalled: Boolean;
var
  Installed: Cardinal;
begin
  Result := RegQueryDWordValue(HKLM64, 'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\X64', 'Installed', Installed) and (Installed = 1);
end;

function VcRedistDownloaded: Boolean;
begin
  Result := VcDownloaded;
end;

procedure InitializeWizard;
begin
  VcPage := CreateDownloadPage('Visual C++ runtime', 'Apache, PHP and MySQL need the Microsoft Visual C++ runtime.', nil);
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = wpSelectDir) and (Pos(LowerCase(ExpandConstant('{commonpf}')), LowerCase(WizardDirValue)) = 1) then
  begin
    MsgBox('devstack keeps your projects and databases next to the program, so it needs a folder you can write to.' + #13#10 +
           'Please choose a folder outside Program Files, for example C:\devstack.', mbError, MB_OK);
    Result := False;
    exit;
  end;
  if (CurPageID = wpReady) and not VcRedistInstalled then
  begin
    VcPage.Clear;
    VcPage.Add('https://aka.ms/vs/17/release/vc_redist.x64.exe', 'vc_redist.x64.exe', '');
    VcPage.Show;
    try
      try
        VcPage.Download;
        VcDownloaded := True;
      except
        MsgBox('The Visual C++ runtime could not be downloaded (' + GetExceptionMessage + ').' + #13#10 +
               'Install it later from https://aka.ms/vs/17/release/vc_redist.x64.exe', mbInformation, MB_OK);
      end;
    finally
      VcPage.Hide;
    end;
  end;
end;

procedure AddIf(var S: String; Comp, Id: String);
begin
  if WizardIsComponentSelected(Comp) then
  begin
    if S <> '' then S := S + ',';
    S := S + Id;
  end;
end;

function PackageList: String;
begin
  Result := '';
  AddIf(Result, 'php\v85', 'php-8.5');
  AddIf(Result, 'php\v84', 'php-8.4');
  AddIf(Result, 'php\v83', 'php-8.3');
  AddIf(Result, 'php\v82', 'php-8.2');
  AddIf(Result, 'web\apache', 'apache');
  AddIf(Result, 'web\nginx', 'nginx-stable');
  AddIf(Result, 'db\mysql84', 'mysql-8.4');
  AddIf(Result, 'db\mysql9', 'mysql-innovation');
  AddIf(Result, 'db\pg17', 'postgresql-17');
  AddIf(Result, 'db\pg18', 'postgresql-18');
  AddIf(Result, 'db\mongo', 'mongodb');
  AddIf(Result, 'cache\redis', 'redis');
  AddIf(Result, 'cache\memcached', 'memcached');
  AddIf(Result, 'tools\mailpit', 'mailpit');
  AddIf(Result, 'tools\composer', 'composer');
  AddIf(Result, 'tools\phpmyadmin', 'phpmyadmin');
  AddIf(Result, 'tools\heidisql', 'heidisql');
  AddIf(Result, 'tools\node', 'nodejs-lts');
  AddIf(Result, 'tools\git', 'git');
  AddIf(Result, 'tools\ngrok', 'ngrok');
end;

function SetupPackages(Param: String): String;
begin
  Result := PackageList;
end;

function HasPackages: Boolean;
begin
  Result := PackageList <> '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  App: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    App := ExpandConstant('{app}');
    if DirExists(App + '\www') or DirExists(App + '\data') or DirExists(App + '\bin') then
    begin
      if MsgBox('Also delete everything devstack created in ' + App + '?' + #13#10 + #13#10 +
                'This includes your projects (www), databases (data), backups and downloaded tools.' + #13#10 +
                'Choose No to keep them.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(App, True, True, True);
    end;
  end;
end;
