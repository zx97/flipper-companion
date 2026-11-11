# Notices and attribution

## Upstream protocol definitions

`crates/flipper-protocol/proto/*.proto` are copied verbatim from
[flipperdevices/flipperzero-protobuf](https://github.com/flipperdevices/flipperzero-protobuf),
distributed under the GNU General Public License v3.0 or later. They describe
the wire protocol implemented by the Flipper Zero firmware.

## Behavioural reference

The USB, DFU, DfuSe and RPC behaviour implemented here was derived from
[flipperdevices/qFlipper](https://github.com/flipperdevices/qFlipper), also
GPL-3.0-or-later. No source file was copied; the implementation is a rewrite.

The power, desktop, application, GPIO, storage timestamp/tar-extract and
STM32WB55 recovery semantics were cross-checked against the
[flipperzero-firmware](https://github.com/flipperdevices/flipperzero-firmware)
RPC handlers (`applications/services/rpc/`). The firmware is GPL-3.0-or-later;
no source file was copied.

GPL-3.0-or-later code can be combined with an AGPL-3.0-or-later work, which is
why this project is licensed under the AGPL: the attribution above is kept for
the protocol definitions and the behavioural reference.

## flippy

`crates/flipper-project` is adapted from
[flippy](https://github.com/elijah629/flippy), MIT licensed,
(c) 2025 elijah629. The original MIT text is kept in
`crates/flipper-project/LICENSE-MIT`. The adapted code covers the shipped
`flip.toml` model, source grammar and fetch-plan generation.

## Icons

`crates/flipper-gui/ui/icons/` contains SVGs from the KDE **Breeze** icon theme
(`breeze-dark` variant), distributed under the GNU Lesser General Public
License v3.0 or later. See `crates/flipper-gui/ui/icons/README.md`.

## Trademarks

Flipper Zero is a product of Flipper Devices Inc. This project is independent
and is not endorsed by or affiliated with Flipper Devices.

## Rust dependencies

The dependency tree is dominated by MIT- and Apache-2.0-licensed crates. The
GPL-3.0-or-later licence of this project is compatible with those licences. The
main components are:

- `slint` — GPL-3.0 / royalty-free / commercial (this project uses the GPL
  option; the GPL and the AGPL are compatible);
- `tokio`, `prost`, `reqwest`, `serde`, `clap`, `nusb`, `tokio-serial` —
  MIT/Apache-2.0;
- `flate2`, `tar`, `sha2`, `md-5`, `hex`, `toml`, `url`, `tempfile` —
  MIT/Apache-2.0.

Run `cargo tree` for the exact dependency set and versions.
