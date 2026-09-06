#!/usr/bin/env bash
# Write the body of a GitHub Release to stdout.
#
# Built by hand rather than with `gh --generate-notes` so that the flashing
# instructions and the status caveat are always present. Given how early this
# project is, a release that looks like a finished product would be misleading.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="${1:?usage: release_notes.sh vX.Y.Z}"
base="$(tools/layout.py memory.x --field FLASH.origin)"

cat <<MSG
> **This firmware is not an RNode yet.** oxinode is an in-progress
> RNode-compatible LoRa modem for the muzi.works Base Duo. The radio works —
> it identifies itself, transmits carriers and sends LoRa packets — but there
> is **no RNode/KISS protocol and no display**, so nothing here answers
> \`rnodeconf\` and Reticulum will not talk to it. See the README for which
> phase this is.

## Files

| File | What it does |
|---|---|
| \`oxinode-blink-$version.uf2\` | Blinks the green LED. Proves flashing works. |
| \`oxinode-usb-cdc-$version.uf2\` | Enumerates as a USB serial port and echoes. |
| \`oxinode-radio-$version.uf2\` | LR1121 bring-up: identifies the radio, starts its oscillator, and offers a console for carriers and test packets. |

\`.elf\` files are the exact binaries the images were built from, kept so a fault
address can be resolved later. \`SHA256SUMS\` covers every asset.

## Flashing

These are built for a **muzi.works Base Duo running the stock Adafruit UF2
bootloader with the S140 SoftDevice**, linked at \`$base\`. They will not run on a
board with a different bootloader layout.

1. Double-tap the reset button. A USB drive appears.
2. Copy the \`.uf2\` onto it. The board reboots into the new firmware.

Recovery is always another double-tap of reset: the bootloader sits above the
application and is never overwritten. Once \`usb-cdc\` is running, opening its
serial port at 1200 baud and closing it puts the board back in the bootloader
without touching the button.

## The radio image

\`radio\` is a diagnostic, not a product. It enumerates one serial port, waits
for a terminal to open it, then walks the whole radio bring-up and says what it
found — SPI pin selection read back from the peripheral, the reset and BUSY
trace, the chip's identity and firmware version, oscillator startup, the RF
switch masks, and the interrupt line.

It then takes single-character commands on the same port:

| Key | What it does |
|---|---|
| \`1\` \`2\` \`3\` | Continuous carrier at −17, 0 and +14 dBm |
| \`p\` \`o\` | Send one LoRa packet, from standby-XOSC or standby-RC |
| \`0\` | Stop transmitting |
| \`t\` \`?\` | Die temperature and supply; chip status |
| \`r\` | Reboot the radio and redo the bring-up |
| \`b\` | Reboot into the bootloader |

**Attach an antenna to the sub-GHz SMA before pressing anything that transmits.**
Transmitting into an open port can damage the power amplifier. Every carrier
stops itself after ten seconds; an unmodulated carrier is a bench diagnostic and
not something FCC Part 15.247 contemplates.

MSG

# Changelog since the previous tag, if there is one. Sorted by date rather than
# by author so it reads as a sequence of changes.
previous="$(git describe --tags --abbrev=0 "${version}^" 2>/dev/null || true)"
if [[ -n "$previous" ]]; then
    echo "## Changes since $previous"
    echo
    git log --no-merges --date-order --pretty='- %s (%h)' "$previous..$version"
else
    echo "## Changes"
    echo
    echo "First tagged release."
fi
