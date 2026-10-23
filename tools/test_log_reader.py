#!/usr/bin/env python3
"""Tests for log_reader.py that need no board: the descriptor walk, and that
the constants are the ones the firmware uses."""

import os
import re
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import log_reader as lr  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def iad(first, count, cls):
    return bytes([8, 0x0B, first, count, cls, 0, 0, 0])


def interface(number, cls, sub, proto, endpoints):
    return bytes([9, 0x04, number, 0, endpoints, cls, sub, proto, 0])


def endpoint(address, attributes):
    return bytes([7, 0x05, address, attributes, 64, 0, 0])


def config(*parts):
    body = b"".join(parts)
    total = 9 + len(body)
    return bytes([9, 0x02, total & 0xFF, total >> 8, 3, 1, 0, 0x80, 50]) + body


# What the RNode image presents: a CDC-ACM function (control interface with
# class-specific descriptors and an interrupt endpoint, then a data interface
# with two bulk endpoints), then the log.
CDC = (
    iad(0, 2, 0x02)
    + interface(0, 0x02, 0x02, 0x00, 1)
    + bytes([5, 0x24, 0x00, 0x10, 0x01])
    + bytes([5, 0x24, 0x01, 0x00, 0x01])
    + bytes([4, 0x24, 0x02, 0x02])
    + bytes([5, 0x24, 0x06, 0x00, 0x01])
    + endpoint(0x81, 0x03)
    + interface(1, 0x0A, 0x00, 0x00, 2)
    + endpoint(0x02, 0x02)
    + endpoint(0x82, 0x02)
)
LOG = iad(2, 1, 0xFF) + interface(2, 0xFF, 0x6F, 0x01, 1) + endpoint(0x83, 0x02)


class FindLogInterface(unittest.TestCase):
    def test_the_rnode_image(self):
        self.assertEqual(lr.find_log_interface(config(CDC, LOG)), lr.LogInterface(2, 0x83))

    def test_an_older_image_with_two_cdc_ports_has_none(self):
        self.assertIsNone(lr.find_log_interface(config(CDC, CDC)))

    def test_a_vendor_interface_with_another_subclass_is_not_ours(self):
        other = interface(2, 0xFF, 0x00, 0x00, 1) + endpoint(0x83, 0x02)
        self.assertIsNone(lr.find_log_interface(config(CDC, other)))

    def test_an_out_endpoint_on_the_log_interface_is_not_the_one(self):
        out_only = interface(2, 0xFF, 0x6F, 0x01, 1) + endpoint(0x03, 0x02)
        self.assertIsNone(lr.find_log_interface(config(CDC, out_only)))

    def test_an_endpoint_belongs_to_the_interface_it_follows(self):
        # A log interface with no endpoint, then a CDC function: its bulk IN
        # is the CDC's, not the log's.
        empty = interface(0, 0xFF, 0x6F, 0x01, 0)
        self.assertIsNone(lr.find_log_interface(config(empty, CDC)))

    def test_the_cdc_bulk_in_before_it_is_not_mistaken_for_it(self):
        # The data interface's 0x82 comes first in the walk.
        self.assertEqual(lr.find_log_interface(config(CDC, LOG)).endpoint, 0x83)

    def test_a_truncated_descriptor_gives_none_not_an_exception(self):
        whole = config(CDC, LOG)
        for cut in range(len(whole) - 1):
            lr.find_log_interface(whole[:cut])  # must not raise
        self.assertIsNone(lr.find_log_interface(whole[:-1]))

    def test_a_zero_length_descriptor_stops_the_walk(self):
        self.assertIsNone(lr.find_log_interface(config(CDC) + bytes([0, 0x04]) + LOG))


class MatchesTheFirmware(unittest.TestCase):
    """The reader and the firmware agree because these say so, not because
    someone remembered to edit both."""

    @classmethod
    def setUpClass(cls):
        with open(os.path.join(ROOT, "core", "src", "usb.rs")) as f:
            cls.core = f.read()
        with open(os.path.join(ROOT, "src", "bin", "rnode.rs")) as f:
            cls.rnode = f.read()

    def const(self, source, name):
        m = re.search(rf"const {name}: u\d+ = (0x[0-9A-Fa-f]+|\d+);", source)
        self.assertIsNotNone(m, f"{name} not found")
        return int(m.group(1), 0)

    def test_interface_triple(self):
        for name in ("LOG_INTERFACE_CLASS", "LOG_INTERFACE_SUBCLASS", "LOG_INTERFACE_PROTOCOL"):
            self.assertEqual(getattr(lr, name), self.const(self.core, name), name)

    def test_attach_request(self):
        self.assertEqual(lr.LOG_REQUEST_ATTACH, self.const(self.core, "LOG_REQUEST_ATTACH"))

    def test_vid_and_pid(self):
        self.assertEqual(lr.VID, self.const(self.rnode, "USB_VID"))
        self.assertEqual(lr.PID, self.const(self.rnode, "USB_PID"))


class BoardWentAway(unittest.TestCase):
    def test_a_read_and_a_quiet_spell_keep_reading(self):
        self.assertIs(lr.board_went_away(0), False)
        self.assertIs(lr.board_went_away(lr.LIBUSB_ERROR_TIMEOUT), False)

    def test_not_found_is_a_reset(self):
        # What macOS returned mid-read when a 1200-baud touch reset the board,
        # 2026-10-23; the reader exited instead of following it.
        self.assertEqual(lr.LIBUSB_ERROR_NOT_FOUND, -5)
        self.assertIs(lr.board_went_away(-5), True)

    def test_the_other_ways_a_board_goes_away(self):
        for rc in (lr.LIBUSB_ERROR_NO_DEVICE, lr.LIBUSB_ERROR_IO, lr.LIBUSB_ERROR_PIPE):
            self.assertIs(lr.board_went_away(rc), True)

    def test_anything_else_is_an_error(self):
        for rc in (lr.LIBUSB_ERROR_ACCESS, lr.LIBUSB_ERROR_BUSY, -8, -12):
            self.assertIsNone(lr.board_went_away(rc))


if __name__ == "__main__":
    unittest.main()
