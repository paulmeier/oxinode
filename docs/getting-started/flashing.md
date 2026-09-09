# Flashing

oxinode runs on a Base Duo that still has its factory bootloader. Nothing here
needs a debug probe, and nothing here can overwrite the bootloader, so a bad
flash is always recoverable with a double-tap of the reset button.

## Prebuilt images

Every [tagged release](https://github.com/paulmeier/oxinode/releases) carries
`.uf2` files for each image, the `.elf` files they were built from, and a
`SHA256SUMS`. Every CI run on `main` also uploads the same set as a build
artifact, which is the easier way to try an unreleased change.

| Image | What it is |
|---|---|
| `oxinode-rnode-*.uf2` | The product image: RNode over USB and Bluetooth, the panel, the pad, the GPS |
| `oxinode-radio-*.uf2` | A radio bring-up diagnostic with a serial console |
| `oxinode-usb-cdc-*.uf2` | USB serial echo, to prove the USB path |
| `oxinode-blink-*.uf2` | Blinks the green LED, to prove flashing |

See [Firmware images](../architecture/images.md) for what each does.

## Over the UF2 drive

1. Double-tap the reset button. A drive named `muzi-Base` appears.
2. Copy the `.uf2` onto it. The board reboots into the new firmware.

!!! warning "Not on macOS 26"
    The bootloader's drive is not a real FAT volume. It is a synthetic
    filesystem that exists only to catch 512-byte UF2 blocks and write them
    to flash. macOS 26 serves FAT through FSKit in userspace, and against a
    shim like this the writes never reach the chip: the copy succeeds, the
    file appears in `ls`, and the board keeps running its old firmware with
    no error anywhere. Use serial DFU instead.

The bootloader also silently discards any UF2 block whose family id it does
not recognise, and then never reboots. This bootloader accepts `0x239A0081`
(formed from the board's USB VID and PID) and `0xADA52840` (the generic
Adafruit nRF52840 id). It rejects the `0x1B57745F` that the microsoft/uf2
registry lists for the nRF52840. A third id, `0xD663823C`, rewrites the
bootloader itself; never emit it. `tools/uf2-flash.sh` reads the right id from
the board's own `CURRENT.UF2` rather than trusting a constant.

## Over serial DFU

This is the path the repository's `cargo run` uses, and the one that works
everywhere. It goes through the bootloader's SLIP-framed serial DFU interface
using `adafruit-nrfutil`, which `tools/dfu-flash.sh` bootstraps into
`tools/.venv/` on first use if it is not on your `PATH`.

From a checkout, with the [toolchain installed](../development/setup.md):

```bash
cargo run --release --bin blink
```

The product image carries the Bluetooth stack and needs its feature set:

```bash
cargo run --release --no-default-features --features ble --bin rnode
```

`cargo run` builds the ELF, checks it against `memory.x`, packages it, and
sends it down `/dev/cu.usbmodem*`. With two boards attached the runner refuses
to guess; name the port:

```bash
OXINODE_DFU_PORT=/dev/cu.usbmodemXXXX cargo run --release --no-default-features --features ble --bin rnode
```

To flash a prebuilt or previously built ELF directly:

```bash
tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rnode
```

### Getting into the bootloader

The first time, double-tap the reset button. Once an image with USB serial is
on the board, no button is needed: the runner performs a **1200-baud touch**
(opens the first serial port at 1200 baud and closes it), which every oxinode
image with a serial port answers by rebooting into the bootloader. The
firmware ignores the touch for the first two seconds after boot, because macOS
caches the 1200-baud line setting per device path and would otherwise send a
freshly flashed board straight back to its bootloader.

A board already sitting in its bootloader on a host that cannot mount the UF2
drive needs to be told so:

```bash
OXINODE_DFU_IN_DFU=1 tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rnode
```

!!! note "Disconnect a phone first"
    Taking the 1200-baud touch on the product image while a phone is
    connected over Bluetooth has been seen to leave the board in its
    bootloader with serial DFU not answering. Disconnect the phone or reset
    the board before reflashing. See [Known limitations](../reference/limitations.md).

## Checking what is on the chip

The bootloader publishes the chip's current flash contents as `CURRENT.UF2`,
so "did that flash take?" has an answer. Double-tap reset, then:

```bash
tools/verify_flash.py target/thumbv7em-none-eabihf/release/rnode
```

It prints the vector table found on the board next to the one you built and
says `MATCH` or `MISMATCH`.

## Recovery

Double-tap reset. The bootloader lives above the application region and no
oxinode image can reach it. If an image panics, it reboots into the bootloader
on its own, so a board that went quiet after a flash is usually already
sitting in DFU waiting to be reflashed.

There is no DFU button on this board and never was: the bootloader's
`BUTTON_1` and `BUTTON_2` are both an unconnected pin. Double-tap reset and
the software path are the only two ways in.
