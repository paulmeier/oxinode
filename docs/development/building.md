# Building

## Two feature sets

The firmware has two mutually exclusive feature sets, because the Bluetooth
stack swaps the `critical-section` implementation for the whole image (see
[Bluetooth](../architecture/bluetooth.md)):

| | Build | Images |
|---|---|---|
| default (`cs-single-core`) | `cargo build --release` | `blink`, `usb-cdc`, `radio`, `display` |
| `ble` | `cargo build --release --no-default-features --features ble` | `rnode`, `ble` |

`src/lib.rs` refuses a build that enables both, and `--all-features` is
therefore not a thing this crate can be built with. The `rnode` and `ble`
binaries declare `required-features = ["ble"]`, so a default build skips them
silently rather than building them wrong.

```bash
cargo build --release                                                  # the four default images
cargo build --release --no-default-features --features ble --bin rnode  # the product image
```

Release builds are `opt-level = "s"` with fat LTO, one codegen unit,
`panic = "abort"`, and debug info kept (it costs nothing in flash and makes a
fault address resolvable). Debug builds keep `opt-level = "s"` too, because
they still have to fit in 784 KB and run at 64 MHz.

## The host crates

`oxinode-core`, `monopanel` and `oxinode-sim` build for the host, which has to
be named because the workspace defaults to the Cortex-M:

```bash
HOST="$(rustc -vV | sed -n 's/^host: //p')"
cargo test -p oxinode-core --target "$HOST"
cargo test -p monopanel --target "$HOST" --all-features
cargo test -p oxinode-sim --target "$HOST"
```

`monopanel` has one optional feature, `embedded-graphics`, and the two builds
are different code: without it the crate has no dependencies at all.

## Flashing what you built

`cargo run` is wired to `tools/dfu-flash.sh` as the runner:

```bash
cargo run --release --bin blink
cargo run --release --no-default-features --features ble --bin rnode
```

See [Flashing](../getting-started/flashing.md) for the serial DFU path, port
selection with two boards attached, and recovery.

## The log level

`defmt` filters at **compile** time. With `DEFMT_LOG` unset the default level
is `error`, so `info!` and `debug!` vanish from the binary and the log port
stays silent with no hint as to why. `.cargo/config.toml` sets it to `debug`,
because the `lr11xx` driver logs at debug and that is the whole reason the
log transport exists. `trace` adds a line per SPI command, which is worth
turning on for a specific bring-up problem:

```bash
DEFMT_LOG=trace cargo run --release --bin radio
```

## Building from a git worktree

Cargo merges `.cargo/config.toml` from the worktree *and* from the repository
above it when the worktree lives inside the main checkout, so every
`rustflags` entry is passed twice and the linker refuses the second
`-Tlink.x` with *region 'FLASH' already defined*. Either put worktrees outside
the repository, or set `RUSTFLAGS` explicitly to the config's three flags,
which replaces both:

```bash
RUSTFLAGS="-C link-arg=-Tlink.x -C link-arg=-Tdefmt.x -C target-cpu=cortex-m4" cargo build --release
```

## Image checks

Every build's ELF is checked against `memory.x` by `tools/test.sh` and again
by the flash runner before anything is sent to the board. See
[Memory layout](../hardware/memory-layout.md) and [Host tools](../reference/tools.md).
