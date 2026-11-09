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
; Always a per-user install: devstack must never run elevated, and its folder must stay
; writable by the user (an elevated "all users" install would break both).
PrivilegesRequired=lowest
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
Name: "php"; Description: "PHP (several versions can be installed side by side)"; Types: recommended
Name: "php\v85"; Description: "PHP 8.5"
Name: "php\v84"; Description: "PHP 8.4"; Types: recommended
Name: "php\v83"; Description: "PHP 8.3"
Name: "php\v82"; Description: "PHP 8.2"
Name: "php\v81"; Description: "PHP 8.1"
Name: "php\v80"; Description: "PHP 8.0"
Name: "php\v74"; Description: "PHP 7.4"
Name: "php\v73"; Description: "PHP 7.3"
Name: "php\v72"; Description: "PHP 7.2"
Name: "php\v71"; Description: "PHP 7.1"
Name: "php\v70"; Description: "PHP 7.0"
Name: "db"; Description: "Databases"; Types: recommended
Name: "db\mysql"; Description: "MySQL"; Types: recommended
Name: "db\mysql\new"; Description: "MySQL Innovation (newest)"
Name: "db\mysql\v97"; Description: "MySQL 9.7 LTS"
Name: "db\mysql\v84"; Description: "MySQL 8.4 LTS"; Types: recommended
Name: "db\mysql\v80"; Description: "MySQL 8.0"
Name: "db\mysql\v57"; Description: "MySQL 5.7"
Name: "db\mysql\v56"; Description: "MySQL 5.6 (needs the Visual C++ 2010 runtime)"
Name: "db\mariadb"; Description: "MariaDB"
Name: "db\mariadb\v118"; Description: "MariaDB 11.8 LTS"
Name: "db\mariadb\v114"; Description: "MariaDB 11.4 LTS"
Name: "db\mariadb\v1011"; Description: "MariaDB 10.11 LTS"
Name: "db\mariadb\v106"; Description: "MariaDB 10.6 LTS"
Name: "db\pg"; Description: "PostgreSQL"
Name: "db\pg\v18"; Description: "PostgreSQL 18"
Name: "db\pg\v17"; Description: "PostgreSQL 17"
Name: "db\pg\v16"; Description: "PostgreSQL 16"
Name: "db\pg\v15"; Description: "PostgreSQL 15"
Name: "db\pg\v14"; Description: "PostgreSQL 14"
Name: "db\pg\v13"; Description: "PostgreSQL 13"
Name: "db\mongo"; Description: "MongoDB"
Name: "python"; Description: "Python (with pip and venv)"
Name: "python\v315"; Description: "Python 3.15"
Name: "python\v314"; Description: "Python 3.14"
Name: "python\v313"; Description: "Python 3.13"
Name: "python\v312"; Description: "Python 3.12"
Name: "python\v311"; Description: "Python 3.11"
Name: "python\v310"; Description: "Python 3.10"
Name: "python\v39"; Description: "Python 3.9"
Name: "python\v38"; Description: "Python 3.8"
Name: "node"; Description: "Node.js"
Name: "node\lts"; Description: "Node.js LTS (current)"
Name: "node\v22"; Description: "Node.js 22"
Name: "node\v20"; Description: "Node.js 20"
Name: "node\v18"; Description: "Node.js 18"
Name: "node\v16"; Description: "Node.js 16"
Name: "cache"; Description: "Caches"
Name: "cache\redis"; Description: "Redis"
Name: "cache\memcached"; Description: "Memcached"
Name: "tools"; Description: "Tools"; Types: recommended
Name: "tools\mailpit"; Description: "Mailpit (catches PHP mail)"; Types: recommended
Name: "tools\composer"; Description: "Composer"; Types: recommended
Name: "tools\composerlts"; Description: "Composer 2.2 LTS (needed for PHP 7.0 / 7.1)"
Name: "tools\phpmyadmin"; Description: "phpMyAdmin"; Types: recommended
Name: "tools\heidisql"; Description: "HeidiSQL"
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
Filename: "{app}\devstack.exe"; Description: "Open devstack now"; Flags: postinstall nowait skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{app}\ds.exe"; Parameters: "stop --force"; Flags: runhidden waituntilterminated; RunOnceId: "StopServices"
Filename: "{app}\ds.exe"; Parameters: "autostart off"; Flags: runhidden waituntilterminated; RunOnceId: "Autostart"
Filename: "{app}\ds.exe"; Parameters: "path remove"; Flags: runhidden waituntilterminated; RunOnceId: "Path"

[Code]
var
  VcPage: TDownloadWizardPage;
  VcDownloaded: Boolean;

{ Current Apache Lounge builds are made with Visual Studio 2026 (VS18) and need a 14.50+
  runtime; an older 2015-2022 runtime counts as missing. }
function VcRedistInstalled: Boolean;
var
  Installed, Minor: Cardinal;
begin
  Result := RegQueryDWordValue(HKLM64, 'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\X64', 'Installed', Installed) and (Installed = 1)
        and RegQueryDWordValue(HKLM64, 'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\X64', 'Minor', Minor) and (Minor >= 50);
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
    VcPage.Add('https://aka.ms/vc14/vc_redist.x64.exe', 'vc_redist.x64.exe', '');
    VcPage.Show;
    try
      try
        VcPage.Download;
        VcDownloaded := True;
      except
        MsgBox('The Visual C++ runtime could not be downloaded (' + GetExceptionMessage + ').' + #13#10 +
               'Install it later from https://aka.ms/vc14/vc_redist.x64.exe', mbInformation, MB_OK);
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
  { Order matters: of several versions of one tool, the first one listed becomes the active one. }
  AddIf(Result, 'php\v84', 'php-8.4');
  AddIf(Result, 'php\v85', 'php-8.5');
  AddIf(Result, 'php\v83', 'php-8.3');
  AddIf(Result, 'php\v82', 'php-8.2');
  AddIf(Result, 'php\v81', 'php-8.1');
  AddIf(Result, 'php\v80', 'php-8.0');
  AddIf(Result, 'php\v74', 'php-7.4');
  AddIf(Result, 'php\v73', 'php-7.3');
  AddIf(Result, 'php\v72', 'php-7.2');
  AddIf(Result, 'php\v71', 'php-7.1');
  AddIf(Result, 'php\v70', 'php-7.0');
  AddIf(Result, 'web\apache', 'apache');
  AddIf(Result, 'web\nginx', 'nginx-stable');
  AddIf(Result, 'db\mysql\v84', 'mysql-8.4');
  AddIf(Result, 'db\mysql\v97', 'mysql-9.7');
  AddIf(Result, 'db\mysql\new', 'mysql-innovation');
  AddIf(Result, 'db\mysql\v80', 'mysql-8.0');
  AddIf(Result, 'db\mysql\v57', 'mysql-5.7');
  AddIf(Result, 'db\mysql\v56', 'mysql-5.6');
  AddIf(Result, 'db\mariadb\v118', 'mariadb-11.8');
  AddIf(Result, 'db\mariadb\v114', 'mariadb-11.4');
  AddIf(Result, 'db\mariadb\v1011', 'mariadb-10.11');
  AddIf(Result, 'db\mariadb\v106', 'mariadb-10.6');
  AddIf(Result, 'db\pg\v18', 'postgresql-18');
  AddIf(Result, 'db\pg\v17', 'postgresql-17');
  AddIf(Result, 'db\pg\v16', 'postgresql-16');
  AddIf(Result, 'db\pg\v15', 'postgresql-15');
  AddIf(Result, 'db\pg\v14', 'postgresql-14');
  AddIf(Result, 'db\pg\v13', 'postgresql-13');
  AddIf(Result, 'db\mongo', 'mongodb');
  AddIf(Result, 'python\v313', 'python-3.13');
  AddIf(Result, 'python\v315', 'python-3.15');
  AddIf(Result, 'python\v314', 'python-3.14');
  AddIf(Result, 'python\v312', 'python-3.12');
  AddIf(Result, 'python\v311', 'python-3.11');
  AddIf(Result, 'python\v310', 'python-3.10');
  AddIf(Result, 'python\v39', 'python-3.9');
  AddIf(Result, 'python\v38', 'python-3.8');
  AddIf(Result, 'node\lts', 'nodejs-lts');
  AddIf(Result, 'node\v22', 'nodejs-22');
  AddIf(Result, 'node\v20', 'nodejs-20');
  AddIf(Result, 'node\v18', 'nodejs-18');
  AddIf(Result, 'node\v16', 'nodejs-16');
  AddIf(Result, 'cache\redis', 'redis');
  AddIf(Result, 'cache\memcached', 'memcached');
  AddIf(Result, 'tools\mailpit', 'mailpit');
  AddIf(Result, 'tools\composer', 'composer');
  AddIf(Result, 'tools\composerlts', 'composer-lts');
  AddIf(Result, 'tools\phpmyadmin', 'phpmyadmin');
  AddIf(Result, 'tools\heidisql', 'heidisql');
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

{ Only what devstack itself creates next to the exe. Never DelTree the whole folder: it may
  have been installed into a folder that also holds other files. }
procedure DeleteDevstackItem(Path: String);
begin
  if DirExists(Path) then
    DelTree(Path, True, True, True)
  else if FileExists(Path) then
    DeleteFile(Path);
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
      if MsgBox('Also delete devstack''s folders in ' + App + '?' + #13#10 + #13#10 +
                'This deletes your projects (www), databases (data), backups and downloaded tools ' +
                '(bin, etc, logs, tmp, usr, backup, settings.json). Other files in the folder are kept.' + #13#10 +
                'Choose No to keep everything.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      begin
        DeleteDevstackItem(App + '\bin');
        DeleteDevstackItem(App + '\etc');
        DeleteDevstackItem(App + '\data');
        DeleteDevstackItem(App + '\logs');
        DeleteDevstackItem(App + '\tmp');
        DeleteDevstackItem(App + '\usr');
        DeleteDevstackItem(App + '\backup');
        DeleteDevstackItem(App + '\www');
        DeleteDevstackItem(App + '\settings.json');
        RemoveDir(App); { only succeeds if nothing else is left }
      end;
    end;
  end;
end;
