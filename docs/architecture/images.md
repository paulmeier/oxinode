# Firmware images

`src/bin/` holds eight images. One is the product; the rest exist because the
board has no debug probe, and a failure you can bisect by choosing which image
to flash is worth the duplication.

| Image | Feature set | What it is for |
|---|---|---|
| `rnode` | `ble` | **The product.** RNode over USB and Bluetooth, the panel, the pad, the GPS, provisioning. |
| `radio` | default | Radio bring-up diagnostic with a serial console. |
| `display` | default | OLED bring-up diagnostic. |
| `qspi` | default | QSPI flash bring-up diagnostic, and a cycle for measuring its deep power-down. |
| `buzzer` | default | Buzzer bring-up: P0.22 held high or driven with a square wave, to tell an active buzzer from a passive one by ear. |
| `ble` | `ble` | Bluetooth stack bring-up diagnostic, built to be debugged without a probe. |
| `usb-cdc` | default | Two serial ports, echo on the first, log on the second. Proves the USB path. |
| `blink` | default | Blinks the green LED. Proves the toolchain, the linker script and flashing. |

The two feature sets cannot be built in one invocation because they select
different `critical-section` implementations; see [Building](../development/building.md).

## `rnode`

One CDC-ACM port for KISS, the `defmt` log on a vendor interface beside it
(so the board has one `tty`; see [Debugging](../development/debugging.md#the-log)), the Nordic
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
| `[` `]` | Step the frequency down or up by 100 kHz, saturating at the edges of the band it is in |
| `R` | Toggle the correction for the module's 73 ppm reference error |
| `N` | Switch sync word between `0x12` (RNode) and `0x2b` (Meshtastic) |
| `M` `H` `D` | Load the Meshtastic LongFast preset, the 2.4 GHz bench preset, or the default |
| `A` | Apply the current configuration without transmitting |
| `B` | Toggle receive boost, on by default; takes effect at the next apply, transmit or listen |
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

## `qspi`

Exposes a single CDC port, like `radio`. Before anything else it reads the
W25Q128's JEDEC ID on plain GPIOs, which says whether the last boot left the
chip in deep power-down: a reset of the nRF52 does not wake it, and a
sleeping chip leaves its output undriven, so the ID reads as zeros. It then
wakes the chip (`ABh`), reads the ID and both status registers, and looks at
the test sector, the chip's last 4 KB. It reports the sector as erased, as
holding a pattern an earlier boot wrote (with any bytes that differ counted
and the first located), or as holding something else. Writing a pattern and
reading it after `R` is how a pattern is shown to survive a reset. The
pattern and its check are `oxinode_core::w25q`, tested on the host.

The ID and the status registers are read on GPIOs rather than through the
QSPI peripheral. `embassy-nrf` has the peripheral poll the busy bit before
every custom instruction, which a sleeping chip never answers, and it sends
a write enable ahead of each one, so a status read through it would always
show the latch set. Erasing, writing and reading go through the peripheral,
single-line at 8 MHz unless `q` asks for four lines.

| Key | What it does |
|---|---|
| `i` | The ID as the chip is now, then wake it, the ID again, and the status registers |
| `r` | Read the test sector and say what is there |
| `w` | Erase the test sector, write a new pattern, and read it back |
| `x` | Erase the test sector |
| `q` | Read the test sector on four lines; refused while QE is clear |
| `s` `a` | Deep power-down (`B9h`) or wake (`ABh`), with the ID read after |
| `m` | The measuring cycle, until the next key |
| `R` | Reset the nRF52, leaving the flash as it is |
| `b` | Reboot into the bootloader |

With no terminal within five seconds of boot, the image starts the measuring
cycle by itself: the chip in standby, then in deep power-down, ten seconds
each, over and over, with the LED off, because the LED draws more than the
difference. A meter in the battery lead, with USB unplugged, shows the
difference as a square wave.

## `buzzer`

The Super IO's buzzer sits behind a load switch on P0.22, and what is on the
far side of it is not on any schematic. This image settles it by ear. An
active buzzer has its own oscillator, so P0.22 held high sounds a continuous
tone; a passive one clicks as the rail comes up and goes down, and needs a
square wave, which only works through the switch if the switch is fast
enough to follow it. The square wave comes from the PWM peripheral at 1 MHz
divided down. Between sounds the pin is driven low, never left floating.

Exposes a single CDC port, like `qspi`.

| Key | What it does |
|---|---|
| `h` `H` | P0.22 held high for 1 s, or for 10 s (for a meter) |
| `1`–`8` | A square wave at 1–8 kHz for 1 s |
| `s` | A sweep from 500 Hz to 8 kHz, 400 ms a step, each step logged |
| `w` | P0.22 held high for 50, 100, 150, 200, 300 and 500 ms, 2 s apart: how long a beep has to be to sound as a tone |
| `p` | The alert pattern, held: two 80 ms beeps 80 ms apart |
| `P` | The alert pattern as a 4 kHz square wave |
| `c` | The cycle below, until the next key |
| `R` | Reset |
| `b` | Reboot into the bootloader |

With no terminal within five seconds of boot, it plays a cycle with the LED
off, so it can be listened to on the battery with nothing to read: held for
1 s, 2 kHz for 1 s, 4 kHz for 1 s, the pattern held, the pattern at 4 kHz,
held for 10 s and 4 kHz for 10 s, two seconds apart, then five seconds of
silence and again.

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
