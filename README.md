# oxinode

RNode-compatible LoRa modem firmware in Rust for the
[muzi.works](https://muzi.works) **Base Duo** (Nordic nRF52840 + Semtech
LR1121) with the Super IO expansion board.

A host running [Reticulum](https://reticulum.network/) sees the board as an
ordinary RNode over USB serial or Bluetooth LE: `rnsd` opens the port,
`rnodeconf` provisions it, and Sideband pairs with it, with no custom
interface driver and no patched Reticulum. The Super IO's OLED and navigation
pad give it an interface of its own. It replaces the Meshtastic firmware the
board ships with.

**Documentation: [paulmeier.github.io/oxinode](https://paulmeier.github.io/oxinode/)**

## What it does

- The KISS-framed RNode command set on the first USB serial port, the
  firmware's log on the second.
- The same byte stream over the Nordic UART Service for Sideband, with
  passkey pairing on the OLED and bonds stored in flash.
- `rnodeconf` provisioning (EEPROM image, device signature, TNC mode) in
  flash the bootloader never touches, so it survives a reflash.
- A stock RNode's air format: one header byte per frame, packets split at
  254 and reassembled, carrier sense with random backoff before every
  transmission.
- Radio parameters validated against what the module can actually do and
  refused rather than clamped; the module's 73 ppm reference error cancelled
  in arithmetic.
- Five screens on the 128 × 128 OLED driven by the six-switch pad, with every
  radio parameter editable when no host owns the radio.
- The Super IO's GPS, parsed on the board and shown on the Position screen.
- A host-side simulator that renders every screen and holds it to a golden
  image.

## Quick start

Prebuilt images are attached to every
[release](https://github.com/paulmeier/oxinode/releases). Double-tap the
board's reset button and copy `oxinode-rnode-*.uf2` onto the drive that
appears, then point an `RNodeInterface` at the first serial port. On macOS 26
the drive does not write through; flash over serial DFU instead:

```bash
rustup show && cargo install cargo-binutils --locked
cargo run --release --no-default-features --features ble --bin rnode
```

See [Flashing](https://paulmeier.github.io/oxinode/getting-started/flashing/)
and [Using it with Reticulum](https://paulmeier.github.io/oxinode/getting-started/reticulum/).

## Building and testing

```bash
tools/test.sh
```

runs every check that does not need a board: formatting, lints, the unit
tests, the golden images, the host tooling's tests, a build of every image
and a layout check of each. It is exactly what CI runs. The workspace is four
crates: the firmware (`src/`), `oxinode-core` (everything decidable without a
peripheral, tested on the host), `monopanel` (the on-device interface as a
crate with nothing of oxinode in it), and `oxinode-sim` (the simulator). See
[Local setup](https://paulmeier.github.io/oxinode/development/setup/) and
[Contributing](CONTRIBUTING.md).

## License

Released under the [Reticulum License](LICENSE), the license Reticulum itself
uses. The RNode protocol is implemented from Reticulum's host side as a
clean-room reimplementation; nothing from the GPL-licensed reference firmware
or from Meshtastic is copied. Nordic's SoftDevice Controller, linked for
Bluetooth, is a binary licensed for use on Nordic silicon. See
[License](https://paulmeier.github.io/oxinode/license/).
