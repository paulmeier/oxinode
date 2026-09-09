# Pad and mode switch

The Super IO has six discrete switches (up, down, left, right, OK, back) and
a three-position slide switch labelled **Power OFF / Power ON / GPS ON**.

## The pad

Six buttons, all active low with the chip's internal pull-ups: up P0.21, down
P0.17, left P1.05, right P0.16, OK P0.10, back P0.15.

The driver is split on the same line as everything else in the project.
`oxinode_core::pad` is the state machine: it is told a switch's level and the
time, and says which gestures are due and when it next needs to be asked.
Debounce, auto-repeat, which switches repeat, and the bounce statistics are
all there and tested with a clock that is a number. `oxinode::pad` is the
driver: it claims the six pins, sleeps on the GPIO `PORT` interrupt until any
of them changes or the core's deadline arrives, re-reads all six, feeds the
core, and puts gestures on a bounded channel the modem loop drains once per
iteration.

### Why it is a pad and not a trackball

Meshtastic's variant declares `HAS_TRACKBALL` and reuses its trackball driver
for the pad. A trackball emits a burst of edges as the ball rolls and is read
by counting them. A pad emits one edge and then a *level* that lasts as long
as the finger does. Counting edges from a pad gives one step per press and no
repeat; watching levels on a ball gives a runaway cursor. So the core is about
levels and time, never about edges: an edge is only the moment the firmware
learns a level changed, and it re-reads every pin on every wake regardless.

### The numbers

| constant | value | why |
|---|---|---|
| `DEBOUNCE_MS` | 10 | a level must hold this long to be believed |
| `REPEAT_DELAY_MS` | 400 | a held direction repeats after this |
| `REPEAT_PERIOD_MS` | 125 | and then every this: eight lines a second |

**Debounce is "stable for 10 ms", not "ignore for 10 ms".** Every edge
restarts the timer and the change commits only once the level has held, so
the number of bounces is irrelevant and only their duration matters. The
alternative, accepting the first edge and ignoring the line, commits to
whatever the first edge said, and on a switch that bounces on release that is
a phantom press. Ten milliseconds is twice what tactile domes are specified to
bounce for; the driver measures how long every committed press took to settle
and how many edges it produced and logs it, so the figure can be checked
against a worn switch rather than guessed. A compile-time assertion keeps the
three numbers ordered.

**Repeat only on the four directions.** A held direction scrolls. A held OK
is one OK, because an action chosen once is chosen once, and a held back is
one back. No chording, no long-press, no double-press: six switches, six
gestures, one meaning each.

**A late poll gets one repeat, not a burst.** The next repeat is scheduled
from the poll that emitted the last one, so if the render loop stalls for a
second the user gets one step, not eight. The channel holds eight gestures
and drops the newest when full, counting and logging the drops.

### Interrupt-driven, and quiet

`embassy-nrf` sets each pin's `SENSE` to the level it is not at and the chip
raises one `PORT` interrupt when it gets there. While nothing is pressed no
timer runs and nothing polls. The obvious version has a race (read the pin,
then arm the wait, and an edge in between is lost until the next one); the
driver closes it by arming each pin to wait for the level it is not at *as of
the arm*, which returns immediately if the pin has already moved.

### P0.10 is an NFC pin

On this part P0.09 and P0.10 belong to the NFC peripheral until the `PROTECT`
bit of `UICR.NFCPINS` is cleared, and that is a word in the chip's user
information page, not a register. The failure mode is indistinguishable from
a dead OK button.

`embassy-nrf` does not name P0.09 or P0.10 at all without the
`nfc-pins-as-gpio` feature, so the feature is on, and it is not only a name.
With it, `embassy_nrf::init` reads `UICR.NFCPINS` on every boot; if bit 0 is
already clear it does nothing; otherwise it writes the word back with bit 0
cleared as a single masked word write and resets once so it takes. Flash bits
go from 1 to 0 without an erase, `embassy-nrf` refuses rather than erasing if
asked for a 0→1 change, and `REGOUT0` is a different word this write does not
touch. So the thing to be afraid of (a page erase that loses `REGOUT0` and
drops the board to 1.8 V with the panel's boost converter and the QSPI flash
expecting 3.3) cannot happen down this path.

On a board that shipped running Meshtastic the bit is already clear, because
Meshtastic builds with `CONFIG_NFCT_PINS_AS_GPIOS`. The product image logs
both words at every boot so the answer can be read before anything is
flashed:

```
board: nav pad usable=true (UICR.NFCPINS), regulator=3.3 V
```

## The mode switch

P1.09 and P0.12 carry the three-position switch. Both lines are read with no
pull, as Meshtastic reads them, because an internal pull fighting an unknown
external one could read the same in every position.

| position | P1.09 | P0.12 | log |
|---|---|---|---|
| Power ON | high | low | `board: mode switch P1.09=high P0.12=low -> power on` |
| GPS ON | low | high | `board: mode switch P1.09=low P0.12=high -> gps on` |
| Power OFF | — | — | the board is unpowered; nothing runs to read anything |

Each line is driven *high* in its own position, the opposite polarity to the
six switches beside it, and the third position is not a mode: it cuts the
board's power, USB included. "Neither high" is mid-travel, or a board with no
Super IO on it, where both unpulled lines float; `Mode::decode` in the core
names it and the two readings above are its test vectors.

The only thing the firmware does with the position is decide whether the GPS
receiver is powered. See [GPS](gps.md).
