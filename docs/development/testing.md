# Testing

```bash
tools/test.sh
```

That is what CI runs on every push and pull request, and what the release
workflow runs before it publishes anything. It covers formatting, shell
script syntax and lint, clippy over every crate and both feature sets, the
unit tests, the golden images, the host tooling's tests, a build of every
image, a flash-runner smoke test, and a layout check of every built image.

## What is tested where

The firmware crate has no unit tests and will not get any. It depends on
`embassy-nrf`, which only builds for `thumbv7em-none-eabihf`, and with no
debug probe there is nowhere to run a harness. Writing tests against mocks of
a HAL proves the mock works. Instead the project is arranged so that the parts
worth testing are testable somewhere else:

- **`monopanel`** is tested on a bitmap at two panel sizes, with and without
  its optional feature. The tests are where the layout rules live: the chrome
  never overlaps the content, a menu that does not fit is windowed around its
  highlight, a scrollbar appears exactly when a page does not fit, the
  bracket gaps around a highlighted menu row are equal.
- **`oxinode-core`** holds everything decidable without a peripheral. The
  KISS decoder against hostile streams; the command set against both hosts'
  parsers; the whole `configure_device` conversation; the EEPROM image and
  the device record, including a test that flips every byte and asserts none
  decode; the configuration validation and every chip encoding in both
  directions; the reference correction round trip; the airtime formula with
  its working shown; the carrier-sense decision; the air header and split for
  every length from 0 to 508; the pad's timing against a clock that is a
  number; the NMEA framing and parser against captured and malformed
  sentences; every screen's lines from a populated and an empty state; the
  editor's steppers and refusals; and the incremental redraw.
- **`oxinode-sim`** renders every screen, menu item, editor, refusal and
  notice and compares each against a committed PNG under `sim/golden/`, at
  128 × 128 and again at 128 × 64. The pixel assertions in the core say where
  nothing is drawn; a golden image says what it looks like.
- **The host tooling** (UF2 writer, `memory.x` reader, image checker, flash
  runner) is tested directly with `unittest`. The checker's tests mostly feed
  it deliberately broken images and assert that it rejects each one; a check
  that cannot fail is worse than no check, because it gets believed.
- **The built images** are checked against `memory.x`: every loadable byte
  inside FLASH, RAM usage inside RAM, the initial stack pointer in RAM, the
  reset vector in FLASH with the Thumb bit set.

## Golden images

A mismatch fails `cargo test -p oxinode-sim` and leaves the rendering and a
diff image (the expected picture dimmed, every differing pixel in red) in
`target/golden-diff/`. If the change was intended, regenerate and commit:

```bash
cargo run -p oxinode-sim --target "$(rustc -vV | sed -n 's/^host: //p')" -- golden --update
```

The set is generated from the screen list and the field list rather than
written out, so a screen, menu item or editable field added to the core is a
*missing* golden image on the next run rather than a screen nobody looks at.
The comparison decodes both PNGs and compares pixels, because encoder output
is not stable across versions and bytes are not the thing being tested.

## Running one piece

```bash
HOST="$(rustc -vV | sed -n 's/^host: //p')"
cargo test -p oxinode-core --target "$HOST" kiss          # one module's tests
cargo test -p oxinode-core --target "$HOST" air           # the header and split
cargo test -p monopanel --target "$HOST" --all-features
cargo test -p oxinode-sim --target "$HOST"
python3 -m unittest discover -s tools -p 'test_*.py'
cargo clippy --no-default-features --features ble --lib --bins -- -D warnings
```

## On the board

What the host cannot tell you is anything electrical: the pad's debounce and
repeat, I²C timing, the panel's refresh, the radio's timing. The images log
their evidence (the `radio` image reports every bring-up step; the product
image logs every committed pad press with how long it took to settle, every
transmission with its sense count and wait, every configuration with the
commanded frequency), so a change that touches hardware should be checked by
reading the log rather than by watching an LED. See
[Debugging without a probe](debugging.md).

## The exchange with another RNode

What a host test cannot tell you is whether another RNode reads this board's
frames the way this board reads its own. `tools/air_exchange.py` runs the
exchange that settles it, with both boards on one host:

```bash
python3.11 tools/air_exchange.py --a /dev/cu.usbmodem101 --b /dev/cu.usbmodem3101
```

Each side is a Reticulum instance of its own with one `RNodeInterface`, so
what goes on the air is what `rnsd` sends. A 200-byte packet goes each way,
then a 400-byte packet each way, so the split at 254 and the reassembly are
exercised in both directions; the sender and the receiver compare hashes, and
every oxinode's log is captured for the whole run. The product image logs
every frame it hears with its header byte, sequence nibble and split flag,
every join, and every transmission with the header it went under, so a
disagreement shows as the frame it happened on:

```
rx: 255 bytes, header 0x11 (seq 1, split true), rssi -63 dBm, snr 16 dB
rx: 255 bytes, half of a split packet; holding it
rx: 147 bytes, header 0x11 (seq 1, split true), rssi -62 dBm, snr 18 dB
rx: 400 byte packet to the host, from two frames joined
```

Between two oxinodes all four exchanges pass. Against a stock RNode running
the reference firmware the exchange has not been run, and that is the one
that matters; see [Known limitations](../reference/limitations.md). The
runner needs no change for it: one `--b` is a stock RNode's port and
`--b-log none`, since a stock RNode has no log port.
