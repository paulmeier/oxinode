# Phase 6 — provisioning

Phase 5 made the board an RNode as far as `rnsd` is concerned. Phase 6 makes it
one as far as **`rnodeconf`** is concerned: a device with an identity, a
signature, and a configuration it remembers.

Before this phase, `rnodeconf -i` got as far as the handshake and stopped:

```
Device connected
Current firmware version: 1.52
Reading EEPROM...
Could not download EEPROM from device. Is a valid firmware installed?
```

## Where the definition comes from

The same place as phase 5, and for the same reasons: the **host**. This time it
is `RNS/Utilities/rnodeconf.py` from RNS 1.5.0 rather than `RNodeInterface.py` —
its `ROM` class, its `KISS` class, and the `parse_eeprom` that decides whether
to trust a device. RNode_Firmware_CE is GPLv3 and is not read, ported or
transliterated.

`rnodeconf` is a second host with a second parser, and it reads commands `rnsd`
never sees. Both tables are now transcribed into `host_unescapes`, together
with a test asserting they agree wherever they overlap — nothing in either
codebase forces that, and a divergence would mean a frame that is correct for
one host and corrupt for the other.

## What a provisioned RNode actually is

An EEPROM image with five things in it:

| Bytes | What |
|---|---|
| `0x00`–`0x0A` | product, model, hardware revision, 4-byte serial, 4-byte manufacture time |
| `0x0B`–`0x1A` | **MD5** of those eleven bytes |
| `0x1B`–`0x9A` | **RSA signature** over that MD5, 128 bytes |
| `0x9B` | lock byte `0x73`, written **last** |
| `0x9C`–`0xA7` | the stored radio configuration, with its own validity byte |

The host reads it, recomputes the MD5, verifies the signature against a list of
vendor keys plus any local signing key on the machine, and reports the device
as unverified if none of them match.

The device does not have to verify any of this — it cannot; the keys are on the
host. What it does have to do is **store it faithfully and mean the same thing
by "provisioned" as the host does**. So `Eeprom::status` checks the checksum
too, which is why there is an MD5 implementation in `oxinode-core`. It is a
checksum in the literal sense: the trust comes from the signature over it.

Without that, a board whose provisioning was half-written would look fine in
its own log right up until a host disagreed.

## Three decisions

### The image is 256 bytes, and that is not a capacity

`CMD_ROM_WRITE` carries `[address, value]` with a **one-byte** address. 256 is
the whole addressable space, so `Eeprom::read` and `Eeprom::write` are total —
there is no bounds check to get wrong, and a test walks all 256. The host's own
reader caps a `CMD_ROM_READ` frame at 1024 bytes, so there is room to spare in
the other direction.

### This board says it is a homebrew RNode

`PRODUCT_HMBRW` `0xf0`, `MODEL_FF` `0xff`, `BOARD_HMBRW` `0x32`. That is the
honest answer — oxinode is none of the boards in `rnodeconf`'s table — and it
is also the safe one. For an nRF52 device whose model is `0xff` and whose board
is not a RAK4631, `rnodeconf --update` refuses with *"No firmware found for
this board. Cannot update."* Claiming to be, say, model `0x12` would make it
offer to flash RAK4631 firmware onto an LR1121.

**One consequence to know rather than be surprised by:** `rnodeconf -i` prints

```
Frequency range    : 100.0 MHz - 1100.0 MHz
Max TX power       : 14 dBm
Modem chip         : Unknown
```

Those come from the host's own table for model `0xff`, not from the device. The
module is rated 20 dBm sub-GHz over 902–928 MHz, and
`oxinode_core::lr1121::config` is what actually enforces that. The alternative
was to claim a model whose table entry happens to be closer and be wrong about
which board this is, which is worse.

### The device hash is oxinode's own definition

`CMD_DEV_HASH` is the one thing here that no host pins. `rnodeconf --sign` asks
the device for thirty-two bytes, signs whatever it is given with the machine's
device key, and hands the signature back to be stored. The requirements are
therefore only that it be deterministic and specific to this device.

So it is SHA-256 over the identity block **and the MCU's factory device ID**:
the provisioning and the physical chip. Two people provisioning their own
boards with their own `rnodeconf` counters will both get serial number 1; the
device ID is what stops a signature made for one from validating the other.
Reprovisioning changes it, which is right — the old signature attested to the
old identity.

It is answered **only when the device is provisioned**. An unprovisioned board
that answered would let a signature be made over erased bytes, and the next
provisioning would inherit it.

## Where it is stored, and why that matters

`0xEA000` — the first page of the 40 KB the Adafruit bootloader reserves with
`DFU_APP_DATA_RESERVED`. That region is not a spare corner of flash we found:
it is the region the bootloader sets aside and then refuses to write through on
both of its flashing paths. Serial DFU rejects any image that would reach it,
and the UF2 drive silently drops blocks addressed above it.

Which is exactly the property provisioning needs. `rnodeconf` gives a board an
identity once; flashing a new oxinode image afterwards must not take it away.
`memory.x` has documented that 40 KB since phase 1 as the reason our ceiling is
784 K rather than 824 K. This is the first thing to use it.

The address is derived rather than written down twice: `build.rs` now exports
`APP_FLASH_END` alongside `APP_FLASH_ORIGIN`, and the end of the application
region is the start of the reserved one by definition.

### The record has a checksum, and the reason is the radio

Flash writes are not atomic — the board can lose power between erasing a page
and finishing the write. For the identity block a torn record would be
survivable, because the host's own MD5 would catch it and say so.

For the **stored radio configuration** it would not. A torn record can leave a
plausible-looking frequency, and a device in TNC mode brings its radio up from
that at boot with nobody watching. So the record carries a CRC-32 over
everything except the four bytes holding it, and a record that fails it is
discarded rather than partially trusted. There is no third answer between "a
record this firmware wrote" and "a board nobody has provisioned".

A test flips every byte of a record in turn and asserts none of them decode.

### Writes are debounced, not committed

`rnodeconf` provisions with **155 single-byte writes six milliseconds apart**,
and a page erase stalls this CPU for about 85 ms. Committing each one would
need thirteen seconds to absorb one second of commands — which is why
`rnodeconf`'s own source calls the nRF52 EEPROM implementation "janky" and
sleeps for tens of seconds around it.

So `Action::Persist` means "eventually". The image is held in RAM, the timer
restarts on every write, and one commit follows 250 ms after the last. The
whole burst costs one erase.

The cost is a 250 ms window in which an unplugged board loses the run. That is
survivable and visible — the host reads the image back immediately afterwards
and would find it short. Losing it because the flash could not keep up would
not be.

Two other things trigger a commit: a 1200-baud touch, since a reflash is when
losing a provisioning would be least welcome, and `CMD_RESET`, since the host
looks at the result afterwards.

## TNC mode

A device with a stored configuration is supposed to come up on air by itself.
That is what `rnodeconf --tnc` asks for and the only reason to store a
configuration at all.

The stored values go through **the same validation as anything a host sends**,
and for the same reason: they were written by a tool that does not know what
this radio can do. A configuration that does not validate leaves the radio off
with `last_error` saying why, exactly as an impossible request from a host
would. The alternative is a board that boots with nobody watching and programs
the chip with whatever it finds.

## What the hardware says

### Provisioning

`rnodeconf <port> --rom --product f0 --model ff --hwrev 1`, against a board
that had never been provisioned, with a signing key generated locally by
`rnodeconf -k`:

```
Bootstrapping device EEPROM...
EEPROM written! Validating...
```

and afterwards:

```
Device info:
	Product            : Hombrew RNode (Band capabilities unknown) (f0:ff:32)
	Device signature   : Validated - Local signature
	Firmware version   : 1.52
	Hardware revision  : 1
	Serial number      : 00:00:00:01
	Manufactured       : 2026-09-06 18:19:28
	Device mode        : Normal (host-controlled)
```

`EEPROM checksum correct`, and **`Device signature validated`** — the host
recomputed the MD5 over what the device handed back and verified a 1024-bit RSA
signature over it.

The dump was also parsed independently, in Python rather than by the code under
test: 256 bytes exactly, the MD5 over bytes 0–10 matching the sixteen stored at
`0x0B`, the lock byte at `0x9B`, and everything above it erased.

### The device hash is what the module computes

Asked over the wire and recomputed in Python from the definition:

```
device hash from board: df5ef34534115dc93dcd0a3232c8712180aa88fa3a78922a5dbc178f650f050c
sha256(info || mcu id): df5ef34534115dc93dcd0a3232c8712180aa88fa3a78922a5dbc178f650f050c
```

`rnodeconf --sign` then reported **`Device signed`**, which it only does after
receiving a hash — an unanswered `CMD_DEV_HASH` makes it say "No device hash
present, skipping device signing".

### It survives a reset

`rnodeconf --tnc --freq 915000000 --bw 125000 --txp 17 --sf 8 --cr 5`, then
`CMD_RESET`, and the first two lines of the log on the way back up:

```
0.000549 INFO  store: loaded from 0xea000, provisioned=true, configured=true
0.000640 INFO  oxinode RNode: serial 65C11224153B1DFD, image at 0x26000
0.369384 INFO  radio: reset -- BUSY rose and fell as documented
0.419982 INFO  radio: 32 MHz oscillator running, die 20.010355 C
0.421508 INFO  radio: up
0.421630 INFO  tnc: resuming the stored configuration
0.421661 INFO  config: 915000000 Hz (915067069 Hz commanded), SF8 BW125000 CR4/5, 17 dBm
```

Four hundred milliseconds from reset to a configured radio, with no host
attached — and 915,067,069 Hz is phase 4's reference correction applied to a
frequency that came out of flash rather than off the wire.

### A wipe is a wipe, and it also survives

`rnodeconf --eeprom-wipe`, then `rnodeconf -i`:

```
WARNING: EEPROM is being wiped! Power down device NOW if you do not want this!
...
EEPROM is invalid, no further information available
```

and after a reset:

```
store: loaded from 0xea000, provisioned=false, configured=false
radio: up
```

No `tnc: resuming` line: the stored configuration went with the identity, the
radio came up host-controlled, and the record that decoded is a valid record
containing an erased image rather than an absent one.

Reprovisioning afterwards produced serial `00:00:00:02` — `rnodeconf`'s own
counter — with the signature validated again.

### The target firmware hash round-trips

```
rnodeconf <port> -H 00112233...eeff   ->  Firmware hash set
rnodeconf <port> -K                   ->  The target firmware hash is: 00112233...eeff
```

### Phase 5 still holds

Ten consecutive interface bring-ups against the provisioned board, each in a
fresh process:

```
run  1: online: True  bitrate: 3125.0  r_sf/cr/bw: 8 5 125000
...
run 10: online: True  bitrate: 3125.0  r_sf/cr/bw: 8 5 125000
```

Ten out of ten, every field reported, and the same 3125 bps
`oxinode_core::lr1121::config` computes.

**A method note, because this nearly became a false regression.** Two earlier
series of the same test showed half the runs coming up with `r_sf` or `r_cr`
still `None` — a device apparently dropping setters, which would have looked
exactly like phase 5's open packet-loss item getting worse. It was not the
board. The second series had been started while the first was still running,
and both were opening the same serial port. The measurement was contended, not
the firmware. Re-run once with nothing else holding the port, it is ten for
ten.

The same shape as phase 5's wrong first hypothesis, and the same lesson: check
what else is touching the thing you are measuring before you believe the
measurement.

## An interop limitation worth knowing about

`rnodeconf`'s bootstrap ends by resetting the board and then finding "the" port
again by **matching USB serial numbers**, taking the first match. A composite
device has two CDC functions and therefore two ports *with the same serial
number*, and which one comes first is not stable:

```
before reset: /dev/cu.usbmodem3101 serial 65C11224153B1DFD
  matching ports, in the order rnodeconf sees them:
    /dev/cu.usbmodem3103
    /dev/cu.usbmodem3101
  rnodeconf would choose: /dev/cu.usbmodem3103
  bytes back: 0
  RESULT: silence -- that is not the KISS port
```

So the last step of `--rom` can report *"Could not download EEPROM from device"*
when the provisioning has in fact succeeded. That is what happened on **both**
bootstrap runs here; `rnodeconf -i` seconds later showed a fully provisioned,
signature-validated device each time. Before a reset the KISS port came first
in that listing; after one, the log port did, in every observation.

There is nothing the firmware can do about it. A USB serial number belongs to
the device, not to an interface, and the two ports must share it. The
alternatives are worse: dropping the log port would cost the only diagnostic
channel this board has, and answering `CMD_ROM_READ` on the log port would
corrupt the log to work around someone else's tie-break.

The workaround is one command: `rnodeconf -i <port>`.

## What is deliberately not answered

`CMD_HASHES` with kind `0x02` asks the device for the hash of the firmware it
is **running**, and oxinode does not answer it. An image cannot contain a hash
of itself, and producing one at runtime means hashing flash over an extent the
linker does not hand us in a form worth trusting — the region past the image is
whatever the last flash left there, so the answer would not be reproducible
from the `.bin` anyway.

Answering with the *target* hash instead would be worse than silence: the host
compares the two, and a wrong match reads as a verified firmware.

The cost is real and worth stating. `rnodeconf -L` — an undocumented flag —
tracebacks rather than reporting the absence:

```
AttributeError: 'NoneType' object has no attribute 'hex'
```

That is `rnodeconf` dereferencing a `None` it does not check for, but it is our
silence that produces the `None`. Computing a real firmware hash is possible —
`cortex-m-rt`'s linker symbols do bound the loadable image — and is left open
rather than done, because a hash that cannot be checked against anything is not
worth the fragility of reading someone else's link-script symbol names.
