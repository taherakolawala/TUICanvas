# One-time setup, then starts the development watcher in this window.
#   powershell -ExecutionPolicy Bypass -File scripts\setup.ps1
# 1. Installs Rust (GNU toolchain, no Visual Studio needed) if cargo is missing.
# 2. Creates the initial git history and pushes it, if this folder isn't a repo yet.
# 3. Runs the watcher (Ctrl+C to stop).
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (($env:Path -split ';') -notcontains $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host 'Installing Rust (GNU toolchain)...' -ForegroundColor Cyan
    $installer = Join-Path $env:TEMP 'rustup-init.exe'
    Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $installer
    & $installer -y --default-host x86_64-pc-windows-gnu --default-toolchain stable --profile minimal
    if ($LASTEXITCODE -ne 0) { throw 'Rust installation failed.' }
}
& cargo --version

if (-not (Test-Path (Join-Path $root '.git'))) {
    Write-Host 'Creating the initial commits...' -ForegroundColor Cyan
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root '.tools\initial-commits.ps1')
    if ($LASTEXITCODE -ne 0) { Write-Host 'Initial commits did not complete; see the message above. Continuing.' -ForegroundColor Yellow }
}

& (Join-Path $PSScriptRoot 'watch.ps1')
