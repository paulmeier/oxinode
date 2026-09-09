# The display

The Super IO carries a **128 × 128, 1.12-inch OLED** on an SH1107 controller,
reached over I²C on P0.24/P0.25 at address `0x3C`, with a 12 V boost for the
panel enabled by P0.23.

## The bus

The controller's logic runs off 3V3; only the panel bias comes from the boost.
So "it answered on the bus" is not evidence that the glass can light, and a
scan gives identical results with P0.23 low or high. Both lines idle high with
the internal pull-ups off, so the 5.1 kΩ resistors on the board are doing
their job. The bus is scanned **with writes**, because a great many OLED
modules implement writes and not reads, and the direction a display is driven
in is the direction to scan.

### The TWIM lock-up

The nRF52's TWIM can be left in a state it does not come out of after a
transaction that ends in an address NACK, and `embassy-nrf` implements no
workaround. It does not report an error. It produces plausible answers: the
same address answers a read one moment and not the next, a scan disagrees with
a probe half a millisecond earlier, and a bus with one device on it appears to
have two. Cycling the peripheral's `ENABLE` register clears it; `PSEL` and
`FREQUENCY` survive. `oxinode::display::reset_peripheral` does that, the scan
does it after every transaction (a scan NACKs by design), and the panel driver
does it after any failure, because a failed frame that poisons the next one
turns a glitch into a display that never works again.

## The controller

The SH1107 is 16 pages of 128 columns, one byte per eight vertically adjacent
pixels, with the usual page/column write cursor. The datasheet describes its
axis mapping twice and inconsistently; on this panel, established with an
asymmetric test pattern, the layout is the familiar SSD1306 one (page and bit
along y, column along x) with both axes inverted. The inversion is undone by
the controller, with segment remap `0xA1` and reversed common scan `0xC8` in
the init sequence, rather than by two subtractions per pixel.

`0xA5` (all pixels on, independent of RAM) separates "the panel is dead" from
"the driver is writing the wrong bytes".

A full 2048-byte frame takes about 218 ms at 100 kHz. The framebuffer
(`oxinode_core::sh1107::Frame`) tracks which of the sixteen pages have
changed, and pages are committed by comparing the *result* of a render against
what the controller was last sent, so a change of one digit costs the two
pages it touches (about 27 ms) rather than the whole panel. The modem loop
sends at most two pages per pass, so the panel never blocks the radio for more
than about 28 ms, and a whole screen fills in over about half a second.

`Frame` also implements `monopanel::Canvas`, in twenty lines, which is what
lets the interface crate draw on it directly. See
[The interface](../architecture/interface.md).

## The font

Fifty-five glyphs at 5 × 7: space, the digits, `A` to `Z`, and fifteen
symbols. Lowercase folds to uppercase; anything outside the table renders as a
hollow box, so a missing glyph looks like a missing glyph rather than a space.
The glyphs were drawn as ASCII art and generated into a table, and the tests
render them back into art and compare against the shape they were drawn as.
The font lives in `monopanel`.

## The RNode display protocol

A Reticulum host decides this device has a display purely from its platform
byte (`RNodeInterface` sets `self.display = True` for any nRF52) and then
offers an **external framebuffer** an application can push pictures into, a
way to read it back, and a way to read what is on the screen. Neither buffer
is the shape of this panel:

| | size | layout |
|---|---|---|
| external framebuffer (`CMD_FB_WRITE`/`CMD_FB_READ`) | 64 × 64, 512 bytes | row-major, 8 bytes a row |
| display readback (`CMD_DISP_READ`) | 128 × 64, 1024 bytes | page-major, a byte is 8 rows |
| this panel | 128 × 128, 2048 bytes | page-major |

Both byte counts are hard-coded on the host, which accumulates until it has
exactly that many, so a device that sends a different number sends a frame
that never completes. Each direction gets the answer that loses least:

- **The framebuffer is drawn at double size.** 64 × 64 doubled is exactly
  128 × 128: every source pixel becomes a 2 × 2 block, and a picture fills
  the panel with no cropping.
- **The readback is halved vertically, by OR.** Pairs of rows are folded, so
  everything that is lit stays visible. It is lossy and documented; a test
  asserts a pixel anywhere on the panel survives the fold.

The two compose predictably: readback pixel `(X, Y)` is source pixel
`(X/2, Y)`, exactly, which makes the whole path checkable byte for byte from
the host with no eyes involved. Rows pushed by the host are stored and the
panel repainted on its own tick, because a full flush per row would be
fourteen seconds of bus traffic for one picture.

`CMD_FB_EXT`, `CMD_FB_WRITE`, `CMD_FB_READ`, `CMD_DISP_READ` and
`CMD_DISP_INT` are implemented. `CMD_DISP_BLNK`, `CMD_DISP_ROT`,
`CMD_DISP_RCND` and `CMD_BLINK` are known and not implemented. `CMD_DISP_ADR`
is reported as not applicable: the panel's address is discovered by scanning
the bus at boot, so a host setting it would replace a measurement with a
guess.

## The bring-up image

`display` brings the bus up, reads the pins back from the peripheral's own
registers, scans the bus with the 12 V rail off and on, draws the test
pattern, and then renders a status page. It is a diagnostic for a panel that
stays dark; the product image contains the same driver.
