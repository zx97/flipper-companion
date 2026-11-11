# Migration from qFlipper

This project is a rewrite, not a port. The table below tracks feature parity so
that the scope is explicit.

## 1. Module mapping

| qFlipper | flipper-companion | Status |
|---|---|---|
| `dfu/dfusefile.*` | `flipper-dfu/src/dfuse.rs` | Rewritten, stricter |
| `dfu/dfumemorylayout.*` | `flipper-dfu/src/layout.rs` | Rewritten, bounded |
| `dfu/dfusedevice.*` | `flipper-dfu/src/protocol.rs` | Rewritten as a generic state machine |
| `dfu/libusb/*`, `dfu/win32/*` | `flipper-dfu/src/usb.rs` (`nusb`) | Unified, pure Rust |
| `backend/deviceregistry.*` | `flipper-companion-app::Companion::scan` | Rewritten |
| `backend/flipperzero/*` | `flipper-companion-protocol` + `flipper-core::device` | Split by responsibility |
| `backend/flipperzero/protobufsession.*` | `flipper-protocol/src/session.rs` (actor) | Rewritten |
| `plugins/flipperproto0` | `flipper-protocol/proto` + `prost` | Build-time codegen, no plugin |
| `backend/flipperzero/recovery*` | — | Planned (STM32WB55 FUS) |
| `backend/flipperzero/utility/*` | — | Partially replaced by `flipper-companion-app`; upload flow planned |
| `backend/flipperzero/toplevel/*` | `flipper-companion-app` orchestration | Partial: flash path done |
| `backend/flipperupdates.*`, `updateregistry.*` | `flipper-companion-update` | Rewritten |
| `backend/versioninfo.*` | `flipper-core/src/version.rs` | Rewritten with `Ord` |
| `application/*.qml` | `flipper-gui/ui/app.slint` | Minimal UI, growing |
| `application/applicationupdater.*` | — | Planned (self-update) |
| `cli/*` | `flipper-companion-cli` | Rewritten with clap |
| `driver-tool/*` | — | Not planned: `nusb` requires WinUSB, not a custom driver |

## 2. Feature matrix

| Feature | qFlipper | flipper-companion | Notes |
|---|---|---|---|
| Device discovery | ✅ | ✅ | VCP + DFU |
| Firmware/device info | ✅ | ✅ | Modern + legacy property sets |
| Flash `.dfu` | ✅ | ✅ | CRC validated before USB |
| Install from `.dfu` file | ✅ | ✅ (CLI), ✅ (GUI, DFU only) | |
| Reboot OS/DFU/update | ✅ | ✅ | |
| File manager | ✅ | ✅ CLI + GUI | list/stat/info/read/write/mkdir/rm/mv/md5/timestamp/tar-extract |
| Screen streaming | ✅ | ✅ CLI capture + GUI live | ASCII/PBM export and 128×64 RGBA mirror |
| Virtual display / remote input | ✅ | ✅ (screen mirror + keys) | Up/Down/Left/Right/OK/Back |
| Backup internal storage | ✅ | ✅ CLI + GUI | qFlipper-compatible `int/` `.tgz` |
| Restore internal storage | ✅ | ✅ CLI + GUI | two-step confirmation |
| Firmware update from directory | ✅ | ✅ direct DFU and SD-manifest flow | `update_tgz` extraction, MD5 verified upload, `System_update` |
| Repair / recovery mode | ✅ | ✅ read-only info; writes implemented, hardware validation pending | OTP, option bytes, FUS, radio/FUS flashing |
| Region provisioning | ✅ | ✅ CLI | official band database |
| Factory reset | ✅ | ✅ CLI + GUI | `factory_reset` |
| Power / battery information | ✅ | ✅ CLI + GUI detail | `System_power_info` |
| Desktop lock / app control / GPIO | partial | ✅ CLI | firmware extras; writes require `--yes` |
| Project-side `flip.toml` (flippy) | — | ✅ CLI | new/map/store, flippy-compatible format |
| CLI | ✅ | ✅ | Different grammar, JSON output |
| Self-update | ✅ | ❌ planned | Needs packaging/signing |
| Windows installer | ✅ | ❌ planned | |
| macOS bundle | ✅ | ❌ planned | |
| Linux AppImage | ✅ | ❌ planned | |
| Translations | ✅ | ❌ planned | Slint supports gettext |
| Tests | ❌ | ✅ 88 tests | No hardware required |

### 2.1 flippy compatibility

The shipped `flippy` CLI manages a project-side `flip.toml` and a `store/`
directory. Its functions are available under the `project` namespace:

| flippy | flipper-companion |
|---|---|
| `flippy new <path>` | `flipper project new <path>` |
| `flippy map firmware set <source>` | `flipper project map firmware set <source>` |
| `flippy map repo add <source> [name]` | `flipper project map repo add <source> [name]` |
| `flippy store create` | `flipper project store create` |
| `flippy store fetch [--optimize]` | `flipper project store fetch [--optimize]` |

The `flip.toml` format, source grammar (`ofw`, `unleashed`, `momentum`,
`rogue-free`, HTTP(S), `file://`, `path://`, SCP-style Git remotes) and the
fetch-plan output are compatible. The unfinished `upload`/`git` modules in the
flippy repository are not wired into its shipped CLI and are therefore not
ported; repository synchronisation remains a future extension.

## 3. Behavioural differences

1. **CRC is mandatory.** A `.dfu` whose CRC does not match is rejected before
   any USB transfer. qFlipper only warns during parsing.
2. **Strict bounds checks.** Element sizes, target sizes, target counts and
   memory layouts are validated against the file and against a 64 MiB cap.
3. **No dynamic plugin.** Protocol messages are compiled into the binary. This
   removes the `QPluginLoader` failure mode and deployment complexity.
4. **Typed errors.** Callers cannot ignore a failure silently; there is no
   boolean-plus-error-string convention.
5. **No blocking on the UI thread.** The GUI worker owns all I/O.
6. **Progress semantics.** DFU progress is split 50 % erase / 50 % write, like
   the original. Downloads report the server's `Content-Length`; when absent,
   the bar stays at zero rather than guessing.
7. **DFU serial selection.** The CLI prefers the requested serial, then accepts
   another DFU device (the ST bootloader may report a different serial number).
8. **No final manifest request.** Mirrors qFlipper to stay compatible with the
   bootloader.
9. **Device probing.** `scan` opens a short RPC session per VCP device. If the
   port is busy, the device is still listed with an explanatory error instead
   of disappearing.

## 4. Roadmap

### Phase 1 — foundation (done)

- Domain model, DfuSe/DFU, protobuf RPC, update directory.
- CLI: devices, info, validate, flash, storage, screen, reboot, firmware.
- GUI: device list, details, `.dfu` flash, update check, log.
- 88 tests, clippy-clean, formatted, CI.

### Phase 2 — complete update flow (done)

- [x] GUI channel selector and update installation.
- [x] Persist the selected channel in `~/.config/flipper-companion/config.toml`.
- [x] GUI backup/restore, factory reset and alert.
- [x] Live screen mirroring and key input in the GUI.
- [x] Remote file browser in the GUI.
- [x] Power, desktop, app and GPIO commands.
- [x] flippy-compatible project configuration.

- [x] Upload an extracted `update_tgz` bundle to `/ext/update/<dir>` using
  `Storage_write`.
- [x] Call `System_update` with the remote `update.fuf` path and follow the
  `UpdateResponse` result codes.
- [x] Skip unchanged files by comparing MD5 checksums, verify every upload.
- [ ] Handle the "SD card missing" and "not enough space" paths in the GUI.
- [ ] Hardware validation of the full SD-card update path.
- [ ] GUI: changelog, channel selector, install button with progress.

### Phase 3 — recovery (read validated, writes pending)

- [x] STM32WB55 option bytes (nBOOT0/nSWBOOT0) and boot mode.
- [x] FUS state, radio version tables, factory/OTP information.
- [x] FUS commands, radio/FUS image flashing, explicit recovery exit.
- [x] Read-only recovery path validated on hardware (factory info, option
  bytes, FUS state, radio versions, exit).
- [ ] Validate option-byte writes and radio/FUS flashing on hardware.
- [ ] Full automatic repair flow (backup → recovery → firmware → radio →
  restore). The individual operations are available through the CLI.

### Phase 4 — companion features (mostly done)

- [x] Backup/restore of the internal storage as `.tgz` bundles.
- [x] GUI file manager and continuous screen streaming.
- [x] Interactive remote input (keypad).
- [x] Region provisioning and factory reset.
- [x] Settings persistence for the update channel.
- [ ] Keyboard mapping and frame orientation handling for unusual firmware.
- [ ] Theme/log-level settings.

### Phase 5 — distribution

- AppImage, DMG and Windows installer.
- Code signing and notarisation.
- Self-update with signature verification.
- Translations.
- Hardware-in-the-loop CI (a self-hosted runner with a Flipper).

## 5. Known limitations

- The GUI flashes only devices already in DFU mode; use the CLI or the
  `reboot` command first.
- The GUI has no file picker yet; the `.dfu` path is typed or pasted. A native
  dialog can be added without touching the worker.
- `flipper firmware flash` assumes the `full_dfu` artifact exists for the
  selected target; it does not fall back to `any` (full images are
  target-specific).
- `flipper firmware install` requires a running firmware with a mounted SD
  card; the upload/`System_update` path has been exercised on hardware, but a
  full update run still needs a final end-to-end validation.
- Recovery writes (option bytes, radio/FUS images) are implemented but have
  not been run on hardware. Only the read-only recovery path and the DFU exit
  were validated.
- The GUI screen mirror uses the raw frame bytes; unusual screen orientations
  are not rotated yet.
- Device probing requires read/write access to `/dev/ttyACM*`; the udev rule
  covers the usual case.
- Windows requires the WinUSB driver for the DFU interface; the official
  installer handled this with `libwdi`. Deployment documentation is still to be
  written.
