# Using it with Reticulum

The board is an RNode. Everything a Reticulum host does with an RNode, it does
with this one, unmodified.

## The two serial ports

The product image enumerates as a composite USB device named `oxinode RNode`
with two CDC-ACM serial ports:

| Port | Carries |
|---|---|
| first (`/dev/cu.usbmodemXXX1` on macOS) | the KISS-framed RNode protocol |
| second (`/dev/cu.usbmodemXXX3`) | the firmware's `defmt` log |

The order is fixed: the KISS port is first so that it is the lower-numbered
tty, which is where `rnsd` and the flasher's 1200-baud touch look.

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

- **Power is refused, not clamped.** The module is rated for 20 dBm sub-GHz.
  Ask for more and the radio stays off, Reticulum notices the mismatch, and it
  prints *make sure that your hardware actually supports the parameters
  specified in the configuration*. The specific reason goes to the log port.
- **The frequency you get is the frequency you asked for.** The chip is
  commanded 73 ppm higher to cancel the module's reference error, and the
  protocol reports the wanted frequency back, which is what Reticulum
  compares. See [The radio](../hardware/radio.md).

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
device; the firmware is what enforces the module's real 902 to 928 MHz and
20 dBm.

Provisioning survives a reflash. The EEPROM image, the signature, the stored
configuration and the Bluetooth bonds live in a device record in the 40 KB of
flash the bootloader reserves above the application and refuses to write. See
[Provisioning and storage](../architecture/provisioning.md).

!!! note "`rnodeconf` may pick the wrong port after a reset"
    `rnodeconf --rom` ends by resetting the board and finding "the" port again
    by USB serial number, taking the first match. Both CDC ports carry the
    same serial number and the order is not stable, so it can end with *Could
    not download EEPROM from device* after the provisioning has in fact
    succeeded. `rnodeconf -i <port>` a moment later shows the truth. There is
    nothing the firmware can do about it that would not be worse.

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
against a real stock RNode has not yet been run; see
[Known limitations](../reference/limitations.md).

## Reading the log

The second port carries `defmt` frames, not text. Decode them with the ELF the
image was built from:

```bash
cargo install defmt-print
stty -f /dev/cu.usbmodemXXX3 115200
defmt-print -e target/thumbv7em-none-eabihf/release/rnode < /dev/cu.usbmodemXXX3
```

Always open the port at an explicit baud rate. The log pump holds the boot
lines until a terminal opens the port, so opening it late still shows boot
from the top; see [Debugging without a probe](../development/debugging.md).
