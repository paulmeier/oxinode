# Contributing

Contributions are welcome: bug reports, hardware observations, bench
measurements, and code. This page is what a change needs to fit in.

## Before you start

Open an issue for anything larger than a fix, so the shape can be agreed
before the work. For hardware findings (a pin that behaves differently, a
board revision that differs, a module that identifies as something else), an
issue with the boot log attached is the most useful form.

## The rules the code follows

- **The core is pure.** Anything decidable without a peripheral goes in
  `oxinode-core` and gets tests there. Nothing in the core may depend on
  `embassy-*`, `cortex-m`, or any chip. The firmware crate is kept thin
  enough to be right by inspection, because it cannot be tested.
- **`monopanel` knows nothing of oxinode.** A `use oxinode_core::` under
  `panel/src` is a build error, and the crate has no mandatory dependencies.
- **Refuse, do not clamp.** A configuration the hardware cannot do is
  rejected with a named reason, never silently adjusted.
- **Bound every wait on the chip**, and name the step in the timeout.
- **Unknown is a dash.** A value the board does not know is an `Option`,
  never a plausible zero.
- **Every constant taken from the host's behaviour says so.** The protocol is
  implemented from Reticulum's host side; a constant in
  `oxinode_core::rnode` carries a note naming the host behaviour that pins it.
- **Nothing from the GPL reference firmware or Meshtastic is read, ported or
  transliterated.** Interface data (command bytes, frame layouts, published
  parameters) is fine; code is not. See [License](../license.md).
- **Comments say why.** Measured numbers carry the measurement; a workaround
  names the failure it works around.

## Making a change

1. Branch from `main`.
2. Make the change, with tests where the change is testable. A new screen,
   menu item or editable field needs a golden image, which the simulator
   generates: `sim golden --update`, then commit the PNGs.
3. Run `tools/test.sh`. It is exactly what CI runs.
4. If the change touches hardware behaviour, check it on a board and put the
   relevant log lines in the pull request. The images are built to produce
   evidence; quote it.
5. Open a pull request against `main`. Describe what changed and why, and
   what was verified where (host tests, simulator, board).

Commit messages are one line saying what the change does, in the present
tense, with a body if the why needs more than the diff shows.

## Formatting and lints

`cargo fmt --all` and `cargo clippy` with `-D warnings` over every crate and
both feature sets, as `tools/test.sh` runs them. Shell scripts pass
`shellcheck --severity=warning`. Python tests run under `unittest`.

## Documentation

This site lives under `docs/` and is built with MkDocs and the Material
theme. `pip install -r docs/requirements.txt`, then `mkdocs serve`. CI builds
it with `--strict`, so a broken link fails the build. A change in behaviour
that a user or a contributor would notice should change the page that
describes it.

## Hardware you will need

A Base Duo with the Super IO, and a USB cable. An RTL-SDR and a second board
are useful for radio work; nothing else is required. See
[Local setup](setup.md).

## License

By contributing you agree that your contribution is licensed under the
[Reticulum License](../license.md), like the rest of the project.
