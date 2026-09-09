# Contributing to oxinode

Thanks for taking an interest. The full guide is in the documentation at
[paulmeier.github.io/oxinode/development/contributing](https://paulmeier.github.io/oxinode/development/contributing/);
this is the short version.

## Reporting

Open an issue. For hardware findings (a pin that behaves differently, a board
revision that differs, a module that identifies as something else), attach
the boot log: the product image prints what the hardware is in its first few
lines. See
[Debugging without a probe](https://paulmeier.github.io/oxinode/development/debugging/)
for how to capture it.

## Changing code

1. Branch from `main`.
2. Keep to the split: anything decidable without a peripheral goes in
   `oxinode-core` with tests; `monopanel` knows nothing of oxinode; the
   firmware crate stays thin. Refuse invalid configurations, never clamp
   them. Bound every wait on the chip. Unknown values are a dash, not a zero.
3. Nothing from the GPL-licensed RNode reference firmware or from Meshtastic
   is read, ported or transliterated. Interface data is fine; code is not.
4. A new screen, menu item or editable field needs a golden image:
   `cargo run -p oxinode-sim --target "$(rustc -vV | sed -n 's/^host: //p')" -- golden --update`,
   then commit the PNGs.
5. Run `tools/test.sh`. It is exactly what CI runs.
6. If the change touches hardware behaviour, check it on a board and quote
   the log lines in the pull request.
7. Open a pull request against `main` saying what changed, why, and what was
   verified where.

## Documentation

`docs/` is a MkDocs site. `pip install -r docs/requirements.txt` and
`mkdocs serve`. CI builds it with `--strict`.

## License

By contributing you agree that your contribution is licensed under the
[Reticulum License](LICENSE), like the rest of the project.
