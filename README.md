# plum

Minimalist terminal archive browser for Windows. **Everything is native** —
no ouch, no WSL at runtime. UI in [Ratatui](https://ratatui.rs).

## Features

- **View** archives: zip, 7z, rar, tar, tar.gz, tgz, tar.xz, tar.zst, tar.lz4, tar.bz2 —
  natively parsed, with per-file **sizes and dates** and recursive folder stats.
- **Fast Preview**: press `Space` or `p` to view text files or hex dump of binary files.
- **Search & Filter**: press `/` to search and filter files instantly.
- **Compress**: zip, 7z, tar.zst, tar.gz, tar.xz, tar.lz4, tar, tar.bz2 via `plum --compress` or Explorer context menu.
- **Password Protection**: AES-256 encryption for `.zip` and `.7z` with interactive TUI password management (`p` to edit, `v` to show/hide, `c` to clear).
- **Extract**: extract whole archive (`e`) or single selected entry (`x`) with native progress indicator.
- **Automatic UAC Elevation**: transparently requests administrator permissions if reading or extracting to protected system folders.
- Browse **plain folders** with the same tree/sizes UI.
- Theme via `theme.toml` (colors + terminal font for the WT profile).
- Registered as Explorer handler: Windows 11 modern context menu (COM `IExplorerCommand`),
  "Open with" for 13 extensions (`install.ps1`), and Windows Terminal "Plum" profile.

## CLI

```
plum <archive-or-folder>                                    # browse
plum --compress <files...>                                  # interactive compression UI (select format + level + password)
plum --check <archive>                                      # print listing stats (headless)
plum --extract <archive> <dir>                              # extract without UI (auto-elevates if needed)
plum --extract-single <archive> <entry> <dir>               # extract single item without UI
plum --pack <input> <out> <fmt> [level] [--password <pass>] # compress without UI (level: store|fast|normal|max)
```

Keys (Browse mode): `↑↓/jk` move · `Enter/→` open · `←/Backspace/Esc` up · `Space/p` preview · `/` search · `e` extract all · `x` extract selected · `?` help · `q` quit.
Keys (Compress mode): `↑↓/jk` format · `←→/hl` level · `p` password · `v` show/hide password · `c` clear password · `Enter` pack · `?` help · `Esc/q` quit.
Mouse: click to enter/select, right click to go up, wheel to scroll.

## Build & install

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1               # build via WSL toolchain
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Restart   # register in Windows & restart Explorer
```

The project cross-compiles from WSL targeting `x86_64-pc-windows-msvc`;
artifacts go to `~/plum-target` (ext4), and `plum.exe` + `plum_shext.dll` land in the repo root.

## Layout

```
src/lib.rs         — module root
src/native_list.rs — archive parsing/extraction (tar/zip/gz/xz/zst/lz4/bz2)
src/theme.rs       — theme.toml loading
src/tree.rs        — in-memory tree + cached subtree stats
src/app.rs         — state, event loop, rendering
src/main.rs        — thin entry point
install.ps1        — ProgID, associations, context menu, WT profile, conhost font
theme.toml         — colors + terminal font for the WT profile
```

## How it works

- Listing streams straight from `tar`/`zip` readers — decompression happens
  natively at NVMe speed; no external processes.
- Subtree stats (items/bytes per folder) are cached in each tree node and
  invalidated along the insertion path.
- `theme.toml` also carries `font = <face>`, which `install.ps1` bakes into the
  Windows Terminal "Plum" profile fragment.