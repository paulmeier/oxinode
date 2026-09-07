# Phase 10 — the navigation pad driver

Where phase 10 stands: **built, flashed, the UICR question answered; the
switches not yet exercised.** The driver compiles into the product image,
every timing decision in it is tested on the host, and the product image logs
what the hardware checklist at the end needs read. The first boot on the
board said:

```
board: nav pad usable=true (UICR.NFCPINS), regulator=3.3 V
board: mode switch P1.09=low P0.12=high -> middle (polarity unconfirmed)
```

What remains is a finger on each switch and the mode switch in its other two
positions.

The interface models six gestures; the Super IO has exactly six switches. This
phase connects the two, and most of it is about *time* rather than pins.

## The shape

Two modules, split on the same line as everything else in this project: what
needs a board, and what does not.

**`oxinode_core::pad`** is the state machine. It knows nothing about pins.
It is told a switch's level and the time, and it says which gestures are due
and when it next needs to be asked. Debounce, auto-repeat, which switches
repeat, the no-catch-up rule, the bounce statistics — all of it is here, and
all of it is tested with a clock that is a number. Thirty-one tests, including
one that drives the machine the way the firmware does: levels arrive when their
edges happen, and the clock advances to whichever of the next edge and the
pad's own deadline comes first.

**`oxinode::pad`** is the driver. It claims the six pins with the chip's
pull-ups, sleeps on the GPIO `PORT` interrupt until any of them changes or the
core's deadline arrives, re-reads all six, feeds the core, and puts the
gestures on a bounded channel. The modem loop in `rnode` drains that channel
once per iteration. It also reads the mode switch, once, for the boot log.

## Why a pad is not a trackball

Meshtastic's variant for this board declares `HAS_TRACKBALL` and names the
lines `TB_UP`, `TB_DOWN`, `TB_LEFT`, `TB_RIGHT`, `TB_PRESS`, because it reuses
its trackball driver for the pad. muzi's own specification says "Navigation
Pad Buttons + OK + Back", and the board has six discrete switches.

The pin numbers are the same either way. The driver is not. A trackball emits
a burst of edges as the ball rolls and is read by counting them, which is why
the variant sets `TB_DIRECTION FALLING`. A pad emits one edge and then a
*level* that lasts as long as the finger does. Count edges from a pad and you
get one step per press and no key repeat; watch levels on a ball and you get a
runaway cursor. Neither failure is visible in a pin map, which is why this
phase exists as a phase and not as a `#define`.

So the core is about levels and time, never about edges. An edge is only the
moment the firmware learns that a level changed — and the firmware does not
even trust that: it re-reads every pin on every wake, and the core treats a
re-read that agrees as not an edge.

## The numbers, and where they come from

| constant | value | why |
|---|---|---|
| `DEBOUNCE_MS` | 20 | a level must hold this long to be believed |
| `REPEAT_DELAY_MS` | 400 | a held direction repeats after this |
| `REPEAT_PERIOD_MS` | 125 | and then every this: eight lines a second |

**Debounce is "stable for 20 ms", not "ignore for 20 ms".** Every edge
restarts the timer, and the change commits only once the level has held. That
makes the *number* of bounces irrelevant and only their *duration* matter, and
it means a bounce that ends back where it began commits nothing at all. The
alternative — accept the first edge and ignore the line for a while — commits
to whatever the first edge said, and on a switch that bounces on release that
is a phantom press.

**Twenty is a starting figure, not a measurement.** Tactile dome switches
settle in 1–10 ms when new and get worse with wear; 20 ms covers that with
margin while adding less latency than one iteration of the render loop. It
is not settled yet, and the issue is right to ask for it to be. So the driver
measures: every committed press reports how long the contact took to stop
bouncing and how many edges it produced, and the product image logs them:

```
pad: down press, settled in 3 ms after 2 bounce(s)
```

Press each switch a few dozen times, take the worst `settled in`, and the
right debounce is that with margin. If the worst is under 5 ms — likely, for
switches this new — 20 is generous and 10 would do; the constant is pinned by
a test so changing it is a deliberate edit in two places. A compile-time
assertion keeps the three numbers ordered, because a repeat period shorter
than the debounce is a driver that repeats bounces.

**Repeat only on the four directions.** A held direction scrolls; that is the
whole reason for repeat. A held OK is one OK, because an action chosen once is
chosen once, and a held back is one back, because a menu that is left is left.
No chording, no long-press, no double-press: six switches, six gestures, one
meaning each. Two switches held at once produce their own two gestures
independently and nothing either would not have produced alone.

**A late poll gets one repeat, not a burst.** The next repeat is scheduled
from the poll that emitted the last one, not from where the schedule says it
should have been. If the render loop stalls for a second, the user gets one
step, not eight — which, with the bounded channel, is what "a burst of presses
cannot outrun a redraw" actually means in practice. The channel holds eight
and drops the newest when full, and counts and logs the drops so they cannot
stay invisible; in normal use it never holds more than one.

## Interrupt-driven, and quiet

Every wait on a pin goes through the GPIO `PORT` event. `embassy-nrf` sets the
pin's `SENSE` to the level it is *not* at and the chip raises one interrupt
when it gets there. While nothing is pressed, no timer runs and nothing polls:
the driver is a task parked on six `SENSE` bits. While something is pressed,
the core's `deadline` says when the next thing is due — the end of a debounce,
or the next repeat — and the driver sleeps until then or until the next edge.

There is a race in the obvious version of this: read the pin, then arm the
wait, and an edge in between is lost until the *next* one, which leaves a
switch believed stuck for as long as nobody touches it. The driver closes it
by arming each pin to wait for the level it is not at *as of the arm*, which
returns immediately if the pin has already moved — and by re-reading all six
on every wake regardless of which one woke it.

## P0.10, the NFC pin, and what `embassy-nrf` does about it

This is the part the issue said to read before writing, and reading the HAL
turned up something the issue did not know.

P0.10 carries OK. On this part P0.09 and P0.10 belong to the NFC peripheral
until the `PROTECT` bit of `UICR.NFCPINS` is cleared, and that is a word in
the chip's user information page, not a register. The failure mode is
indistinguishable from a dead button.

**`embassy-nrf` does not name P0.09 or P0.10 at all without the
`nfc-pins-as-gpio` feature.** There is no `P0_10` peripheral to claim. So the
feature is now on, and the feature is not only a name. With it,
`embassy_nrf::init` does this on every boot, before anything else runs:

1. reads `UICR.NFCPINS`;
2. if bit 0 is already clear, does nothing;
3. otherwise writes the word back with bit 0 cleared, through the NVMC, as a
   single masked word write — and then resets the chip once so it takes.

The thing the issue was rightly afraid of — a page erase that loses
`UICR.REGOUT0` and drops the board to 1.8 V with the panel's boost converter
and the QSPI flash expecting 3.3 — cannot happen down this path. Flash bits go
from 1 to 0 without an erase; that is what a word write is. `embassy-nrf`'s
`uicr_write_masked` refuses (returns `Failed`) rather than erasing if it is
ever asked for a 0→1 change, and `REGOUT0` is a different word that this write
does not touch. oxinode also leaves `Config::dcdc.reg0_voltage` at `None`, so
nothing in the firmware ever writes that word either.

Two consequences worth knowing:

* **Phases 1–8 were already brushing against this.** Without the feature,
  `init` tries to write bit 0 *to 1* (NFC mode). On a board Meshtastic has
  programmed that is a 0→1 change, so it returned `Failed` and emitted a
  `warn!` — which in oxinode's build is a no-op, because `embassy-nrf` is
  built without its `defmt` feature. Nothing was written and nothing was
  visible. That is consistent with the bit being clear already; it is also
  consistent with it being set, since a no-op write and a no-op warning look
  the same. Only the boot log settles it.
* **Enabling the feature is the decision the issue said to take
  deliberately, and this is it being taken.** If the bit is clear — expected,
  because Meshtastic builds with `CONFIG_NFCT_PINS_AS_GPIOS` and the board
  shipped running it — the first boot of this image changes nothing. If it is
  not, the first boot clears it and resets once, which is exactly what
  Meshtastic's first boot did, by the same mechanism. The current product
  image already logs the bit at every boot, so the answer can be read *before*
  this image is flashed by opening the log port on whatever is on the board
  now:

  ```
  board: nav pad usable=true (UICR.NFCPINS), regulator=3.3 V
  ```

  `usable=true` means the bit is clear and the feature will be a no-op.
  `usable=false` means the first boot of a phase 10 image will clear it. The
  `regulator=3.3 V` half of the line is there to show `REGOUT0` survived,
  every boot, forever.

## The mode switch

P1.09 and P0.12 carry a three-position switch. Meshtastic names them
`SWITCH_MODE1` ("Top Position") and `SWITCH_MODE2` ("Middle Position"), reads
the second with `pinMode(INPUT)` — no pull — and treats it low as "GPS off".
That is the whole of what the sources say: there is no schematic for the Super
IO, so how the switch is wired is not known.

So the driver reads both lines with no pull, as Meshtastic does — an internal
pull fighting an unknown external one could read the same in every position —
and logs the raw levels and a reading of them at boot:

```
board: mode switch P1.09=low P0.12=high -> middle (polarity unconfirmed)
```

The reading assumes each line is driven *high* in its own position, which is
what Meshtastic's use implies: the line it names for the middle position is
the GPS switch, low means off, and the middle position is the one that turns
the GPS on. That is the opposite polarity to the six switches beside it, and
it is a hypothesis until the line above has been read with the switch in all
three positions. If it is wrong, the fix is two `!` in `ModeSwitch::read` and
the log is what will say so. `Mode::decode` itself — one line per position,
neither for the bottom, both is invalid — is in the core and tested.

The switch does nothing yet. What it *should* do is a question for the GPS
phase, since the GPS is what Meshtastic uses it for.

## Getting the boot log at all

The first attempt to read that line got eighteen bytes of it. The product
image's log pump sent whether or not anything was listening, on the theory
that the log is continuous rather than a startup sequence — and the host
discards what arrives on a port nobody has open. A board re-enumerates
faster than a terminal can be attached to it, so the lines that say what the
hardware *is* were gone every time, and a reset over the KISS port to
provoke them again just lost them again.

So the pump now waits for DTR on the log port, as the bring-up images always
did. The ring behind it holds 4 KB, which is the whole of boot; a terminal
that opens seconds later gets it from the top, and a board nobody listens to
fills the ring and drops the oldest, as before. Reading it is:

```bash
python3 -c "import serial,sys; s=serial.Serial(sys.argv[1],115200); \
  sys.stdout.buffer.write(s.read(4096))" /dev/cu.usbmodemXXX3 \
  | defmt-print -e target/thumbv7em-none-eabihf/release/rnode
```

with an explicit baud rate, because macOS re-applies whatever the port was
last opened at — including a flash tool's 1200-baud touch.

## What it does not do

* Nothing in the interface answers a gesture yet. Phase 10 is the driver; the
  screens that act on `Nav` are phase 11. In the meantime a gesture is logged
  at the point it is taken off the channel and brings the next redraw
  forward, so a person at the board can see that it was heard.
* The simulator does not simulate the pad's timing. Its input script is the
  gestures *after* this driver, which is the right seam: the simulator tests
  what a gesture does, and this phase tests when one happens.

## Hardware checklist

What the issue's "done when" needs, and the log line that answers each:

- [x] **`UICR.NFCPINS` read on hardware.** `usable=true, regulator=3.3 V`
      on 2026-09-07, on the first boot of the phase 10 image. The bit was
      already clear, so the feature's write was a no-op and `REGOUT0` is
      still what the bootloader set. Recorded in the README.
- [ ] **All six switches produce their gesture.** Press each; expect
      `pad: <name> press, settled in …` for `up`, `down`, `left`, `right`,
      `ok`, `back`, and at debug level `ui: <gesture>` from the modem loop.
- [ ] **A held direction repeats; a held OK does not.** Hold down for two
      seconds: one `press` and then `pad: down repeat` at 125 ms intervals
      after 400 ms. Hold OK for two seconds: one `press`, nothing else.
- [ ] **Debounce settled against measurements.** Take the worst `settled in`
      over a few dozen presses per switch, write the number and the choice
      into the table above, and change `DEBOUNCE_MS` and its test together.
- [ ] **The mode switch read.** `board: mode switch …` with the switch in
      each of its three positions; confirm or flip the polarity in
      `ModeSwitch::read`, and record the three readings.
