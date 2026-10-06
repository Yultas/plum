# Rebuilds plum.exe and plum_shext.dll via cross-compilation from WSL (Alpine).
# Usage: .\build.ps1   or   .\build.ps1 -Run   (build + launch)
param([switch]$Run)

$ErrorActionPreference = "Stop"

# Stop any running plum processes to unlock plum.exe
Stop-Process -Name plum -Force -ErrorAction SilentlyContinue

# If plum_shext.dll is loaded by Explorer, unlock it
$dllPath = Join-Path $PSScriptRoot 'plum_shext.dll'
$restartedExplorer = $false
if (Test-Path $dllPath) {
    try {
        $stream = [System.IO.File]::OpenWrite($dllPath)
        $stream.Close()
    } catch {
        Write-Host "Releasing DLL lock from Explorer..."
        Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
        $restartedExplorer = $true
        Start-Sleep -Milliseconds 600
    }
}

$script = 'export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:$HOME/.cargo/bin:$PATH; ' +
    'export CARGO_TARGET_DIR=$HOME/plum-target; ' +
    'cd /mnt/c/Users/yulta/.dev/plum; ' +
    'cargo xwin build --release --target x86_64-pc-windows-msvc && ' +
    'cp -f $HOME/plum-target/x86_64-pc-windows-msvc/release/plum.exe ./plum.exe && ' +
    'cd /mnt/c/Users/yulta/.dev/plum/shext && ' +
    'cargo xwin build --release --target x86_64-pc-windows-msvc && ' +
    'cp -f $HOME/plum-target/x86_64-pc-windows-msvc/release/plum_shext.dll /mnt/c/Users/yulta/.dev/plum/plum_shext.dll && ' +
    'echo BUILD_OK'

wsl -e sh -c $script
if ($LASTEXITCODE -ne 0) { Write-Error "Build failed (exit $LASTEXITCODE)" }

if ($restartedExplorer) {
    if (-not (Get-Process -Name explorer -ErrorAction SilentlyContinue)) {
        Start-Process explorer
    }
}

if ($Run) { Start-Process ".\plum.exe" }