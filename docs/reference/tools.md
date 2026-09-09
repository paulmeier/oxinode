# Host tools

Everything under `tools/` runs on the host. The shell scripts are written for
macOS's bash 3.2; the Python needs only the standard library.

| Tool | What it does |
|---|---|
| `test.sh` | Every check that does not need a board. What CI runs. See [Testing](../development/testing.md). |
| `dfu-flash.sh` | The `cargo run` runner: ELF → layout check → DFU package → serial DFU. Bootstraps `adafruit-nrfutil` into `tools/.venv/` if needed. |
| `uf2-flash.sh` | The alternative runner: ELF → UF2 → the bootloader's drive, for hosts where the drive works. Reads the family id from the board's `CURRENT.UF2`. |
| `uf2conv.py` | A UF2 writer from the format specification, with tests. |
| `verify_flash.py` | Compares the vector table in the board's `CURRENT.UF2` against a built ELF and says `MATCH` or `MISMATCH`. |
| `layout.py` | Reads `memory.x` for the other tools: `tools/layout.py memory.x --field FLASH.origin`. |
| `check_layout.py` | Validates a built ELF (and optionally its UF2) against `memory.x`. See [Memory layout](../hardware/memory-layout.md). |
| `crate_version.py` | Reads the version out of `Cargo.toml`. |
| `package.sh` | Builds every image and lays out the files that go on a release under `dist/`. |
| `release_notes.sh` | Writes the body of a release to stdout. |
| `test_*.py` | Tests for the above, run by `test.sh` with `unittest`. |

## `dfu-flash.sh`

```bash
tools/dfu-flash.sh target/thumbv7em-none-eabihf/release/rnode
```

Flashing goes over the bootloader's SLIP-framed serial DFU (Nordic's older
protocol, not Secure DFU, which is why it needs Adafruit's tool). The script
checks the image against `memory.x`, finds the board, performs a 1200-baud
touch if the board is running an oxinode image, waits for the bootloader,
sends the package, and then resets the host's cached line setting on the port
so the next thing that opens it does not perform another touch.

With one board attached it finds the port itself. With two it refuses to
guess, because a second Base Duo running someone else's firmware enumerates
on the same glob and flashing it would overwrite that firmware.

| Variable | |
|---|---|
| `OXINODE_DFU_PORT` | use this serial port instead of searching |
| `OXINODE_DFU_PORT_GLOB` | the glob to search (default `/dev/cu.usbmodem*`) |
| `OXINODE_DFU_IN_DFU` | `1`/`0` to assert the board is already in its bootloader instead of detecting it (needed on a host that cannot mount the UF2 drive) |
| `OXINODE_DFU_BOOTLOADER` | `1`/`0` to answer the bootloader probe directly |
| `OXINODE_DFU_RETRY_DELAY` | seconds between retries (default 2) |
| `OXINODE_DFU_SETTLE` | seconds to wait after a flash before reopening the port to reset the cached baud rate (default 3) |

`tools/test_dfu_flash.py` exercises the script end to end without a board
through those variables, which is why `test.sh` runs it after the firmware
build: it needs a real linked ELF to push through.

## `check_layout.py`

```bash
tools/check_layout.py target/thumbv7em-none-eabihf/release/rnode --uf2 target/thumbv7em-none-eabihf/release/rnode.uf2
```

Checks that every loadable segment lies inside FLASH, that RAM usage lies
inside RAM, that the initial stack pointer points into RAM, and that the
reset vector points into FLASH with the Thumb bit set. With `--uf2`, also
that the UF2's blocks cover the image at the right addresses. Its tests
feed it deliberately broken images and assert that each is rejected.

## `package.sh` and `release_notes.sh`

```bash
tools/package.sh                     # version from Cargo.toml, marked -dev
tools/package.sh --version v0.1.0    # must match Cargo.toml's version
tools/release_notes.sh v0.1.0
```

See [Releasing](../development/releasing.md).
