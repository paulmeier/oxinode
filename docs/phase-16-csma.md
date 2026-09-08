# Phase 16 — listening before transmitting

Where phase 16 stands: **built, measured on two boards, and the phase 15
collision no longer loses the host's reply.** The modem senses the channel
before every transmission, backs off a random number of slots when it is
busy, and spends the wait in receive, so a packet that arrives while it is
waiting is received rather than transmitted over. Verified on 2026-09-08 by
repeating the phase 15 over-the-air run with the spike image announcing its
two destinations back to back, which is the form that lost the host's reply
every time. Not done: packets that begin in a part of the sense cycle the
receiver cannot sync to are avoided but not received, and that costs one leg
of roughly one exchange in five on a busy bench. It is open at the bottom,
with two ways to close it.

This is 15d of the phase 15 breakdown, done first because it is the one
that applies to the RNode image as it stands: two oxinodes that both have
traffic collided on the bench with no host doing anything wrong, and the
host saw packets go missing with no error.

## What the modem did before

`Modem::transmit` loaded the buffer and issued `SetTx`. Nothing asked
whether the air was in use, because until phase 15 nothing on this bench
transmitted unless a host told it to, and one host does not collide with
itself. The phase 15 spike put a second transmitter on the bench and the
first over-the-air run lost the host's packet and the board's second
announce to each other every time for two minutes: the board announced two
destinations back to back, 0.48 s of airtime and then 0.51 s more, and the
host answered the first within a hundred milliseconds of decoding it, while
the second was still on the air.

## The arrangement

The decision is in `oxinode-core`, in `core/src/lr1121/csma.rs`, and holds
no radio: it is fed one sense at a time and answers with what to do next.
The chip-facing half is in `src/modem.rs`.

**Slots.** Twelve symbols, bounded to 6–100 ms: 24.6 ms at the spike's
SF8 and 125 kHz, the floor at SF7 and 500 kHz, the ceiling from SF12 at
125 kHz on up. A slot is a fraction of a packet at any data rate rather than
a number of milliseconds.

**DIFS, then a window.** The channel has to be sensed clear for two slots
and then for a further `0..=15` slots drawn at random. Any busy sense starts
the count over with a fresh draw. Two boards that both want the air the
moment it goes quiet draw different windows; one goes first and the other
hears it.

**The draw.** SplitMix64's output function over the timer's tick count at
the moment the transmission was asked for, plus one for each redraw. Not a
random number generator and not pretending to be one: the property that
matters is that two boards seeded from their own uptimes do not draw the
same window every time, and neighbouring seeds -- consecutive ticks -- give
unrelated draws. Both are tested.

**A budget.** Ten airtimes of the packet being held, bounded to 2–20 s, and
then the packet goes regardless, with the report saying so. The stock
firmware waits forever; this modem's loop also serves USB, Bluetooth and the
panel, and a channel that reads busy for ten airtimes running is more likely
a detector fooled by noise than a neighbour with that much to say. It never
happened in any run.

This follows the stock RNode firmware's arrangement -- slots of twelve
symbols bounded in milliseconds, a DIFS of two slots, a contention window of
fifteen -- taken from its published configuration values and descriptions of
its behaviour, which are facts about how a stock RNode shares a channel. Its
code is GPL and none of it was copied, ported or transliterated; the
`README`'s licensing section still holds. Two of its refinements were left
out on purpose: contention-window bands chosen by measured channel
utilisation, and a noise-floor estimate with an interference threshold. Both
are listed below.

## Sensing, and what took four runs

Each sense is a LoRa **channel activity detection**: the chip correlates
four symbols against the modulation it is configured for and raises
`CadDone`, with `CadDetected` alongside if it found one. `CadDetected` is
not routed to the interrupt pin -- it would wake the MCU for every packet on
a busy band -- but it is in the pending word, which is where the modem reads
it. The detection thresholds are Semtech's reference values for this chip
family, 50 and 10; four symbols rather than the reference two, because this
is asked to hear a packet already in progress and not only a preamble. It
does: in every run the busy counts track the other board's transmissions
symbol for symbol, mid-packet included.

That was run 1, and it worked as far as it went:

```
16:39:57.457 tx: 243 bytes in 676940 us, after 28 senses (5 busy) and 663552 us waiting
16:39:59.493 tx: 167 bytes in 482391 us, after 18 senses (10 busy) and 417792 us waiting
```

Every transmission waited for the other board to finish. And every packet
that arrived *during* a wait was lost, because the waiting board was not
receiving: it was in standby between senses. Three exchanges in that run
lost a leg, and the timestamps put every one of them inside the other
board's backoff. Avoiding a collision by going deaf is a smaller loss than
the collision -- one packet instead of two -- but it is still a loss, and it
is the packet the whole exercise is about.

So the slot between senses is spent in **receive**, single-shot with the
chip's own timeout. Run 2 did that and received nothing at all during a
wait, in either direction. The chip's receive timer stops, by default, on
header detection, and a slot is shorter than a preamble and a header
together: a packet that started mid-slot was timed out a few milliseconds
before it could have been decoded, every time, and the next sense then
found data symbols it could flag but not sync to.

Two chip settings fix that, and they are the substance of the modem change:

* **`StopTimeoutOnPreamble`.** The receive timer stops on the preamble
  instead of the header, so a packet that starts inside a slot is received
  whole, however long it is, and the wait stretches to fit it.
* **CAD exiting into receive.** A positive detection leaves the chip in
  receive for the airtime of an empty packet -- 51.7 ms at the spike's
  configuration -- instead of returning it to standby. A detection made on
  a preamble is a packet about to arrive, and this is how it is caught.

Run 3 received during waits on both boards:

```
16:55:11.491 air: 131 bytes while waiting to send, rssi -46 dBm, snr 17 dB
16:55:14.401 air: 167 bytes while waiting to send, rssi -46 dBm, snr 15 dB
16:55:15.648 air: 215 bytes while waiting to send, rssi -46 dBm, snr 17 dB
```

and failed two transmissions with `NoInterrupt` after a second of silence,
which the host took as a hardware transmit error and answered by taking the
interface offline and reconnecting it five seconds later. A timer stopped
on the preamble is a timer that never runs out: a false preamble, or one
whose header a collision corrupted, leaves the chip receiving with nothing
to end it. The LR1121 has a second timer for exactly this,
`SetLoRaSynchTimeout`, in symbols from the start of the receive; it is set
on the listening windows to a slot plus the configured preamble plus a
header plus a margin -- 41 symbols at the spike's configuration -- and taken
off again when `start_rx` returns the chip to continuous receive, where a
symbol timeout would make the modem go deaf after a hundred milliseconds of
quiet. The software deadline that caught run 3's hang stays as a backstop,
and now abandons the receive and reports busy instead of failing the
transmission.

Run 4 is the result. Two minutes, the spike announcing both destinations
back to back every twenty seconds, the host answering through the second
board as an ordinary `RNodeInterface`:

| | run 1 | run 2 | run 3 | run 4 |
|---|---|---|---|---|
| sensing | CAD, standby between | CAD, receive between | + preamble stop, CAD into receive | + symbol timeout |
| `rnode` transmissions | 35 | 32 | 32 | 39 |
| busy senses / senses (`rnode`) | 166 / 706 | 156 / 543 | 76 / 490 | 92 / 645 |
| packets received while waiting (both boards) | 0 | 0 | 10 | 18 |
| transmit failures | 0 | 0 | 2 | 0 |
| host packets echoed | 4 of 6 | 1 of 4 | 2 of 4 | 4 of 5 |
| LXMF messages answered | 3 of 3 | 2 of 3 | 3 of 3 | 3 of 4 |
| forced past the budget | 0 | 0 | 0 | 0 |

Run 1 had the spike's announces ten seconds apart, as phase 15 left them;
runs 2 to 4 have them back to back. Run 1's spacing is why its numbers look
better than run 2's.

## The phase 15 collision, on run 4

The host's reply arrives while the board is still trying to send its second
announce. The board hears it, hands it to the node, sends the announce, and
then sends the echo the node queued in answer:

```
17:00:49.295 air: sent 167 bytes in 482238 us (airtime 481792 us) after 29 senses (2 busy, 688128 us waiting)
17:00:50.123 air: 131 bytes while waiting to send, rssi -46 dBm, snr 16 dB
17:00:50.123 send: 131 bytes to [76, 3a, 46, f6] (encrypted in 93719 us)
17:00:51.776 air: 167 bytes while waiting to send, rssi -46 dBm, snr 17 dB
17:00:52.615 air: sent 180 bytes in 513000 us (airtime 512512 us) after 32 senses (7 busy, 761856 us waiting)
17:00:53.035 air: sent 131 bytes in 390075 us (airtime 389632 us) after 3 senses (1 busy, 49152 us waiting)
```

```
[  23.906] announce oxinode.spike from <3d71795ba984cd80d8e197788b0401eb>
[  23.908] sent 131 bytes to oxinode.spike: b'hello from the host #1' (receipt True)
[  27.699] packet at oxinode.spike: b'echo: hello from the host #1' (131 bytes on the wire, via RNodeInterface[spike rnode])
```

The 167-byte packet heard at 17:00:51.776 is the host's own announce,
handed to the second board while the first was still waiting; the host's
side of that wait is the `rnode` line below, three busy senses, and the
announce going out only once the air was clear:

```
17:01:12.400 tx: 167 bytes in 482391 us, after 12 senses (3 busy) and 270336 us waiting
```

Every one of the five announce pairs in run 4 had the host's reply land on
the second announce, and none was lost to it. That is the finding phase 15
asked for.

## What it costs

A clear channel costs the DIFS and the window: two to seventeen slots, a
median of nine, so about 220 ms before a packet at the spike's rate and
about 50 ms at SF7 and 500 kHz. Measured over run 4, the wait before a
transmission was a median of 295 ms on the `rnode` board and 381 ms on the
spike, means of 382 and 483 ms, and the longest 1.35 s -- the busy senses
and the packets received during the wait are in those figures. Each sense
is about 10 ms of the chip listening for four symbols and deciding; a busy
sense that falls into receive costs up to the 51.7 ms it waits for a header
that a mid-packet detection never provides.

The report is in the log on every transmission, on both images:

```
tx: 131 bytes in 390228 us, after 13 senses (1 busy) and 294912 us waiting
```

and a wait that runs past the budget is a warning, which no run produced.

## What is still lost, and why

Two of run 4's nine exchanges lost one leg -- packet #3's echo and the
fourth LXMF message -- and both were sensed. The `rnode` board's log for the
first:

```
17:01:34.883 tx: 167 bytes in 482391 us, after 35 senses (11 busy) and 835584 us waiting
```

Eleven busy senses that were the spike's echo going past, flagged and not
received. The sense cycle is a detection of about 10 ms followed by a slot
of receive, and a packet that starts during the detection, or in the last
few symbols of the slot, is in the wrong place: the detection fires on its
preamble and falls into receive, but by then too little preamble remains
for the receiver to sync on. Every later sense finds data symbols, which the
detector hears and the receiver cannot decode. Something under half of the
cycle is a window like that, and the exchanges lost in run 4 are the ones
whose packet began in it. Before this phase that packet was lost too, and
so was the one the board transmitted over it.

Two ways to close it, either of which is its own small phase:

* **A longer preamble.** The stock firmware sends at least eighteen
  symbols and targets 24 ms of preamble, and that is why: a receiver that
  starts listening a few symbols late still has enough to sync on. oxinode
  sends the eight the host asks for. Any receiver decodes a longer preamble
  than it expects, so this needs no agreement with the other end, only a
  decision about what the firmware sends when the host says eight.
* **Sensing from inside receive.** Stay in continuous receive and sense
  with the chip's preamble and header interrupt bits, which are in the
  pending word, and `GetRssiInst` against a noise floor -- the stock
  firmware's carrier detect. No cycle, so no window; the price is the
  noise-floor estimate this phase left out, and weak mid-packet signals
  below the threshold that CAD would have heard.

## Not done

* **Contention-window bands.** The stock firmware widens the window as
  measured channel utilisation rises. One band here.
* **The noise floor and interference threshold.** See above.
* **The stock RNode's air header and the 255-byte split**, which phase 15
  put in the same phase as this. Not touched here: the boards still send
  raw Reticulum packets and refuse anything over 255 bytes. *Done as
  phase 17; see [phase-17-air-header.md](phase-17-air-header.md).*
* **CAD thresholds tuned on this board.** Semtech's reference values,
  which hear a −46 dBm neighbour without a miss; at range they are untested.
* **A heard packet's routing in `rnode`.** It goes to the host that asked
  for the transmission, the way that host's answers do. With a phone
  connected and a USB host transmitting at the same moment, that is the USB
  host, where the main loop would have chosen the phone.
* **The spike image now announces both destinations back to back again**,
  as it did on the first phase 15 run, because that is the collision this
  phase exists to reproduce. It is not a product image.

## Done when

- [x] **The modem senses the channel before every transmission and backs
      off when it is busy.** `Modem::sense` and the loop in
      `Modem::transmit`; the decision in `oxinode_core::lr1121::csma`,
      fourteen tests.
- [x] **The wait is bounded.** Ten airtimes, 2–20 s, then forced with a
      report and a warning; never reached on the bench.
- [x] **A packet that arrives during the wait is received.** Eighteen in
      run 4, on both boards, and handed up: the node answered them, the
      host got its frames.
- [x] **The phase 15 collision no longer loses the host's reply.** Five of
      five announce pairs in run 4, spike announcing back to back, echo
      received every time.
- [x] **A receive that never ends cannot fail a transmission.** The symbol
      timeout on the listening windows, and the software deadline that
      reports busy instead of `NoInterrupt`; no transmit failures in run 4
      against two in run 3.
- [ ] **A packet that begins during a sense is received.** It is avoided
      instead. The two remedies are above.

## Reproducing

```
# the product image on one board and the spike on the other. With two
# boards attached the flasher must be told which; a board already sitting
# in its bootloader needs OXINODE_DFU_IN_DFU=1 as well.
cargo build --release --no-default-features --features ble --bin rnode
cargo build --release --features spike --bin rns-spike
OXINODE_DFU_PORT=/dev/cu.usbmodemBBB tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rnode
OXINODE_DFU_PORT=/dev/cu.usbmodemAAA tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rns-spike

# each board's second port through defmt-print, with the matching ELF, then
# the host through the rnode board's first port
tools/spike_peer.py --rnode /dev/cu.usbmodemBBB --seconds 120
```

The `tx:` lines on the `rnode` log and the `air: sent` lines on the spike's
carry the sense counts; `while waiting` marks a packet received during a
wait. Lay the three logs against each other by wall clock: the peer prints
its own, and a reader that stamps each decoded line with the host's clock
is a dozen lines of Python around `defmt-print`.
