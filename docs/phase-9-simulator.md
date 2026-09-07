# Phase 9 — a panel simulator, so the interface can be built without a board

The first half of phase 9 built the interface shell as pure code: a `Nav` that
takes an `Input` and says what happened, and render functions that take a
`Frame` and draw. Twenty-eight tests covered it by asserting about pixels, and
none of it had been *looked at*. This is the second half: a host crate,
`oxinode-sim`, that renders any frame to a picture, drives the navigator from a
script or the keyboard, and holds every screen and menu to a golden image.

The reason it comes before the screens get their content is the cycle time.
Without it, a change to a menu's centring is a flash, a squint at a
1.12-inch panel, and a photograph to discuss it with; with it, the change is a
red patch on a diff image in the test output.

## What was built

**A renderer.** `Frame` to PNG at 4×, with a one-pixel grid between cells.
The grid is not decoration: the only kind of layout bug this interface has is a
one-pixel misalignment, and at 4× without a grid that is a faint smudge. With
it, every panel pixel is a countable cell. A `Frame` can also be rebuilt from
a 2048-byte dump of the controller's RAM, through the same address arithmetic
the firmware draws with, so a frame read back from the board renders the same
way.

**An input script.** `right right select down select`, with `word*N` for
repeats and `#` comments. Every golden image is defined by one, and the menu
walk for a test is a string rather than a sequence of calls.

**A terminal mode.** Arrow keys and Enter against the real menu tree. Two
character sets: braille, which fits a whole panel in 64 columns by 32 rows,
and half blocks, which need 64 rows and are chosen automatically when the
terminal has them. Raw mode comes from `stty`, not a crate — the whole
requirement is "one byte at a time, no echo, and put it back afterwards", and
every host this runs on has `stty`.

**Golden images.** Twenty of them, generated from the screen list rather than
written out: every screen, every item of every menu, and a scrolled page at
the top, the middle and the bottom. A screen or menu item added to the core is
therefore a *missing* golden image on the next run, not a screen nobody looks
at. The comparison decodes both PNGs and compares pixels, because encoder
output is not stable across versions and bytes are not the thing being
tested. On a mismatch the rendering and a diff image — the expected picture
dimmed, with every differing pixel in red — land in `target/golden-diff/`.

**One composition.** `ui::page` in `oxinode-core` draws a whole page: chrome,
content lines from the scroll position, the scrollbar, and the menu if one is
open. It went into the core rather than the simulator so that what is looked
at on the host is what the firmware draws; a renderer that lived only in the
simulator would be testing a composition the board never runs.

## What it found

The first golden image caught something. The highlighted menu row read
`>Reboot <`: the label was drawn flush against the opening bracket and a full
advance away from the closing one, so the whole thing sat three pixels off
centre. The width calculation had budgeted for a gap on both sides; the
drawing code left one of them out. Every pixel assertion passed, because every
one of them was about where the menu does *not* draw. A test now measures the
gap inside each bracket and requires them equal — and it was checked against
the old code, where it fails with `gap after > is 1`.

That is the whole argument for the golden images in one line: the assertions
say where nothing is drawn, and a picture says what it looks like.

## What it does not cover

Worth stating plainly, so the simulator is not trusted past its evidence:

* button electrical behaviour, debounce and auto-repeat timing;
* I²C timing and the panel's own refresh behaviour;
* anything about real modem state, which the caller supplies here exactly as
  it does on the board.

Those need hardware, and they are phases 10 and 11.

## Using it

See [Looking at a screen without a board](../README.md#looking-at-a-screen-without-a-board)
in the README. In short, with the host target named because the workspace
defaults to the Cortex-M:

```bash
alias sim='cargo run -q -p oxinode-sim --target "$(rustc -vV | sed -n "s/^host: //p")" --'
sim render --script "right*4 select down" -o system-reboot.png
sim steps  --script "right select down select" --out /tmp/steps
sim tty
sim golden            # what tools/test.sh runs
sim golden --update   # after an intended change; commit the result
```
