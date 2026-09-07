# Phase 7 — the display

The Super IO board carries a **128 × 128, 1.12-inch OLED** on an SH1107,
reached over I²C on P0.24/P0.25, with a 12 V boost for the panel enabled by
P0.23.

That resolution is the first interesting thing about this phase, because
**it is not the one the RNode protocol assumes**. Reticulum's `CMD_DISP_READ`
accumulates exactly 1024 bytes before it considers a display frame complete,
which is 128 × 64 at one bit per pixel; `CMD_FB_READ` accumulates 512, which is
64 × 64. This panel is 2048 bytes. Nothing about that is resolved yet — it is
step 4's problem — but it is worth knowing before any of the layout is designed
around it.

## Steps

1. **The bus** — I²C up, pins proved from the peripheral's own registers, and
   an address scan. Nothing drawn.
2. **The controller** — the SH1107 command set, a framebuffer, and a test
   pattern that settles what the datasheet does not.
3. **A status page** — something worth looking at, rendered from what the modem
   knows.
4. **The RNode display protocol** — `CMD_FB_*`, `CMD_DISP_READ` and the
   display settings a host can change, against a panel twice the size the
   protocol expects.

---

## Step 1 — the bus

Three facts came out of it, and I got the first one wrong.

**There is one device on the bus, and it is at 0x3C.** The first version of
this scan probed with **reads only**, reported a device at 0x3D, and then took
an address NACK on the very first command. Two things were wrong with that.

A read-only scan asks a question whose answer does not predict the one that
matters: a great many OLED modules implement writes and not reads, so the
direction a display is actually driven in is the direction to scan. That much
was a straightforward oversight.

The 0x3D reading was something worse — it was not a device at all. See below.

**The 12 V rail is not what makes the controller answer.** Scanning with P0.23
low and again with it high gives identical results. The SH1107's logic runs off
3V3 and only its panel bias comes from the boost, so *"it answered on the bus"
is not evidence that the glass can light*. Worth establishing before a dark
screen gets blamed on a driver.

**Both lines idle high, with the rail on or off**, read as plain GPIOs with the
internal pull-ups off — so the 5.1 kΩ resistors on the board are there and
nothing is holding the bus. That check costs one GPIO read and should have been
the first thing in the image rather than the fourth; a stuck SDA makes every
transaction fail identically and is invisible from the peripheral's side.

### The nRF52's TWIM locks up after a NACK

This is the finding of the phase so far, and it wasted an hour by producing
*plausible* answers rather than errors.

The symptom was inconsistency. The same address answered a read one moment and
not the next. A write succeeded, and the identical write NACKed. A full bus scan
disagreed with a targeted probe run half a millisecond earlier. Between them,
the display appeared to be at 0x3D, then at 0x3C, then nowhere at all:

```
probe 0x3c: read ok,           write ok,           read again ok
probe 0x3d: read address-nack, write ok,           read again address-nack
scan:       0x3c -- read false, write true
            0x3d -- read false, write true
init at 0x3c: address-nack
```

Every line of that is from one run, seconds apart, on a bus with one device on
it.

The nRF52's TWIM can be left in a state it does not come out of after a
transaction that ends in a NACK, and `embassy-nrf` implements no workaround.
Cycling the peripheral's `ENABLE` register clears it; `PSEL` and `FREQUENCY`
are separate registers and survive. A scan NACKs a hundred times by design, so
it now does that after every transaction, and `Panel` does it after any failure
— because a failed frame that poisons the next one turns a glitch into a
display that never works again.

With that in place the same run reads:

```
probe 0x3c: read ok,           write ok,           read again ok
probe 0x3d: read address-nack, write address-nack, read again address-nack
scan:       1 device, 0x3c, reads and writes
```

Consistent, and consistent with there being exactly one OLED on the board at
the ordinary address. **The earlier "0x3D" was the lock-up, not a device**, and
the commit that recorded it as a board fact was wrong.

## Step 2 — the controller

The SH1107 is a 128 × 128 controller: 16 pages of 128 columns, one byte per
eight adjacent pixels, with the usual page/column write cursor.

### Which way the axes run, and why it took a photograph

The datasheet says it twice, and not consistently. Figure 10 maps a byte's
D0–D7 onto **segment** outputs and the column address onto **common** outputs,
which puts the page axis along x — the ninety-degree rotation the SH1107 is
known for, being a portrait-panel controller. The reset section says "SEG0 is
mapped to the top line of the display", which puts it along y.

Reading it more carefully would not have settled it. So the test pattern was
built to settle it instead: a border, a solid square just inside the top-left,
and a deliberately **wide, short** bar that would be unmistakable either way.

The bar came out vertical and the square came out bottom-right. Between them
those fix the map exactly:

```
page and bit -> y      column -> x      and both axes inverted
```

So the layout is the familiar SSD1306 one after all, and this panel is mounted
the other way up. The inversion is the controller's own to undo — segment remap
`0xA1` and reversed common scan `0xC8`, two bytes in the init sequence — rather
than two subtractions per pixel in the framebuffer.

The wrong answer looked *almost* right, which is the reason the pattern is now
asymmetric in three separate ways: a corner square, a bar that is wide rather
than tall, and a staircase whose steps lengthen downwards.

### What the panel does

Confirmed on hardware: the controller accepts its init sequence, `0xA5` lights
every pixel independently of RAM — which separates "the panel is dead" from
"the driver is writing the wrong bytes", since it needs no RAM to be correct —
and a full 2048-byte frame goes out in **218,536 µs** at 100 kHz. That is 2128
bytes of bus traffic including the per-page cursor commands and the control
byte, which at nine bits a byte is 192 ms of line time; the rest is per-transfer
overhead. 400 kHz will bring it under 60 ms when there is a reason to want it.

The framebuffer tracks which of the sixteen pages have changed, so an update
that touches one line costs 12 ms rather than 218.

## Step 3 — a status page

The panel is the only thing this board can say to somebody holding it rather
than sitting at the host, so it shows what is otherwise hard to find out: what
the radio is tuned to, whether it is on air, whether anything has been heard,
and how strong it was.

### The font is uppercase, and it was drawn rather than typed

Fifty-five glyphs — space, the digits, `A`–`Z` and fifteen symbols — at 5 × 7.
Lowercase folds to uppercase rather than being dropped, so `"Freq"` renders as
`FREQ`; anything outside the table renders as a hollow box, so a missing glyph
looks like a missing glyph rather than like a space.

Uppercase-only is a chosen limitation. A status panel reads perfectly well in
capitals, and the alternative was twenty-six more glyphs of hand-drawn art for
a screen that shows numbers and four-letter labels.

They were written as readable ASCII art and converted to the table by a
generator. Fifty-five glyphs of hand-entered hex is two hundred and seventy-five
chances to make a mistake whose only symptom is a wrong pixel. The tests render
glyphs *back* into art and compare against the shape they were drawn as, which
is what would catch a generator that transposed rows and columns or numbered
the bits from the bottom.

### The test that matters is that every field reaches the screen

`changing_any_field_changes_the_picture` renders the page thirteen times,
altering one field each time, and asserts the framebuffer differs. That is the
test that catches a field somebody added to the struct and forgot to draw —
which is invisible in every other way, because the page still renders and still
looks plausible.

### Redrawing a page is not the same as changing it

The first version updated in 218 ms, which is the cost of the whole panel, for
a change of one digit. The framebuffer tracks which of its sixteen pages have
changed, but a renderer that clears and redraws marks all sixteen whatever the
picture ends up looking like — and clearing first is the only sane way to
write a renderer, because the alternative is erasing exactly what you drew last
time.

So the page is rendered into a scratch frame and committed with
`Frame::copy_from`, which compares the *result* rather than the process and
dirties only the pages that actually differ. Measured on hardware:

```
full flush        218566 us
partial update     27313 us   (one line of text: two pages)
```

Eight times faster, and the difference between a display that can be refreshed
while the radio is working and one that cannot.

## Step 4 — the RNode display protocol

A Reticulum host decides this device has a display purely from its platform
byte — `RNodeInterface` sets `self.display = True` for any nRF52 — and then
offers three things: an **external framebuffer** an application can push
pictures into, a way to read that back, and a way to read what is on the screen.

### Two buffers, two shapes, and neither is this panel

| | size | layout |
|---|---|---|
| external framebuffer | 64 × 64, 512 bytes | **row-major**, 8 bytes a row |
| display readback | 128 × 64, 1024 bytes | **page-major**, a byte is 8 rows |
| this panel | 128 × 128, 2048 bytes | page-major |

Those are not guesses. The host writes the framebuffer a line at a time as
`[line, 8 bytes]` and reads it back by accumulating until it has 512; it reads
the display by accumulating until it has 1024. Both counts are hard-coded on
the host, so a device that sends a different number of bytes sends a frame that
never completes.

So the panel has to be presented as something it is not, twice over, and each
direction gets the answer that loses least:

* **the framebuffer is drawn at double size.** 64 × 64 doubled is exactly
  128 × 128, so a picture fills the panel with no cropping and no
  interpolation — every source pixel becomes a 2 × 2 block. Showing it at 1:1
  in a quarter of the screen would waste three quarters of a display somebody
  is looking at.
* **the readback is halved vertically, by OR.** Sending only the top half would
  be true about half the screen and silent about the rest; folding pairs of
  rows keeps *everything that is lit* visible. It is lossy and it is
  documented; the alternative was lossy and would have looked complete. A test
  asserts that a pixel anywhere on the panel survives the fold, which is what
  makes it a rendition rather than a sample — under sampling, a one-pixel line
  would vanish half the time.

### What is answered, and what is not

`CMD_FB_EXT`, `CMD_FB_WRITE`, `CMD_FB_READ`, `CMD_DISP_READ` and
`CMD_DISP_INT` are implemented. `CMD_DISP_BLNK`, `CMD_DISP_ROT`,
`CMD_DISP_RCND` and `CMD_BLINK` are real features that are not built yet.

`CMD_DISP_ADR` is reported as **not applicable**, which is a different thing:
the panel's address is *discovered* by scanning the bus at boot, so a host
setting it would be replacing a measurement with a guess.

### Two things the integration had to get right

**Writing a picture must not repaint per row.** The host sends sixty-four rows
in a burst; a full flush after each would be fourteen seconds of bus traffic to
show one image. Rows are stored and the panel is repainted on its own half-second
tick, where the frame diff makes the cost proportional to what changed.

**The panel must never block the radio.** A full repaint is 218 ms and the
modem loop cannot be away that long, so `Panel::flush_pages` sends at most two
pages a pass — 28 ms — and a whole screen fills in over about half a second.

The outbox grew from 2048 bytes to 4096, because the largest frame is no longer
a packet: a display read is 1024 bytes of screen, which escapes to 2051 in the
worst case, and a picture is exactly the kind of data that is full of `0xC0`.

### What the hardware says, without anybody looking at it

The two transformations compose into something predictable. A source pixel
`(x, y)` is drawn at `(2x…2x+1, 2y…2y+1)`, and the readback folds panel rows in
pairs — so readback pixel `(X, Y)` is source pixel `(X/2, Y)`, exactly. That
makes the whole path checkable byte for byte from the host, with no eyes
involved:

```
FB_READ  : 512 bytes  MATCH
DISP_READ: 1024 bytes
screen   : 4326 pixels lit, 0 disagree with the pushed picture
```

A 64 × 64 picture — a border, a triangle, and a notch cut out of one corner so
a mirrored or rotated result could not pass — was pushed a row at a time, read
back identical, and then found on the screen transformed exactly as predicted.
All 8192 readback pixels agree. That one run covers `CMD_FB_EXT`, all
sixty-four `CMD_FB_WRITE` rows, `CMD_FB_READ`, `CMD_DISP_READ`, both buffer
layouts, the doubling, the fold, and the escaping in both directions.

Confirmed by eye as well: the triangle appears the right way up and filling the
panel, and switching the external framebuffer off brings the status page back.

### And Reticulum still comes up

With the panel in the loop:

```
RNodeInterface[oxinode RNode] is configured and powered up
  online   : True
  bitrate  : 3125.0
  r_sf/cr/bw: 8 5 125000
```

and `rnodeconf -i` still reports `Device signature validated`. The display costs
the modem nothing it can measure, which is what `flush_pages` was for.

## Flashing this phase cost more resets than the last three phases together

Worth writing down, because none of it was about the display.

**The touch window is 100 ms.** `adafruit-nrfutil` holds the port open at 1200
baud for 100 ms and then allows 1.5 s for the board to reboot and re-enumerate.
This image sampled DTR every 60/940 ms while waiting for a terminal, so it
missed the window and rebooted *seconds* after the flasher had given up. It
polls every 20 ms now.

**Polling that fast exposed the opposite failure.** The 1200-baud line coding
lives in the **host's** cached terminal settings for a device path, not in
anything on the board, and macOS re-applies them when it next opens that path.
So a freshly flashed board can be opened at 1200 by nobody in particular,
closed, and sent straight back to its bootloader — on every boot.
`is_bootloader_touch_after` ignores the condition for the first two seconds,
which sits between the 1.5 s the flasher allows and anything a person would
notice.

**And the same cache made my own log reader a flashing tool.** `cat
/dev/cu.usbmodem*` opens with whatever the host has cached, so after a failed
touch, reading the log *is* a touch. `tools/dfu-flash.sh` now resets the cached
rate to 115200 after every flash, successful or not, and the log is read with a
reader that opens at an explicit baud rate.

Three separate mechanisms, one shared cause: **the 1200-baud touch is state on
the host, and it outlives the operation that set it.**
