# Phase 4 — runtime radio configuration

Phase 3 proved the radio works. Every parameter it used was a constant compiled
into the image: one frequency, one spreading factor, one bandwidth, one power.
A host could not change any of them, which is the whole of what an RNode does.

Phase 4 is the layer that turns those constants into values. It is not a
protocol — nothing here parses a byte off a wire — and it is not a new radio
capability. It is the seam phase 5 will sit on, and it exists as its own phase
because the parts worth getting right are pure functions that can be tested on
a host, and the parts that touch the chip are a handful of commands that have
already been proved once.

## What "configurable" has to mean here

An RNode host sets five things, one at a time, in any order:

| | units on the wire |
|---|---|
| frequency | Hz |
| bandwidth | Hz |
| spreading factor | 7–12 |
| coding rate | 5–8, the denominator of 4/n |
| transmit power | dBm |

Two consequences follow from *one at a time*, and both shape the design:

* **Intermediate states are invalid and that is normal.** A host moving from
  SF7/125 kHz to SF12/500 kHz passes through configurations that are fine, and
  a host moving from 915 MHz to 868 MHz passes through one that is outside the
  band the antenna is cut for. Rejecting a *set* is wrong; refusing to *apply*
  an invalid combination is right. So the mutable value and the thing that may
  be given to the chip are different types.
* **The chip's units are not the wire's units.** Bandwidth is a code, not
  hertz; coding rate is 1–4, not 5–8; and the commanded frequency is not the
  frequency anybody wants, because of the 73 ppm below. Every one of those
  conversions is somewhere a wrong answer produces a radio that works
  perfectly at the wrong settings, which is the worst kind of bug to have on
  hardware with no probe.

## Steps

1. **`RadioConfig`** — the value, its validation, and the chip encodings.
   Pure; `oxinode-core`; tested on the host.
2. **The frequency correction** — the 73 ppm phase 3 left open, as arithmetic
   that can be checked before it is trusted.
3. **`Modem`** — one place that programs a config into the LR1121, and one
   place each for transmit and receive. Bounded everywhere, like everything
   else that talks to this chip.
4. **The console** — every parameter settable at runtime, so the layer can be
   exercised against real hardware rather than argued about.
5. **Hardware validation** — the acceptance tests, on the bench, against the
   second board.

---

## Step 1 — `RadioConfig`

`oxinode_core::lr1121::config`. Two types, because a host holds invalid
configurations as a matter of course:

* `RadioConfig` is plain data with no invariants — the thing a host mutates one
  field at a time.
* `ValidConfig` can only be built by passing `check()`, and is the only thing
  the modem accepts. "Did anybody validate this?" becomes a question the
  compiler answers rather than one for code review.

`check()` names each limit separately — `FrequencyOutOfBand`,
`UnsupportedBandwidth`, `SpreadingFactorOutOfRange`, `CodingRateOutOfRange`,
`PowerAboveModuleRating`, `PowerUnreachable`, `PreambleTooShort` — because a
host on the far end of a serial line has nothing else to go on. A single
"invalid" would be unactionable.

What it deliberately does *not* check is whether an RNode host could express
the configuration. SF5 and SF6 are outside the protocol's 7–12 and are still
valid here: what the chip can do and what a protocol can say are separate
questions, and conflating them would make the bench console unable to reach
hardware that works. `is_rnode_representable()` answers the other question.

### The coding rate is the one that would have been silent

The host's coding rate is the denominator of 4/n, so 5 to 8. The chip's is 1 to
4. Passing the host's number straight through does not produce an error — it
selects `CodingRate::Long45` through `Long48`, the **long interleaver** at the
wrong rate. The radio would transmit happily and nothing else would hear it.
`coding_rate_code` and its inverse are tested in both directions.

### Power

`pa::pa_config_for` picks a PA. It prefers the low-power one throughout its
range, including the −9 to +14 dBm it shares with the high-power one, and that
preference is a decision rather than arithmetic: the low-power PA draws less,
and it is the only one this project has ever measured. Phase 3 keyed carriers
at −17, 0 and +14 dBm and watched them on an SDR; nothing has ever come out of
the high-power PA on this board.

Above 14 dBm the internal regulator cannot supply the PA, so `pa::high_power`
switches to VBAT at exactly that boundary. Getting this wrong is not an error
the chip reports — it is a brown-out. The pre-existing `HIGH_POWER` constant
hard-codes the internal regulator because it was written as a one-off
diagnostic, which is why a layer taking an arbitrary power could not use it.

Nothing clamps. A host that asks for 21 dBm is refused, not quietly given 20:
a clamp is a lie that the host cannot detect.

## Step 2 — the 73 ppm correction

`oxinode_core::lr1121::reference`. Phase 3 measured this board's transmitter at
−73.3 ppm and then proved the number belongs to the module rather than the
board, so the error is a stable property of the part and cancelling it is
arithmetic rather than a workaround.

It is carried in **tenths of a ppm as an integer**. The measurement has a tenth
of a ppm of resolution and the correction is not a whole number of ppm; a float
would be a different number on the host than on the target, for no benefit, at
a frequency near 2³⁰ where that difference is tens of hertz.

The first-order form (`f × (1 + p)` rather than `f / (1 − p)`) is used
deliberately. The two differ by 5 parts per billion — 5 Hz at 915 MHz — against
a measurement uncertainty of 0.5 ppm, or 460 Hz. That is an approximation two
orders of magnitude below the noise, not a shortcut.

**Whether to apply it is a configuration field, not a constant.** Corrected,
this board is right in absolute terms and 73 ppm away from every other
nRFLR1121 — including the second Base Duo on the bench. Uncorrected, it is
wrong in absolute terms and agrees with them exactly. Which is right depends on
who is listening, and the firmware is not entitled to decide that. The default
is corrected, because an RNode's peers are other RNodes.

The compile-time assertions check the round trip: correcting 902, 915 and
928 MHz and then applying the measured error must land back within 100 Hz.

## Step 3 — `Modem`

`src/modem.rs`. One `apply`, one `transmit`, one `start_rx`/`receive`. It takes
a `ValidConfig` and nothing else.

Both of phase 3's hard-won habits are kept, and both are load-bearing:

* **Every await is bounded and named.** `lr11xx` waits on BUSY with no timeout,
  so a command that leaves BUSY high hangs the driver, and on a board with no
  debug probe a hang and a crash look identical. `ModemError::Timeout` carries
  which step expired.
* **Every sequence ends by asking for a status.** The LR11xx protocol returns
  the status of the *previous* command, so a driver returning `Ok` has told you
  about the command before the one you care about.

Two failures phase 3 got right by hand are now structural:

* `transmit` returns `WrongInterrupt` rather than a report when something other
  than `TxDone` fired. That is precisely the case a bare timeout on the
  interrupt line reports as success.
* The chip's transmit timeout **saturates** at the field's 24-bit width rather
  than wrapping. Three airtimes at SF12 and 62.5 kHz is over eight minutes and
  does not fit; a wrap would have become a few-millisecond timeout on the
  slowest configuration the chip offers — a transmitter that gives up mid-packet
  only at the settings nobody tests.

`SetPacketType` still goes first and is still not optional. Phase 3 spent a
bisection establishing that, against documentation which says otherwise.

## Step 4 — the console

The bring-up image now holds a configuration instead of constants. `S`, `W`,
`C` and `P` cycle spreading factor, bandwidth, coding rate and power; `[` and
`]` step the frequency by 100 kHz, saturating at the band edges rather than
wrapping; `R` toggles the reference correction; `N` switches sync word between
the private-network `0x12` and Meshtastic's `0x2b`; `M` loads the peer board's
LongFast settings and `D` the default; `A` applies without transmitting.

Cycling rather than typing, because this console reads raw bytes off a serial
port with no line editing, and a key that always does something is easier to
use and much easier to read back in a log.

An invalid intermediate state is **kept and reported**, not reverted. A host
setting one parameter at a time is entitled to hold one.

Every step of a sweep logs the frequency it actually tuned to. A sweep whose
steps are labelled only by their offset is assuming the offset reached the chip,
which is the one thing the sweep exists to establish about everything else.

## Step 5 — what the hardware says

### The configuration reaches the chip, and every parameter matters

Configured at runtime to the peer's channel by pressing `M`, then `y`:

```
config: 906875000 Hz wanted, 906875000 Hz commanded (uncorrected), SF11 BW250000 CR4/5, 14 dBm
config: preamble 16, sync 0x2b, crc true, explicit header, 1074 bps, 354304 us airtime for 16 bytes
rx: PACKET 1: 37 bytes, RSSI -69 dBm, SNR 12 dB
rx: bytes [ff, ff, ff, ff, 5c, a1, 3d, ba, ...]
rx: done at 0 Hz offset -- 9 packets
```

Nine real Meshtastic packets from `0xba3da15c`, reached entirely by keys at
runtime rather than by a rebuild.

Two negative controls, both from the same starting point:

| change | packets |
|---|---|
| none | 9 |
| SF11 → SF12 (one press of `S`) | **0** |
| sync `0x2b` → `0x12` (one press of `N`) | **0** |

and a third: tuned 2 MHz away with twenty presses of `[`, **0 packets**; back
at 906.875 MHz, **9 packets**. So spreading factor, sync word and frequency each
demonstrably reach the chip. Without these, "it received something" is equally
consistent with a configuration layer that does nothing at all.

### The airtime prediction tracks the configuration

Four configurations, four transmissions, `TxDone` timed against the airtime
computed from the configuration:

| SF | BW | CR | computed | measured | difference |
|---|---|---|---|---|---|
| 8 | 125 kHz | 4/5 | 92,672 µs | 93,109 µs | +437 µs |
| 10 | 125 kHz | 4/5 | 329,728 µs | 330,169 µs | +441 µs |
| 10 | 250 kHz | 4/5 | 164,864 µs | 165,313 µs | +449 µs |
| 10 | 250 kHz | 4/8 | 214,016 µs | 214,477 µs | +461 µs |

The residual is not merely small — it is **constant** across a 3.5× range of
airtimes. A fixed 440–460 µs is what the `SetTx` transaction, the PLL lock and
the PA ramp cost; a wrong symbol-time formula would scale with the airtime and
a wrong coding-rate translation would move between rows. Neither does. That is
a much stronger statement than "within tolerance".

### The frequency correction moves the receive window, in the predicted direction

This is the phase 3 open item, and it needed care, because the instrument phase
3 used for it — the SDR, good to 0.5 ppm — has been removed from the bench.

What is left is the peer board, and the peer board is a blunt instrument here.
Phase 3 measured a reception window of ±120 kHz at SF11/250 kHz; on this bench
the peer now arrives at −69 dBm rather than −45 dBm and the window is wider
than ±300 kHz. That is not a contradiction — a LoRa receiver with 60 dB of
margin over its sensitivity tolerates far more frequency error than one at the
limit — but it does mean a sweep built on phase 3's numbers measures nothing,
because every offset in the middle receives in both configurations.

So: find the edges first. A reconnaissance sweep of ±700 kHz in 100 kHz steps,
then the same sweep three times as **off / on / off**, so that drift shows up
as a difference between the two runs that share a setting.

| offset (kHz) | −700 | −600 | −500 | −400 | −300 | −200 | −100 | 0 | +100 | +200 | +300 | +400 | +500 | +600 | +700 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| correction **off** | 0 | 0 | 0 | 0 | 1 | 0 | 3 | 4 | 4 | 3 | 2 | 0 | 0 | 0 | 0 |
| correction **on** | 0 | 0 | 0 | 1 | 2 | **4** | 3 | 3 | 2 | 2 | **0** | 0 | 0 | 0 | 0 |
| correction **off** | 0 | 0 | 0 | 0 | 2 | 0 | 4 | 4 | 3 | 2 | 3 | 0 | 0 | 0 | 0 |

Two cells do the work, and they do it in opposite directions:

* **+300 kHz**: 2 packets, then **0**, then 3. Alive, dead, alive.
* **−200 kHz**: 0 packets, then **4**, then 0. Dead, alive, dead.

The window moved **down** by about one 100 kHz step and moved back again. The
prediction was −66.5 kHz, and the sign is what the arithmetic demands: the
correction moves this board away from a peer that carries the same error, so
every edge must fall. The two `off` runs agree with each other closely enough —
including a reproducible oddity at −300/−200 kHz — that the difference cannot be
drift.

### ...and by the amount it should

One 100 kHz step is a direction, not a number. Turning it into one needed two
changes to the method, and the second mattered more than the first.

The obvious change is a finer step: 20 kHz instead of 100 kHz. On its own it did
not help. Two nominally identical `off` runs put the edge a whole step apart,
because a peer beaconing every two seconds delivers three or four packets in an
eight-second dwell and an edge located from counts that small is an impression
rather than a measurement.

The change that mattered was **dwelling for 24 seconds instead of 8**. Twelve
packets a point costs three times the wall clock and turns a ragged sequence
into a monotonic roll-off:

| offset (kHz) | +220 | +240 | +260 | +280 | +300 | +320 | +340 | +360 |
|---|---|---|---|---|---|---|---|---|
| correction **off** | 4 | 3 | 10 | 10 | 8 | 9 | 6 | — |
| correction **on** | 11 | 9 | 4 | 1 | 0 | 0 | 0 | — |
| correction **off**, again | 8 | 4 | 11 | 9 | 11 | 7 | 2 | — |
| correction **off**, upper range | — | — | — | — | 9 | 7 | 2 | 0 |

Taking the plateau as full and interpolating each half-power point:

| | 50% edge |
|---|---|
| correction **off** | **+328 kHz** |
| correction **on** | **+254 kHz** |
| shift | **−74 kHz** |

against a predicted **−66.5 kHz**. Each edge is located to roughly ±10 kHz at a
20 kHz step, so their difference carries about ±14 kHz — the measurement is
−74 ± 14 kHz, or −81 ± 15 ppm against 73.3 ppm predicted. The last run also
reproduces the second `off` run exactly where they overlap (7 packets at
+320 kHz, 2 at +340 kHz), which is the evidence that the edge is stable between
runs rather than the two agreeing by luck.

The point of the precision is not the third significant figure. It is that a
correction applied at half strength would put the shift at −33 kHz and one
applied twice at −133 kHz, and both are several times the uncertainty away. The
arithmetic is being applied once, at full strength, in the right direction.

### One thing left unexplained

Both `off` runs are *low* at +220 and +240 kHz — 4 and 3, then 8 and 4 — in the
middle of a passband that gives 10 or 11 on either side. The `on` run is full
there. That asymmetry is what a fixed-frequency artifact would look like rather
than a property of the receiver's response: the correction shifts the commanded
frequency by 66 kHz, so a spur sitting at a fixed frequency appears at different
*offsets* in the two runs, and the offset it would appear at in the `on` run is
below the bottom of the sweep. Consistent, but not established — recorded rather
than resolved.

### What this does and does not establish

It establishes that the correction is applied, that it is applied once and in
the right direction, and that its magnitude agrees with the arithmetic to within
the resolution of a 20 kHz sweep. It does not re-measure the 73.3 ppm itself to
phase 3's precision; that needed the SDR, and phase 3 already did it.
