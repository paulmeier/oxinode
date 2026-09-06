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
