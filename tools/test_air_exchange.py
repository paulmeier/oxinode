#!/usr/bin/env python3
"""Tests for the parts of the exchange runner that need neither a board nor RNS.

The runner's claims about Reticulum's framing are the ones worth pinning: a
packet's size on the air is what the runner says it is, the plan covers both
sizes in both directions, and the config it writes cannot join a shared
instance. The peer itself needs Reticulum and two RNodes and is exercised by
running it.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import air_exchange as ax  # noqa: E402


class Sizes(unittest.TestCase):
    def test_payload_for_a_size_on_the_air(self):
        # flags, hops, a 16-byte destination hash and a context byte
        self.assertEqual(ax.data_len_for_raw(400), 381)
        self.assertEqual(ax.data_len_for_raw(200), 181)

    def test_the_defaults_sit_either_side_of_the_split(self):
        small, large = ax.DEFAULT_SIZES
        self.assertLess(small, ax.FRAME_PAYLOAD)
        self.assertGreater(large, ax.FRAME_PAYLOAD)
        self.assertLessEqual(large, ax.MTU)

    def test_the_bounds_are_reticulums(self):
        self.assertEqual(ax.MDU, 464)
        self.assertEqual(ax.data_len_for_raw(ax.RAW_MAX), ax.MDU)
        self.assertEqual(ax.data_len_for_raw(ax.RAW_MIN), 1)
        for bad in (0, ax.RAW_MIN - 1, ax.RAW_MAX + 1, ax.MTU + 1):
            with self.assertRaises(ValueError):
                ax.data_len_for_raw(bad)


class Plan(unittest.TestCase):
    def test_every_size_both_ways_small_first(self):
        self.assertEqual(
            ax.plan((200, 400), ("A", "B")),
            [
                ax.Exchange("A", "B", 200),
                ax.Exchange("B", "A", 200),
                ax.Exchange("A", "B", 400),
                ax.Exchange("B", "A", 400),
            ],
        )

    def test_names_are_carried(self):
        plan = ax.plan((300,), ("oxinode", "rnode"))
        self.assertEqual([(e.sender, e.receiver) for e in plan], [("oxinode", "rnode"), ("rnode", "oxinode")])


class Ports(unittest.TestCase):
    PORTS = [
        ax.PortInfo("/dev/cu.usbmodem1101", "65C11224153B1DFD", 0x1209, 0x0003),
        ax.PortInfo("/dev/cu.usbmodem3101", "D3304814DC467229", 0x1209, 0x0003),
        # A stock RNode: a USB-serial bridge, nothing to read a log from.
        ax.PortInfo("/dev/cu.usbserial-0001", "0001", 0x10C4, 0xEA60),
        # A bring-up image: an oxinode, but not one with the log interface.
        ax.PortInfo("/dev/cu.usbmodem2101", "AAAA", 0x1209, 0x0004),
    ]

    def test_an_oxinode_is_found_by_its_kiss_port(self):
        self.assertEqual(ax.usb_serial_for("/dev/cu.usbmodem1101", self.PORTS), "65C11224153B1DFD")
        self.assertEqual(ax.usb_serial_for("/dev/cu.usbmodem3101", self.PORTS), "D3304814DC467229")

    def test_a_stock_rnode_has_no_log(self):
        self.assertIsNone(ax.usb_serial_for("/dev/cu.usbserial-0001", self.PORTS))

    def test_another_image_is_not_read(self):
        self.assertIsNone(ax.usb_serial_for("/dev/cu.usbmodem2101", self.PORTS))

    def test_a_port_that_is_not_attached_has_none(self):
        self.assertIsNone(ax.usb_serial_for("/dev/ttyACM0", self.PORTS))

    def test_the_vid_and_pid_are_the_rnode_images(self):
        with open(os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src", "bin", "rnode.rs")) as f:
            rnode = f.read()
        self.assertIn(f"const USB_VID: u16 = 0x{ax.OXINODE_VID_PID[0]:04x};", rnode)
        self.assertIn(f"const USB_PID: u16 = 0x{ax.OXINODE_VID_PID[1]:04x};", rnode)


class Config(unittest.TestCase):
    def test_one_rnode_no_shared_instance_no_transport(self):
        text = ax.config_text("/dev/cu.usbmodem101", ax.DEFAULT_RADIO)
        self.assertIn("share_instance = No", text)
        self.assertIn("enable_transport = No", text)
        self.assertIn("type = RNodeInterface", text)
        self.assertIn("port = /dev/cu.usbmodem101", text)
        for key, value in ax.DEFAULT_RADIO.items():
            self.assertIn(f"{key} = {value}", text)
        self.assertNotIn("AutoInterface", text)

    def test_radio_values_are_the_callers(self):
        radio = dict(ax.DEFAULT_RADIO, frequency=868000000, txpower=2)
        text = ax.config_text("/dev/x", radio)
        self.assertIn("frequency = 868000000", text)
        self.assertIn("txpower = 2", text)


class Reports(unittest.TestCase):
    def test_a_receipt_parses(self):
        r = ax.parse_report("RECEIVED raw=400 data=381 sha256=abc rssi=-40 snr=9.5")
        self.assertEqual(r, {"kind": "RECEIVED", "raw": 400, "data": 381, "sha256": "abc", "rssi": -40.0, "snr": 9.5})

    def test_missing_signal_is_none(self):
        r = ax.parse_report("RECEIVED raw=200 data=181 sha256=abc rssi=None snr=None")
        self.assertIsNone(r["rssi"])
        self.assertIsNone(r["snr"])

    def test_verdicts(self):
        sent = {"kind": "SENT", "raw": 400, "sha256": "abc"}
        self.assertEqual(ax.verdict(sent, dict(sent, kind="RECEIVED")), "ok")
        self.assertEqual(ax.verdict(sent, None), "lost")
        self.assertEqual(ax.verdict(sent, {"kind": "TIMEOUT"}), "lost")
        self.assertEqual(ax.verdict(sent, {"kind": "RECEIVED", "raw": 254, "sha256": "abc"}), "size 254 != 400")
        self.assertEqual(ax.verdict(sent, {"kind": "RECEIVED", "raw": 400, "sha256": "zzz"}), "payload differs")


class Interpreter(unittest.TestCase):
    def test_shebang_forms(self):
        self.assertEqual(ax.interpreter_from_shebang("#!/opt/py/bin/python3.11\nimport x\n"), "/opt/py/bin/python3.11")
        self.assertEqual(ax.interpreter_from_shebang("#!/usr/bin/env python3\n"), "python3")
        self.assertIsNone(ax.interpreter_from_shebang("import x\n"))
        self.assertIsNone(ax.interpreter_from_shebang(""))
        self.assertIsNone(ax.interpreter_from_shebang("#!\n"))


class Summary(unittest.TestCase):
    def test_one_line_per_exchange(self):
        ex = ax.Exchange("A", "B", 400)
        sent = {"kind": "SENT", "raw": 400, "sha256": "abc"}
        received = {"kind": "RECEIVED", "raw": 400, "sha256": "abc", "rssi": -41.0, "snr": 9.25}
        lines = ax.summary_lines([ax.Result(ex, sent, received, "ok"), ax.Result(ex, sent, None, "lost")])
        self.assertEqual(len(lines), 3)
        self.assertIn("A -> B", lines[1])
        self.assertIn("-41", lines[1])
        self.assertIn("9.2", lines[1])
        self.assertTrue(lines[1].endswith("ok"))
        self.assertTrue(lines[2].endswith("lost"))


if __name__ == "__main__":
    unittest.main()
