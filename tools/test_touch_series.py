#!/usr/bin/env python3
"""Tests for touch_series.py that need no board."""

import os
import re
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import touch_series as ts  # noqa: E402
from air_exchange import PortInfo  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Lines as defmt-print decoded them from 7F23 on 2026-10-23, trimmed.
BEFORE = [
    "1689.970825 DEBUG tx: 167 bytes in 1 frames under header 0x90 (seq 9), 272247 us",
    "1714.262542 DEBUG tx: 167 bytes in 1 frames under header 0xb0 (seq 11), 272216 us",
]
LINKED_BOOT = [
    "0.000000 INFO  vbus: inherited interrupt enables 0x00000380, now 0x380; detected=true ready=false",
    "0.252532 INFO  nus: pumping for [75, 3a, 29, 8f, 9e, dc], att mtu 23",
    "0.478790 INFO  nus: link encrypted, authenticated, from a stored bond",
    "3.238861 INFO  config: 915000000 Hz (915067069 Hz commanded), SF7 BW125000 CR4/5, 11 dBm",
]
UNLINKED_BOOT = [
    "0.000000 INFO  vbus: inherited interrupt enables 0x00000380, now 0x380; detected=true ready=false",
    "0.252532 INFO  nus: pumping for [75, 3a, 29, 8f, 9e, dc], att mtu 23",
    "1.144287 INFO  gps: searching; sats 0/0; no fix; 1 sentences, 0 bad",
]


class Boots(unittest.TestCase):
    def test_an_empty_log_has_no_boots(self):
        self.assertEqual(ts.boots([]), [])

    def test_a_reset_starts_a_boot(self):
        self.assertEqual(ts.boots(BEFORE + LINKED_BOOT), [ts.Boot(False), ts.Boot(True)])

    def test_a_link_belongs_to_the_boot_it_came_up_in(self):
        # The link of an earlier boot does not carry into the next one.
        self.assertEqual(ts.boots(LINKED_BOOT + UNLINKED_BOOT), [ts.Boot(True), ts.Boot(False)])

    def test_a_connection_without_encryption_is_not_a_link(self):
        self.assertEqual(ts.boots(UNLINKED_BOOT), [ts.Boot(False)])

    def test_the_readers_notes_are_not_a_reset(self):
        notes = ["log_reader: board went away; waiting for it to come back", "log_reader: reading 65C11224153B1DFD"]
        self.assertEqual(ts.boots(LINKED_BOOT[:2] + notes + LINKED_BOOT[2:]), [ts.Boot(True)])

    def test_a_reset_caught_after_its_first_line(self):
        # The reader may come back after the board's first lines have gone by.
        self.assertEqual(ts.boots(LINKED_BOOT + LINKED_BOOT[2:]), [ts.Boot(True), ts.Boot(True)])

    def test_the_link_line_is_the_one_the_firmware_logs(self):
        with open(os.path.join(ROOT, "src", "nus.rs")) as f:
            self.assertIn(f'"{ts.LINK_UP}, ', f.read())


class Outcome(unittest.TestCase):
    def test_first_try(self):
        self.assertEqual(ts.outcome(0, "Activating new firmware\nDevice programmed.\n"), ts.FIRST_TRY)

    def test_after_the_retry(self):
        out = "Target is not in DFU mode.\ndfu-flash: the board is in its bootloader; retrying (attempt 1)\nDevice programmed.\n"
        self.assertEqual(ts.outcome(0, out), ts.AFTER_RETRY)

    def test_a_failure_is_a_failure_whatever_it_printed(self):
        self.assertEqual(ts.outcome(1, "retrying (attempt 3)\ndfu-flash: could not reach the bootloader."), ts.FAILED)

    def test_the_retry_line_is_the_one_the_flasher_prints(self):
        with open(os.path.join(ROOT, "tools", "dfu-flash.sh")) as f:
            self.assertRegex(f.read(), re.escape(ts.RETRIED) + r" \(attempt")


class Summary(unittest.TestCase):
    def test_counts(self):
        self.assertEqual(
            ts.summary([ts.FIRST_TRY, ts.AFTER_RETRY, ts.FIRST_TRY, ts.FAILED]),
            "4 touches: 3 flashed (2 first try, 1 after the retry), 1 failed",
        )


class MatchesTheFirmware(unittest.TestCase):
    def test_reset_frame(self):
        with open(os.path.join(ROOT, "core", "src", "rnode", "command.rs")) as f:
            text = f.read()
        self.assertRegex(text, rf"pub const RESET: u8 = 0x{ts.CMD_RESET:02X};")
        self.assertRegex(text, rf"pub const CONFIRM_BYTE: u8 = 0x{ts.CONFIRM_BYTE:02X};")
        self.assertEqual(ts.RESET_FRAME, b"\xc0\x55\xf8\xc0")


class PortFor(unittest.TestCase):
    PORTS = [
        PortInfo("/dev/cu.Bluetooth-Incoming-Port", None, None, None),
        PortInfo("/dev/cu.usbmodem3101", "D3304814DC467229", 0x1209, 0x0003),
        PortInfo("/dev/cu.usbmodem1101", "65C11224153B1DFD", 0x1209, 0x0003),
    ]

    def test_by_serial(self):
        self.assertEqual(ts.port_for("65C11224153B1DFD", self.PORTS), "/dev/cu.usbmodem1101")

    def test_another_device_with_that_serial_is_not_the_board(self):
        # The bootloader enumerates with its own VID and PID.
        ports = [PortInfo("/dev/cu.usbmodem1101", "65C11224153B1DFD", 0x239A, 0x0029)]
        self.assertIsNone(ts.port_for("65C11224153B1DFD", ports))

    def test_absent(self):
        self.assertIsNone(ts.port_for("0000", self.PORTS))


if __name__ == "__main__":
    unittest.main()
