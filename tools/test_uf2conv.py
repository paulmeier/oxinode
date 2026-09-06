#!/usr/bin/env python3
"""Tests for the UF2 writer.

A malformed UF2 is not a crash, it is a board that silently does not run the
firmware you think you flashed. Framing errors are also invisible to the writer
itself, so these tests decode the output independently rather than trusting it.
"""

import struct
import unittest

import uf2conv


def decode(data):
    """Unpack a UF2 file into (header fields, payload) tuples."""
    assert len(data) % uf2conv.BLOCK_SIZE == 0
    out = []
    for i in range(len(data) // uf2conv.BLOCK_SIZE):
        raw = data[i * uf2conv.BLOCK_SIZE : (i + 1) * uf2conv.BLOCK_SIZE]
        fields = struct.unpack_from("<IIIIIIII", raw, 0)
        (end,) = struct.unpack_from("<I", raw, uf2conv.BLOCK_SIZE - 4)
        out.append((fields, raw[32 : 32 + fields[4]], end, raw))
    return out


BASE = 0x26000


class TestFraming(unittest.TestCase):
    def test_every_block_is_exactly_512_bytes(self):
        for size in [1, 255, 256, 257, 4000, 4096]:
            with self.subTest(size=size):
                out = uf2conv.to_uf2(b"\xa5" * size, BASE, uf2conv.FAMILY_NRF52840)
                self.assertEqual(len(out) % uf2conv.BLOCK_SIZE, 0)

    def test_magic_words(self):
        out = uf2conv.to_uf2(b"x" * 600, BASE, uf2conv.FAMILY_NRF52840)
        for fields, _payload, end, _raw in decode(out):
            self.assertEqual(fields[0], uf2conv.MAGIC_START0)
            self.assertEqual(fields[1], uf2conv.MAGIC_START1)
            self.assertEqual(end, uf2conv.MAGIC_END)

    def test_start_magic_is_the_ascii_tag(self):
        # "UF2\n" little-endian; a wrong value here makes the bootloader ignore
        # the file entirely and it looks like nothing happened.
        self.assertEqual(struct.pack("<I", uf2conv.MAGIC_START0), b"UF2\n")


class TestBlockCounts(unittest.TestCase):
    def test_exact_multiple_of_payload_size(self):
        out = decode(uf2conv.to_uf2(b"z" * 512, BASE, uf2conv.FAMILY_NRF52840))
        self.assertEqual(len(out), 2)

    def test_partial_final_block(self):
        out = decode(uf2conv.to_uf2(b"z" * 513, BASE, uf2conv.FAMILY_NRF52840))
        self.assertEqual(len(out), 3)
        self.assertEqual(out[-1][0][4], 1, "final payloadSize should be the remainder")

    def test_single_short_block(self):
        out = decode(uf2conv.to_uf2(b"z", BASE, uf2conv.FAMILY_NRF52840))
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0][0][4], 1)

    def test_block_numbering_and_total(self):
        out = decode(uf2conv.to_uf2(b"z" * 1000, BASE, uf2conv.FAMILY_NRF52840))
        for i, (fields, _p, _e, _raw) in enumerate(out):
            self.assertEqual(fields[5], i, "blockNo")
            self.assertEqual(fields[6], len(out), "numBlocks")

    def test_empty_input_is_rejected(self):
        # Silently producing a zero-block file would look like a successful flash.
        with self.assertRaises(ValueError):
            uf2conv.to_uf2(b"", BASE, uf2conv.FAMILY_NRF52840)


class TestAddressing(unittest.TestCase):
    def test_addresses_start_at_base_and_advance_by_payload_size(self):
        out = decode(uf2conv.to_uf2(b"z" * 1000, BASE, uf2conv.FAMILY_NRF52840))
        for i, (fields, _p, _e, _raw) in enumerate(out):
            self.assertEqual(fields[3], BASE + i * uf2conv.PAYLOAD_SIZE)

    def test_base_address_is_honoured(self):
        # Writing to 0x0 instead of 0x26000 would overwrite the MBR.
        out = decode(uf2conv.to_uf2(b"z" * 10, 0x1000, uf2conv.FAMILY_NRF52840))
        self.assertEqual(out[0][0][3], 0x1000)


class TestFamilyAndFlags(unittest.TestCase):
    def test_family_id_present_flag_is_set(self):
        # Without the flag the family word is ignored, and the file would be
        # accepted by a bootloader for a different chip.
        out = decode(uf2conv.to_uf2(b"z" * 10, BASE, uf2conv.FAMILY_NRF52840))
        self.assertTrue(out[0][0][2] & uf2conv.FLAG_FAMILY_ID_PRESENT)

    def test_family_id_is_written(self):
        out = decode(uf2conv.to_uf2(b"z" * 10, BASE, uf2conv.FAMILY_NRF52840))
        self.assertEqual(out[0][0][7], uf2conv.FAMILY_NRF52840)

    def test_registry_family_constant(self):
        # microsoft/uf2's registered value for a Nordic nRF52840.
        self.assertEqual(uf2conv.FAMILY_NRF52840, 0xADA52840)

    def test_default_family_is_the_boards_not_the_registrys(self):
        # The Base Duo's bootloader uses a vendor value that is not in the
        # registry at all. Defaulting to the "obviously correct" nRF52840 id
        # produced a file the bootloader silently ignored -- it accepted the
        # copy, wrote nothing, and never rebooted. Read from the board's own
        # CURRENT.UF2.
        self.assertEqual(uf2conv.FAMILY_MUZI_BASE, 0x239A0081)
        self.assertEqual(uf2conv.DEFAULT_FAMILY, uf2conv.FAMILY_MUZI_BASE)
        self.assertNotEqual(uf2conv.DEFAULT_FAMILY, uf2conv.FAMILY_NRF52840)


class TestFamilyDetection(unittest.TestCase):
    def test_reads_family_from_a_uf2(self):
        for family in [uf2conv.FAMILY_MUZI_BASE, uf2conv.FAMILY_NRF52840, 0x1B57745F]:
            with self.subTest(family=family):
                out = uf2conv.to_uf2(b"z" * 10, BASE, family)
                self.assertEqual(uf2conv.family_of(out), family)

    def test_only_needs_the_header(self):
        # Callers read this off a mounted bootloader drive, where CURRENT.UF2 is
        # the whole flash and pulling all of it over USB MSC is slow.
        out = uf2conv.to_uf2(b"z" * 10000, BASE, uf2conv.FAMILY_MUZI_BASE)
        self.assertEqual(
            uf2conv.family_of(out[: uf2conv.HEADER_SIZE]), uf2conv.FAMILY_MUZI_BASE
        )

    def test_rejects_non_uf2(self):
        with self.assertRaises(ValueError):
            uf2conv.family_of(b"\x00" * 64)
        with self.assertRaises(ValueError):
            uf2conv.family_of(b"")


class TestPayload(unittest.TestCase):
    def test_round_trip(self):
        for size in [1, 255, 256, 257, 4000]:
            with self.subTest(size=size):
                data = bytes((i * 7 + size) & 0xFF for i in range(size))
                blocks = decode(uf2conv.to_uf2(data, BASE, uf2conv.FAMILY_NRF52840))
                self.assertEqual(b"".join(p for _f, p, _e, _r in blocks), data)

    def test_short_final_block_is_zero_padded_not_truncated(self):
        blocks = decode(uf2conv.to_uf2(b"\xff" * 300, BASE, uf2conv.FAMILY_NRF52840))
        _fields, _payload, _end, raw = blocks[-1]
        # 44 real bytes, then padding out to the footer.
        self.assertEqual(raw[32:76], b"\xff" * 44)
        self.assertEqual(raw[76 : uf2conv.BLOCK_SIZE - 4], b"\x00" * (476 - 44))


if __name__ == "__main__":
    unittest.main()
