#!/usr/bin/env bash
# Everything that can be checked without a board attached.
#
# Split three ways, because the pieces cannot share a build target:
#   * the interface crate, oxinode-core, the panel simulator and the Python
#     tools run on the host, where a test harness exists;
#   * the firmware crate only compiles for thumbv7em-none-eabihf and cannot be
#     unit tested at all (no std, no probe, no harness) -- so it is checked by
#     building it and inspecting the result;
#   * the built images are validated against memory.x, which is the closest
#     thing to an integration test that does not need hardware.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

host="$(rustc -vV | sed -n 's/^host: //p')"
fail=0

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
try() { if ! "$@"; then fail=1; printf '\033[31mFAILED: %s\033[0m\n' "$*"; fi; }

step "formatting"
try cargo fmt --all --check

# The flashing scripts are as load-bearing as the firmware, and macOS ships bash
# 3.2, so they get checked too. `bash -n` catches syntax errors everywhere, and
# where the linter is installed it catches the rest.
# (Do not start a comment with the linter's own name -- it reads that as a
# directive and errors out.)
step "shell scripts"
for script in tools/*.sh; do
    try bash -n "$script"
done
if command -v shellcheck >/dev/null; then
    try shellcheck --severity=warning tools/*.sh
else
    echo "shellcheck not installed; skipping (bash -n still ran)"
fi

step "lint (firmware, thumbv7em-none-eabihf)"
try cargo clippy --lib --bins -- -D warnings

# The BLE build is a separate feature set, not an extra feature: it swaps the
# critical-section implementation, so `--all-features` would enable both and
# fail the guard in src/lib.rs. Hence a second invocation rather than one.
step "lint (firmware, ble)"
try cargo clippy --no-default-features --features ble --lib --bins -- -D warnings

# The product image with the Reticulum stack in it: `node` on top of `ble`.
step "lint (firmware, ble + node)"
try cargo clippy --no-default-features --features ble,node --lib --bins -- -D warnings

# The interface crate has one optional feature, the embedded-graphics
# adapter, and the two builds are different code: without it the crate has
# no dependencies at all, which is a property worth checking on every run.
step "lint (monopanel, $host)"
try cargo clippy -p monopanel --target "$host" --all-targets -- -D warnings
try cargo clippy -p monopanel --target "$host" --all-targets --all-features -- -D warnings

step "unit tests (monopanel, $host)"
try cargo test -p monopanel --target "$host"
try cargo test -p monopanel --target "$host" --all-features

# Twice, like the interface crate: with `node` the crate links a Reticulum
# stack, and without it it must still build with none.
step "lint (core, $host)"
try cargo clippy -p oxinode-core --target "$host" --all-targets -- -D warnings
try cargo clippy -p oxinode-core --target "$host" --all-targets --features node -- -D warnings

step "unit tests (core, $host)"
try cargo test -p oxinode-core --target "$host"
try cargo test -p oxinode-core --target "$host" --features node

# The simulator is a third host crate; it renders the interface and compares
# every screen and menu against the golden images in sim/golden, at the
# board's panel size and at 128 x 64. A mismatch leaves a diff image in
# target/golden-diff/ and fails here.
step "lint (sim, $host)"
try cargo clippy -p oxinode-sim --target "$host" --all-targets -- -D warnings

step "unit tests and golden images (sim, $host)"
try cargo test -p oxinode-sim --target "$host"

step "unit tests (host tooling)"
try python3 -m unittest discover -s tools -p 'test_*.py'

step "build firmware"
try cargo build --release

# Runs here rather than with the other host tests because it needs a real linked
# ELF to push through the runner.
step "flash runner smoke test"
try python3 tools/test_dfu_flash.py

step "image layout vs memory.x"
for bin in blink usb-cdc radio display; do
    elf="target/thumbv7em-none-eabihf/release/$bin"
    [[ -f "$elf" ]] || { echo "missing $elf"; fail=1; continue; }
    rust-objcopy -O binary "$elf" "$elf.bin"
    tools/uf2conv.py "$elf.bin" -o "$elf.uf2" \
        -b "$(tools/layout.py memory.x --field FLASH.origin)" 2>/dev/null
    try tools/check_layout.py "$elf" --uf2 "$elf.uf2"
done

# Last, and named binaries only, so the images checked above are not quietly
# replaced by BLE-flavoured rebuilds of themselves. `rnode` -- the product
# image -- lives here: it carries the Bluetooth stack and so needs this
# feature set.
step "build firmware (ble: rnode, ble)"
try cargo build --release --no-default-features --features ble --bin rnode --bin ble

step "ble image layouts vs memory.x"
for bin in rnode ble; do
    elf="target/thumbv7em-none-eabihf/release/$bin"
    [[ -f "$elf" ]] || { echo "missing $elf"; fail=1; continue; }
    rust-objcopy -O binary "$elf" "$elf.bin"
    tools/uf2conv.py "$elf.bin" -o "$elf.uf2" \
        -b "$(tools/layout.py memory.x --field FLASH.origin)" 2>/dev/null
    try tools/check_layout.py "$elf" --uf2 "$elf.uf2"
done

# The same image with the stack in it, in a target directory of its own so
# the `rnode` above -- the RNode, and what a developer flashes -- is not
# replaced by it. Held to ceilings rather than to the chip: the stack, its
# allocator, the 64 KB heap and the board's identity measured 541,720 bytes
# of flash and 120,828 of RAM on 2026-10-23, and the ceilings leave room for
# what the messaging work adds next (announcing, peers, the LXMF subset)
# without leaving room for a dependency bump to eat the headroom unnoticed. Raising one is a
# deliberate change, with the reason, in the change that needs it. See
# docs/hardware/memory-layout.md.
NODE_MAX_FLASH=589824   # 576 KiB of the 784 KiB the bootloader leaves
NODE_MAX_RAM=131072     # 128 KiB of 256 KiB; the stack has the rest
step "build firmware (ble + node: rnode)"
try cargo build --release --no-default-features --features ble,node --bin rnode \
    --target-dir target/node

step "node image layout vs memory.x and its ceilings"
elf="target/node/thumbv7em-none-eabihf/release/rnode"
if [[ -f "$elf" ]]; then
    rust-objcopy -O binary "$elf" "$elf.bin"
    tools/uf2conv.py "$elf.bin" -o "$elf.uf2" \
        -b "$(tools/layout.py memory.x --field FLASH.origin)" 2>/dev/null
    try tools/check_layout.py "$elf" --uf2 "$elf.uf2" \
        --max-flash "$NODE_MAX_FLASH" --max-ram "$NODE_MAX_RAM"
else
    echo "missing $elf"
    fail=1
fi

if (( fail )); then
    printf '\n\033[31msome checks failed\033[0m\n'
    exit 1
fi
printf '\n\033[32mall checks passed\033[0m\n'
