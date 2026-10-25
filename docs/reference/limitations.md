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
- **The GPS module is not configured.** It is taken as it wakes; nothing asks
  it for a faster rate or standby.
- **The QSPI flash holds nothing.** The product image wakes it at boot,
  checks its ID and puts it in deep power-down; the `qspi` image proves it
  reads, writes and keeps a pattern across a reset. What lives on it is the
  messaging store's to decide (#65).
- **An announce made without a GPS time says it was made at time zero.** The
  board has no other clock, and zero is Reticulum's "not known". It costs
  nothing over the air directly; a receiver holding a path to the board
  through another node keeps that path until it expires rather than taking
  the newer one.
- **The node receives only messages that fit in one packet.** LXMF's
  opportunistic delivery, up to 295 bytes of content, is the whole of it: a
  sender that opens a link first, or sends something longer as a resource,
  or leaves it with a propagation node, reaches nothing on the board, and the
  log says a packet came that the board does not read. That is
  [#67](https://github.com/paulmeier/oxinode/issues/67).
- **Received messages are held in RAM, sixteen of them in all.** A reboot
  loses them ([#65](https://github.com/paulmeier/oxinode/issues/65)), and
  when a seventeenth comes the oldest one read goes, or the oldest if none
  has been; a conversation whose messages have all gone leaves the Messages
  screen.
- **The six canned lines are fixed.** *OK*, *Yes*, *No*, *On my way*,
  *Can't talk now* and *Where are you?* are in the firmware, and cannot be
  changed from the panel or a host. Anything else is typed on the
  keyboard.
- **The keyboard types what the font draws, at the end of the text.** It
  has the letters, the digits, space and `! # % ' ( ) * + , - . / : = ?`;
  no accents, no `" $ & @ ;`, and no emoji. The cursor is always at the end:
  a typo further back is deleted back to. The display name cannot be edited
  with it yet ([#66](https://github.com/paulmeier/oxinode/issues/66)).
- **What the board sends is one packet: 287 bytes of text, no title, no
  fields.** That is a little under LXMF's 295, so it fits through a
  transport node too. Longer is refused, never cut; links and resources are
  [#67](https://github.com/paulmeier/oxinode/issues/67).
- **Messages the board sends are held in RAM, eight of them**, and lost on a
  reboot ([#65](https://github.com/paulmeier/oxinode/issues/65)). A ninth
  lets the oldest delivered one go, then the oldest failed one; with all
  eight still on their way, it is refused.
- **A message is encrypted to the recipient's identity key, not to a
  ratchet** it may have announced. Sideband and Python LXMF accept both by
  default; a peer that enforces ratchets will not read the board's messages.
- **With no GPS fix the board's clock is borrowed.** A message is stamped
  with the GPS's time; with no fix, with the newest time a message arrived
  stamped with, run on since; with neither, zero, which a phone shows as
  1970.
- **A message to a peer that is switched off takes minutes to fail**: three
  attempts, the waits doubling from 50 times the airtime, about three and a
  half minutes for a short reply at the default settings and longer at
  SF12. One to a peer whose key the board never heard fails after a minute
  of asking for its path.
- **What a message carries beyond its title and text is not shown.** An
  image, a file, a voice message or telemetry is recorded as "has an
  attachment", and the attachment itself is not kept. Formatted content
  (Micron, Markdown) is kept as the sender wrote it, markup and all.
- **A message from a sender the board has not heard stays unverified** until
  the sender's announce arrives; the board asks for it once when the message
  comes and again on each retry. One whose key never arrives is never shown
  as verified. Stamps are neither asked for nor checked.
- **A board cannot be given a new Reticulum identity** without erasing its
  identity page with a probe. `rnodeconf --eeprom-wipe` leaves it alone on
  purpose (see [Provisioning and storage](../architecture/provisioning.md#the-nodes-identity)).
  A panel action for it, behind a confirmation, goes with the stored
  conversations ([#65](https://github.com/paulmeier/oxinode/issues/65)).
- **The 2.4 GHz path is accepted and held, and not yet good.** The product
  image validates for both bands, the panel shows the band, switches it in
  one step and edits a 2.4 GHz frequency in place, and a stored 2.4 GHz
  configuration comes back
  after a reboot. What stands between that and a usable link is carrier
  sense: channel activity detection at 812.5 kHz reads a quiet desk as busy
  on nearly every sense, so every transmission waits out its whole budget
  and then goes anyway, and a two-board exchange loses frames to the wait.
  That is [#47](https://github.com/paulmeier/oxinode/issues/47), and it is
  specific to 812.5 kHz: at 203.125 and 406.25 kHz the same desk clears in
  a few senses and every exchange goes when asked. The RSSI calibration
  table for the band is now sent with every configuration (the 2.4 GHz
  table reads 9 dB stronger than the sub-GHz one the chip boots with), and
  image calibration is settled from the datasheet (the 2.4 GHz receiver has
  none to do); the measurements still owed — the power at the u.FL and the
  reference error at 2478 MHz, which need an instrument that tunes there,
  and the three-bandwidth exchange across a room with antennas on the
  u.FLs — are
  [#44](https://github.com/paulmeier/oxinode/issues/44), and what to tell
  `rnodeconf` about a board with two bands is
  [#45](https://github.com/paulmeier/oxinode/issues/45). See
  [The radio](../hardware/radio.md#the-24-ghz-path).

## Interoperability quirks

- **`rnodeconf -i` reports the band and power from its own table** for model
  `0xff`, not from the device. The firmware enforces the real limits.
- **`CMD_HASHES` for the running firmware's hash is not answered**, so
  `rnodeconf -L` tracebacks. See [The RNode protocol](../architecture/rnode-protocol.md).
- **The host does not un-escape single-byte fields**, so an SNR of exactly
  −16 dB is clamped rather than sent as the frame delimiter.

## Observations, recorded

- **A 1200-baud touch with a phone connected twice wedged the bootloader**,
  on 2026-09-07: the board ended in DFU with serial DFU not answering, even to
  the flasher's retry, and only a double-tap recovered it. It does not
  reproduce on the current firmware: `tools/touch_series.py`, the recipe as a
  tool, reflashed RNode 7F23 by touch 10 times out of 10 on 2026-10-23, each
  touch taken after a fresh boot with the iPhone's link encrypted and carrying
  traffic, and a first draft of the tool did 13 out of 13 the same day. Five
  of the 23 needed the flasher's retry, which is the known race with the
  bootloader's enumeration, not this. Neither suspect explains it. The
  product image's `.bss` does cover the bootloader's double-reset word at
  `0x20007F7C` (the Bluetooth controller's memory sits on it), but the
  bootloader reads that word only after a reset-pin reset; a touch enters DFU
  through `GPREGRET` whatever the word holds. And the controller held an open
  link at every one of the 23 resets. Only the iPhone was tried, and the image
  of 2026-09-07 still had two serial ports. Unattributed, and kept here until
  a run of the tool loses one.
- **A synthetic host that wrote six setters back to back with no reads once
  lost roughly one four-byte frame in ten.** It was not the reply direction,
  not SPI and USB contending for EasyDMA, not the decoder, and it never
  happened with Reticulum. It does not reproduce on the current firmware:
  `tools/setter_burst.py`, the recipe as a tool, answered every setter on
  both bench boards on 2026-10-23, 4,800 of 4,800, reading after each round,
  reading nothing until the end, and reopening the port each round. The image
  with the log on a second CDC port answered 3,000 of 3,000 the same day
  under a first draft of the tool (one that could, in principle, credit a
  late reply to the next round; its no-reads total of 600 of 600 could not
  be fooled that way), so the change to one serial port is not what removed
  it. Unattributed, and kept here until a run of the tool loses one.
- **The LR1121 reports its last reset as `Analog`** after a pulse on NRESET.
- **The receive window with the correction off dips at +220 and +240 kHz**
  inside a full passband, consistent with a fixed-frequency spur.
- **The panic handler reboots into the bootloader.** That is right for
  bench work and wrong for a fielded device, which should probably reset
  into its application and keep trying.
