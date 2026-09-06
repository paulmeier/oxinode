#!/usr/bin/env bash
# Cargo runner: flash over the bootloader's serial DFU interface.
#
# This is the primary path, not the UF2 drive. The Adafruit bootloader exposes
# both, but on macOS 26 the mass-storage route does not work: FAT volumes are now
# served by FSKit in userspace, and the bootloader's drive is not a real FAT
# volume -- it is a synthetic shim that only understands UF2 blocks. Writes land
# in a host-side cache and never reach the chip. The copy succeeds, the board
# keeps running its old firmware, and nothing anywhere reports an error.
#
# Serial DFU has none of that in the path, and it fails loudly when it fails.
# tools/uf2-flash.sh is kept for hosts where the drive does work.
#
# Environment overrides, mostly so tools/test_dfu_flash.py can exercise this
# without a board attached:
#   OXINODE_DFU_PORT   use this serial port instead of searching
#   OXINODE_DFU_IN_DFU 1/0 to assert bootloader state instead of detecting it
set -euo pipefail

elf="${1:?usage: dfu-flash.sh <elf>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"

# The Adafruit bootloader speaks Nordic's older SLIP-framed DFU, not Secure DFU,
# which is why it needs Adafruit's own tool rather than nrfutil or nrfdfu.
nrfutil="$(command -v adafruit-nrfutil || true)"
if [[ -z "$nrfutil" ]]; then
    venv="$here/.venv"
    if [[ ! -x "$venv/bin/adafruit-nrfutil" ]]; then
        echo "dfu-flash: adafruit-nrfutil not found; setting up $venv" >&2
        python3 -m venv "$venv" >&2
        # A fresh venv's bundled pip can be old enough to fall back to the legacy
        # setup.py install path for this package. Upgrade first.
        "$venv/bin/pip" install --quiet --upgrade pip wheel >&2
        "$venv/bin/pip" install --quiet adafruit-nrfutil >&2
    fi
    nrfutil="$venv/bin/adafruit-nrfutil"
fi

port="${OXINODE_DFU_PORT:-}"
if [[ -z "$port" ]]; then
    for p in /dev/cu.usbmodem*; do
        if [[ -e "$p" ]]; then port="$p"; break; fi
    done
fi
if [[ -z "$port" ]]; then
    cat >&2 <<'MSG'
dfu-flash: no serial port found.

  If the board is running an oxinode image with USB serial, it should appear as
  /dev/cu.usbmodem*. Otherwise double-tap reset to enter the bootloader, which
  exposes one too.
MSG
    exit 1
fi

# Is the board already in its bootloader? If so, do not ask the tool to perform
# a 1200-baud touch: there is no application to reset, and it would sit waiting
# for a re-enumeration that never comes.
in_dfu="${OXINODE_DFU_IN_DFU:-}"
if [[ -z "$in_dfu" ]]; then
    in_dfu=0
    shopt -s nullglob
    for d in /Volumes/* /media/"${USER:-}"/* /run/media/"${USER:-}"/*; do
        if [[ -f "$d/INFO_UF2.TXT" ]]; then in_dfu=1; fi
    done
    shopt -u nullglob
fi

hex="${elf}.hex"
pkg="${elf}.zip"
uf2="${elf}.uf2"
bin="${elf}.bin"

base="$("$here/layout.py" "$root/memory.x" --field FLASH.origin)"

rust-objcopy -O ihex "$elf" "$hex"
rust-objcopy -O binary "$elf" "$bin"

# Sanity-check the image against memory.x before it goes near the board. The UF2
# is a by-product here, built only so the existing checker has something to
# validate; DFU sends the package, not the UF2.
"$here/uf2conv.py" "$bin" -o "$uf2" -b "$base" >/dev/null
"$here/check_layout.py" "$elf" --uf2 "$uf2" --memory-x "$root/memory.x" --quiet

rm -f "$pkg"
"$nrfutil" dfu genpkg --dev-type 0x0052 --application "$hex" "$pkg" >/dev/null

# Assemble the whole argument list in one array rather than keeping a separate
# array of extras: macOS ships bash 3.2, where expanding an *empty* array trips
# `set -u`. Built here, after $pkg exists -- for the same reason.
dfu_args=(--verbose dfu serial -pkg "$pkg" -p "$port" -b 115200 --singlebank)
if [[ "$in_dfu" -eq 0 ]]; then
    # A running oxinode image reboots into the bootloader on a 1200-baud open,
    # so flashing needs no button press. See src/bin/usb_cdc.rs.
    dfu_args+=(--touch 1200)
fi

size="$(wc -c < "$bin" | tr -d ' ')"
echo "dfu-flash: $port  ($size bytes at $base)"
"$nrfutil" "${dfu_args[@]}"
