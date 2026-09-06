#!/usr/bin/env python3
"""Tests for the pre-flash layout checker.

A check that cannot fail is worse than no check, because it is believed. So most
of this suite builds deliberately broken images and asserts that the checker
rejects them, naming the specific defect each one carries.
"""

import struct
import unittest

import check_layout
import layout
import uf2conv

FLASH = layout.Region(0x00026000, 784 * 1024)
RAM = layout.Region(0x20000000, 256 * 1024)
REGIONS = {"FLASH": FLASH, "RAM": RAM}

EHDR_SIZE = 52
PHDR_SIZE = 32


def build_elf(segments, entry, machine=check_layout.EM_ARM, elf_class=1, endian=1):
    """Assemble a minimal but genuine ELF32 file.

    `segments` is a list of (vaddr, paddr, payload, memsz) tuples; payload may be
    empty for a .bss-like segment. Program headers are emitted in order, followed
    by the payloads.
    """
    phoff = EHDR_SIZE
    data_start = phoff + PHDR_SIZE * len(segments)

    payloads = bytearray()
    phdrs = bytearray()
    for vaddr, paddr, payload, memsz in segments:
        offset = data_start + len(payloads)
        phdrs += struct.pack(
            "<IIIIIIII",
            check_layout.PT_LOAD,
            offset,
            vaddr,
            paddr,
            len(payload),
            memsz,
            0b110,
            4,
        )
        payloads += payload

    ehdr = bytearray(EHDR_SIZE)
    ehdr[0:4] = b"\x7fELF"
    ehdr[4] = elf_class
    ehdr[5] = endian
    ehdr[6] = 1
    struct.pack_into("<HHI", ehdr, 16, 2, machine, 1)  # e_type=EXEC
    struct.pack_into("<III", ehdr, 24, entry, phoff, 0)  # entry, phoff, shoff
    struct.pack_into("<IHHHHHH", ehdr, 36, 0, EHDR_SIZE, PHDR_SIZE, len(segments), 0, 0, 0)
    return bytes(ehdr) + bytes(phdrs) + bytes(payloads)


def vector_table(initial_sp=RAM.end, reset=0x00026101):
    return struct.pack("<II", initial_sp, reset)


def good_elf(**kw):
    """A well-formed image: vector table + code in flash, .data, .bss."""
    text = vector_table(**kw) + b"\x00" * 0xF8 + b"\xaa" * 0x400
    return build_elf(
        [
            (FLASH.origin, FLASH.origin, text, len(text)),
            (RAM.origin, FLASH.origin + len(text), b"\x01\x02\x03\x04", 4),
            (RAM.origin + 4, RAM.origin + 4, b"", 0x200),
        ],
        entry=0x00026101,
    )


class TestParsingRejectsNonImages(unittest.TestCase):
    def test_not_an_elf(self):
        with self.assertRaises(check_layout.LayoutError):
            check_layout.parse_elf(b"MZ\x90\x00" + b"\x00" * 100)

    def test_truncated(self):
        with self.assertRaises(check_layout.LayoutError):
            check_layout.parse_elf(b"\x7fELF")

    def test_wrong_class(self):
        data = bytearray(good_elf())
        data[4] = 2
        with self.assertRaises(check_layout.LayoutError) as cm:
            check_layout.parse_elf(bytes(data))
        self.assertIn("32-bit", str(cm.exception))

    def test_wrong_endianness(self):
        data = bytearray(good_elf())
        data[5] = 2
        with self.assertRaises(check_layout.LayoutError) as cm:
            check_layout.parse_elf(bytes(data))
        self.assertIn("little-endian", str(cm.exception))

    def test_wrong_architecture(self):
        # Catches building for the host by accident, which otherwise produces a
        # perfectly valid file that would then be flashed to a Cortex-M.
        elf = build_elf(
            [(FLASH.origin, FLASH.origin, vector_table(), 8)], entry=0x26101, machine=62
        )
        with self.assertRaises(check_layout.LayoutError) as cm:
            check_layout.parse_elf(elf)
        self.assertIn("ARM", str(cm.exception))

    def test_no_loadable_segments(self):
        with self.assertRaises(check_layout.LayoutError):
            check_layout.parse_elf(build_elf([], entry=0x26101))


class TestAcceptsAGoodImage(unittest.TestCase):
    def test_no_problems(self):
        elf = check_layout.parse_elf(good_elf())
        self.assertEqual(check_layout.check_elf(elf, REGIONS), [])

    def test_bss_contributes_no_flash(self):
        elf = check_layout.parse_elf(good_elf())
        self.assertEqual(sum(s.filesz for s in elf.segments), 0x500 + 4)

    def test_header_only_segment_is_ignored(self):
        # lld emits a LOAD covering the ELF header itself; counting it would both
        # inflate the flash figure and corrupt the reconstructed image.
        segments = [
            (0x20000, 0x20000, b"", 0x154),
            (FLASH.origin, FLASH.origin, vector_table() + b"\x00" * 0xF8, 0x100),
        ]
        raw = bytearray(build_elf(segments, entry=0x26101))
        # Point the first program header at file offset 0, as lld does.
        struct.pack_into("<I", raw, EHDR_SIZE + 4, 0)
        elf = check_layout.parse_elf(bytes(raw))
        self.assertEqual(len(elf.segments), 1)
        self.assertEqual(elf.segments[0].paddr, FLASH.origin)


class TestRejectsBrokenImages(unittest.TestCase):
    def assert_rejected(self, elf_bytes, needle):
        problems = check_layout.check_elf(check_layout.parse_elf(elf_bytes), REGIONS)
        self.assertTrue(problems, "expected the checker to complain")
        self.assertTrue(
            any(needle in p for p in problems),
            f"no problem mentioned {needle!r}; got {problems}",
        )

    def test_linked_at_zero(self):
        # The failure this whole script exists to prevent: an image at 0x0 gets
        # written over the MBR and SoftDevice, taking the bootloader with it.
        text = vector_table(reset=0x101) + b"\x00" * 0xF8
        self.assert_rejected(build_elf([(0, 0, text, len(text))], entry=0x101), "outside FLASH")

    def test_image_overruns_the_bootloader(self):
        # 784K is all the bootloader will accept; one byte more and it refuses
        # the whole transfer.
        text = vector_table() + b"\x00" * (FLASH.length - 8 + 1)
        self.assert_rejected(
            build_elf([(FLASH.origin, FLASH.origin, text, len(text))], entry=0x26101),
            "outside FLASH",
        )

    def test_runtime_address_in_neither_region(self):
        text = vector_table() + b"\x00" * 0xF8
        self.assert_rejected(
            build_elf(
                [
                    (FLASH.origin, FLASH.origin, text, len(text)),
                    (0x40000000, FLASH.origin + 0x100, b"\x01", 1),
                ],
                entry=0x26101,
            ),
            "neither FLASH",
        )

    def test_stack_pointer_outside_ram(self):
        # An SP pointing at flash faults on the first push, before any of our
        # code runs -- indistinguishable from a dead board.
        self.assert_rejected(good_elf(initial_sp=0x26000), "stack pointer")

    def test_stack_pointer_just_past_ram(self):
        self.assert_rejected(good_elf(initial_sp=RAM.end + 4), "stack pointer")

    def test_stack_pointer_at_top_of_ram_is_allowed(self):
        # The stack grows down from the top, so RAM.end itself is correct.
        elf = check_layout.parse_elf(good_elf(initial_sp=RAM.end))
        self.assertEqual(check_layout.check_elf(elf, REGIONS), [])

    def test_reset_vector_outside_flash(self):
        self.assert_rejected(good_elf(reset=0x00001101), "reset vector")

    def test_reset_vector_without_thumb_bit(self):
        # Cortex-M has no ARM mode; an even reset vector is an immediate usage
        # fault at boot.
        self.assert_rejected(good_elf(reset=0x00026100), "Thumb bit")

    def test_entry_point_without_thumb_bit(self):
        elf = build_elf(
            [(FLASH.origin, FLASH.origin, vector_table() + b"\x00" * 0xF8, 0x100)],
            entry=0x00026100,
        )
        self.assert_rejected(elf, "Thumb bit")

    def test_missing_regions(self):
        elf = check_layout.parse_elf(good_elf())
        self.assertTrue(check_layout.check_elf(elf, {"FLASH": FLASH}))


class TestUf2Checking(unittest.TestCase):
    def setUp(self):
        self.image = bytes((i * 13) & 0xFF for i in range(700))
        self.uf2 = uf2conv.to_uf2(self.image, FLASH.origin, uf2conv.DEFAULT_FAMILY)

    def test_accepts_matching_uf2(self):
        blocks = check_layout.parse_uf2(self.uf2)
        self.assertEqual(check_layout.check_uf2(blocks, REGIONS, self.image, FLASH.origin), [])

    def test_rejects_payload_mismatch(self):
        blocks = check_layout.parse_uf2(self.uf2)
        problems = check_layout.check_uf2(blocks, REGIONS, self.image[:-1], FLASH.origin)
        self.assertTrue(any("does not match" in p for p in problems))

    def test_rejects_wrong_family(self):
        # The real failure this guards: a file built for the registry's nRF52840
        # id is silently ignored by this board's bootloader, which then never
        # reboots and gives no error at all.
        wrong = uf2conv.to_uf2(self.image, FLASH.origin, uf2conv.FAMILY_NRF52840)
        blocks = check_layout.parse_uf2(wrong)
        problems = check_layout.check_uf2(blocks, REGIONS, self.image, FLASH.origin)
        self.assertTrue(any("family" in p for p in problems), problems)

    def test_family_is_checked_against_the_argument_not_a_constant(self):
        # Another board, another bootloader, another id -- the checker has to
        # take the expected value from the caller.
        other = uf2conv.to_uf2(self.image, FLASH.origin, 0x1B57745F)
        blocks = check_layout.parse_uf2(other)
        self.assertEqual(
            check_layout.check_uf2(
                blocks, REGIONS, self.image, FLASH.origin, family=0x1B57745F
            ),
            [],
        )

    def test_rejects_wrong_base_address(self):
        wrong = uf2conv.to_uf2(self.image, 0x1000, uf2conv.FAMILY_NRF52840)
        blocks = check_layout.parse_uf2(wrong)
        problems = check_layout.check_uf2(blocks, REGIONS, self.image, FLASH.origin)
        self.assertTrue(any("address" in p for p in problems))
        self.assertTrue(any("outside FLASH" in p for p in problems))

    def test_rejects_corrupt_framing(self):
        for offset, name in [(0, "start magic"), (508, "end magic")]:
            with self.subTest(field=name):
                corrupt = bytearray(self.uf2)
                corrupt[offset] ^= 0xFF
                with self.assertRaises(check_layout.LayoutError):
                    check_layout.parse_uf2(bytes(corrupt))

    def test_rejects_truncated_file(self):
        with self.assertRaises(check_layout.LayoutError):
            check_layout.parse_uf2(self.uf2[:-1])
        with self.assertRaises(check_layout.LayoutError):
            check_layout.parse_uf2(b"")

    def test_rejects_oversized_payload_field(self):
        corrupt = bytearray(self.uf2)
        struct.pack_into("<I", corrupt, 16, 999)
        with self.assertRaises(check_layout.LayoutError):
            check_layout.parse_uf2(bytes(corrupt))

    def test_rejects_inconsistent_block_numbering(self):
        corrupt = bytearray(self.uf2)
        struct.pack_into("<I", corrupt, 20, 7)  # blockNo of the first block
        blocks = check_layout.parse_uf2(bytes(corrupt))
        problems = check_layout.check_uf2(blocks, REGIONS, self.image, FLASH.origin)
        self.assertTrue(any("numbered" in p for p in problems))


if __name__ == "__main__":
    unittest.main()
