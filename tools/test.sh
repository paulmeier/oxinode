#!/usr/bin/env bash
# Everything that can be checked without a board attached.
#
# Split three ways, because the pieces cannot share a build target:
#   * oxinode-core, the panel simulator and the Python tools run on the host,
#     where a test harness exists;
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

step "lint (core, $host)"
try cargo clippy -p oxinode-core --target "$host" --all-targets -- -D warnings

step "unit tests (core, $host)"
try cargo test -p oxinode-core --target "$host"

# The simulator is a second host crate; it renders the interface and compares
# every screen and menu against the golden images in sim/golden. A mismatch
# leaves a diff image in target/golden-diff/ and fails here.
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
# image -- lives here since phase 8: it carries the Bluetooth stack and so
# needs this feature set.
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

if (( fail )); then
    printf '\n\033[31msome checks failed\033[0m\n'
    exit 1
fi
printf '\n\033[32mall checks passed\033[0m\n'
