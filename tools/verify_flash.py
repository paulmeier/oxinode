#!/usr/bin/env python3
"""Compare what is actually on the board against an image we built.

The UF2 bootloader publishes the chip's current flash contents as CURRENT.UF2 on
its mass-storage drive. That makes "did the flash actually take?" an answerable
question rather than a guess about LED colours -- which matters a lot here,
because a bootloader silently ignores UF2 blocks it does not like, reports no
error, and leaves the previous firmware running.

    tools/verify_flash.py target/thumbv7em-none-eabihf/release/blink
"""

import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import check_layout  # noqa: E402
import layout  # noqa: E402


def find_drive():
    """Locate a mounted UF2 bootloader volume, or None."""
    roots = []
    if sys.platform == "darwin":
        roots = ["/Volumes"]
    else:
        roots = [f"/media/{os.environ.get('USER', '')}", "/run/media", "/mnt"]
    for root in roots:
        if not os.path.isdir(root):
            continue
        for entry in sorted(os.listdir(root)):
            path = os.path.join(root, entry)
            if os.path.isfile(os.path.join(path, "INFO_UF2.TXT")):
                return path
    return None


def flash_image(uf2_bytes, origin, length):
    """Extract a contiguous region of flash from a CURRENT.UF2 dump."""
    blocks = check_layout.parse_uf2(uf2_bytes)
    by_addr = {b.address: b.payload for b in blocks}
    out = bytearray()
    addr = origin
    while addr < origin + length:
        base = addr - (addr % check_layout.uf2conv.PAYLOAD_SIZE)
        chunk = by_addr.get(base)
        if chunk is None:
            break
        off = addr - base
        out += chunk[off:]
        addr = base + len(chunk)
    return bytes(out[:length])


def describe_vector_table(image, origin):
    if len(image) < 8:
        return "  <no vector table>"
    sp, reset = struct.unpack("<II", image[:8])
    return f"  initial SP {sp:#010x}, reset vector {reset:#010x} (image at {origin:#x})"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("elf", help="the ELF whose image should be on the board")
    ap.add_argument("--memory-x", default="memory.x")
    ap.add_argument("--drive", help="bootloader volume (default: autodetect)")
    args = ap.parse_args()

    drive = args.drive or find_drive()
    if not drive:
        print("verify-flash: no UF2 bootloader drive found.", file=sys.stderr)
        print("  Double-tap reset to bring the board back up in DFU mode.", file=sys.stderr)
        return 2

    current = os.path.join(drive, "CURRENT.UF2")
    if not os.path.isfile(current):
        print(f"verify-flash: {drive} has no CURRENT.UF2", file=sys.stderr)
        return 2

    flash = layout.load(args.memory_x)["FLASH"]

    with open(args.elf, "rb") as f:
        elf = check_layout.parse_elf(f.read())
    expected = bytearray()
    for s in sorted(elf.segments, key=lambda s: s.paddr):
        if s.filesz == 0:
            continue
        off = s.paddr - flash.origin
        if off < 0:
            continue
        if len(expected) < off:
            expected += b"\x00" * (off - len(expected))
        expected[off : off + s.filesz] = elf.data[s.offset : s.offset + s.filesz]
    expected = bytes(expected)

    with open(current, "rb") as f:
        on_board = flash_image(f.read(), flash.origin, len(expected))

    print(f"board  : {drive}")
    print(f"image  : {args.elf} ({len(expected)} bytes)")
    print()
    print("on the board:")
    print(describe_vector_table(on_board, flash.origin))
    print("what we built:")
    print(describe_vector_table(expected, flash.origin))
    print()

    if on_board == expected:
        print("MATCH - the board is running this image")
        return 0

    if all(b == 0xFF for b in on_board[:256]):
        print("MISMATCH - the application region is erased; nothing is installed")
    else:
        first = next(
            (i for i, (a, b) in enumerate(zip(on_board, expected)) if a != b), None
        )
        print(f"MISMATCH - flash holds different firmware (first difference at +{first:#x})")
    print("\nThe bootloader accepted the file and wrote nothing. Check the family id:")
    print(f"  tools/uf2conv.py --print-family {current}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
