# Host tools

Everything under `tools/` runs on the host. The shell scripts are written for
macOS's bash 3.2; the Python needs only the standard library, except the
exchange runner, whose two peers need Reticulum, the bench peer, which needs
Reticulum and LXMF, the test-vector writer, which needs Reticulum too, the touch series, which borrows pyserial from the Python
Reticulum runs under, and the log reader, which calls libusb through
`ctypes`.

| Tool | What it does |
|---|---|
| `test.sh` | Every check that does not need a board. What CI runs. See [Testing](../development/testing.md). |
| `dfu-flash.sh` | The `cargo run` runner: ELF → layout check → DFU package → serial DFU. Bootstraps `adafruit-nrfutil` into `tools/.venv/` if needed. |
| `uf2-flash.sh` | The alternative runner: ELF → UF2 → the bootloader's drive, for hosts where the drive works. Reads the family id from the board's `CURRENT.UF2`. |
| `uf2conv.py` | A UF2 writer from the format specification, with tests. |
| `verify_flash.py` | Compares the vector table in the board's `CURRENT.UF2` against a built ELF and says `MATCH` or `MISMATCH`. |
| `log_reader.py` | Reads the product image's `defmt` log from its vendor USB interface, for `defmt-print`. See [Debugging without a probe](../development/debugging.md#the-log). |
| `setter_burst.py` | Writes RNode setters back to back and counts the ones the board answers, in three host behaviours. The recipe for issue #36. |
| `touch_series.py` | Reflashes a board by 1200-baud touch, round after round, each touch taken with a phone's Bluetooth link up, and counts the times serial DFU does not answer. The recipe for issue #23. |
| `air_exchange.py` | Two RNodes on the bench exchanging packets through Reticulum: a size under 254 bytes and one over, each way, with every log kept. See [Testing](../development/testing.md#the-exchange-with-another-rnode). |
| `peer.py` | A Reticulum and LXMF peer on the bench, through the second board: checks the board's announces, sends it messages and times the proofs, and checks the messages it sends back. The other end of every messaging test. See [below](#peerpy). |
| `lxmf_vectors.py` | Writes the LXMF messages `oxinode_core::node::lxmf` is tested against, packed by stock LXMF for opportunistic delivery and put in packets as LXMF puts them on the air: a plain message, the same message sent again, one with a title and fields, one with a stamp, one from a stranger with the stranger's announce, and the delivery proof Reticulum makes for the first. Run by hand; its output is committed. |
| `node_vectors.py` | Writes the Reticulum packets `oxinode_core::node` is tested against, from stock Python Reticulum: an `lxmf.delivery` announce and a path request. Run by hand; its output is committed. The board's own announce and path response beside them are written the other way, by `cargo test -p oxinode-core --features node` with `OXINODE_WRITE_VECTORS=1`, and `test_peer.py` holds them to the peer's reading and Reticulum's. |
| `layout.py` | Reads `memory.x` for the other tools: `tools/layout.py memory.x --field FLASH.origin`. |
| `check_layout.py` | Validates a built ELF (and optionally its UF2) against `memory.x`, and with `--max-flash` and `--max-ram` against a budget. See [Memory layout](../hardware/memory-layout.md). |
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
captured through `log_reader.py` for the whole run, so each frame's header
byte and sequence nibble and every join are in the notes; a stock RNode has
no log, and its side is Reticulum's log.

The peers run under whichever Python has Reticulum: this one if it does,
otherwise the one `rnsd` on the `PATH` runs under. Everything goes to
`target/air-exchange/<time>/`: `summary.txt`, a Reticulum log per peer per
exchange, and `<name>.defmt.log` per oxinode.

| Option | |
|---|---|
| `--a`, `--b` | the two KISS ports |
| `--a-name`, `--b-name` | what to call them in the logs (default `A`, `B`) |
| `--a-log`, `--b-log` | the board's USB serial number to read the log from, or `none`; by default looked up from the KISS port, for the product image only |
| `--elf` | the ELF the oxinodes were built from (default the release `rnode`) |
| `--sizes` | sizes on the air, comma separated (default `200,400`) |
| `--frequency`, `--bandwidth`, `--txpower`, `--spreadingfactor`, `--codingrate` | the radio configuration, given to both; `--frequency 2478000000 --bandwidth 812500 --txpower 11` is the 2.4 GHz path, with both antennas on the u.FL |
| `--timeout` | seconds to wait for an RNode to come up, or a packet to arrive (default 30) |
| `--out` | where the logs go |

Exit status is zero only when every exchange delivered the bytes that were
sent. `tools/test_air_exchange.py` pins what it claims about Reticulum's
framing: the overhead that makes a 400-byte packet 400 bytes on the air, the
plan, and the config that keeps the two instances apart.

## `peer.py`

```bash
python3.11 tools/peer.py --rnode /dev/cu.usbmodem3101 --board-log 65C11224153B1DFD --send 3
```

The other end of a messaging test: stock Python Reticulum and LXMF in a
configuration directory of their own, speaking through the second bench board
as an ordinary `RNodeInterface`, to a board under test that has no host and is
a node of its own. It keeps one identity between runs, in `tools/.peer/`, and
announces it as `Bench peer` when it starts and again when it first hears the
board, so a board that came up later still learns it.

- **Announces.** Every frame is read as it comes off the air, before
  Reticulum drops repeats, and every `lxmf.delivery` announce is checked by
  the peer itself: the name hash, the destination hash against the key, the
  Ed25519 signature over what Reticulum signs, and the display name and stamp
  cost in the app data as LXMF reads them. Each is printed with its hops,
  whether it carries a ratchet, the announcing node's clock, its size and its
  signal, and a repeat is marked as one. An announce sent in answer to a path
  request is checked the same way but called a path response, and is not
  counted as the board announcing. The board is the hash given with
  `--board`, or the first `lxmf.delivery` announce that checks out.
- **Messages to the board.** Once the board has announced, and a few seconds
  after, it sends opportunistic LXMF messages one at a time through LXMF's own
  router, and prints the proof and how long it took, or the failure. Content
  too long to go in one packet is refused before the run starts, because LXMF
  would quietly send it over a link instead. With `--repeat`, each message
  delivered is sent once more as a new packet with the same message hash, as
  LXMF does when a proof is lost, and needs a proof of its own: the board's
  log says it was proved again and not kept twice.
- **A sender the board has not heard.** With `--no-announce` the peer never
  announces, so a message from it comes from a stranger: the board keeps it
  unverified and asks for the peer's path, Reticulum answers with the peer's
  announce, and the board's log says the message was then verified. Give it
  an `--identity` the board has not heard since it booted.
- **Messages from the board.** Every message is printed with LXMF's own
  signature check, and the peer reads its framing itself and checks that the
  hash comes out as LXMF's.

The run stops when everything asked for has happened -- and, if the board
sent it a message, `--linger` seconds after the last one, so its proof goes
out -- or after `--seconds`,
and exits non-zero if anything is missing or did not check out: so it is a
regression run as well as something to watch. Everything goes to
`target/peer/<time>/`: `events.log`, `summary.txt`, `reticulum.log`, and the
`defmt` log of each oxinode through `log_reader.py`.

| Option | |
|---|---|
| `--rnode` | the KISS port of the board the peer speaks through |
| `--board` | the board's `lxmf.delivery` hash; by default the first one heard that checks out |
| `--board-log` | the board under test's USB serial number, to keep its log |
| `--rnode-log` | the RNode's USB serial number, or `none`; by default looked up from `--rnode` |
| `--board-elf`, `--rnode-elf` | the ELFs the two run, for their logs (default the node build and the release `rnode`) |
| `--expect-announces` | announces from the board to see (default 1) |
| `--send` | messages to send it (default 0) |
| `--expect-proofs` | proofs to see (default one per message sent) |
| `--repeat` | send each delivered message again as a new packet, and expect a proof for that too |
| `--no-announce` | never announce, so the board has to ask for the peer's key |
| `--expect-messages` | messages from the board to see (default 0) |
| `--text`, `--title` | what to send; `{n}` and `{time}` in the text are filled in |
| `--settle`, `--gap` | seconds from the board's first announce to the first message (default 5), and from one message's end to the next (default 10) |
| `--announce-every` | seconds between the peer's own announces, beyond the two above (default 0, none) |
| `--seconds` | the longest the run lasts (default 120) |
| `--listen` | run all of `--seconds` even once everything expected has happened |
| `--linger` | seconds to go on after the last message from the board, so its proof goes out and the board logs it delivered (default 8) |
| `--name`, `--identity` | the peer's display name and where its identity is kept |
| `--frequency`, `--bandwidth`, `--txpower`, `--spreadingfactor`, `--codingrate` | the radio configuration, which must be the board under test's stored one |
| `--timeout`, `--out` | as for `air_exchange.py` |

The RNode the peer speaks through must have no phone connected: while one is,
the board sends what it hears to the phone, and the peer hears nothing. If
Reticulum refuses the configuration, the radio may be locked by an earlier
host; the peer says so with Reticulum's error rather than waiting.

`tools/test_peer.py` pins what it claims about the framing: the announce
`node_vectors.py` wrote, read field by field and with a ratchet and a second
header added; the 295 bytes of content that fit in one packet; where the
message hash comes from and why a stamp is not part of it; and the msgpack
subset it reads with. Run under the Python Reticulum is installed in, it also
holds each reading to Reticulum's and LXMF's own.

## `log_reader.py`

```bash
tools/log_reader.py [--serial <USB serial number>] [--once] | defmt-print -e <elf>
tools/log_reader.py --list
```

Finds a board running the product image (`1209:0003`), finds the interface
whose class, subclass and protocol are `LOG_INTERFACE_*` in
`oxinode_core::usb`, claims it, sends the attach request and copies its bulk
IN endpoint to stdout. It detaches on the way out however it is stopped, and
follows its board across a reset unless `--once` is given. Needs libusb
(`brew install libusb`; on Linux, libusb-1.0 and a udev rule for `1209:0003`).
`tools/test_log_reader.py` checks the descriptor walk and that the constants
are the firmware's.

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
