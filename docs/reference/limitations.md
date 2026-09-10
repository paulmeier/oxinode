# Known limitations

What the firmware does not do, does not do yet, or has not been checked. Each
item is either a bounded piece of work or an observation recorded so nobody
has to rediscover it.

## Not yet verified

- **Against a stock RNode over the air.** Every frame carries the stock air
  header, packets split at 254 and reassemble by the stock receiver's rules,
  and two oxinodes exchange every size up to 508 bytes. The exchange with a
  real stock RNode has not been run. The test that settles it is
  `tools/air_exchange.py`: Reticulum on each, a packet under 254 bytes and a
  400-byte packet each way, and every log kept. It passes between two
  oxinodes; it needs a stock RNode on the bench for the run that counts,
  and a Base Duo cannot be one, since the reference firmware does not drive
  the LR1121.
- **Through `rnsd` with a resource transfer.** The split has been exercised
  by a KISS script that sends what `RNodeInterface` sends; a link with a
  resource transfer between two Reticulum instances on two boards would carry
  500-byte packets and is the natural next check.
- **A frame lost on the air.** The reassembler's handling of a missing first
  or second half is tested on the host, not on a bench that cannot lose a
  frame to order.
- **The GPS receiver's current draw**, off and on. Both halves are one
  reading away from anyone with a meter in the battery lead.
- **CAD thresholds at range.** They are Semtech's reference values, which
  hear a −46 dBm neighbour without a miss.

## Bounded work not done

- **A packet that begins during a channel sense is avoided, not received.**
  Something under half of the sense cycle is a window where too little
  preamble remains for the receiver to sync. Two remedies (a longer preamble
  than the host's eight symbols, or sensing from inside continuous receive)
  are described on [On the air](../architecture/air.md).
- **Flash writes stall the Bluetooth controller.** The device record is
  written with the NVMC directly, and a page erase stalls the CPU for about
  85 ms, which the link layer cannot hold a connection through. A
  provisioning run with a phone connected drops the connection. The fix is
  to write through the controller's own flash scheduler, which fits the
  write into a timeslot.
- **Contention-window bands and a noise-floor estimate**, the two refinements
  of the stock carrier-sense arrangement not implemented.
- **`CMD_DISP_BLNK`, `CMD_DISP_ROT`, `CMD_DISP_RCND`, `CMD_BLINK`,
  `CMD_BT_CTRL`, `CMD_BT_PIN`** are understood and not implemented.
- **Display brightness and Bluetooth on/off from the panel.** By the
  host-ownership rule they are the panel's to change; nothing on the panel
  edits them yet.
- **The fields a host cannot set** (preamble, sync word, CRC, header mode,
  IQ) are not editable from the panel, on purpose: a host could not read them
  back, and a board whose sync word differs from what its host believes is a
  board that hears nothing and cannot say why.
- **A modal's lines do not scroll**, so on a 128 × 64 panel a refused
  editor's reason is off the bottom. The right fix is content that fits.
- **The GPS module is not configured.** It is taken as it wakes; nothing asks
  it for a faster rate or standby.
- **A heard packet's routing** while a phone is connected and a USB host is
  transmitting at the same moment goes to the USB host, where the main loop
  would have chosen the phone.
- **The QSPI flash** is not driven.
- **The 2.4 GHz path** is not driven.

## Interoperability quirks

- **`rnodeconf` may pick the wrong port after a reset.** Both CDC ports share
  the USB serial number and the order is not stable, so `--rom` can end with
  *Could not download EEPROM from device* after succeeding. `rnodeconf -i`
  shows the truth. See [Using it with Reticulum](../getting-started/reticulum.md).
- **`rnodeconf -i` reports the band and power from its own table** for model
  `0xff`, not from the device. The firmware enforces the real limits.
- **`CMD_HASHES` for the running firmware's hash is not answered**, so
  `rnodeconf -L` tracebacks. See [The RNode protocol](../architecture/rnode-protocol.md).
- **The host does not un-escape single-byte fields**, so an SNR of exactly
  −16 dB is clamped rather than sent as the frame delimiter.

## Observations, recorded

- **A 1200-baud touch with a phone connected can wedge the bootloader.** The
  board ends in DFU with serial DFU not answering, and only a double-tap
  recovers it. The `ble` bring-up image has never done this; the product
  image uses more RAM, which puts the bootloader's double-reset magic word
  inside its `.bss`, but whether that is the mechanism is not established.
  Disconnect the phone or reset the board before reflashing.
- **A synthetic host that writes six setters back to back with no reads
  loses roughly one four-byte frame in ten.** It is not the reply direction,
  not SPI and USB contending for EasyDMA, not the decoder. It does not happen
  with Reticulum, which reads continuously while it writes. Real, not
  attributed, and not reachable by the clients the firmware serves.
- **The LR1121 reports its last reset as `Analog`** after a pulse on NRESET.
- **The receive window with the correction off dips at +220 and +240 kHz**
  inside a full passband, consistent with a fixed-frequency spur.
- **The panic handler reboots into the bootloader.** That is right for
  bench work and wrong for a fielded device, which should probably reset
  into its application and keep trying.
