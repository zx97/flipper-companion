# Contributing

## Before opening a pull request

```sh
make check      # fmt-check + clippy + test
make build
```

or, equivalently:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

The first three commands must be clean. CI runs the same set. `make check`
requires the `rustfmt` and `clippy` components; install them with
`rustup component add rustfmt clippy` if needed.

## Ground rules

- **No `unsafe`.** If a dependency forces it, isolate it and document why.
- **No global mutable state.** Pass configuration and clients explicitly.
- **One error enum per crate.** Use `thiserror`; convert to `anyhow` only at
  binary boundaries.
- **Domain types at the edges.** Do not expose `prost` types or `nusb` types
  from `flipper-companion-app`; map them to `flipper-companion-core` types.
- **Tests for behaviour, not coverage.** A parser change comes with a
  malformed-input test; a protocol change comes with a fake-device transcript.
- **No new dependency without a reason.** Prefer the standard library and the
  crates already in `Cargo.toml`.

## Hardware tests

Unit tests never open a USB or serial device. Manual validation commands that
touch hardware:

```sh
flipper devices
flipper power
flipper screen --format pbm --output /dev/shm/frame.pbm
flipper backup /dev/shm/internal.tgz
flipper recovery info        # device must be in DFU mode
```

Recovery writes (option bytes, radio/FUS images) and `factory-reset` are
destructive: use a disposable device and double-check the target. The GUI is
validated with `--tab=1 --stream` for screen mirroring.

## Adding a project (flippy) feature

`crates/flipper-project` mirrors flippy's shipped `flip.toml` behaviour. Keep
the file format and source grammar compatible; add round-trip tests for new
fields.

## Adding an RPC command

1. Check that the message exists in `crates/flipper-protocol/proto`.
2. Add a method to `RpcSession` in `session.rs`. Keep it short: build the
   `Content`, call `call`/`call_one`, match the response.
3. Map the result to a domain type in `flipper-companion-core` when a domain type exists.
4. Extend `spawn_fake_device` in the session tests with a branch for the new
   request and assert the typed result.

## Adding a DFU or parser feature

1. Add the data model to `flipper-companion-dfu`.
2. Write the failing test first. For protocol changes, extend `MockTransport`
   so the expected control transfers are asserted explicitly.
3. Validate real images with:

   ```sh
   FLIPPER_TEST_DFU=/path/to/full_firmware.dfu \
       cargo test -p flipper-companion-dfu --test real_image
   ```

## Commit messages

Use the imperative mood and a short scope:

```text
dfu: validate target sizes before flashing
rpc: collect multi-part property responses
gui: throttle progress events to one per percent
```

## Documentation

- Code comments and documentation are in English.
- Architecture and protocol changes must update `docs/architecture.md` or
  `docs/protocol.md`.
- Feature parity changes must update `docs/migration.md`.
