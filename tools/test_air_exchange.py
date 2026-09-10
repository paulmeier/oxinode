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
    def test_macos_pairs_the_two_cdc_ports(self):
        self.assertEqual(ax.log_port_for("/dev/cu.usbmodem101"), "/dev/cu.usbmodem103")
        self.assertEqual(ax.log_port_for("/dev/cu.usbmodem3101"), "/dev/cu.usbmodem3103")

    def test_anything_else_is_not_guessed(self):
        self.assertIsNone(ax.log_port_for("/dev/ttyACM0"))
        self.assertIsNone(ax.log_port_for("/dev/cu.usbserial-0001"))
        self.assertIsNone(ax.log_port_for("/dev/cu.usbmodem103"))


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
