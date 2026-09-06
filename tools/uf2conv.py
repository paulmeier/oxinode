#!/usr/bin/env python3
"""Convert a raw binary into a UF2 file.

Implemented from the UF2 format specification
(https://github.com/microsoft/uf2/blob/master/README.md), not adapted from any
existing converter. The format is a stream of fixed 512-byte blocks, each
carrying up to 476 bytes of payload plus a 32-byte header and a trailing magic
word; Adafruit's bootloader expects 256-byte payloads, so that is what we emit.
"""

import argparse
import struct
import sys

BLOCK_SIZE = 512
PAYLOAD_SIZE = 256
HEADER_SIZE = 32
FOOTER_SIZE = 4

MAGIC_START0 = 0x0A324655  # "UF2\n"
MAGIC_START1 = 0x9E5D5157
MAGIC_END = 0x0AB16F30

FLAG_FAMILY_ID_PRESENT = 0x00002000

# The family id in the registry for a Nordic nRF52840. NOT what the Base Duo's
# bootloader uses -- kept only so the difference is documented rather than
# rediscovered.
FAMILY_NRF52840 = 0xADA52840

# What the muzi Base Duo's bootloader actually expects, read from the family id
# in its own CURRENT.UF2. It is not in microsoft/uf2's registry at all; 0x239A is
# Adafruit's USB vendor id, so this looks like a vendor-assigned value baked into
# this bootloader build.
#
# This matters more than it sounds. A block whose family id does not match is
# silently skipped: the bootloader accepts the file, writes nothing, never
# reaches its expected block count, and so never reboots. There is no error
# anywhere. Prefer `family_of()` on the board's own CURRENT.UF2 over trusting
# this constant.
FAMILY_MUZI_BASE = 0x239A0081

DEFAULT_FAMILY = FAMILY_MUZI_BASE


def family_of(data: bytes) -> int:
    """Read the family id out of a UF2 file's first block.

    Only the first 32 bytes are needed, so a caller reading from a mounted
    bootloader drive does not have to pull the whole flash dump over USB.
    """
    if len(data) < HEADER_SIZE:
        raise ValueError("not enough data for a UF2 header")
    magic0, magic1, flags, _addr, _size, _no, _total, family = struct.unpack_from(
        "<IIIIIIII", data, 0
    )
    if magic0 != MAGIC_START0 or magic1 != MAGIC_START1:
        raise ValueError("not a UF2 file")
    if not flags & FLAG_FAMILY_ID_PRESENT:
        raise ValueError("UF2 block carries no family id")
    return family


def to_uf2(data: bytes, base_address: int, family_id: int) -> bytes:
    num_blocks = (len(data) + PAYLOAD_SIZE - 1) // PAYLOAD_SIZE
    if num_blocks == 0:
        raise ValueError("input is empty")

    out = bytearray()
    for block_no in range(num_blocks):
        chunk = data[block_no * PAYLOAD_SIZE : (block_no + 1) * PAYLOAD_SIZE]
        header = struct.pack(
            "<IIIIIIII",
            MAGIC_START0,
            MAGIC_START1,
            FLAG_FAMILY_ID_PRESENT,
            base_address + block_no * PAYLOAD_SIZE,
            len(chunk),
            block_no,
            num_blocks,
            family_id,
        )
        assert len(header) == HEADER_SIZE
        block = header + chunk.ljust(BLOCK_SIZE - HEADER_SIZE - FOOTER_SIZE, b"\x00")
        block += struct.pack("<I", MAGIC_END)
        assert len(block) == BLOCK_SIZE
        out += block
    return bytes(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("input", nargs="?", help="raw binary (objcopy -O binary)")
    ap.add_argument("-o", "--output", help="UF2 file to write")
    ap.add_argument(
        "-b",
        "--base",
        default="0x26000",
        help="flash address the binary is linked at (default: %(default)s)",
    )
    ap.add_argument(
        "-f",
        "--family",
        default=hex(DEFAULT_FAMILY),
        help="UF2 family id (default: %(default)s)",
    )
    ap.add_argument(
        "--print-default-family",
        action="store_true",
        help="print the compiled-in default family id and exit",
    )
    ap.add_argument(
        "--print-family",
        metavar="UF2",
        help="print the family id of an existing UF2 file and exit; point this "
        "at a bootloader drive's CURRENT.UF2 to learn what the board expects",
    )
    args = ap.parse_args()

    if args.print_default_family:
        print(hex(DEFAULT_FAMILY))
        return 0

    if args.print_family:
        with open(args.print_family, "rb") as f:
            print(hex(family_of(f.read(HEADER_SIZE))))
        return 0

    with open(args.input, "rb") as f:
        data = f.read()

    if not args.input or not args.output:
        ap.error("input and --output are required unless --print-family is used")

    uf2 = to_uf2(data, int(args.base, 0), int(args.family, 0))
    with open(args.output, "wb") as f:
        f.write(uf2)

    print(
        f"{args.input}: {len(data)} bytes at {int(args.base, 0):#x}"
        f" -> {args.output}: {len(uf2) // BLOCK_SIZE} UF2 blocks",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
