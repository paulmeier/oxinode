#!/usr/bin/env python3
"""Read the memory map out of `memory.x`.

This is the Python half of the same job `oxinode-core::linker_script` does for
the firmware build. Two implementations is not ideal, but the firmware cannot
shell out to Python at build time and the host tools should not have to link
Rust, so both exist and both pin the same expected values against the real
`memory.x` in their tests. If they ever disagree, one of those tests fails.
"""

import argparse
import re
import sys
from typing import Dict, NamedTuple, Optional


class Region(NamedTuple):
    origin: int
    length: int

    @property
    def end(self) -> int:
        return self.origin + self.length

    def contains_range(self, start: int, length: int) -> bool:
        return start >= self.origin and start + length <= self.end


# NAME : ORIGIN = <n>, LENGTH = <n>   -- anchored at the start of a line so that
# the layout diagram in memory.x's comment block, which is full of addresses and
# region names, cannot be mistaken for a declaration.
_REGION_RE = re.compile(
    r"^\s*(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*:\s*"
    r"ORIGIN\s*=\s*(?P<origin>[0-9A-Fa-fxX]+)\s*,\s*"
    r"LENGTH\s*=\s*(?P<length>[0-9A-Fa-fxX]+[KkMm]?)\s*$",
    re.MULTILINE,
)

_NUMBER_RE = re.compile(r"^(?:0[xX](?P<hex>[0-9A-Fa-f]+)|(?P<dec>[0-9]+))(?P<suffix>[KkMm]?)$")


def parse_number(text: str) -> Optional[int]:
    """Parse a linker-script integer: 0x1234, 4096, 824K, 1M."""
    m = _NUMBER_RE.match(text.strip())
    if not m:
        return None
    if m.group("hex") is not None:
        value = int(m.group("hex"), 16)
        # `0x400K` is not something ld accepts; refuse rather than reinterpret.
        if m.group("suffix"):
            return None
    else:
        value = int(m.group("dec"), 10)
        if m.group("suffix") in ("K", "k"):
            value *= 1024
        elif m.group("suffix") in ("M", "m"):
            value *= 1024 * 1024
    return value


def parse_memory_x(text: str) -> Dict[str, Region]:
    """Return every region declared in a linker script's MEMORY block."""
    regions: Dict[str, Region] = {}
    for m in _REGION_RE.finditer(text):
        origin = parse_number(m.group("origin"))
        length = parse_number(m.group("length"))
        if origin is None or length is None:
            continue
        regions[m.group("name")] = Region(origin, length)
    return regions


def load(path: str) -> Dict[str, Region]:
    with open(path, "r", encoding="utf-8") as f:
        return parse_memory_x(f.read())


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("memory_x", help="path to memory.x")
    ap.add_argument(
        "--field",
        help="print a single value, e.g. FLASH.origin or RAM.length",
    )
    args = ap.parse_args()

    regions = load(args.memory_x)
    if not regions:
        print(f"layout: no MEMORY regions found in {args.memory_x}", file=sys.stderr)
        return 1

    if args.field:
        name, _, attr = args.field.partition(".")
        if name not in regions:
            print(f"layout: no region named {name!r}", file=sys.stderr)
            return 1
        if attr not in ("origin", "length", "end"):
            print(f"layout: no field named {attr!r}", file=sys.stderr)
            return 1
        print(hex(getattr(regions[name], attr)))
        return 0

    for name, r in regions.items():
        print(f"{name:<8} {r.origin:#010x}..{r.end:#010x}  {r.length:>9} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
