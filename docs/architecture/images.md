# Firmware images

`src/bin/` holds six images. One is the product; the rest exist because the
board has no debug probe, and a failure you can bisect by choosing which image
to flash is worth the duplication.

| Image | Feature set | What it is for |
|---|---|---|
| `rnode` | `ble` | **The product.** RNode over USB and Bluetooth, the panel, the pad, the GPS, provisioning. |
| `radio` | default | Radio bring-up diagnostic with a serial console. |
| `display` | default | OLED bring-up diagnostic. |
| `ble` | `ble` | Bluetooth stack bring-up diagnostic, built to be debugged without a probe. |
| `usb-cdc` | default | Two serial ports, echo on the first, log on the second. Proves the USB path. |
| `blink` | default | Blinks the green LED. Proves the toolchain, the linker script and flashing. |

The two feature sets cannot be built in one invocation because they select
different `critical-section` implementations; see [Building](../development/building.md).

## `rnode`

Two CDC-ACM ports (KISS on the first, `defmt` log on the second), the Nordic
UART Service carrying the same KISS stream, the LR1121 driven through the
protocol state machine, the SH1107 panel drawn from modem state, the pad, the
GPS, and the device record. Its structure is on the
[overview](overview.md). A board whose radio does not come up still runs the
interface with `no radio` where the radio state goes, so it can still be
provisioned and looked at.

## `radio`

Exposes a single CDC port, waits for a terminal to open it (so its one-shot
startup log is not written into a port with no reader), then walks the whole
[radio bring-up](../hardware/radio.md) and says what it found: SPI pin
selection read back from the peripheral, the reset and BUSY trace, the chip's
identity and firmware version, oscillator startup, the RF switch masks, the
interrupt line proved by a deliberately provoked `cmd_error`. It then takes
single-character commands on the same port. The radio's parameters are a
value rather than constants, so this is also how a configuration is tried on
the bench:

| Key | What it does |
|---|---|
| `1` `2` `3` | Continuous carrier at −17, 0 and +14 dBm |
| `0` | Stop transmitting |
| `S` `W` `C` `P` | Cycle spreading factor, bandwidth, coding rate, power |
| `[` `]` | Step the frequency down or up by 100 kHz, saturating at the band edges |
| `R` | Toggle the correction for the module's 73 ppm reference error |
| `N` | Switch sync word between `0x12` (RNode) and `0x2b` (Meshtastic) |
| `M` `D` | Load the Meshtastic LongFast preset, or the default |
| `A` | Apply the current configuration without transmitting |
| `p` | Send one LoRa packet with the current configuration |
| `y` `z` `E` | Listen; sweep the frequency coarsely; sweep the receive window's edge |
| `t` `?` | Die temperature and supply; chip status and configuration |
| `r` | Reboot the radio and redo the bring-up |
| `b` | Reboot into the bootloader |

Cycling rather than typing, because the console reads raw bytes with no line
editing, and a key that always does something is easier to read back in a
log. An invalid intermediate configuration is kept and reported, not
reverted; a host setting one parameter at a time is entitled to hold one.
Every step of a sweep logs the frequency it actually tuned to.

The `M` preset and the `oxinode_core::meshtastic` module that computes its
frequency (Meshtastic hashes the channel name with djb2 and takes the
remainder over the channel count) are bench scaffolding for receiving from a
stock Meshtastic board, not a feature.

This image puts raw frames on the air, with no RNode header; it is for
proving the chip transmits and receives at all.

## `display`

Brings the I²C bus up, reads the pins back from the peripheral's registers,
scans the bus with the 12 V rail off and on, draws an asymmetric test
pattern (a corner square, a wide short bar, a staircase) that settles which
way the axes run, and then renders a status page. See
[The display](../hardware/display.md).

## `ble`

Brings the Bluetooth stack up in a way that can be debugged without a probe,
and keeps four things that were built for that purpose:

- **A keystroke gate.** The bring-up is never attempted on its own; it waits
  for a byte on the log port. A board that comes up is therefore always an
  enumerated, reflashable board.
- **A breadcrumb** in `GPREGRET2`, written before the bring-up and cleared
  after, so the next boot can say the last one did not come back.
- **Fault and unhandled-interrupt handlers** that record the cause and reboot
  into the bootloader, so a crash is visible from the host and looks different
  from a stall.
- **A stall capture.** `TIMER1` is armed around the bring-up; if it fires, its
  handler reads the interrupted program counter out of the exception frame,
  snapshots the `POWER`/`CLOCK` interrupt state, writes them to RAM the linker
  does not zero, and resets into the application, which reports them on the
  next keystroke.

See [Debugging without a probe](../development/debugging.md) and
[Bluetooth](bluetooth.md).

## `usb-cdc` and `blink`

`usb-cdc` enumerates two ports and echoes on the first, including across the
64-byte packet boundary that needs a zero-length packet behind it and the
`0xC0`/`0xDB` bytes KISS treats as delimiters. It also takes the 1200-baud
touch. `blink` proves that an image links at `0x26000`, boots behind the
SoftDevice, and keeps time on the external crystal.
