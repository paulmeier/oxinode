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
#   OXINODE_DFU_IOREG      a command to run instead of `ioreg -r -c
#                          IOUSBHostDevice -l`, which is how the board behind
#                          the port, and whether it is in its bootloader, is told
#   OXINODE_PYTHON         try this interpreter first for reopening the port
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

# A Python that can `import serial`, for clear_cached_baud below.
#
# This used to be whatever `python3` was, with its errors discarded. On a Mac
# where `python3` is a conda or Homebrew interpreter without pyserial, the
# ImportError went to /dev/null, the cached 1200 baud was never cleared, and
# the next thing to open the port -- macOS probing the re-enumerated device
# included -- touched the board back into its bootloader. Nothing said why.
#
# adafruit-nrfutil depends on pyserial, so the interpreter on its own `#!`
# line is a good bet; the rest are the usual suspects.
pythons=()
if [[ -n "${OXINODE_PYTHON:-}" ]]; then pythons+=("$OXINODE_PYTHON"); fi
shebang="$(head -n 1 "$nrfutil" 2>/dev/null || true)"
if [[ "$shebang" == "#!"* ]]; then
    # `#!/path/python3.11` or `#!/usr/bin/env python3`: the last word.
    interp="${shebang#\#!}"
    interp="${interp##* }"
    case "${interp##*/}" in python*) pythons+=("$interp") ;; esac
fi
pythons+=(python3.11 python3)
python=""
for c in "${pythons[@]}"; do
    if command -v "$c" >/dev/null 2>&1 && "$c" -c 'import serial' 2>/dev/null; then
        python="$c"
        break
    fi
done

warn_no_python() {
    cat >&2 <<MSG

dfu-flash: WARNING: no Python with pyserial found (tried: ${pythons[*]}).
  The 1200 baud of the touch stays cached on the host for $port, and the
  next thing to open it -- including macOS probing the board after it
  re-enumerates -- will reboot it back into its bootloader. Install pyserial
  for one of those, or point OXINODE_PYTHON at an interpreter that has it.

MSG
}

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
#
# *This* board, that is. The question used to be "is any bootloader drive
# mounted", and with two boards attached and the other one in its bootloader,
# the answer was yes for the wrong board: the touch was skipped and the flash
# failed against a running application. So the board is told apart by its USB
# serial number, which the bootloader shares with the application -- it
# enumerates as VID 0x239A PID 0x0081 ("muzi Base") under the same serial.

# One line per USB device: "<serial> <vid> <pid> <callout>...", with VID and
# PID in decimal, as ioreg prints them. A callout belongs to the nearest
# device above it, which is right behind a hub too: a device's own properties
# come before its children.
usb_devices() {
    if [[ -n "${OXINODE_DFU_IOREG:-}" ]]; then
        "$OXINODE_DFU_IOREG"
    else
        ioreg -r -c IOUSBHostDevice -l
    fi 2>/dev/null | awk '
        function flush() { if (have) print serial, vid, pid callouts; have = 0 }
        /\+-o .*<class IOUSBHostDevice/ {
            flush(); have = 1; serial = "-"; vid = "-"; pid = "-"; callouts = ""; next
        }
        !have { next }
        serial == "-" && /"USB Serial Number" = "/ { split($0, a, "\""); serial = a[4] }
        vid == "-" && /"idVendor" = [0-9]/ { vid = $NF }
        pid == "-" && /"idProduct" = [0-9]/ { pid = $NF }
        /"IOCalloutDevice" = "/ { split($0, a, "\""); callouts = callouts " " a[4] }
        END { flush() }'
}

can_ask_usb() { [[ -n "${OXINODE_DFU_IOREG:-}" ]] || command -v ioreg >/dev/null; }

# The serial number behind the port, read once: after a touch the port goes
# away and comes back as the bootloader, and the serial is how it is found.
board_serial=""
if can_ask_usb; then
    # ioreg names the callout (cu.) node; accept the dial-in (tty.) one too.
    callout="${port/\/tty./\/cu.}"
    board_serial="$(usb_devices | awk -v p="$callout" \
        '{ for (i = 4; i <= NF; i++) if ($i == p) { print $1; exit } }')"
fi

bootloader_present() {
    if [[ -n "${OXINODE_DFU_BOOTLOADER:-}" ]]; then
        echo "${OXINODE_DFU_BOOTLOADER}"
        return
    fi
    if can_ask_usb; then
        # 0x239A = 9114, 0x0081 = 129. With no serial there is nothing to
        # match, and the answer is no rather than someone else's bootloader.
        usb_devices | awk -v s="$board_serial" \
            's != "" && $1 == s && $2 == 9114 && $3 == 129 { f = 1 } END { print f + 0 }'
        return
    fi
    # No ioreg: not macOS. Fall back to the drive, which cannot say whose it
    # is -- so with two boards attached, set OXINODE_DFU_IN_DFU yourself.
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
    if [[ -z "$python" ]]; then
        warn_no_python
        return 0
    fi
    # Its errors are shown, not discarded: a port that will not open at
    # 115200 leaves the 1200 in place, and that needs saying.
    if ! "$python" - "$port" <<'EOF'
import sys, time
import serial
s = serial.Serial(sys.argv[1], 115200, timeout=0.1)
time.sleep(0.2)
s.close()
EOF
    then
        echo "dfu-flash: WARNING: could not reopen $port at 115200 with $python;" \
            "the touch's 1200 baud may still be cached for it." >&2
    fi
}

size="$(wc -c < "$bin" | tr -d ' ')"
echo "dfu-flash: $port  ($size bytes at $base)"
if [[ -n "$board_serial" ]]; then
    if [[ "$in_dfu" == 1 ]]; then state="in its bootloader"; else state="running an application"; fi
    echo "dfu-flash: USB serial $board_serial, $state"
fi
# Before flashing as well as after: the warning is the same, and it is more
# use while there is still time to Ctrl-C.
if [[ -z "$python" ]]; then warn_no_python; fi

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
# That state is recoverable and obvious: the bootloader enumerates under the
# board's own USB serial number (and mounts a drive with INFO_UF2.TXT on it,
# which is the fallback where there is no ioreg). So rather than reporting a failure that needs a human to
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
