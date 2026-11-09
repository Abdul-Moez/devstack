# Builds the release executables and the installer: dist\devstack-setup-<version>.exe
# Usage (from the repo root):  powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
Write-Host "Building devstack $version" -ForegroundColor Cyan

cargo test --release
if ($LASTEXITCODE -ne 0) { throw "tests failed" }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Host "Inno Setup not found; installing it with winget..." -ForegroundColor Yellow
    winget install --id JRSoftware.InnoSetup -e --accept-source-agreements --accept-package-agreements
    $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup install failed; install it from https://jrsoftware.org/isinfo.php" }
}

New-Item -ItemType Directory -Force dist | Out-Null
& $iscc "/DAppVersion=$version" "/DSourceDir=..\target\release" installer\devstack.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

Get-ChildItem dist\devstack-setup-*.exe | ForEach-Object {
    Write-Host ("Done: {0} ({1:N1} MB)" -f $_.FullName, ($_.Length / 1MB)) -ForegroundColor Green
}
