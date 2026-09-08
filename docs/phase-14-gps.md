# Phase 14 — the GPS, and the Position screen

Where phase 14 stands: **the module powered, heard, and parsed; the pin
direction settled on the board; a fix acquired and shown on the screen; and
the receiver switchable from the mode switch and the menu.** The first runs
were indoors and saw no satellites; taken outside on 2026-09-08 the board
got a fix and the Position screen reported it correctly. Not yet done: the
current measurement, because there was no meter in the loop. It is open at
the bottom.

The Super IO carries a GNSS module with a ceramic antenna on a UART behind a
load switch, and until now oxinode had never driven it. The `Position` screen
said so. This phase gives it something to say.

## The pin direction, settled

The two sources name the UART from opposite ends. The schematic labels P0.20
`UART_GPS_TX` and P0.19 `UART_GPS_RX`; the Meshtastic variant declares
`GPS_RX_PIN` P0.20 and `GPS_TX_PIN` P0.19. Read each from its own end and
they agree: the schematic names the net from the module's side (the module's
TX arrives on P0.20), and the variant names the pin from the MCU's side (the
MCU receives on P0.20). So **the module transmits on P0.20 and the nRF52840
listens there; P0.19 is the MCU's transmit pin and the module's receive**.

That reading was not trusted. The firmware probes: it opens the UART on the
expected pins at 9600 baud, then 115200, then 38400, then the same three with
the pins swapped, listening three seconds each, and stops at the first
attempt on which a sentence with a good checksum arrives. On 2026-09-08 the
first attempt was the one:

```
0.239562 INFO  gps: power on (P1.01 high)
0.891387 INFO  gps: NMEA at 9600 baud, module TX on P0.20
```

Six hundred and fifty milliseconds from rail to first sentence, at the rate
every likely module wakes at, on the pin both sources meant. The probe stays
in the product image anyway: it costs nothing when the first attempt is
right, and a different Super IO revision that wired it the other way would
be a log line rather than a silent screen.

## Power is a load switch

P1.01 is not a module enable. It drives an NMOS that gates a high-side PMOS
feeding a switched 3V3 rail out to the expansion connector, rated 500 mA,
and the module hangs off that rail. Driving it high is what makes anything
appear on the UART; the phase 0 notes had that right, and it is now the
first thing the task does when the receiver is wanted.

Off is not merely the switch low. The receiver's output floats when its rail
is cut, and a UART left listening to a floating line burns current on the
input stage and fills the ring with noise. So the UART driver is *dropped*
when the receiver goes off, which disconnects both pins, and built again when
it comes on. That is why the firmware's `Gps` owns the peripherals rather
than a driver: the driver is a thing that exists while the module is powered.

## The default is the switch

The GPS is the largest continuous draw on the board and the issue asked for
the default to be a considered one. The consideration is this: the mode
switch has a position labelled GPS ON, the reference firmware turns its GPS
on and off from it, and a physical control whose position can be seen is the
right place for the largest draw to live. So:

* **At boot, the switch decides.** GPS ON powers the receiver; Power ON does
  not. A board with no Super IO -- both lines floating, which phase 10 named
  `Neither` -- has no GPS to power and stays off.
* **When the switch moves, it decides again.** The task reads it every
  quarter second, and a move to GPS ON turns the receiver on, a move to Power
  ON turns it off.
* **Between moves, the menu decides.** `GPS On/Off` on the Position screen's
  menu flips the receiver regardless of where the switch is, and the switch,
  unmoved, does not flip it back. A switch caught mid-travel, where neither
  line is high, says nothing and changes nothing.

The rule is `oxinode_core::gps::Control`, twenty lines, and every branch of
it is a test. The Position screen's first row is `GPS` with `off`, `no
data`, `searching` or `fix` against it, and while it is off the screen says
what turns it on -- so whether the receiver is drawing current is the first
thing the screen says, which is what the issue asked for.

## NMEA, as much as the screen needs

The receiver talks NMEA 0183: `$`, a body, `*`, two hex digits of checksum,
`\r\n`, never more than 82 characters. The framing is in
`oxinode_core::gps::Lexer`, one byte at a time as the ring buffer hands
them over, and it is unforgiving on purpose. A line whose checksum fails is
dropped whole -- a corrupted digit in a latitude is a position a few hundred
kilometres out, which is worse than no position. A byte that cannot appear
in a sentence, which is what the wrong baud rate produces, abandons the line
it was in. A `$` mid-line starts over. A line past the limit is framing
lost, not a long sentence. Every one of those is a test, and so is a
sentence reassembled from seven-byte fragments.

Three sentences are read, by `oxinode_core::gps::parse`:

* **`GGA`** is the fix: time, quality, satellites used, latitude, longitude,
  altitude. Quality zero is no fix whatever the coordinate fields say, and
  some receivers do say `0000.00000,N` there.
* **`RMC`** carries the date, which `GGA` does not, and a second opinion on
  validity. Its coordinates are taken too, with `GGA`'s altitude kept, so a
  receiver that sends one and not the other still yields a fix.
* **`GSV`** carries how many satellites one constellation can see, which is
  the number worth watching before there is a fix. Each talker -- `GP`,
  `GL`, `GB`, `GA`, `GQ` -- reports its own view, and the screen shows the
  sum.

Everything else -- `GSA`, `GLL`, `VTG`, `TXT`, proprietary `P...` lines --
is `Other`: framed, checked, and ignored. Coordinates are micro-degrees in
an `i32` and altitude is decimetres; nothing is floating point, so a value
printed to six places was held to six places all the way. The parser is
tested against captured sentences from a Quectel-class module through its
first fix, a u-blox line with four-place minutes, the southern and western
hemispheres, a negative altitude, and eighteen malformed bodies that frame
correctly and are wrong inside.

## What the receiver knows

`oxinode_core::gps::Receiver` folds sentences into a `Position`: the status,
satellites used and in view, the last fix, its age, the time and the date.
It is timed from outside -- every call carries the board's millisecond clock
-- so its rules are testable at any speed:

* Powered and nothing readable for five seconds is `no data`. Talking with
  no fix is `searching`. A fix confirmed within the last five seconds is
  `fix`.
* **A lost fix is kept and aged.** The receiver that had a fix and stopped
  confirming it shows `searching`, the last coordinates, and `Fix age` in
  seconds, minutes or hours. A position a minute stale is still a position;
  the age is the honest part.
* **Power off forgets everything.** A position shown after the receiver was
  deliberately turned off is a claim nobody is standing behind, and the
  module's own memory, not ours, is what makes the next fix quick.

The firmware's part, `src/gps.rs`, is a task beside the Bluetooth one. It
owns the load switch and the UART's pins and peripherals, runs the probe,
feeds bytes to the receiver, and publishes the `Position` into a mutex the
modem loop copies out once per redraw. The two meet nowhere else, except the
menu item, which raises a signal. Nothing on the radio's path waits for the
receiver, and `run`'s already long signature gained no argument.

`TIMER2`, `PPI_CH10`, `PPI_CH11` and `PPI_GROUP0` are the buffered UART
driver's: it counts received bytes with the timer so the ring can be read
before a DMA transfer ends. `TIMER0` and channels 17 to 31 are the link
layer's, `TIMER1` is the stall guard's; the UART's interrupt is yielded to
MPSL's priority scheme like the others.

## The screen

Eight rows, `label value` like every other screen, right-aligned:

```
GPS              fix
Sats            7/11
Fix age          3 s
UTC         13:47:09
Date      2026-09-08
Lat        47.376887
Lon         8.541694
Alt          408.0 m
```

Every value is a dash until it is known, by the rule phase 11 wrote down. A
receiver that is off shows `off` and seven dashes and a line saying the
switch or the menu turns it on; one that is searching shows the satellites
it can see and the time, which it has before it has a position. The widest
a coordinate can be, `-180.000000`, fits its row, and a test says so. The
golden images are `position` (off), `position-populated` (a fix),
`position-searching`, and the menu with its new item, plus the 128 × 64
version of the fix.

## What it does not do

* **The current draw is not measured.** Nothing in the loop could measure
  it. The receiver can be turned off from the menu and the switch, and the
  UART is released when it is, so both halves of the measurement are one
  reading away from anyone with a meter in the battery lead.
* Position reporting over the air, by the issue's own decision: an RNode is
  a modem, and Reticulum has no notion of a device's coordinates.
* The module is not configured. It is taken as it wakes -- 9600 baud, its
  default sentence set -- and the MCU's transmit pin is connected and idle.
  A later phase could ask it for `GSV`, or for a faster rate, or for
  standby; none of that is needed to fill the screen.
* The receiver's state is not stored. The switch is the persistent default,
  and a menu choice lasts until the next boot or the next move of the
  switch.

## Done when

- [x] **The module powers up and produces NMEA, with the pin direction
      question settled and documented.** P1.01 high, then 9600 baud with
      the module's TX on P0.20 -- the first probe attempt, on the board, on
      2026-09-08. `src/gps.rs` and the log above.
- [x] **A fix is acquired and the `Position` screen shows it.** Indoors the
      receiver saw nothing for two minutes of clean sentences; under sky on
      2026-09-08 it fixed, and the screen reported it correctly.
- [x] **With no fix, the screen says so rather than showing zeroes.**
      `the_position_screen_without_a_fix_says_so`: off, silent and searching
      each name themselves, and no number stands where a coordinate would.
- [x] **Parsing is in `oxinode-core` and unit tested against captured
      sentences, including malformed ones.** `core/src/gps.rs`, twenty-five
      tests: fourteen captured lines, corruption of every kind the framing
      can meet, and eighteen malformed bodies.
- [ ] **The receiver can be turned off, and the current draw both ways is
      measured and recorded.** Turned off, yes: the menu item, the switch,
      and `Control`'s tests. Measured, no: there was no meter.
