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
#   OXINODE_DFU_PORT       use this serial port instead of searching
#   OXINODE_DFU_IN_DFU     1/0 to assert bootloader state instead of detecting it
#   OXINODE_DFU_BOOTLOADER 1/0 to answer the bootloader probe directly
#   OXINODE_DFU_RETRY_DELAY seconds to wait between retries (default 2)
#   OXINODE_DFU_SETTLE     seconds to wait after a flash before reopening the
#                          port to reset the host's cached baud rate (default 3)
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

# Choosing a port used to mean "the first one that matches the glob", which was
# fine with one board attached and actively dangerous with two: a second Base
# Duo running someone else's firmware enumerates on the same glob, and flashing
# it would overwrite that firmware. So an ambiguous choice is refused rather
# than guessed.
port="${OXINODE_DFU_PORT:-}"
if [[ -z "$port" ]]; then
    candidates=()
    for p in ${OXINODE_DFU_PORT_GLOB:-/dev/cu.usbmodem*}; do
        [[ -e "$p" ]] && candidates+=("$p")
    done
    # bash 3.2 (which is what macOS ships) treats expanding an empty array as an
    # unbound variable under `set -u`, so the count is checked first.
    if [[ ${#candidates[@]} -eq 0 ]]; then
        echo "dfu-flash: no /dev/cu.usbmodem* port found. Is the board attached?" >&2
        exit 1
    elif [[ ${#candidates[@]} -eq 1 ]]; then
        port="${candidates[0]}"
    else
        echo "dfu-flash: more than one USB serial port is attached:" >&2
        for p in "${candidates[@]}"; do echo "    $p" >&2; done
        echo "Refusing to guess which one to flash -- picking wrong overwrites" >&2
        echo "the other board. Set OXINODE_DFU_PORT to the one you mean." >&2
        exit 1
    fi
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
# Named because the retry below asks the same question again, after the touch
# has had a chance to do its work.
bootloader_present() {
    if [[ -n "${OXINODE_DFU_BOOTLOADER:-}" ]]; then
        echo "${OXINODE_DFU_BOOTLOADER}"
        return
    fi
    local found=0
    shopt -s nullglob
    for d in /Volumes/* /media/"${USER:-}"/* /run/media/"${USER:-}"/*; do
        if [[ -f "$d/INFO_UF2.TXT" ]]; then found=1; fi
    done
    shopt -u nullglob
    echo "$found"
}

in_dfu="${OXINODE_DFU_IN_DFU:-}"
if [[ -z "$in_dfu" ]]; then
    in_dfu="$(bootloader_present)"
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
run_dfu() {
    local args=(--verbose dfu serial -pkg "$pkg" -p "$port" -b 115200 --singlebank)
    if [[ "$1" -eq 0 ]]; then
        # A running oxinode image reboots into the bootloader on a 1200-baud
        # open, so flashing needs no button press. See src/bin/usb_cdc.rs.
        args+=(--touch 1200)
    fi
    # The tool's exit status says nothing. A transfer that times out halfway
    # through -- which leaves the board in its bootloader with the old
    # application already erased -- is caught inside adafruit-nrfutil, printed
    # with a traceback, and turned into a `return False` that its command-line
    # layer ignores: exit 0. Taking that at its word once meant reporting a
    # flash that had not happened and skipping the retry that would have
    # finished it. The one reliable signal is the tool's own last line, so
    # the output is watched for it, and shown as it goes.
    local out
    out="$("$nrfutil" "${args[@]}" 2>&1 | tee /dev/stderr)" || true
    [[ "$out" == *"Device programmed."* ]]
}

# Leave the host's cached line settings at 115200 rather than at 1200.
#
# The 1200-baud touch lives in the *host's* terminal settings for a device
# path, not in anything on the board, and macOS re-applies them the next time
# something opens that path. So after a flash that used a touch, an innocent
# `cat /dev/cu.usbmodem*` opens at 1200 and closes again -- which is a second
# touch, and the freshly flashed image goes straight back to its bootloader.
#
# That cost several rounds of "why is it in DFU again". Opening the port once
# at 115200 and closing it resets the cache, and is harmless: DTR at 115200 is
# what every ordinary terminal does.
clear_cached_baud() {
    local port="$1"
    # The board is re-enumerating, so wait for the node before opening it.
    local waited=0
    while [[ ! -e "$port" && "$waited" -lt 20 ]]; do
        sleep 0.5
        waited=$(( waited + 1 ))
    done
    [[ -e "$port" ]] || return 0
    # An explicit open at 115200 rather than `stty`, because stty applies the
    # cached settings on the way in and the point is to not do that. pyserial
    # sets the rate as part of opening.
    "${OXINODE_PYTHON:-python3}" - "$port" <<'EOF' 2>/dev/null || true
import sys, time
import serial
s = serial.Serial(sys.argv[1], 115200, timeout=0.1)
time.sleep(0.2)
s.close()
EOF
}

size="$(wc -c < "$bin" | tr -d ' ')"
echo "dfu-flash: $port  ($size bytes at $base)"

if run_dfu "$in_dfu"; then
    # The board is re-enumerating; give it a moment before touching the port.
    sleep "${OXINODE_DFU_SETTLE:-3}"
    clear_cached_baud "$port"
    exit 0
fi

# The touch is a race, and losing it used to mean doing this by hand.
#
# `--touch 1200` opens the port at 1200 baud, the running image reboots into
# its bootloader, and the tool then reopens the port -- but the board has to
# re-enumerate first, and on a slow host the tool gets there before the
# bootloader does. It gives up, and leaves the board sitting in DFU with the
# application already gone.
#
# That state is recoverable and obvious: the bootloader mounts a drive with
# INFO_UF2.TXT on it. So rather than reporting a failure that needs a human to
# re-run the same command with OXINODE_DFU_IN_DFU=1, look for the bootloader
# and finish the job. The touch is not repeated, because there is no longer an
# application to touch.
for attempt in 1 2 3; do
    sleep "${OXINODE_DFU_RETRY_DELAY:-2}"
    if [[ "$(bootloader_present)" != "1" ]]; then
        continue
    fi
    echo "dfu-flash: the board is in its bootloader; retrying (attempt $attempt)" >&2
    if run_dfu 1; then
        sleep "${OXINODE_DFU_SETTLE:-3}"
        clear_cached_baud "$port"
        exit 0
    fi
done

# Also on the way out. A *failed* touch leaves the cached rate at 1200 just as
# a successful one does, and then the next thing to open the port -- a log
# reader, or the host probing a freshly attached device -- performs another
# touch. That is how one lost race turns into a board that will not stay out of
# its bootloader.
clear_cached_baud "$port"

cat >&2 <<'MSG'

dfu-flash: could not reach the bootloader.

  If a drive with INFO_UF2.TXT is mounted, the bootloader is running but its
  serial DFU service is not answering. Double-tap the reset button and try
  again -- that is the only recovery, since this board has no DFU button and a
  1200-baud touch needs a port that opens.
MSG
exit 1
