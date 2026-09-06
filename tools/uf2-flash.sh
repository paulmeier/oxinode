#!/usr/bin/env bash
# Cargo runner: ELF -> raw binary -> UF2 -> the board's bootloader drive.
#
# Invoked as `tools/uf2-flash.sh <path-to-elf>` by `cargo run`; see
# .cargo/config.toml. Put the board in bootloader mode first (double-tap reset,
# or let a running oxinode image do it for you).
set -euo pipefail

elf="${1:?usage: uf2-flash.sh <elf>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"

# Find the bootloader drive before building anything, because the board is the
# authority on the UF2 family id and we would otherwise have to guess. INFO_UF2.TXT
# is the marker the format requires, so probe for that rather than a volume name.
shopt -s nullglob
drives=()
case "$(uname -s)" in
    Darwin) roots=(/Volumes/*) ;;
    Linux)  roots=(/media/"$USER"/* /run/media/"$USER"/* /mnt/*) ;;
    *)      roots=() ;;
esac
for d in "${roots[@]}"; do
    [[ -f "$d/INFO_UF2.TXT" ]] && drives+=("$d")
done

if [[ ${#drives[@]} -gt 1 ]]; then
    echo "uf2-flash: more than one UF2 drive mounted; refusing to guess:" >&2
    printf '    %s\n' "${drives[@]}" >&2
    exit 1
fi

drive="${drives[0]:-}"

# Single source of truth for the load address is memory.x; don't repeat it here.
base="$("$here/layout.py" "$root/memory.x" --field FLASH.origin)"

# The family id is the board's business, not ours. A bootloader silently skips
# every block whose family it does not recognise: the copy appears to succeed,
# nothing is written, and the board never reboots. The bootloader publishes its
# own id in CURRENT.UF2, so read it from there whenever a board is attached.
family=""
if [[ -n "$drive" && -f "$drive/CURRENT.UF2" ]]; then
    family="$("$here/uf2conv.py" --print-family "$drive/CURRENT.UF2" 2>/dev/null || true)"
fi
if [[ -z "$family" ]]; then
    family="$("$here/uf2conv.py" --print-default-family)"
    echo "uf2-flash: no board to ask; assuming family $family" >&2
fi

bin="${elf}.bin"
uf2="${elf}.uf2"

rust-objcopy -O binary "$elf" "$bin"
"$here/uf2conv.py" "$bin" -o "$uf2" -b "$base" -f "$family"

# Cheap insurance against flashing something that cannot possibly boot: verify
# the image against memory.x before it goes anywhere near the board.
"$here/check_layout.py" "$elf" --uf2 "$uf2" --memory-x "$root/memory.x" \
    --family "$family" --quiet

if [[ -z "$drive" ]]; then
    cat >&2 <<'MSG'

uf2-flash: no UF2 bootloader drive found.

  Double-tap the board's reset button; a USB drive containing INFO_UF2.TXT
  should appear within a second or two. Then re-run.

  The image is built and converted either way:
MSG
    echo "    $uf2" >&2
    exit 1
fi

echo "uf2-flash: $(grep -i '^Model:' "$drive/INFO_UF2.TXT" || echo "$drive")"
echo "uf2-flash: family $family, load address $base"

# macOS writes an AppleDouble sidecar (`._name`) alongside any file copied to a
# FAT volume, and tries to attach extended attributes. The bootloader's
# filesystem is a synthetic shim that only understands UF2 blocks, so that extra
# traffic is at best noise and at worst wedges the transfer. COPYFILE_DISABLE
# and -X suppress both.
#
# The bootloader reboots into the new image as soon as the last block lands, so
# the copy is expected to end with an I/O error on some hosts. Tolerate that.
copy=(cp)
[[ "$(uname -s)" == "Darwin" ]] && { copy=(cp -X); export COPYFILE_DISABLE=1; }
if ! "${copy[@]}" "$uf2" "$drive/"; then
    echo "uf2-flash: copy reported an error (usually just the board rebooting)" >&2
fi
sync 2>/dev/null || true

# A bootloader that accepted the image reboots into it, and the drive goes away.
# A drive that is still mounted several seconds later means the blocks were
# rejected -- almost always a family id mismatch, which produces no error
# anywhere else.
for _ in $(seq 1 10); do
    [[ -d "$drive" ]] || { echo "uf2-flash: board rebooted into the new image"; exit 0; }
    sleep 1
done

cat >&2 <<MSG

uf2-flash: the board did NOT reboot, so the bootloader rejected the image.

  $drive is still mounted. The bootloader accepts the file, skips every block
  it does not like, and reports nothing. The usual cause is a family id
  mismatch; this run used $family.

  What the board says it wants:
    $here/uf2conv.py --print-family "$drive/CURRENT.UF2"

  What is actually on the chip right now:
    $here/verify_flash.py "$elf"
MSG
exit 1
