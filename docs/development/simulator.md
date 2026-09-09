# The simulator

The interface is pure code, so it can be looked at on the host. `oxinode-sim`
renders any screen to a PNG, plays the menus in a terminal, and holds every
screen and menu to a committed golden image. Without it, a change to a menu's
centring is a flash, a squint at a 1.12-inch panel, and a photograph; with it,
the change is a red patch on a diff image in the test output.

The crate needs the host target named, because the workspace defaults to the
Cortex-M:

```bash
alias sim='cargo run -q -p oxinode-sim --target "$(rustc -vV | sed -n "s/^host: //p")" --'
```

```
oxinode-sim render [--script S] [--state F] [--panel WxH] [--text] -o out.png
oxinode-sim steps  [--script S] [--state F] [--panel WxH] --out DIR
oxinode-sim tty    [--script S] [--state F] [--braille | --half]
oxinode-sim golden [--update] [--dir DIR] [--diff DIR]
oxinode-sim raw    FILE -o out.png
```

## A picture of a screen

A script is the keys you would press: `right right select down select`, with
`word*N` for repeats and `#` for comments. The result is a PNG at 4× with a
one-pixel grid between cells, which at 128 × 128 reads better than the panel
does. The grid is not decoration: the only kind of layout bug this interface
has is a one-pixel misalignment, and at 4× without a grid that is a faint
smudge.

```bash
sim render --script "right*4 select down" -o system-reboot.png
```

**A picture per step**, to see a path through the menus as a strip:

```bash
sim steps --script "right select down select" --out /tmp/steps
```

## In the terminal

Arrow keys move, Enter selects, Esc or Backspace goes back, `q` quits. The
panel is drawn in braille, or in half blocks on a terminal tall enough for 64
rows of them. The actions a menu item would fire are shown on the status line
rather than performed, exactly as the core hands them to the firmware; so is
what is being edited and whether it was refused. Raw mode comes from `stty`,
not a crate.

```bash
sim tty
```

## What the screens draw from

The screens draw from a `State`, the same plain values the board copies out
of its modem loop, and the simulator has three fixtures for it:

| `--state` | What it is |
|---|---|
| `populated` (default) | a board mid-session with every field known and a host on USB |
| `empty` | a board that knows nothing yet, so every screen shows how it says so |
| `standalone` | a TNC with no host attached, which is the one whose settings the panel may change |

The populated Radio screen is longer than the panel, which is what exercises
scrolling and the scrollbar:

```bash
sim render --script "right down*3" -o radio-scrolled.png
sim render --state empty --script "right*4" -o system-empty.png
```

## Editing a setting

The Radio menu's first five items open an editor. The scene plays the board's
part: with nobody on the line it opens the editor and a confirmed value lands
in the state, so the next picture shows it; with a host on the line it opens
the notice instead. The scene applies the same `Action::changes_the_radio`
the firmware does, so the golden images of the notice are the board's
behaviour and not an impersonation of it. Here the fifth press is *TX Power*,
`up*4` takes 17 dBm to 21, and the last `select` is refused:

```bash
sim render --state standalone --script "right select down*5 select up*4 select" -o refused.png
sim render --script "right select down select" -o locked.png
```

## Another panel

The interface draws on any size of canvas, and `--panel WxH` renders on one
the board does not have. 128 × 64 is the size the golden set is also held at,
with four lines between the bars and the nine-item Radio menu shown two rows
at a time:

```bash
sim render --panel 128x64 --script "right select down*4" -o radio-menu-wide.png
```

## A frame from the board

A 2048-byte dump of the controller's RAM renders the same way, through the
same address arithmetic the firmware draws with:

```bash
sim raw frame.bin -o frame.png
```

The product image's `CMD_DISP_READ` returns the panel folded to 128 × 64 in
the SSD1306 layout the hosts expect (1024 bytes); rendering that with
`--panel 128x64` is how a screenshot is taken from a board over USB with no
eyes involved.

## Golden images

```bash
sim golden            # what tools/test.sh runs
sim golden --update   # after an intended change; commit the result
```

The set under `sim/golden/` is generated from the screen list and the field
list: every screen empty and populated, every item of every menu, a scrolled
page at the top, the middle and the bottom, every editor open, the refusals
that can be reached, the frequency editor with its cursor moved, the screen
after a confirmed edit, the notice for USB and for a phone, a pairing in
progress, and the receiver searching. `sim/golden/128x64/` holds the same
screens on the other panel. On a mismatch the rendering and a diff land in
`target/golden-diff/`.

## What it does not tell you

Anything electrical: the pad's debounce and auto-repeat (tested in the core
with a clock that is a number and measured on the board by the driver), I²C
timing, the panel's own refresh, and anything about real modem state, which
the caller supplies here exactly as it does on the board. The simulator's
input script is the gestures *after* the pad driver, which is the right seam:
the simulator tests what a gesture does, and the board tests when one
happens.
