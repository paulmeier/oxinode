# Phase 17 — the header byte, and the split at 255

Where phase 17 stands: **built, tested on the host, and verified on two
boards with packets of every size up to Reticulum's `HW_MTU` of 508.**
Every LoRa frame oxinode sends now carries the stock RNode's one-byte air
header in front -- a sequence nibble and a split flag -- and a packet
longer than a frame goes as two frames the receiver joins by that nibble.
Verified on 2026-09-08 with two oxinodes driven over KISS by a host script:
100, 254, 255, 300, 400, 500 and 508 bytes, each way, fourteen of fourteen
byte-for-byte. Not done: the same exchange against a stock RNode, because
there is not one on this desk. The frame layout is what a stock RNode puts
on the air, taken as interface data, and the receiver's rules are pinned
by a test that writes a stock split out by hand.

This is the rest of 15d from the phase 15 breakdown, which put "what
oxinode puts on the air" in one phase: carrier sense went in as phase 16,
and this is the other half.

## What the modem did before

`rnode` handed the host's packet to `Modem::transmit` unchanged, and
`Modem::transmit` refused anything over 255 bytes. Phase 15 recorded the
two consequences under "for the record": oxinode could not carry a
full-MTU Reticulum packet, and an oxinode and a stock RNode did not
interoperate over the air at all -- the stock firmware read the first byte
of every oxinode packet as a header, and oxinode read every stock header as
the first byte of a packet.

Neither bit during phases 15 and 16 because both boards on the bench were
oxinodes and every packet the spike sent was under 255 bytes. Reticulum's
`RNodeInterface` assumes the firmware splits: its `HW_MTU` is 508, which is
two frames of 254, and `rnsd` will hand the modem a 500-byte packet the
moment a link carries a resource.

## The arrangement

The decision is in `oxinode-core`, in `core/src/rnode/air.rs`, and holds
no radio: frames in, packets out, and packets in, frames out. The
chip-facing half is two call sites in `src/bin/rnode.rs` and one new
method in `src/modem.rs`.

**The header.** One byte in front of every frame: the sequence nibble in
the high four bits, the split flag in bit 0, the other three low bits zero.
A frame with the flag clear is a whole packet; one with it set is one half
of a packet.

**The split.** A packet of up to 254 bytes is one frame of up to 255. A
longer one is two frames under the same header with the flag set: the
first carries the first 254 bytes and is a full 255-byte frame, the second
carries the rest. 508 bytes is two full frames, and 509 does not go --
`Split::new` refuses it, and the KISS decoder's capacity is `HW_MTU`, so a
host cannot ask.

**Reassembly.** The receiver holds at most one first half. A whole packet
is handed up as it arrives, and throws away any half being held. A split
frame with nothing held, or with a different nibble held, is a new first
half and replaces what was there. A split frame with the same nibble held
is the second half: the two are joined, handed up, and the signal
reported is the mean of the two frames'. These are a stock RNode receiver's
rules, and a test writes a stock split of a 300-byte packet out by hand --
254 bytes, then 46, one header -- so the split point is pinned by the test
rather than by the code under it.

**The second frame goes straight after the first.** `Modem::transmit`
listens before it sends, and the first frame of a split takes that wait as
any packet does. The second does not: `Modem::send` is the tail of
`transmit` with no carrier sense in front of it, and the second frame goes
the moment the first is done. The receiver is holding the first half for
it, and a stock RNode holds it until *any* other packet arrives and throws
it away, so a wait here would be a window for a neighbour to spoil the
join. The channel read clear a moment ago, and a neighbour that sensed it
heard this board's first frame.

Two departures from the stock arrangement, both deliberate:

* **The sequence counts.** The stock firmware draws its nibble at random,
  so two consecutive split packets from one board share a nibble one time
  in sixteen -- and a receiver that lost the second half of the first joins
  it to the first half of the second, and hands up 508 bytes of two
  packets. `Sequence` steps instead, from a seed drawn from the chip's ID,
  so consecutive packets from one board never share a nibble. A receiver
  cannot tell the difference.
* **A first half goes stale.** The stock receiver holds a half until
  something replaces it. `Reassembler` holds it for two airtimes of a full
  frame plus a second -- 2.4 s at the bench configuration, under a minute
  at SF12 and 62.5 kHz -- because the second half follows the first with
  nothing between them, and one that has not come by then is not coming. A
  later split with the same nibble is then a new packet rather than the
  other half of a stale one. A reconfiguration clears it outright: the
  other half of a packet from the old channel is not coming on the new one.

The frame layout is interface data -- the header's fields, the flag, the
split point, the same header on both halves, the receiver's rules for
joining and discarding -- and is what any receiver has to know to read a
stock RNode's frames. It was taken from the RNode firmware's published
constants and descriptions of its behaviour, the same way phase 16 took its
carrier-sense parameters. Its code is GPL and none of it was read, copied,
ported or transliterated; the `README`'s licensing section still holds.

## On the host: sixteen tests

`cargo test -p oxinode-core air`. The ones the phase asked for:

* **A single frame.** Every length from 0 to 254 is one frame of length
  plus one, header first with the flag clear, the packet behind it.
* **A two-frame split.** 255, 400 and 508 bytes are two frames under one
  header with the flag set, the first full, the second the rest; joined
  back to the original with the signal averaged.
* **A missing second frame.** Nothing is handed up for the first half, and
  the next packet -- a whole one, or a split with a different nibble -- is
  delivered on its own with the stale half thrown away rather than joined
  to it.
* **A missing first frame.** The second is held as if it were a first,
  which is all a receiver can do with it, and is never delivered alone.
* **Out of order.** The two halves of one packet are indistinguishable by
  header, so a receiver joins them in arrival order; the test pins that it
  does so without panicking and delivers the right length.
* **The sequence nibble.** Steps through all sixteen values and wraps,
  consecutive packets never share one, and the header of each packet
  carries the nibble it was given.

And the rest: every length from 1 to 508 through the split and back; a
stock RNode's split written out by hand; a stale half not completed; empty
frames and header-only packets handed up as nothing; a second half longer
than there is room for cut at 508 rather than overrunning; and the maximum
age following the airtime.

## On the bench

Two boards, both running `rnode`, both driven by one host script over
KISS -- the same detect, five setters and power-on that `rnsd` sends, then
`CMD_DATA` with a packet of random bytes through one board and the
`CMD_DATA` that comes out of the other compared to it. 915 MHz, 125 kHz,
SF8, CR 4/5, 14 dBm, the two boards on one desk.

| bytes | frames | A → B | B → A | time to `CMD_READY` |
|---|---|---|---|---|
| 100 | 1 | OK | OK | 1.11 s, 0.45 s |
| 254 | 1 | OK | OK | 1.49 s, 2.52 s |
| 255 | 2 | OK | OK | 1.58 s, 1.90 s |
| 300 | 2 | OK | OK | 2.17 s, 1.24 s |
| 400 | 2 | OK | OK | 1.78 s, 2.23 s |
| 500 | 2 | OK | OK | 3.16 s, 2.52 s |
| 508 | 2 | OK | OK | 2.52 s, 2.04 s |

Fourteen of fourteen, byte for byte. The time to `CMD_READY` is the
carrier-sense wait plus the airtime; the packet reached the other host
about 50 ms after it. The boards' logs for the 400-byte packet each way:

```
18:03:14.730 tx: 400 bytes in 2 frames, 1138884 us, after 9 senses (1 busy) and 196608 us waiting
18:03:16.666 rx: 255 bytes, half of a split packet; holding it
```

```
18:03:14.259 rx: 255 bytes, half of a split packet; holding it
18:03:17.108 tx: 400 bytes in 2 frames, 1138885 us, after 19 senses (1 busy) and 442368 us waiting
```

1,138.9 ms is the airtime of a 255-byte frame and a 147-byte one at this
rate, 707.7 and 431.2 ms, with nothing between them; the 508-byte packet
took 1,415.3 ms, two full frames. The second frame's arrival is not logged
separately because it is not a separate event to the host: it completes
the packet, which goes up as `STAT_RSSI`, `STAT_SNR` and `DATA` the way any
packet does.

Before that, the phase 15 exchange was repeated with the spike image on one
board and `rnode` on the other, both rebuilt, the host talking Reticulum
through the `rnode` board as an `RNodeInterface`: four of four host packets
echoed, every LXMF message answered, every frame on the air one byte
longer than the packet in it. That is the two images agreeing on the
header; the table above is the split.

## What it costs

One byte per frame, which at the bench rate is 2.4 ms of airtime on a
packet of any size. A packet over 254 bytes pays a second preamble and
header -- 51.7 ms here -- and the second frame's airtime, which it would
have paid anyway if it could have gone at all. The reassembler is 508 bytes
of RAM and one comparison per frame.

## Not done

* **Against a stock RNode.** There is not one on this desk. The layout is
  the stock one and the receiver's rules are pinned by a hand-written stock
  split, but an exchange with the real thing is the test that settles it,
  and it should be run the first time one is to hand: `rnsd` on each, a
  400-byte packet each way, and the two logs.
* **A half lost on the air.** The receiver's handling of a missing first
  or second frame is tested on the host and not on the bench; the bench
  cannot lose a frame to order.
* **Through `rnsd`.** The bench test drives the modem over KISS the way
  `RNodeInterface` does, with the same setters and the same data frames,
  but it is not `rnsd`. A link with a resource transfer between two
  Reticulum instances, each on its own board, would carry 500-byte packets
  and is the natural next check.
* **The `radio` bring-up image** still puts raw frames on the air. It is a
  phase 3 image for proving the chip transmits and receives at all, and a
  header would only get in the way of that.

## Done when

- [x] **Every frame carries the header byte**, on `rnode` and on the
      spike. `Split` in `oxinode_core::rnode::air`; every length from 0 to
      508 tested.
- [x] **A packet longer than a frame goes as two and comes back as one.**
      `Reassembler`; the split at 254, the same header on both halves, the
      signal averaged. Sixteen tests.
- [x] **A missing or out-of-order half does not deliver a wrong packet.**
      Tested on the host: nothing is handed up for a lone half, and a stale
      one is discarded rather than joined.
- [x] **The sequence nibble changes between consecutive packets.** It
      steps, from a seed off the chip's ID; tested.
- [x] **A full-MTU Reticulum packet crosses between two oxinodes.** 508
      bytes each way on the bench, and every size below it that was tried.
- [ ] **An oxinode and a stock RNode exchange packets over the air.** The
      layout is the stock one; the exchange has not been run.

## Reproducing

```
# both boards on the product image. With two boards attached the flasher
# must be told which; a board sitting in its bootloader needs
# OXINODE_DFU_IN_DFU=1 as well.
cargo build --release --no-default-features --features ble --bin rnode
OXINODE_DFU_PORT=/dev/cu.usbmodemAAA tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rnode
OXINODE_DFU_PORT=/dev/cu.usbmodemBBB tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rnode

# then each board's second port through defmt-print with the rnode ELF,
# and the host script on both first ports
```

The host script is a hundred lines of Python and pyserial: KISS framing
with `CMD_DATA` escaped and the single-byte frames not, the five setters
and `CMD_RADIO_STATE`, then for each size a packet of random bytes one way
and a wait for `CMD_READY` on the sender and `CMD_DATA` on the receiver.
The `tx:` line on the sender's log says how many frames it went as, and
`half of a split packet; holding it` on the receiver's marks the first
half arriving.

Two things met on the way that are about the bench rather than the phase.
The spike image did not reboot into its bootloader on the flasher's own
1200-baud touch, twice; opening its first port at 1200 baud from Python,
raising and dropping DTR, and closing it put the board in the bootloader
every time, after which the flasher with `OXINODE_DFU_IN_DFU=1` did the
rest. And building from a git worktree under `.claude/worktrees/` links
with every `rustflags` entry twice -- cargo merges `.cargo/config.toml`
from the worktree *and* from the repository above it, and the linker
refuses the second `-Tlink.x` with "region 'FLASH' already defined".
`RUSTFLAGS` set to the config's three flags replaces both and builds
cleanly; the target-specific `CARGO_TARGET_*_RUSTFLAGS` variable did not.
`tools/test.sh` was run that way, every step passing, with its two build
steps repeated by hand under `RUSTFLAGS` and their layout checks after.
