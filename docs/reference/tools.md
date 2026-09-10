# Host tools

Everything under `tools/` runs on the host. The shell scripts are written for
macOS's bash 3.2; the Python needs only the standard library, except the
exchange runner, whose two peers need Reticulum.

| Tool | What it does |
|---|---|
| `test.sh` | Every check that does not need a board. What CI runs. See [Testing](../development/testing.md). |
| `dfu-flash.sh` | The `cargo run` runner: ELF → layout check → DFU package → serial DFU. Bootstraps `adafruit-nrfutil` into `tools/.venv/` if needed. |
| `uf2-flash.sh` | The alternative runner: ELF → UF2 → the bootloader's drive, for hosts where the drive works. Reads the family id from the board's `CURRENT.UF2`. |
| `uf2conv.py` | A UF2 writer from the format specification, with tests. |
| `verify_flash.py` | Compares the vector table in the board's `CURRENT.UF2` against a built ELF and says `MATCH` or `MISMATCH`. |
| `air_exchange.py` | Two RNodes on the bench exchanging packets through Reticulum: a size under 254 bytes and one over, each way, with every log kept. See [Testing](../development/testing.md#the-exchange-with-another-rnode). |
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

## `air_exchange.py`

```bash
python3.11 tools/air_exchange.py --a /dev/cu.usbmodem101 --b /dev/cu.usbmodem3101
```

Runs the exchange that says whether two RNodes agree on the air: for each
size (200 and 400 bytes on the air by default, either side of the split at
254), a packet from the first to the second and one back, each through a
Reticulum instance of its own with one `RNodeInterface`, so what goes on the
air is what `rnsd` sends. The sender and the receiver compare hashes, the size
on the air is asserted from the packet Reticulum built, and the receiver
reports the signal the packet came with. Every oxinode's `defmt` log is
captured from its log port for the whole run, so each frame's header byte and
sequence nibble and every join are in the notes; a stock RNode has no log
port, and its side is Reticulum's log.

The peers run under whichever Python has Reticulum: this one if it does,
otherwise the one `rnsd` on the `PATH` runs under. Everything goes to
`target/air-exchange/<time>/`: `summary.txt`, a Reticulum log per peer per
exchange, and `<name>.defmt.log` per oxinode.

| Option | |
|---|---|
| `--a`, `--b` | the two KISS ports |
| `--a-name`, `--b-name` | what to call them in the logs (default `A`, `B`) |
| `--a-log`, `--b-log` | a log port, or `none`; on macOS an oxinode's is derived from its KISS port (`...1` to `...3`) |
| `--elf` | the ELF the oxinodes were built from (default the release `rnode`) |
| `--sizes` | sizes on the air, comma separated (default `200,400`) |
| `--frequency`, `--bandwidth`, `--txpower`, `--spreadingfactor`, `--codingrate` | the radio configuration, given to both |
| `--timeout` | seconds to wait for an RNode to come up, or a packet to arrive (default 30) |
| `--out` | where the logs go |

Exit status is zero only when every exchange delivered the bytes that were
sent. `tools/test_air_exchange.py` pins what it claims about Reticulum's
framing: the overhead that makes a 400-byte packet 400 bytes on the air, the
plan, and the config that keeps the two instances apart.

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
