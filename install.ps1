# Registers plum in Windows:
#  1) ProgID "Plum.Archive" + "Open with" entries for all supported archive types
#  2) Windows 11 Modern Context Menu (COM IExplorerCommand overriding Windows CompressTo)
#  3) Classic context menu "Compress to…" and "Archive with Plum" (fallback / Shift+F10)
#  4) Windows Terminal profile "Plum" (font + color scheme from theme.toml)
#  5) Classic conhost font fallback for plum console windows
# Usage:
#   powershell -ExecutionPolicy Bypass -File .\install.ps1            # install all
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -Restart   # install + restart Explorer
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -Remove    # uninstall
param([switch]$Remove, [switch]$NoMenu, [switch]$Restart)

$exe = Join-Path $PSScriptRoot 'plum.exe'
$shextDll = Join-Path $PSScriptRoot 'plum_shext.dll'

if (-not $Remove) {
    if (-not (Test-Path $exe)) { Write-Error "plum.exe not found next to this script: $exe"; exit 1 }
    if (-not (Test-Path $shextDll)) { Write-Error "plum_shext.dll not found next to this script: $shextDll"; exit 1 }
}

# ---- parse theme.toml (colors + font) ---------------------------------------
$theme = @{ bg = '#181725'; surface = '#3F414E'; text = '#EBEAEC'; dim = '#A1A4B2'; accent = '#8E97FD'; ok = '#6CB28E'; err = '#FA6E5A'; font = 'Cascadia Mono' }
$themePath = Join-Path $PSScriptRoot 'theme.toml'
if (-not (Test-Path $themePath)) {
    $appTheme = Join-Path $env:APPDATA 'plum\theme.toml'
    if (Test-Path $appTheme) { $themePath = $appTheme }
}
if (Test-Path $themePath) {
    Get-Content $themePath | ForEach-Object {
        if ($_ -match '^\s*(\w+)\s*=\s*(.+?)\s*$') {
            $k = $Matches[1].ToLower(); $v = $Matches[2].Trim('"')
            if ($theme.ContainsKey($k)) { $theme[$k] = $v }
        }
    }
}

$exts = 'zip','7z','tar','gz','tgz','xz','zst','lz4','bz2','bz3','lzma','sz','br'
$progid = 'HKCU\Software\Classes\Plum.Archive'
$compressClsid = '{7AE6900F-6EB0-44A2-9CA1-DB2F7EF352AF}'
$clsidKey = "HKCU\Software\Classes\CLSID\$compressClsid"

if ($Remove) {
    reg delete 'HKCU\Software\Classes\Plum.Archive' /f 2>$null | Out-Null
    reg delete 'HKCU\Software\Classes\Applications\plum.exe' /f 2>$null | Out-Null
    reg delete 'HKCU\Software\Classes\Applications\plum-tui.exe' /f 2>$null | Out-Null
    reg delete 'HKCU\Console\plum' /f 2>$null | Out-Null
    reg delete 'HKCU\Console\plum-tui' /f 2>$null | Out-Null
    reg delete $clsidKey /f 2>$null | Out-Null
    reg delete 'HKCU\Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Blocked' /v '{b8cdcb65-b1bf-4b42-9428-1dfdb7ee92af}' /f 2>$null | Out-Null
    foreach ($e in $exts) {
        reg delete "HKCU\Software\Classes\.$e\OpenWithProgids" /v Plum.Archive /f 2>$null | Out-Null
    }
    reg delete 'HKCU\Software\Classes\Directory\shell\ArchiveWithPlum' /f 2>$null | Out-Null
    reg delete 'HKCU\Software\Classes\*\shell\ArchiveWithPlum' /f 2>$null | Out-Null
    reg delete 'HKCU\Software\Classes\Directory\shell\CompressWithPlum' /f 2>$null | Out-Null
    reg delete 'HKCU\Software\Classes\*\shell\CompressWithPlum' /f 2>$null | Out-Null

    # Remove WT fragments
    $fragDirs = @(
        (Join-Path $env:LOCALAPPDATA 'Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\Fragments'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\Windows Terminal\Fragments')
    )
    foreach ($d in $fragDirs) {
        $f = Join-Path $d 'plum.json'
        if (Test-Path $f) { Remove-Item $f -Force -ErrorAction SilentlyContinue }
    }

    Write-Host 'Removed: ProgID, COM handler, associations, context menus, terminal profile.'
    if ($Restart) {
        Stop-Process -Name explorer -Force
    }
    exit 0
}

# ---- 1) ProgID + file associations ------------------------------------------
$cmd = '\"' + $exe + '\" \"%1\"'

reg add $progid /ve /d 'Plum Archive' /f | Out-Null
reg add "$progid\shell\open\command" /ve /d $cmd /f | Out-Null
reg add 'HKCU\Software\Classes\Applications\plum.exe\shell\open\command' /ve /d $cmd /f | Out-Null

foreach ($e in $exts) {
    reg add "HKCU\Software\Classes\.$e" /ve /d 'Plum.Archive' /f | Out-Null
    reg add "HKCU\Software\Classes\.$e\OpenWithProgids" /v Plum.Archive /t REG_SZ /d 'Plum Archive' /f | Out-Null
}

# ---- 2) COM Modern Context Menu (Windows 11 top-level "Compress to…") --------
if (-not $NoMenu) {
    # Override Windows 11 CLSID_CompressToCommand with Plum's IExplorerCommand DLL.
    # This places Plum's "Compress to…" in the modern top-level menu and eliminates Windows' native menu.
    reg add $clsidKey /ve /d 'Plum Compress Command' /f | Out-Null
    reg add "$clsidKey\InProcServer32" /ve /d $shextDll /f | Out-Null
    reg add "$clsidKey\InProcServer32" /v 'ThreadingModel' /d 'Apartment' /f | Out-Null

    # Block legacy Windows "Send to Compressed (zipped) folder" shell extension
    reg add 'HKCU\Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Blocked' /v '{b8cdcb65-b1bf-4b42-9428-1dfdb7ee92af}' /d 'Windows Compressed Folder' /f | Out-Null

    # Classic / Shift+F10 fallback verbs
    reg add 'HKCU\Software\Classes\Directory\shell\ArchiveWithPlum' /ve /d 'Archive with Plum' /f | Out-Null
    reg add 'HKCU\Software\Classes\Directory\shell\ArchiveWithPlum' /v 'MultiSelectModel' /d 'Document' /f | Out-Null
    reg add 'HKCU\Software\Classes\Directory\shell\ArchiveWithPlum\command' /ve /d $cmd /f | Out-Null
    reg add 'HKCU\Software\Classes\*\shell\ArchiveWithPlum' /ve /d 'Archive with Plum' /f | Out-Null
    reg add 'HKCU\Software\Classes\*\shell\ArchiveWithPlum' /v 'MultiSelectModel' /d 'Document' /f | Out-Null
    reg add 'HKCU\Software\Classes\*\shell\ArchiveWithPlum\command' /ve /d $cmd /f | Out-Null

    $cargs = '\"' + $exe + '\" --compress \"%1\"'
    reg add 'HKCU\Software\Classes\Directory\shell\CompressWithPlum' /ve /d 'Compress to…' /f | Out-Null
    reg add 'HKCU\Software\Classes\Directory\shell\CompressWithPlum' /v 'MultiSelectModel' /d 'Document' /f | Out-Null
    reg add 'HKCU\Software\Classes\Directory\shell\CompressWithPlum\command' /ve /d $cargs /f | Out-Null
    reg add 'HKCU\Software\Classes\*\shell\CompressWithPlum' /ve /d 'Compress to…' /f | Out-Null
    reg add 'HKCU\Software\Classes\*\shell\CompressWithPlum' /v 'MultiSelectModel' /d 'Document' /f | Out-Null
    reg add 'HKCU\Software\Classes\*\shell\CompressWithPlum\command' /ve /d $cargs /f | Out-Null
}

# ---- 3) Windows Terminal profile fragment ------------------------------------
$scheme = [ordered]@{
    name        = 'Plum'
    background  = $theme.bg
    foreground  = $theme.text
    cursorColor = $theme.accent
    selectionBackground = $theme.surface
    black       = $theme.bg
    white       = $theme.text
    green       = $theme.ok
    red         = $theme.err
    blue        = $theme.accent
    brightBlack = $theme.dim
}

$fragment = [ordered]@{
    profiles = @(
        [ordered]@{
            name         = 'Plum'
            commandline  = "`"$exe`""
            font         = @{ face = $theme.font }
            colorScheme  = 'Plum'
            icon         = $exe
        }
    )
    schemes = @($scheme)
}

$fragDirs = @(
    (Join-Path $env:LOCALAPPDATA 'Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\Fragments'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\Windows Terminal\Fragments')
)
foreach ($d in $fragDirs) {
    New-Item -ItemType Directory -Force $d | Out-Null
    [IO.File]::WriteAllText((Join-Path $d 'plum.json'), ($fragment | ConvertTo-Json -Depth 5))
}

# ---- 4) classic conhost fallback (plum console window title) -----------------
reg add 'HKCU\Console\plum' /v FaceName /d $theme.font /f | Out-Null
reg add 'HKCU\Console\plum' /v FontSize /t REG_DWORD /d 0x00100000 /f | Out-Null

Write-Host "Installed: associations for $($exts -join ', ')"
Write-Host "COM Shell Extension registered: $shextDll"
Write-Host "Windows Terminal profile 'Plum' created (font: $($theme.font))"
Write-Host ''
if ($Restart) {
    Write-Host 'Restarting Windows Explorer to apply context menu changes...'
    Stop-Process -Name explorer -Force
} else {
    Write-Host 'Tip: To apply the new context menu immediately, restart Explorer:'
    Write-Host '  powershell -ExecutionPolicy Bypass -File .\install.ps1 -Restart'
}