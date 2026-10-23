# Using it with Reticulum

The board is an RNode. Everything a Reticulum host does with an RNode, it does
with this one, unmodified.

## The serial port

The product image enumerates as a composite USB device named `oxinode RNode`
with one CDC-ACM serial port and one vendor interface:

| Interface | Carries |
|---|---|
| the serial port (`/dev/cu.usbmodemXXX1` on macOS) | the KISS-framed RNode protocol, and the flasher's 1200-baud touch |
| `oxinode log`, vendor-specific | the firmware's `defmt` log, read with `tools/log_reader.py` |

One serial port per board is deliberate: `rnodeconf` finds a board again after
resetting it by USB serial number, and with two ports sharing that number it
could land on the wrong one.

!!! note "macOS asks once"
    The first time a Mac sees a board with this layout (including a board
    updated from an image with two serial ports) it holds the device back
    until you allow the accessory. If the port does not appear after a flash,
    look for the *Allow accessory to connect?* prompt, or replug the board.

## `rnsd`

Point an `RNodeInterface` at the first port:

```ini
[[oxinode]]
  type = RNodeInterface
  interface_enabled = True
  port = /dev/cu.usbmodemXXX1
  frequency = 915000000
  bandwidth = 125000
  txpower = 14
  spreadingfactor = 8
  codingrate = 5
```

Reticulum runs its detect handshake, sets the five radio parameters, powers the
radio on, reads them back, and reports the interface up:

```
[Notice] RNodeInterface[oxinode RNode] is configured and powered up
    Status    : Up
    Rate      : 3.12 kbps
```

Two things to know when choosing parameters:

- **Power is refused, not clamped.** The module is rated for 20 dBm sub-GHz
  and 11 dBm at 2.4 GHz. Ask for more and the radio stays off, Reticulum
  notices the mismatch, and it prints *make sure that your hardware actually
  supports the parameters specified in the configuration*. The specific
  reason goes to the log.
- **The frequency you get is the frequency you asked for.** The chip is
  commanded 73 ppm higher to cancel the module's reference error, and the
  protocol reports the wanted frequency back, which is what Reticulum
  compares. See [The radio](../hardware/radio.md).
- **The frequency decides the band.** 902–928 MHz is the SMA connector with
  the bandwidths 62.5 to 500 kHz; 2400–2483.5 MHz is the u.FL with 203.125,
  406.25 or 812.5 kHz and 11 dBm at most. `frequency = 2478000000`,
  `bandwidth = 812500`, `txpower = 11` brings the interface up on the 2.4 GHz
  path, with the antenna on the u.FL. Carrier sense on that band is not yet
  right ([#47](https://github.com/paulmeier/oxinode/issues/47)); see
  [Known limitations](../reference/limitations.md).

## `rnodeconf`

`rnodeconf` from RNS works against the board for everything it does with an
nRF52 RNode except firmware updates.

```bash
rnodeconf /dev/cu.usbmodemXXX1 -i            # device info
rnodeconf -k                                 # make a local signing key, once
rnodeconf /dev/cu.usbmodemXXX1 --rom --product f0 --model ff --hwrev 1
rnodeconf /dev/cu.usbmodemXXX1 --sign
rnodeconf /dev/cu.usbmodemXXX1 --tnc --freq 915000000 --bw 125000 --txp 14 --sf 8 --cr 5
```

The board identifies as a **homebrew RNode** (product `0xf0`, model `0xff`,
board `0x32`). That is the honest answer and the safe one: `rnodeconf --update`
refuses to offer firmware for it rather than offering to flash a RAK4631 image
onto an LR1121. It also means `rnodeconf -i` prints the band and power from its
own table for model `0xff` (100 to 1100 MHz, 14 dBm) rather than from the
device; the firmware is what enforces the module's real bands, 902 to 928 MHz
at 20 dBm and 2400 to 2483.5 MHz at 11 dBm.

Provisioning survives a reflash. The EEPROM image, the signature, the stored
configuration and the Bluetooth bonds live in a device record in the 40 KB of
flash the bootloader reserves above the application and refuses to write. See
[Provisioning and storage](../architecture/provisioning.md).

### TNC mode

`rnodeconf --tnc` stores a radio configuration and marks the device to come up
on air by itself. The stored values go through the same validation as anything
a host sends; a configuration that does not validate leaves the radio off with
the reason in the log rather than programming the chip unattended. A board in
TNC mode reaches a configured radio about 420 ms after reset with no host
attached.

## Sideband over Bluetooth

The nRF52840 has no Bluetooth Classic, only LE, so in Sideband tick
**Hardware → RNode → "Device requires BLE"**. The board advertises as
`RNode XXXX`, where the four hex digits come from the chip's factory address.

1. Connect from Sideband. The board refuses the first write as
   "insufficient authentication", which makes the phone pair.
2. The OLED shows a six-digit passkey in a box over whatever screen it was on.
   Type it on the phone. The link is encrypted and authenticated; "Just Works"
   pairing is not offered.
3. The bond is stored in the device record, so after a reset the phone gets
   back in without being asked again. Up to four phones are remembered.

Set Sideband's transmit power to what the board can do (14 dBm is a safe
figure) before connecting with a radio configuration; the default is above the
module's rating and the radio is refused, with *the RNode radio is locked
because its modem configuration is incomplete* on the phone and the reason on
the board's log.

USB keeps working while a phone is connected. Answers to commands go back the
way the command came, so `rnodeconf` over USB works with a phone on the line;
unsolicited frames, such as received packets, go to the phone while there is
one and to USB otherwise. The Bluetooth screen on the panel shows whether the
board is advertising or has a phone connected.

To forget every bonded phone, use **Forget Phones** on the Bluetooth screen's
menu. See [Bluetooth](../architecture/bluetooth.md) for how the stack is put
together.

## Two oxinodes, and stock RNodes

Every LoRa frame carries the stock RNode's one-byte air header, and packets
over 254 bytes go as two frames, so two oxinodes carry full-MTU Reticulum
packets between them and the frame layout is a stock RNode's. The exchange
against a real stock RNode has not yet been run; `tools/air_exchange.py` is
the test, and [Known limitations](../reference/limitations.md) has the state.

## Reading the log

The log interface carries `defmt` frames, not text. Read it with
`tools/log_reader.py` (it needs libusb: `brew install libusb`) and decode it
with the ELF the image was built from:

```bash
cargo install defmt-print
tools/log_reader.py | defmt-print -e target/thumbv7em-none-eabihf/release/rnode
```

With two boards attached, add `--serial <USB serial number>`. The log pump
holds the boot lines until a reader attaches, so starting it late still shows
boot from the top; see [Debugging without a probe](../development/debugging.md).
