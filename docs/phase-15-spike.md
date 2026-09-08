# Phase 15 — messaging from the node: the spike

Where phase 15 stands: **measured, not built.** The issue asked whether a
Reticulum stack fits on this board and talks to a real one before anything
is designed, and said no implementation should start until that was known.
This is the record of finding out. It ends with a recommendation and a
phase breakdown, and with question 5 deliberately unanswered, because #9 has
to be built and used first.

The stack measured is [rns-rs](https://github.com/lelloman/rns-rs), as
decided at the start: `rns-core` 0.1.17 and `rns-crypto` 0.1.10 from
crates.io, with `default-features = false`, which is their `no_std` shape.
Leviculum was not built; see question 6.

## What was built to measure it

**A throwaway image**, `rns-spike`, in `src/bin/rns_spike.rs`, behind a
`spike` feature so the product image never links any of this:

```
cargo build --release --features spike --bin rns-spike
OXINODE_DFU_PORT=/dev/cu.usbmodemNNN cargo run --release --features spike --bin rns-spike
```

It brings the radio up through the same `Modem` the RNode image uses,
generates an identity, times every primitive an announce and an encrypted
packet need, and then runs `rns-core`'s `TransportEngine` with two
interfaces: the air, and the first USB port as a Reticulum `SerialInterface`
-- HDLC framing, which is what `rnsd` speaks to a serial line and needs no
second board. It announces `oxinode.spike` and an `lxmf.delivery`
destination, echoes encrypted packets, and answers LXMF messages with LXMF
messages. The second USB port is the defmt log, as always.

**The other end**, `tools/spike_peer.py`: stock Python Reticulum 1.5.0 and
LXMF 1.1.1 in a configuration directory of their own, driving either the
spike image's serial port (`--serial`) or a second oxinode running `rnode`
as an ordinary `RNodeInterface` (`--rnode`). It announces, sends an
encrypted packet and an opportunistic LXMF message whenever it hears the
board, and prints what comes back with the signature check LXMF itself
performs.

Nothing in the image is meant to survive the spike. The LXMF piece in it is
the one part worth keeping an eye on, because it is a measurement of its own:
`rns-rs` has no LXMF, and the spike shows what the smallest useful piece of
one costs -- a hundred and twenty lines, written against the Python source,
wrong once (see below) and right the second time.

## Question 1: does it build, and what does it cost?

**It builds.** `rns-core` and `rns-crypto` compile for `thumbv7em-none-eabihf`
with no patches, in nine seconds from clean, on the same stable toolchain the
rest of oxinode uses. The image needs a global allocator -- `rns-core` is
`no_std` plus `alloc`, its tables are `BTreeMap`s and its packets are
`Vec`s -- and `embedded-alloc` 0.7 supplies one on a static 64 KB region, so
the heap shows up in the RAM figure below rather than hiding in the gap the
stack grows into. It also needs a `log` sink; a twenty-line bridge formats
each line on the heap and hands it to defmt.

Measured by `tools/check_layout.py`, release profile, `opt-level = "s"`,
fat LTO:

| image | flash | RAM (static) | what it holds |
|---|---:|---:|---|
| `rns-spike` | 283,768 | 80,348 | USB, radio, `rns-core`, a 64 KB heap |
| `radio` (phase 3 bring-up) | 72,848 | 7,824 | USB, radio, diagnostics -- the nearest thing to the spike with the stack removed |
| `rnode` (the product) | 309,436 | 53,172 | USB, radio, Bluetooth, panel, pad, GPS, store |

So the stack and its allocator cost **about 210 KB of flash** over an image
that has the same USB and radio without them. `rust-nm` attributes the
spike's text section like this, in bytes:

| crate | bytes | note |
|---|---:|---|
| `alloc` | 55,358 | `BTreeMap`, `Vec` and `format!` monomorphised for `rns-core` |
| `rns_core` | 48,060 | the engine, packets, announces, msgpack |
| `sha2` | 35,278 | SHA-256 for hashes, SHA-512 inside Ed25519 |
| `rns_spike` | 27,122 | the image itself, with engine code inlined into it |
| `curve25519_dalek` | 14,722 | X25519 and Ed25519 arithmetic |
| `core` | 12,768 | |
| `embassy_executor` | 10,546 | |
| `rns_crypto` | 8,814 | |
| `aes` | 5,914 | |
| `x25519_dalek`, `hmac`, `libm` | 1,754 | |

Put beside the product image: **`rnode` plus this stack is roughly 520 KB of
the 784 KB the bootloader leaves, two thirds of it.** That fits, with room
for an LXMF and the screens, and not with room for carelessness.

RAM is the more interesting number and it comes from the heap, which the
image reports every ten seconds:

| moment | heap used |
|---|---:|
| after the crypto timing, nothing allocated | 0 |
| `TransportEngine` built, two destinations registered, two interfaces | 28,660 |
| two paths learned, announces and messages flowing (peak over a run) | 37,200 – 39,300 |

The 28 KB at rest is mostly the packet hashlist, allocated eagerly for the
256 entries the spike asked for; the engine's other tables are sized by
configuration too and the spike sized every one of them for a bench, not the
internet. **Peak heap under traffic was under 40 KB with a 64 KB heap**, and
the stack pointer had 173 KB of headroom above the statics throughout.
Adding a 64 KB heap to `rnode`'s 53 KB of statics gives 117 KB of 256, which
leaves what the Bluetooth controller and the stack need with margin. The
message store the roadmap wants is not in this figure; it belongs in the
QSPI flash oxinode does not drive yet.

## Question 2: does it talk?

**Yes, both ways, at every layer the spike reaches: announce, encrypted
packet, and LXMF message.** First over the serial interface, then over the
air.

### Over USB, one board

The peer script's Reticulum opened the spike image's first port as a
`SerialInterface`. The board's log, times in seconds since boot:

```
28.039550 DEBUG serial: frame of 167 bytes
28.086578 INFO  announce heard: [f1, fb, 7b, e9] oxinode.spike hops=1 via 2
28.090423 DEBUG serial: frame of 215 bytes
28.137054 INFO  announce heard: [4f, db, a7, a2] lxmf.delivery hops=1 via 2
```

Each of the host's announces was unpacked, its signature verified and a path
recorded in 47 ms. In the other direction the host's own announce handlers
fired for the board's `oxinode.spike` and `lxmf.delivery` announces -- which
Python Reticulum only does after it has validated them -- and it answered:

```
[  15.798] announce oxinode.spike from <4e56eb82526913878fdb163d27f52b3b>
[  15.802] sent 131 bytes to oxinode.spike: b'hello from the host #1' (receipt True)
[  16.983] packet at oxinode.spike: b'echo: hello from the host #1' (131 bytes on the wire)
```

```
116.202758 INFO  spike message: hello from the host #1 (22 bytes, decrypted in 45806 us)
116.296997 INFO  send: 131 bytes to [f1, fb, 7b, e9] (encrypted in 93902 us)
```

That is a packet encrypted by Python to the board's X25519 key, decrypted by
`rns-crypto`, and one encrypted by `rns-crypto` to the host's key, decrypted
by Python: the token format -- ephemeral key, IV, AES-CBC, HMAC -- agrees on
both sides.

Then LXMF. The host's standard `LXMRouter` sent an opportunistic message
whenever it heard the board's delivery destination, and the board answered
with one built by the hundred and twenty lines in the image:

```
21.514038 INFO  lxmf message from [4f, db, a7, a2]: title=spike content=hello from the host, message 1 signature ok (decrypted in 46325 us)
21.558288 INFO  lxmf: 141 bytes packed and signed in 44158 us
21.653625 INFO  send: 243 bytes to [4f, db, a7, a2] (encrypted in 94940 us)
```

```
[  23.738] LXMF from <e77d6c023da52809dbd2adfc6e8698d1>: title='' content='the board says: hello from the host, message 1' timestamp=1788900669.387 signature_validated=True method=1
```

`signature_validated=True` is LXMF's own verdict on the board's Ed25519
signature over the board's msgpack, not the spike's opinion of itself.

The first attempt at this failed on the board with `malformed (not a
list)`, and the reason is worth writing down for whoever writes the real
one: **an opportunistic LXMF message goes on the wire without its leading
destination hash.** The packet is addressed to that hash already, so
`LXMessage.pack` strips the first sixteen bytes before encrypting and the
receiver's router puts its own back before hashing and verifying. The
format description in the LXMF source does not say so; `LXMRouter.
delivery_packet` does.

Two more things the run showed about what a real implementation owes:

* **Proofs.** The board sends none, so the host's `packet.send()` receipts
  never complete and LXMF, seeing no delivery, re-sends each message
  opportunistically every ten seconds -- which is why the board's log shows
  every message arriving several times, and why the host received several
  replies to each. The board also ignored the host's proofs of *its*
  messages (`ignoring packet type 3`). Proof generation and receipt handling
  are in `rns-core`'s packet layer; wiring them is part of the LXMF work,
  not extra.
* **Path requests.** The host broadcast path requests (51-byte packets to a
  plain destination) that the spike's engine, running with transport off,
  ignored. Fine for an endpoint; a node that wants to be found across a
  transport node answers them.

### Over the air, two boards

The second board was flashed back to `rnode` and Reticulum drove it as an
ordinary `RNodeInterface` on the spike image's default radio configuration
-- 915.000 MHz, 125 kHz, SF8, 4/5, 14 dBm, 3125 bps. Announces went over
the air in both directions, at −45 to −50 dBm across the bench:

```
219.359619 DEBUG air: 167 bytes, rssi -50 dBm, snr 15 dB
219.406829 INFO  announce heard: [f1, fb, 7b, e9] oxinode.spike hops=1 via 1
220.038146 DEBUG air: 215 bytes, rssi -50 dBm, snr 15 dB
220.084442 INFO  announce heard: [4f, db, a7, a2] lxmf.delivery hops=1 via 1
```

```
[  20.667] announce oxinode.spike from <b63ad3238341c30789f5c2294c3229be>
[  20.668] sent 131 bytes to oxinode.spike: b'hello from the host #1' (receipt True)
```

And then, on the first run, nothing more: the host's packet never reached
the board and the board's second announce never reached the host, every
time, for two minutes. The reason is in the timestamps. The image announced
its two destinations back to back, 0.48 s of airtime and then 0.51 s more;
the host answered the first within a hundred milliseconds of decoding it,
which is while the board was still transmitting the second. A LoRa radio is
half duplex, and **neither `rnode` nor the spike image listens before it
transmits**: the reply and the second announce were on the air together and
both were lost. The stock RNode firmware does carrier-sense before
transmitting; oxinode's modem has never needed to, because until now nothing
on this bench transmitted unless a host told it to. That is a finding about
`rnode`, recorded below, and it will matter to any on-board stack more than
the crypto does.

Spaced ten seconds apart, the announces stopped colliding with the answers,
and the whole exchange happened over the air, board to board, at −45 dBm:

```
22.909912 INFO  air: sent 180 bytes in 512969 us (airtime 512512 us)
23.684906 DEBUG air: 243 bytes, rssi -45 dBm, snr 15 dB
23.774566 INFO  lxmf message from [4f, db, a7, a2]: title=spike content=hello from the host, message 1 signature ok (decrypted in 46356 us)
23.818847 INFO  lxmf: 141 bytes packed and signed in 44158 us
23.914611 INFO  send: 243 bytes to [4f, db, a7, a2] (encrypted in 95367 us)
24.594818 INFO  air: sent 243 bytes in 676849 us (airtime 676352 us)
24.948486 DEBUG air: 83 bytes, rssi -45 dBm, snr 15 dB
24.948944 DEBUG deliver: ignoring packet type 3
```

```
[  12.681] announce lxmf.delivery from <87976a1957cf235a7f7d501b582e62f0> name='oxinode spike'
[  12.682] sent LXMF <145ebba2...> to <87976a1957cf235a7f7d501b582e62f0>, 146 bytes packed, method 1
[  14.361] LXMF from <87976a1957cf235a7f7d501b582e62f0>: content='the board says: hello from the host, message 1' signature_validated=True method=1
[  43.762] announce oxinode.spike from <3ed262ee744dea03f75a48ee19345bf9>
[  43.762] sent 131 bytes to oxinode.spike: b'hello from the host #1' (receipt True)
[  44.760] packet at oxinode.spike: b'echo: hello from the host #1' (131 bytes on the wire, via RNodeInterface[spike rnode])
```

An announce heard 0.77 s after the board's own went out, an LXMF message
decrypted, verified, answered, signed, encrypted and back on the air within
0.9 s, and the host's proof of delivery (`packet type 3`) arriving 0.35 s
after that. In two minutes the host delivered five LXMF messages and three
packets; the board answered every one it received, and the host validated
every answer it received. Not every answer arrived: with three transmitters
-- the board's announces, the host's announces and path requests, and the
replies -- and nobody listening first, roughly one exchange in three lost
one leg to a collision. That is the carrier-sense finding again, measured
rather than inferred, and the reason 15d below is not optional.

One more limit met on the way: the spike, like `rnode`, sends one LoRa frame
per Reticulum packet and refuses anything over 255 bytes. The messages here
were 131 and 243 bytes on the wire, so it never bit, but Reticulum's MTU is
500 and the stock RNode firmware splits at 255 with a header byte the
receiver reassembles by. oxinode does neither, so it interoperates with
itself and with any peer that keeps packets short. That header byte -- one
byte with a sequence nibble and a split flag, `HEADER_L 1` in the RNode
firmware's `Config.h` -- is also something oxinode does not send or strip,
which means an oxinode and a stock RNode do not talk on the air at all today.
Out of scope for this spike, and it should not stay out of scope for long. *It did not: phase 17.*

## Question 3: how slow is the crypto?

Measured on the board, `opt-level = "s"`, `curve25519-dalek` 5.0.0 on its
portable 32-bit backend, best of five with the mean beside it. The nRF52840
has no assembly-backed Curve25519 in this crate family to compare against;
its CryptoCell-310 does elliptic curves in hardware but only through
Nordic's binary library, which was not tried.

| operation | min | mean |
|---|---:|---:|
| identity: X25519 and Ed25519 key generation | 92.4 ms | -- |
| Ed25519 sign, 200 bytes | 44.1 ms | 44.1 ms |
| Ed25519 verify, 200 bytes | 42.7 ms | 42.7 ms |
| encrypt 100 bytes to a peer: ephemeral X25519, agreement, HKDF, AES-128-CBC, HMAC | 94.5 ms | 94.7 ms |
| decrypt the same | 46.2 ms | 46.2 ms |
| SHA-256 of 500 bytes | 0.43 ms | 0.44 ms |
| announce pack, which is a sign | 44.0 ms | 44.0 ms |
| announce validate, which is a verify | 45.2 ms | 45.2 ms |

The same figures showed up in live traffic: every announce heard cost 47 ms
between the frame arriving and the path being recorded, every LXMF message
received cost 46 ms to decrypt and another 44 to verify, and every one sent
cost 44 ms to sign and 94 to encrypt. **Reading a message is about 90 ms of
CPU; writing one is about 140.** The two X25519 operations are what make
encryption twice the price of decryption.

What that means on this executor: all of it runs synchronously in the modem
loop, so for 45 to 140 ms at a time nothing else on the thread is polled.
USB keeps working, because the controller is hardware and embassy's driver
only has to be polled before the buffers fill. The Bluetooth controller
keeps working, because it runs in interrupts above the executor. The panel
and the pad would stall visibly -- a keypress during a verify is a keypress
answered 50 ms late, and a redraw mid-encrypt is a redraw that waits. The
radio holds one received packet in the LR1121's buffer, so a packet arriving
during a verify is not lost, but a second one behind it would be. None of
that is a reason not to build it; it is a reason to put the stack on its own
priority level or accept the stalls and measure them on the panel.

It is also the same order of cost the Bluetooth pairing already pays: phase 8
found the pure-Rust P-256 noticeable during pairing, and Ed25519 here is the
same kind of arithmetic at the same kind of speed. A pairing happens once
per phone. An announce verify happens for every announce on the channel,
which on a busy LoRa network is the thing to watch: at 45 ms each the board
can verify twenty a second, and a LoRa channel at 3125 bps cannot carry two.
The crypto is slow and it is not the bottleneck.

## Question 4: does it stay an RNode?

**Decision: one image, two modes, and the host decides which.** With a host
on USB or Bluetooth the board is an RNode and the host's Reticulum is the
node; with no host attached the on-board stack owns the radio and the panel
is its face. Not both at once.

The reasoning, from the measurement rather than from taste:

* **Sharing the radio is mechanically easy.** The spike used `rnode`'s own
  `Modem`, and to the engine an interface is a place frames come from and go
  to. A frame off the air could be handed both to the KISS host and to the
  engine, and a frame from either could go on the air. The code for that is
  a few lines in the modem loop.
* **Sharing the radio's *configuration* is not.** Phase 12 established that
  a live session on USB or Bluetooth owns the live radio configuration,
  because Reticulum checks its RNode exactly once and never again. An
  on-board node that also wanted the radio would either be on whatever
  frequency the host chose, which is fine, or be unable to run when the
  host had the radio off, which is the state an RNode spends most of its
  life in. And the host's Reticulum and the board's would be two nodes on
  one antenna, announcing two identities, each hearing the other's traffic
  and neither able to tell the host from the air.
* **The host is the better node when it is there.** It has a clock, storage,
  a full LXMF with propagation nodes, and a screen larger than 128 pixels.
  #9 is the shape of that. The on-board stack is for when there is no host,
  and its whole value is that it runs alone.

So: a mode switch on host presence, which the modem loop already tracks for
the title bar. The `rns-esp32` firmware in the same repository does exactly
this -- standalone until a host sends an RNode `DETECT`, then a bridge until
the host goes quiet -- and it is the right shape here too. The identity has
to persist, in the device record beside the EEPROM and the bonds, so the
board is the same node after a reset; the spike generated a fresh one every
boot and was a different destination every time, which is fine for a
measurement and wrong for a device.

## Question 5: is the smaller thing worth more?

Not answered here, on purpose. The issue says it is answered by building #9
and using it, and #9 is not built. What this spike adds to that decision is
that the larger thing is now known to be possible at a known cost, so the
choice is a real one rather than a bet.

## Question 6: the licence

**Decision: `rns-rs`, with the LXMF written here.** The Reticulum License
that `rns-rs` carries is the permissive licence Reticulum itself uses since
2025: MIT's grant, with two conditions attached -- no use in systems built
to harm people, and no use in training machine-learning models -- and the
usual attribution clause. It is not a copyleft licence; linking it does not
change what oxinode is licensed under, and its two conditions are ones this
firmware can carry. Leviculum's AGPL would have required the whole image to
be AGPL, which is a decision about the project rather than a dependency, and
it was not taken. The cost of that choice is the LXMF: `rns-rs` has none,
and the hundred and twenty lines in the spike are the start of one.

## The recommendation

**Build it, after #9, as the phases below -- and let #9 decide whether the
last of them are worth reaching.** The measurements remove the two reasons
not to: it fits, and it talks. What they do not remove is the size of the
job, which is still the largest thing on the roadmap. The order is chosen so
that each phase leaves a device that is better than the one before it, and
so that the work #9 does is inherited rather than repeated.

| | | |
|---|---|---|
| 15a | **The node in the image** | `rns-core` behind a feature in `rnode`; a persistent identity in the device record; the mode switch on host presence; the heap sized from these numbers; announces on the panel's Home screen. A device that can be seen on a Reticulum network with no host attached. |
| 15b | **Receiving** | The LXMF subset the spike started -- opportunistic messages in, proofs out, signatures checked against the announce table -- and a message store, in RAM first and then in the QSPI flash phase 0 found and nobody has driven. #9's conversation screens drawn from it. |
| 15c | **Sending** | The same LXMF subset out, with #9's canned replies first and then text entry: an on-screen keyboard on four directions and OK, which is its own phase because it is its own problem. |
| 15d | **Carrier sense** | Listening before transmitting, in the modem, for the RNode image as much as for the node -- the collision above will happen to any two oxinodes that both have something to say. The 255-byte frame limit and the stock RNode header byte belong in the same phase, because they are all "what oxinode puts on the air". |
| 15e | **Messages larger than a packet, and messages while away** | Links and resources from `rns-core`, so a message of more than 200 bytes arrives, and propagation-node fetch, so one sent while the board was off is not lost. This is where the estimate becomes a guess again, and it is last so that everything before it is useful without it. |

Each is the size of a phase on this roadmap so far, and 15e may be two. If
#9 turns out to be what the device wanted all along, stopping after 15a --
a board that announces itself and shows who is around -- is a coherent place
to stop.

## For the record: what the spike found about `rnode`

Two things that are not about phase 15 and should not be lost in it:

* **No carrier sense.** `rnode` transmits the moment a host hands it a
  packet. The stock RNode firmware checks the channel first. Two oxinodes
  that both have traffic will collide, and the host will see packets go
  missing with no error. *Done as phase 16; see
  [phase-16-csma.md](phase-16-csma.md).*
* **No air header, no split.** oxinode puts raw Reticulum packets on the
  air; a stock RNode puts one header byte in front and splits at 255. The
  two do not interoperate over the air today, and oxinode cannot carry a
  full-MTU Reticulum packet. *Done as phase 17; see
  [phase-17-air-header.md](phase-17-air-header.md).*

## Reproducing

```
# the spike image on one board. With two boards attached the flasher needs
# to be told which; a board already sitting in its bootloader needs
# OXINODE_DFU_IN_DFU=1 as well, since macOS 26 never mounts the UF2 drive
# the script would otherwise detect it by.
OXINODE_DFU_PORT=/dev/cu.usbmodemAAA cargo run --release --features spike --bin rns-spike
python3 scratchpad/spikelog.py /dev/cu.usbmodemAAB target/thumbv7em-none-eabihf/release/rns-spike 120

# the host, over the spike image's own serial port ...
tools/spike_peer.py --serial /dev/cu.usbmodemAAA --seconds 120

# ... or over the air, through a second board running rnode
tools/spike_peer.py --rnode /dev/cu.usbmodemBBB --seconds 120
```

The peer keeps its identity between runs in its configuration directory, so
the host's destination hashes are stable across a session. The board's are
not, since the spike image makes a new identity every boot.
