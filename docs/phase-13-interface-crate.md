# Phase 13 — the interface as a device-agnostic crate

Where phase 13 stands: **built, held to golden images at two panel sizes,
and changing no pixel on the board's own.** The interface phases 9 to 12
built is now `monopanel`, a workspace crate under `panel/` with nothing of
oxinode in it; oxinode consumes it through its public interface and no other
way, and every one of the forty-five golden images from before the split
still matches.

The question was whether the interface could be built so other devices could
use it, and whether the menu should be a separate project. The answers were
yes, and a separate crate now with a separate repository later, only when
something else actually uses it. This is the crate.

## The boundary, and what it cost

The crate boundary is what buys the design: it makes it impossible for an
oxinode type to leak into the widget layer, and the compiler enforces it on
every build. `panel/Cargo.toml` has one dependency and it is optional; a
`use oxinode_core::` anywhere under `panel/src` is a build error. That is the
entire substance of "reusable", and it is what a module boundary could not
have given.

The repository boundary was not taken, for the reason the issue gave: against
exactly one consumer it is coordination overhead with no return, and
extraction later is cheap if the boundary was drawn cleanly now. The crate is
`publish = false`, named without `oxinode` in it, and its README and manifest
are written for the day it moves.

Four things tied the interface to oxinode, and the issue had them right:

**`Frame` was the SH1107's**, fixed at 128 × 128. The crate draws on a
`Canvas` instead: width, height, set a pixel. Three required methods, with
`fill`, `rect` and `frame_rect` provided on top of them so a canvas that has a
faster fill -- the frame's byte fill -- overrides one method and a canvas
that does not gets a correct one for free. A canvas is not asked to read a
pixel back, because plenty of displays cannot and none of the drawing needs
it; reading is a second, smaller trait, `Readable`, for tests and the
simulator. `Frame` implements both in twenty lines at the foot of
`core/src/sh1107.rs`, and a test there draws the same page onto a frame and
onto the crate's own `Bitmap` and finds every pixel the same. That test is
what makes the simulator's pictures the board's.

**The layout assumed 128 × 128.** Every constant -- the title bar's height,
the content area's top and bottom, the lines that fit, the characters that
fit on one -- is now a field or a method of a `Layout` derived from the
canvas size. The bars are as tall as what they hold, the font and the icons,
and the content gets whatever is left; so a shorter panel loses lines and
nothing else, and a narrower one loses characters. `Layout::of` is `const`,
so oxinode holds the one for its panel as a constant and sizes its line
buffers from it, exactly as before. A test pins the 128 × 128 layout to the
numbers the interface was first drawn with, so the derivation cannot drift
from the pictures approved against them.

**`Screen` and `Action` were oxinode's own enums.** Screens are now data the
application supplies: a slice of descriptors, each a title, a menu title, an
icon and a menu, and the navigator is generic over the action type its menu
items carry. Closing a menu stopped being an action. It was `Action::Close`,
a variant the navigator handled itself and never handed out, and every match
on an action had an arm for it that could not run; now an item that only
closes carries `None`, and oxinode's `Action` has one variant fewer. oxinode
keeps its `Screen` enum, because a `match` over five screens is how the
content is dispatched and an index is a worse thing to match on -- but the
enum is a view over the descriptor table now, not the other way round, and a
test walks both and finds them the same entry for entry.

**The third level was oxinode's editor.** `Nav` held an `Editor` and a
`Lock` in its overlay, both oxinode types with the radio configuration
inside them. The crate has a `Modal` trait instead: a title, and a key
handler that says whether the modal stays, closes, or closes with a value
for the caller. What the crate keeps is what phase 12 decided about the
level -- that it is opened by the caller and not by a menu, that it gets
every key until it is done, and that the screen's scroll is kept under it.
oxinode's `Overlay` is the editor or the notice, and its `Modal`
implementation is phase 12's key table moved across whole. An application
with no third level uses `NoModal`, an empty type, and the navigator is then
exactly the two-level one.

**The font was already generic** and moved across untouched, except that it
draws on a `Canvas` and clips at the canvas's width rather than the panel's.

## What the crate is

Five modules, no dependencies, `no_std`, `forbid(unsafe_code)`:

* `canvas` -- the `Canvas` and `Readable` traits, and a `Bitmap` of any
  size for tests and simulators.
* `layout` -- `Layout`, derived from a width and a height.
* `nav` -- `Input`, `Item`, `Screen`, `Modal`, `Outcome`, `NoModal`, and
  `Nav<A, M>`.
* `draw` -- the title bar, the icon strip, the menu, the scrollbar, and
  `page`, which composes them.
* `font` -- phase 7's 5 × 7 font.

And one optional module, `eg`, behind the `embedded-graphics` feature. It
goes both ways round. `Target` makes any `Canvas` a `DrawTarget`, so the
ecosystem's primitives, text and images can be drawn onto a frame this
interface owns. `Display` makes any monochrome `DrawTarget` a `Canvas`, so
the interface can be drawn onto any of the very large number of display
drivers that speak `embedded-graphics` -- which is the use the issue had in
mind. The one decision an adapter has to make is what to do with an error,
because a `Canvas` cannot fail and a `DrawTarget` can: `Display` keeps the
last one for its owner to look at rather than dropping it or turning every
`set_pixel` in the interface into a `Result`. Both directions are tested,
the second against `embedded-graphics`'s own `MockDisplay` -- a page drawn on
it is the page drawn on a bitmap, pixel for pixel -- and against a display
that fails on every third draw.

The feature is off by default, and `tools/test.sh` lints and tests the crate
both with it and without, because the two builds are different code and
"no dependencies at all" is a property worth checking on every run. One cost
of the feature is in `Cargo.lock`: `embedded-graphics` pins `az` to `~1.2`,
and Cargo resolves one version per compatible range across the workspace, so
`az` and `fixed` -- which `embassy-nrf` uses -- each moved back one minor
version. Both stay inside `embassy-nrf`'s ranges.

## What a second panel size showed

The issue asked for golden images at 128 × 64, and getting them was where
the layout's assumptions actually surfaced. Three things had to change in
the drawing, none of them visible on the square panel:

* **A menu that does not fit.** The Radio menu has nine items: 96 pixels of
  box in a 40-pixel content area. The menu now shows a window of its items
  with the highlight inside it, near the middle where it can be and at an end
  where it must, so moving the highlight moves the window. On a panel that
  holds the whole menu the window is the whole menu and the picture is what
  it always was -- which is why the forty-five square images did not change.
  The window is a pure function of the layout, the count and the selection,
  and is tested as one.
* **The pairing box.** Its height was placed from the 128 × 128 content
  area's middle; it is placed from the layout's now, and on a 128 × 64 fills
  the content area exactly.
* **The scroll.** A `Down` is clamped by how many lines fit, which the
  navigator used to compute from a constant. The renderer tells it now, at
  the same time it tells it how tall the content is, because the renderer
  is the only thing that knows either. A screen that fits the square panel
  scrolls on the wide one, and the test says so.

Twelve images under `sim/golden/128x64/`: every screen populated; the Radio
menu at its top, in its middle and at its bottom, so the window is seen
sliding; the Radio screen scrolled to its end, which is further down than on
the square panel because fewer lines fit; an editor with its refusal; the
notice; and a pairing. They are drawn from the same scenes and the same
scripts as the square set, by a simulator that gained a canvas of any size
for the purpose, and `--panel WxH` on `render` and `steps` draws whatever
size you like.

One thing they show honestly. A modal's lines are not scrolled -- that was
phase 12's decision and the crate keeps it -- so on a four-line panel the
refused editor's reason and the notice's last sentence are off the bottom.
That is a fact about oxinode's content on a panel oxinode does not have, and
the right fix is content that fits, not a scrolling modal. It is left as it
is and noted here.

## What oxinode looks like afterwards

`core/src/font.rs` is gone. `core/src/ui.rs` is oxinode's screens, menus,
actions and overlay -- what the application supplies -- plus a `NavExt` trait
with the five things oxinode asks of the navigator beyond what the crate
provides: the current screen as the enum, and the editor and the notice by
name. `core/src/screens.rs` renders onto any `Canvas` rather than a `Frame`.
The firmware changed in three lines: how the navigator is built, one import,
and the arm for the action that no longer exists. Nothing else in `src/`
noticed.

The tests moved with the code. The navigation tests that were about the
model -- wrapping, opening on `Back`, sideways ignored under a menu, scroll
clamping -- are the crate's now, run over a fixture of three screens
including one with a nine-item menu and one with none. The tests that were
about oxinode -- which actions a host owns, that every field can be refused,
that the notice closes on any key -- stayed in `oxinode-core`, run over the
real screen set. Nothing was dropped: seventy-seven tests in the crate,
three hundred and ninety-eight in the core, forty-five in the simulator, and
the golden set went from forty-five images to fifty-seven.

## What it does not do

* The crate is not published. It is `publish = false` with a name that is
  free on crates.io, waiting for a second device.
* Nothing scrolls a modal. See above.
* The icon strip is centred and does not wrap; six or more screens on a
  64-pixel-wide panel would run off it. No such panel is in view.
* The product image was built and checked against `memory.x` but not
  reflashed: the phase changes no pixel and the golden images say so.

## Done when

- [x] **The interface lives in its own workspace crate, depending on
      nothing from oxinode.** `panel/`, `monopanel`, one optional
      dependency; `oxinode-core` depends on it and not the reverse.
- [x] **`Canvas` is a trait; `Frame` implements it; the layout derives from
      the canvas size.** `monopanel::Canvas`, three methods; `impl Canvas
      for Frame` in `sh1107.rs`; `Layout::of(width, height)`, pinned to the
      original numbers at 128 × 128 by `the_square_panel_lays_out_as_it_always_did`.
- [x] **Screens and menus are supplied by the application, not enumerated
      in the library.** `Nav::new(&SCREENS)` over a `&'static [Screen<A>]`;
      oxinode's table is `ui::SCREENS`.
- [x] **A second panel size renders correctly, proved by golden images at
      128 × 64.** Twelve under `sim/golden/128x64/`, checked by the same
      test as the square set.
- [x] **The optional `embedded-graphics` adapter exists and is tested.**
      `monopanel::eg`, both directions, five tests, run with
      `--all-features` by `tools/test.sh`.
- [x] **oxinode itself consumes the crate through the public interface and
      no other way.** `use monopanel::` in `oxinode-core` and `oxinode-sim`;
      no `pub(crate)`, no re-export the firmware reaches through except
      `Input`, which is the crate's type.
