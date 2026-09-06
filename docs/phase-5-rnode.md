# Phase 5 — the RNode protocol

Phase 4 made the radio configurable. Phase 5 is what makes it an *RNode*: the
KISS-framed command set a Reticulum host speaks down a serial port, so that
`rnsd` opens `/dev/cu.usbmodem*`, finds a modem, configures it, and starts
passing packets — with no patched Reticulum and no custom interface driver.

## Where the protocol definition comes from

**Not from RNode_Firmware_CE.** That is GPLv3 and this project is
MIT/Apache-2.0; none of its code is read, ported or transliterated.

The protocol is taken from the **host** side instead — Reticulum's own
`RNS/Interfaces/RNodeInterface.py`, version 1.5.0, as installed on the bench.
That is the right source for three separate reasons:

* it is the implementation oxinode actually has to satisfy, so it is the
  specification in the only sense that matters;
* it is a *counterpart* rather than a competing implementation of the same
  side, so reading it is interoperability work rather than derivation;
* what is extracted from it — command bytes, field widths, byte order — is
  factual interface data, not expression.

Every constant in `oxinode_core::rnode` therefore carries a note saying which
host behaviour pins it, and the tests are written as "what would the host make
of this?" rather than as "what did the firmware do?".

## What the host actually requires

Reading `configure_device`, `initRadio` and `readLoop` gives the whole contract.

**The handshake.** The host writes four commands in one burst — `CMD_DETECT`
with `0x73`, then `CMD_FW_VERSION`, `CMD_PLATFORM` and `CMD_MCU` each with a
`0x00` argument — waits 200 ms, and gives up if it has not seen a detect
response. So all four have to be answered promptly and unprompted ordering is
not available: the replies must simply arrive.

**The firmware version is a gate, not a label.** `validate_firmware` calls
`RNS.panic()` — it kills the process — unless the reported version is at least
**1.52**. A modem that reports 0.1 does not get a warning; it takes the host
down with it.

**The configuration must be echoed back, and must match.** `validateRadioState`
compares frequency, bandwidth, TX power, spreading factor and radio state
against what it set, and refuses to bring the interface online if any differ.
Frequency is compared to within 100 Hz.

That last one interacts with phase 4 in exactly the way it should. The
**commanded** frequency is 73 ppm above the wanted one, which at 915 MHz is
67 kHz — 670 times the tolerance. So the protocol layer must report the
*wanted* frequency, and phase 4's separation of the two is what makes that a
one-line fact rather than a bug hunt.

## The escaping is not uniform, and that is the trap

KISS escaping is simple: `0xC0` becomes `0xDB 0xDC` and `0xDB` becomes
`0xDB 0xDD`. What is not simple is that **the host only un-escapes some
frames**.

Its parser has one branch per command. Multi-byte fields — data, frequency,
bandwidth, firmware version, the counters, the airtime limits, the channel and
PHY statistics — accumulate through an unescaping step. Single-byte fields —
TX power, spreading factor, coding rate, radio state, lock, RSSI, SNR, errors —
are read **raw**, straight out of the byte stream, with no unescaping at all.

So escaping a single-byte field corrupts it: the host would take `0xDB` as the
value and then take the escape's second byte as the value again. And since the
host's parser treats *every* byte of such a frame as the value, the last one
wins.

Most of the time this cannot bite, because the values are small. It can bite on
SNR, which is signed: `0xDB` is −37 quarter-dB, or −9.25 dB, which is an
ordinary SNR to see. The rule is therefore per-field and belongs in the types
rather than in a comment.

One value is simply unrepresentable in a raw single-byte field: `0xC0` would
end the frame. For SNR that is −16 dB, which is below the demodulation floor at
every spreading factor, so it is clamped rather than escaped — but it is
clamped deliberately, and the test says so.

## Steps

1. **KISS framing** — escape, unescape, and a streaming decoder that survives
   a hostile byte stream. Pure; `oxinode-core`; the largest test surface in the
   project so far.
2. **The command set** — the bytes, their payload shapes, and which of them the
   host un-escapes. Pure.
3. **The protocol state machine** — commands in, responses and actions out,
   with no hardware anywhere near it. This is what lets the entire `rnsd`
   conversation be tested on the host.
4. **The firmware image** — `rnode`, with the KISS stream on the first CDC port
   and the defmt log on the second, driving phase 4's modem.
5. **Hardware validation** — `rnsd` against the real board.

---

## Step 1 — KISS framing

`oxinode_core::rnode::kiss`. Escape, unescape, a streaming decoder, and a
frame writer.

The decoder takes one byte at a time because that is how the bytes arrive: USB
delivers 64-byte packets with no relationship to frame boundaries, so a frame
can span several and one packet can hold the tail of one frame and the head of
the next. What it has to survive is not a well-formed stream — the port is open
to whatever the host sends, including a terminal or a probe from an unrelated
tool. So:

* **an overlong frame is dropped whole**, because a truncated frame is a
  well-formed frame with the wrong contents, which nothing downstream can
  detect;
* **`FEND` resynchronises from any state**, which is what lets a lost byte cost
  one frame rather than the link;
* **an empty frame is padding, not a frame** — and this is not a nicety. The
  host writes its four detect commands in one burst, so each frame's closing
  delimiter is the next one's opener, and a decoder that reported empty frames
  would answer four commands with five.

## Step 2 — the command set

`oxinode_core::rnode::command`. Three things came out of reading the host's
parser that were not obvious from the outside.

### The host does not un-escape every frame

Its reader has a branch per command. Multi-byte fields — data, frequency,
bandwidth, firmware version, counters, airtime limits, statistics — accumulate
through an unescaping step. Single-byte fields — TX power, spreading factor,
coding rate, radio state, lock, RSSI, SNR, errors — are read straight out of
the stream.

So a device that escaped uniformly would corrupt any single-byte field whose
value is `0xC0` or `0xDB`, and since every byte of such a frame overwrites the
value, the host would keep the second byte of the escape sequence. It cannot
bite on TX power or spreading factor, whose values are small. **It bites on
SNR**, which is signed: `0xDB` is −9.25 dB, an entirely ordinary reading.

`host_unescapes` is therefore a transcribed table rather than a rule, and
`encode_response` is the single place that consults it — so "did we escape this
one correctly?" is not a question that can be asked per call site.

The asymmetry runs the other way too: the host writes those same four commands
raw. This firmware's decoder un-escapes uniformly, which is wrong in principle
and cannot bite in practice, because no legal spreading factor (5–12), coding
rate (5–8), radio state (0, 1, 0xFF) or TX power reaches `0xC0` or `0xDB`.
There is a test that walks every legal value of all four, so the simplification
is bounded rather than unnoticed.

### The firmware version is a gate

`validate_firmware` calls `RNS.panic()` — it ends the host process — unless the
reported version is at least 1.52. Reporting oxinode's own version would take
down every host that connected. The constant therefore says "I speak what a
1.52 RNode speaks", with a compile-time assertion against the requirement.

### 915 MHz contains the frame delimiter

Big-endian it is `36 89 CA C0`. The first thing a US host configures is a frame
that must be escaped — a better place to have got escaping wrong than some rare
packet months later. It has a test of its own, which found a hand-computed hex
error in three others while it was at it.

## Step 3 — the protocol

`oxinode_core::rnode::protocol`. Commands in, responses and at most one radio
action out, with no hardware anywhere near it. That shape makes the whole of
`configure_device` — detect, five setters, power on, and the validation the
host performs afterwards — a unit test that runs in microseconds.

Phase 4 pays off twice.

**The frequency reported is the wanted one, not the commanded one.** The host
rejects a frequency that comes back more than 100 Hz from what it set; the
correction phase 4 applies is 67 kHz at 915 MHz, or 670 times that. Reporting
the commanded frequency would make every interface fail to come online, with a
message pointing at a frequency the operator had configured correctly.

**Nothing clamps.** A host asking for 22 dBm on a module rated for 20 is told
22 dBm and then simply does not get a radio: the state comes back off, the host
finds the mismatch, and prints *"make sure that your hardware actually supports
the parameters specified in the configuration"* — which is what happened.
`CMD_ERROR` would have said "hardware initialisation error": harsher, and less
true. The specific limit goes to the log port, the only channel that can carry
it.

## Step 4 — the image

`rnode`, with the KISS stream on the first CDC port and the log on the second.
That order is forced twice: it decides which tty gets the lower number, and DTR
is only visible on the first CDC function, which is where the 1200-baud
bootloader touch has to land.

Two bugs were caught before this reached hardware. The transmit path never
called `transmitted`, so the counter never moved and `CMD_READY` was never sent
— which works until somebody enables flow control and then stops after one
packet. And the SNR was being rounded to whole decibels in the modem and
multiplied back up for the protocol, throwing away up to half a decibel for
nothing.

### The first flash did not enumerate

The board came up as `oxinode RNode`, macOS read its device and string
descriptors, and then stopped: `!registered, !matched`, stable across minutes,
so not a reset loop — a device that answered the first control transfers and
then went quiet.

The one structural difference from the two images that do enumerate is that
both of those touch nothing until the host has finished. `usb-cdc` parks every
task on `wait_connection`; `radio` waits for DTR before it goes near the radio.
This image started a 250 ms bring-up — 191 ms of it the LR1121's reset —
concurrently with enumeration.

So the modem now waits for `wait_connection` **or two seconds, whichever comes
first**. The first half is the fix; the second half is the part that keeps it
honest, because a board on battery with no host must still bring its radio up,
and a fix that depends on a host being present would have swapped one failure
for a quieter one.

This left the board with no serial port at all, and therefore no way to take
the 1200-baud touch — recovery is a physical double-tap of the reset button.
