#!/usr/bin/env python3
"""Check a built firmware image against `memory.x` before it reaches the board.

There is no debug probe on this hardware, so a mislinked image is not a stack
trace -- it is a board that does nothing and a person wondering whether the
radio is dead. Everything here is decidable from the ELF and the UF2 alone:

  * the image is 32-bit little-endian ARM,
  * every loadable byte lands inside FLASH,
  * RAM usage fits in RAM,
  * the vector table's initial stack pointer points into RAM and its reset
    vector points into FLASH with the Thumb bit set -- the two words the CPU
    reads first, and the ones that are wrong when a linker script is wrong,
  * the UF2 carries the same bytes, at the same addresses, in the right family.

The parsing helpers are pure functions over bytes so `test_check_layout.py` can
feed them deliberately broken images and confirm this script actually fails.
"""

import argparse
import os
import struct
import sys
from typing import Dict, List, NamedTuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import layout  # noqa: E402
import uf2conv  # noqa: E402


class LayoutError(Exception):
    """The input is not something we can meaningfully check."""


# --------------------------------------------------------------------------- ELF

EM_ARM = 40
PT_LOAD = 1


class Segment(NamedTuple):
    offset: int
    vaddr: int
    paddr: int
    filesz: int
    memsz: int


class Elf(NamedTuple):
    entry: int
    segments: List[Segment]
    data: bytes

    def byte_at(self, addr: int, count: int) -> bytes:
        """Read `count` bytes from the load address `addr`."""
        for s in self.segments:
            if s.paddr <= addr and addr + count <= s.paddr + s.filesz:
                start = s.offset + (addr - s.paddr)
                return self.data[start : start + count]
        raise LayoutError(f"no loadable data at {addr:#x}")


def parse_elf(data: bytes) -> Elf:
    """Minimal ELF32 reader: just the program headers.

    Program headers are the right level of detail here -- they describe what the
    loader (in our case, the UF2 bootloader) actually writes to flash, which is
    the thing being checked. Section headers would include debug info that never
    reaches the device.
    """
    if len(data) < 52 or data[:4] != b"\x7fELF":
        raise LayoutError("not an ELF file")
    if data[4] != 1:
        raise LayoutError("not a 32-bit ELF")
    if data[5] != 1:
        raise LayoutError("not a little-endian ELF")

    (e_machine,) = struct.unpack_from("<H", data, 18)
    if e_machine != EM_ARM:
        raise LayoutError(f"not an ARM ELF (e_machine={e_machine})")

    e_entry, e_phoff = struct.unpack_from("<II", data, 24)
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 42)
    if e_phentsize < 32:
        raise LayoutError(f"program header entries too small ({e_phentsize})")

    # lld emits a LOAD segment covering the ELF header and the program header
    # table itself. It is file bookkeeping, not firmware -- objcopy drops it, and
    # counting it would inflate the flash figure and corrupt the image we
    # reconstruct to compare against the UF2.
    headers_end = e_phoff + e_phnum * e_phentsize

    segments = []
    for i in range(e_phnum):
        base = e_phoff + i * e_phentsize
        if base + 32 > len(data):
            raise LayoutError("truncated program header table")
        p_type, p_offset, p_vaddr, p_paddr, p_filesz, p_memsz = struct.unpack_from(
            "<IIIIII", data, base
        )
        if p_type != PT_LOAD or p_memsz == 0:
            continue
        if p_offset < headers_end:
            continue
        segments.append(Segment(p_offset, p_vaddr, p_paddr, p_filesz, p_memsz))

    if not segments:
        raise LayoutError("no loadable segments")
    return Elf(e_entry, segments, data)


# --------------------------------------------------------------------------- UF2


class Uf2Block(NamedTuple):
    flags: int
    address: int
    payload: bytes
    block_no: int
    num_blocks: int
    family: int


def parse_uf2(data: bytes) -> List[Uf2Block]:
    """Read a UF2 file back into blocks, validating framing as it goes."""
    if len(data) == 0 or len(data) % uf2conv.BLOCK_SIZE != 0:
        raise LayoutError(f"UF2 length {len(data)} is not a multiple of 512")

    blocks = []
    for i in range(len(data) // uf2conv.BLOCK_SIZE):
        raw = data[i * uf2conv.BLOCK_SIZE : (i + 1) * uf2conv.BLOCK_SIZE]
        (m0, m1, flags, addr, size, block_no, num_blocks, family) = struct.unpack_from(
            "<IIIIIIII", raw, 0
        )
        if m0 != uf2conv.MAGIC_START0 or m1 != uf2conv.MAGIC_START1:
            raise LayoutError(f"block {i}: bad start magic")
        (end,) = struct.unpack_from("<I", raw, uf2conv.BLOCK_SIZE - 4)
        if end != uf2conv.MAGIC_END:
            raise LayoutError(f"block {i}: bad end magic")
        if size > uf2conv.BLOCK_SIZE - uf2conv.HEADER_SIZE - uf2conv.FOOTER_SIZE:
            raise LayoutError(f"block {i}: payload size {size} too large")
        blocks.append(
            Uf2Block(flags, addr, raw[32 : 32 + size], block_no, num_blocks, family)
        )
    return blocks


# ------------------------------------------------------------------------ checks


def check_elf(elf: Elf, regions: Dict[str, layout.Region]) -> List[str]:
    """Return a list of problems; empty means the image looks loadable."""
    problems = []
    flash = regions.get("FLASH")
    ram = regions.get("RAM")
    if flash is None or ram is None:
        return ["memory.x is missing a FLASH or RAM region"]

    for s in elf.segments:
        # Load address: where the bytes are written. `.bss` has no bytes, so it
        # has nothing to say about flash.
        if s.filesz > 0 and not flash.contains_range(s.paddr, s.filesz):
            problems.append(
                f"segment loads at {s.paddr:#x}+{s.filesz:#x}, outside FLASH "
                f"({flash.origin:#x}..{flash.end:#x})"
            )
        # Runtime address: where the CPU sees it. Either it lives in RAM (`.bss`,
        # and `.data` after startup copies it there) or it is executed in place
        # out of flash. Anywhere else is a broken linker script.
        if s.memsz > 0 and not (
            ram.contains_range(s.vaddr, s.memsz) or flash.contains_range(s.vaddr, s.memsz)
        ):
            problems.append(
                f"segment runs at {s.vaddr:#x}+{s.memsz:#x}, in neither FLASH "
                f"({flash.origin:#x}..{flash.end:#x}) nor RAM "
                f"({ram.origin:#x}..{ram.end:#x})"
            )

    if not flash.contains_range(elf.entry & ~1, 0):
        problems.append(f"entry point {elf.entry:#x} is not in FLASH")
    if elf.entry & 1 == 0:
        problems.append(f"entry point {elf.entry:#x} has no Thumb bit; the CPU will fault")

    # The first two words of the vector table are what the CPU loads out of reset.
    try:
        initial_sp, reset_vector = struct.unpack("<II", elf.byte_at(flash.origin, 8))
    except LayoutError as e:
        problems.append(f"cannot read the vector table: {e}")
        return problems

    # The stack starts at the top of RAM and grows down, so RAM end is a legal
    # value even though it is one past the last usable byte.
    if not (ram.origin < initial_sp <= ram.end):
        problems.append(
            f"initial stack pointer {initial_sp:#x} is not in RAM "
            f"({ram.origin:#x}..{ram.end:#x})"
        )
    if not flash.contains_range(reset_vector & ~1, 0):
        problems.append(f"reset vector {reset_vector:#x} is not in FLASH")
    if reset_vector & 1 == 0:
        problems.append(f"reset vector {reset_vector:#x} has no Thumb bit")

    return problems


def check_uf2(
    blocks: List[Uf2Block],
    regions: Dict[str, layout.Region],
    expected: bytes,
    base: int,
    family: int = uf2conv.DEFAULT_FAMILY,
) -> List[str]:
    """Confirm the UF2 would write exactly `expected` starting at `base`.

    `family` must be what the *target bootloader* expects, not what the registry
    says the chip is. A mismatch is skipped silently by the bootloader, so this
    is the only place it can be caught.
    """
    problems = []
    flash = regions.get("FLASH")
    if flash is None:
        return ["memory.x is missing a FLASH region"]

    total = len(blocks)
    rebuilt = bytearray()
    for i, b in enumerate(blocks):
        if b.num_blocks != total:
            problems.append(f"block {i}: claims {b.num_blocks} blocks, file has {total}")
        if b.block_no != i:
            problems.append(f"block {i}: numbered {b.block_no}")
        if b.family != family:
            problems.append(f"block {i}: family {b.family:#x}, expected {family:#x}")
        if not b.flags & uf2conv.FLAG_FAMILY_ID_PRESENT:
            problems.append(f"block {i}: family-id flag not set, so family is ignored")
        expected_addr = base + i * uf2conv.PAYLOAD_SIZE
        if b.address != expected_addr:
            problems.append(f"block {i}: address {b.address:#x}, expected {expected_addr:#x}")
        if not flash.contains_range(b.address, len(b.payload)):
            problems.append(f"block {i}: writes outside FLASH at {b.address:#x}")
        rebuilt += b.payload

    if bytes(rebuilt) != expected:
        problems.append(
            f"UF2 payload does not match the binary "
            f"({len(rebuilt)} bytes vs {len(expected)})"
        )
    return problems


# -------------------------------------------------------------------------- main


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("elf", help="the linked firmware ELF")
    ap.add_argument("--memory-x", default="memory.x", help="linker memory map")
    ap.add_argument("--uf2", help="also check a generated UF2 against the ELF")
    ap.add_argument(
        "--family",
        default=hex(uf2conv.DEFAULT_FAMILY),
        help="family id the target bootloader expects (default: %(default)s). "
        "Read the truth off the board with "
        "`uf2conv.py --print-family /Volumes/<drive>/CURRENT.UF2`.",
    )
    ap.add_argument("-q", "--quiet", action="store_true", help="only report problems")
    args = ap.parse_args()

    regions = layout.load(args.memory_x)
    with open(args.elf, "rb") as f:
        elf = parse_elf(f.read())

    problems = check_elf(elf, regions)

    if args.uf2:
        flash = regions["FLASH"]
        with open(args.uf2, "rb") as f:
            blocks = parse_uf2(f.read())
        # Reconstruct what objcopy would have produced, from the ELF itself, so
        # the UF2 is compared against the image rather than against another file
        # that might be stale.
        image = bytearray()
        for s in sorted(elf.segments, key=lambda s: s.paddr):
            # `.bss` and friends occupy no file space, so they contribute nothing
            # to a raw binary -- and their load address is a RAM address, which
            # would produce a nonsensical offset here.
            if s.filesz == 0:
                continue
            offset = s.paddr - flash.origin
            if offset < 0:
                continue
            if len(image) < offset:
                image += b"\x00" * (offset - len(image))
            image[offset : offset + s.filesz] = elf.data[s.offset : s.offset + s.filesz]
        problems += check_uf2(
            blocks, regions, bytes(image), flash.origin, int(args.family, 0)
        )

    if not args.quiet:
        flash, ram = regions["FLASH"], regions["RAM"]
        flash_used = sum(s.filesz for s in elf.segments)
        ram_used = sum(
            s.memsz for s in elf.segments if ram.contains_range(s.vaddr, s.memsz)
        )
        print(f"{args.elf}")
        print(f"  entry      {elf.entry:#010x}")
        print(
            f"  flash      {flash_used:>7} / {flash.length} bytes"
            f"  ({100 * flash_used / flash.length:.1f}%) at {flash.origin:#x}"
        )
        print(
            f"  ram        {ram_used:>7} / {ram.length} bytes"
            f"  ({100 * ram_used / ram.length:.1f}%)"
        )
        if args.uf2:
            print(f"  uf2        {len(blocks)} blocks, family {int(args.family, 0):#x}")

    if problems:
        print(f"\ncheck_layout: {len(problems)} problem(s):", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1

    if not args.quiet:
        print("  ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
