# Local setup

Everything except flashing works without a board. The host checks run on
macOS and Linux; the flashing scripts are written for macOS's bash 3.2 and
work on Linux too.

## Rust

`rust-toolchain.toml` pins the stable channel with the components and the
target the workspace needs, so `rustup` installs them on first use:

```bash
rustup show          # installs stable, rust-src, rustfmt, clippy, llvm-tools, thumbv7em-none-eabihf
cargo install cargo-binutils --locked
```

`cargo-binutils` supplies `rust-objcopy`, which turns an ELF into the raw
binary the UF2 wraps. The minimum Rust version is 1.85.

The workspace's `.cargo/config.toml` sets the default build target to
`thumbv7em-none-eabihf` (the nRF52840's Cortex-M4F), so building the host
crates needs the host target named. A shell alias helps:

```bash
export OXINODE_HOST="$(rustc -vV | sed -n 's/^host: //p')"
alias sim='cargo run -q -p oxinode-sim --target "$OXINODE_HOST" --'
```

## Python

The host tools under `tools/` are Python 3 with no dependencies beyond the
standard library, except the flasher, which needs `adafruit-nrfutil`. The
flash script bootstraps that into `tools/.venv/` on first use if it is not on
your `PATH`; to install it yourself:

```bash
pip install adafruit-nrfutil
```

## Optional

- **`shellcheck`**, for the lint step over `tools/*.sh`. `tools/test.sh`
  skips it with a note if it is not installed.
- **`defmt-print`**, to decode the firmware's log port:
  `cargo install defmt-print`.
- **MkDocs**, to build this documentation locally:
  `pip install -r docs/requirements.txt`, then `mkdocs serve`.

## A board

A muzi.works Base Duo with the Super IO expansion board, running its factory
bootloader. Nothing else: no debug probe, no programmer. See
[Flashing](../getting-started/flashing.md).

For bench work on the radio, an RTL-SDR is enough to see a carrier and
measure its frequency, and a second Base Duo (running anything, including
stock Meshtastic) is enough to exercise receive. See [The radio](../hardware/radio.md).

## Verify

```bash
tools/test.sh
```

runs every check that does not need a board and takes a few seconds on a
warm tree. See [Testing](testing.md).
