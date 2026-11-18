# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/).

## [0.8.2] - 2026-11-18

### Added

- Windows support: the configuration, data, download and app-source
  directories now follow `USERPROFILE`/`APPDATA`/`LOCALAPPDATA` instead of
  assuming a Unix home, and the file URL source test works on Windows.
  The workspace builds and its test suite passes on Windows 10 (GNU
  toolchain with MinGW-w64, and MSVC with Visual Studio Build Tools).

## [0.8.1] - 2026-11-11

First public release.  The crates are published under the `flipper-companion-`
namespace (`flipper-companion` for the desktop application,
`flipper-companion-cli` for the command-line tool, and the library crates),
each with its own README.

### Added

- USB discovery of Flipper Zero devices in VCP and DFU mode (pure Rust).
- Device information, storage, screen capture/mirroring, remote keypad and
  power/desktop/application RPC commands in the CLI and the Slint GUI.
- Firmware directory browsing, download, validation and installation on the
  SD card, plus `.dfu` flashing over DFU with progress and CRC validation.
- Backup and restore of the internal storage, factory reset, region
  provisioning and STM32WB55 recovery helpers.
- Application catalog: install, uninstall, reinstall and update Flipper
  applications from the official catalog and custom JSON repositories, with
  SHA-256 verification and per-row or batch updates.
- File manager with selection, properties, MD5 on demand, upload/download,
  rename, delete and directory creation.
- Application log tab (level filter, clear, jump to end, rotating log files),
  status bar with operation progress, and a Flipper Zero style keypad.
- `flip.toml` project support compatible with
  [flippy](https://github.com/elijah629/flippy) (`flipper project ...`).
- Manual pages and documentation under `docs/`.

### Security

- App downloads are verified against the SHA-256 published by the catalog
  before anything is written to the SD card.
- A pre-push hook checks that no personal or environment-specific data is
  about to be published.

[0.8.2]: https://github.com/zx97/flipper-companion/releases/tag/v0.8.2
[0.8.1]: https://github.com/zx97/flipper-companion/releases/tag/v0.8.1
