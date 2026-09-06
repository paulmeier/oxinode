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
