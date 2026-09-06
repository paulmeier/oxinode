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
> **This firmware does not do anything useful yet.** oxinode is an in-progress
> RNode-compatible LoRa modem for the muzi.works Base Duo. There is no radio
> support, no RNode/KISS protocol, and no display. See the README for which
> phase this is. Do not expect Reticulum to talk to it.

## Files

| File | What it does |
|---|---|
| \`oxinode-blink-$version.uf2\` | Blinks the green LED. Proves flashing works. |
| \`oxinode-usb-cdc-$version.uf2\` | Enumerates as a USB serial port and echoes. |

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
