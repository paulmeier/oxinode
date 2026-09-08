# Phase 11 — the screens, drawn from real modem state

Where phase 11 stands: **on the board, and read back from it.** Every screen
draws from what the modem actually knows, says so where it knows nothing, and
the pixels the board shows were pulled off it over `CMD_DISP_READ` and looked
at. The first read after the flash found:

```
battery: count 3043, 4110 mV, 100%, charging=false
```

and a System screen reading `Serial 65C11224153B1DFD`, `Identity signed`,
`RAM free 193.1 KB` — all three of them facts the board had rather than
values the screen was given.

The shell could navigate five empty screens. This phase fills them, read-only:
what each screen is allowed to know, and how it gets there without the render
path reaching into the modem loop.

## The shape

One new module in the core, `oxinode_core::screens`, and the rule it enforces
is the one the shell already had: it never decides what is true. It is handed
a `State` — five small structs of plain values, one per screen — and formats
and draws it. No protocol, no clock, no pin. The firmware copies the values
out of the modem loop once per redraw, in one function, and hands the copy
over; the simulator hands over a fixture. The render path is the same code in
both places, which is what makes every screen a host test and a golden image.

Each screen is a list of text lines, and `ui::page` draws them from the
scroll position with a scrollbar when there are more than fit. That gives
every screen scrolling for free and the same shape as every other. A `label
value` row is right-aligned by padding, because the font is fixed-width and a
column of values is what makes a page of numbers readable at a glance.

## What each screen knows

**Home.** Which host has the line — a connected phone, else whoever has the
KISS port open (DTR), else none — and whether anything is talking, which is a
frame in either direction within the last ten seconds. What the radio is
doing. Packets in and out, the last packet's RSSI and SNR, uptime, and the
battery from the divider on P0.31 with the charger's status line beside it.

**Radio.** The protocol's current configuration, which is the same source the
host reads back: frequency, and the frequency the chip is actually tuned to
after the 73 ppm correction; bandwidth, spreading factor, coding rate, power,
the bitrate they come to; preamble, sync word, CRC, header mode, IQ. And what
the radio is *doing* with all that, which is a different thing from what it
was asked — `off`, `receiving`, `refused` with the reason wrapped under it,
`failed` when a valid configuration would not take, or `no radio` when the
chip never came up. Fourteen lines, which is more than the panel holds, so
this is the screen that scrolls.

**Bluetooth.** Absent, advertising or connected; the advertised name; the
passkey while a pairing is in progress; how many phones are bonded, out of
four. The passkey is also drawn as a box over *every* screen while it is set,
not only this one: pairing is started from the phone, and the person holding
it has to read the digits off whatever the board happened to be showing.

**Position.** The GPS is not driven until phase 14, and the screen says so in
a sentence. There is no `fix: Option<Fix>` behind it that is always `None`; a
field that can only ever be empty is a promise the screen cannot keep yet.

**System.** oxinode's version and the RNode protocol version it speaks, the
device serial, the identity, and free RAM. The identity is what the *device*
can say about it: `none`, `bad checksum`, `unsigned`, or `signed`. Signed
means a signature is stored — the signature is RSA over the device hash, made
with the host's private key, and the device has no public key to check it
against. Only the host validates it, and does, on every connect. What the
device verifies itself is the EEPROM checksum, and that is the case it
reports separately.

Free RAM is the gap between the end of static data — the linker's `__sheap`
— and the stack pointer. On a board with no allocator that is the only honest
number: the stack is the one thing that grows, and the gap is what it has left
to grow into. The subtraction is in the core, where it is tested not to wrap.

## The rule about empty state

A screen with no data says it has no data. Every field that can be unknown is
an `Option`, and `None` draws as a dash. A board that has heard nothing shows
`RSSI -`, not `RSSI 0 dBm`; a board with no cell fitted shows `Battery -`,
not `0%`. The `Position` screen is the extreme case: `Lat: 0.000000` is a
claim to be in the Gulf of Guinea. The core has a test that walks the empty
state and refuses any line that ends in a number where a dash belongs, and the
simulator has a golden image of every screen in that state so the dashes are
in the pictures as well as in the assertions.

The same principle is why the battery reading returns nothing below 2.5 V.
With no cell the divider is pulled to nothing and the pin reads near zero,
and that is not a flat battery.

## The battery, for the first time

P0.31 reaches the cell through 806 kΩ over 1.5 MΩ, so the pin sees 0.65048 of
it — 4.2 V arrives as 2.73 V, inside the 3.6 V the SAADC measures at gain 1/6
against its 0.6 V reference. The arithmetic is in `oxinode_core::battery`,
pinned to those exact ADC settings, with the divider ratio checked against the
README's figure and Meshtastic's `ADC_MULTIPLIER` of 1.537 in a test. The
percentage comes from the eleven-point open-circuit table the board's
Meshtastic variant carries, interpolated, because a straight line from 3.0 V
to 4.2 V is twenty points wrong through the middle of a lithium cell's curve.
It is still an estimate — a cell under load reads low, one on the charger
reads high — which is why the voltage is shown beside it.

The charger's `STAT` line on P1.02 is read with the chip's pull-up, because it
is open drain: low means charging.

**Read on hardware, 2026-09-07:** count 3043 at the pin, which the core turns
into 2674 mV there and 4110 mV at the cell — 100 % by the table, with the
charger reporting not charging, which is what a full cell on USB looks like.
The count is logged raw alongside the conversion so the divider can be checked
against a meter without a screen.

## Redraw stays incremental

The modem loop's `Ui` holds two frames: `live`, which is what the controller
has been sent, and `scratch`, which is where the page is drawn. After every
render, `live.copy_from(&scratch)` marks only the pages whose bytes changed,
and the panel gets at most two of them per pass. Nothing marks the whole panel
dirty except the user, from the `Redraw` menu item that exists for it.

A test in the core pins this: the same state rendered twice dirties nothing; a
counter ticking dirties the page its line is on, at most two; and a change
between two short screens leaves the blank rows above the icon strip clean.
Home to Radio *does* redraw every page, because the radio screen fills every
content row — what the test guards is that nothing does so on principle.

## What the pad does now

Gestures go to the navigator and a menu item the user picks comes back as an
`Action`. Four of them are carried out: `Redraw` (the one full repaint),
`Sleep Screen` (the panel off until the next gesture, which wakes it and is
otherwise swallowed), `Reboot` and `Bootloader` (both write the device record
first, for the reason the host's reset does). The other three — `Radio
On/Off`, `Reset Config`, `Forget Phones` — change what a host believes about
the board, and what should happen when the host disagrees is phase 12's
question. Until it is answered they are logged and not done, so the screen
never shows a change the modem did not make.

## The board without a radio

The no-radio path used to draw phase 7's status page with `DEAD` in the
corner. It now runs the same interface, with `no radio` where the radio state
goes on Home and Radio, and the same pad handling. A board whose radio is dead
can still be provisioned and still be looked at.

## Verifying it without eyes

`CMD_DISP_READ` returns the panel folded to 128 × 64 — pairs of rows OR-ed —
in the SSD1306 layout the hosts expect, and that is how every screenshot in
this phase was taken: reset the board over the KISS port, catch the boot log,
send `C0 66 C0`, and render the 1024 bytes that come back. The Home screen
read `Host none` and `Link -` with nothing attached; with the KISS port open
and a frame sent it read `Host USB` and `Link talking`, both within a redraw.

The first read after the flash found the System screen rather than Home,
which is one `Left` from it. The log captured for that boot held no `pad:`
line, but it held only the first eight seconds, and the read was later than
that; the next boot came up on Home and stayed there. The pad logs every
committed press, so if it happens again the log says whether the pad
produced it. It is noted here rather than explained.

## What it does not do

* Nothing is edited from the panel. Phase 12.
* The Position screen is a sentence. Phase 14.
* A pad walk through all five screens has not been done from this desk; the
  screens were read back over USB, which shows what is drawn but not what a
  press does. The navigation itself is phase 9's and phase 10's, both
  verified.

## Done when

- [x] **Every screen renders from real state, with no placeholder values
      anywhere.** `State` is copied out of the modem loop in one function;
      the simulator's `--sample` numbered lines are gone, replaced by
      `--state empty|populated` fixtures.
- [x] **Screens with content longer than the panel scroll, and the scrollbar
      tracks.** The radio screen is fourteen lines against eleven visible;
      `radio-scrolled-{top,middle,bottom}` are its golden images.
- [x] **Golden images cover each screen populated and each screen empty.**
      Twenty-seven images: five screens twice, every menu item, three scroll
      positions, a pairing in progress, and a refused configuration.
- [x] **The render path takes state by value or borrow and reaches into
      nothing.** `screens::render(&mut Frame, &mut Nav, &State)`.
- [x] **Redraw stays incremental.** Pinned by
      `a_redraw_costs_only_the_pages_that_changed`.
