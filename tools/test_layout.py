#!/usr/bin/env python3
"""Tests for the memory.x parser.

The expected values here are pinned to the same literals as the Rust tests in
`core/src/linker_script.rs`. That is the point: two parsers exist, and if they
ever read `memory.x` differently, one of these suites fails.
"""

import os
import unittest

import layout

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MEMORY_X = os.path.join(REPO, "memory.x")


class TestRealMemoryX(unittest.TestCase):
    def setUp(self):
        self.regions = layout.load(MEMORY_X)

    def test_reads_the_projects_own_memory_x(self):
        flash = self.regions["FLASH"]
        # 0x26000 is where the S140 SoftDevice hands off; 0xEA000 is the highest
        # address the board's bootloader will write, 40K below its own code at
        # 0xF4000. Changing either means changing the board.
        self.assertEqual(flash.origin, 0x00026000)
        self.assertEqual(flash.length, 784 * 1024)
        self.assertEqual(flash.end, 0x000EA000)

        ram = self.regions["RAM"]
        self.assertEqual(ram.origin, 0x20000000)
        self.assertEqual(ram.length, 256 * 1024)

    def test_finds_exactly_the_two_regions(self):
        self.assertEqual(set(self.regions), {"FLASH", "RAM"})


class TestParsing(unittest.TestCase):
    def test_prose_mentioning_a_region_is_not_a_declaration(self):
        script = (
            "/* The FLASH region below used to be ORIGIN = 0x00000000, LENGTH = 1M\n"
            " * before the bootloader existed. Do not go back to that.\n"
            " */\n"
            "MEMORY\n{\n"
            "  FLASH : ORIGIN = 0x00026000, LENGTH = 824K\n"
            "}\n"
        )
        self.assertEqual(layout.parse_memory_x(script)["FLASH"].origin, 0x26000)

    def test_tolerates_whitespace_and_ordering(self):
        script = (
            "MEMORY {\n"
            "  RAM:ORIGIN=0x20000000,LENGTH=256K\n"
            "\tFLASH   :   ORIGIN   =   0x26000 ,  LENGTH  =  824K\n"
            "}"
        )
        regions = layout.parse_memory_x(script)
        self.assertEqual(regions["FLASH"], layout.Region(0x26000, 843776))
        self.assertEqual(regions["RAM"], layout.Region(0x20000000, 262144))

    def test_missing_region(self):
        self.assertEqual(layout.parse_memory_x(""), {})
        self.assertNotIn("FLASH", layout.parse_memory_x("RAM : ORIGIN = 0x0, LENGTH = 1K"))

    def test_malformed_declarations_are_skipped(self):
        # An origin with no length is not a usable region.
        self.assertEqual(layout.parse_memory_x("FLASH : ORIGIN = 0x26000"), {})
        self.assertEqual(layout.parse_memory_x("FLASH : ORIGIN = start, LENGTH = 1K"), {})

    def test_number_formats(self):
        cases = {
            "0x26000": 0x26000,
            "0X26000": 0x26000,
            "4096": 4096,
            "824K": 843776,
            "1M": 1024 * 1024,
            "2k": 2048,
        }
        for text, expected in cases.items():
            with self.subTest(text=text):
                self.assertEqual(layout.parse_number(text), expected)

    def test_rejects_nonsense_numbers(self):
        for text in ["", "K", "ABC", "0x", "0x400K", "12 34", "-1"]:
            with self.subTest(text=text):
                self.assertIsNone(layout.parse_number(text))


class TestRegionGeometry(unittest.TestCase):
    def setUp(self):
        self.r = layout.Region(0x26000, 0xCE000)

    def test_end(self):
        self.assertEqual(self.r.end, 0xF4000)

    def test_contains_range(self):
        self.assertTrue(self.r.contains_range(0x26000, 0xCE000))
        self.assertTrue(self.r.contains_range(0x26000, 0))
        self.assertFalse(self.r.contains_range(0x26000, 0xCE001))
        self.assertFalse(self.r.contains_range(0x25FFF, 1))
        self.assertFalse(self.r.contains_range(0, 0))


if __name__ == "__main__":
    unittest.main()
