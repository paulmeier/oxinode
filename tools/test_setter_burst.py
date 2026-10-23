#!/usr/bin/env python3
"""Tests for setter_burst.py that need no board."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import setter_burst as sb  # noqa: E402


def reply(cmd, value):
    return bytes([sb.FEND, cmd, value, sb.FEND])


ALL = b"".join(reply(c, v) for c, v in sb.SETTERS)
ODD = b"".join(reply(c, v) for c, v in sb.SETTERS_ODD)


class Burst(unittest.TestCase):
    def test_six_four_byte_frames(self):
        b = sb.burst()
        self.assertEqual(len(b), 24)
        self.assertEqual(sb.frames(b), [bytes(s) for s in sb.SETTERS])

    def test_no_setter_value_needs_escaping(self):
        # A value of FEND or FESC would make a frame longer than four bytes.
        for _, value in sb.SETTERS + sb.SETTERS_ODD:
            self.assertNotIn(value, (sb.FEND, sb.FESC))

    def test_alternate_rounds_share_no_setter(self):
        self.assertFalse(set(sb.SETTERS) & set(sb.SETTERS_ODD))
        self.assertEqual(sb.setters_for(0), sb.SETTERS)
        self.assertEqual(sb.setters_for(1), sb.SETTERS_ODD)

    def test_the_values_are_ones_an_rnode_accepts(self):
        for cmd, value in sb.SETTERS + sb.SETTERS_ODD:
            if cmd == sb.CMD_SF:
                self.assertIn(value, range(5, 13))
            if cmd == sb.CMD_CR:
                self.assertIn(value, range(5, 9))


class Frames(unittest.TestCase):
    def test_escapes_are_undone(self):
        stream = bytes([sb.FEND, 0x00, sb.FESC, sb.TFEND, sb.FESC, sb.TFESC, sb.FEND])
        self.assertEqual(sb.frames(stream), [bytes([0x00, sb.FEND, sb.FESC])])

    def test_bytes_before_the_first_fend_and_an_open_tail_are_not_frames(self):
        stream = b"\x03\x07" + reply(0x03, 7) + bytes([sb.FEND, 0x05])
        self.assertEqual(sb.frames(stream), [b"\x03\x07"])

    def test_back_to_back_fends_are_not_empty_frames(self):
        self.assertEqual(sb.frames(bytes([sb.FEND, sb.FEND, 0x04, 2, sb.FEND])), [b"\x04\x02"])


class Tally(unittest.TestCase):
    def test_every_setter_answered(self):
        self.assertEqual(sb.tally([ALL, ODD]), sb.Tally(2, 0, 0, (0,) * 6))

    def test_a_missing_reply_is_counted_at_its_position(self):
        without_fourth = b"".join(reply(c, v) for i, (c, v) in enumerate(sb.SETTERS_ODD) if i != 3)
        self.assertEqual(sb.tally([ALL, without_fourth]), sb.Tally(2, 1, 1, (0, 0, 0, 1, 0, 0)))

    def test_a_late_reply_does_not_stand_in_for_the_next_rounds(self):
        # Round 0's replies all arrive in round 1's window, and round 1's
        # never arrive: both rounds are a loss, not a success.
        self.assertEqual(sb.tally([b"", ALL]).unanswered, 12)

    def test_a_reply_with_another_value_does_not_count(self):
        wrong = ALL.replace(reply(0x04, 3), reply(0x04, 9))
        self.assertEqual(sb.tally([wrong]).by_position, (0, 0, 0, 0, 0, 1))

    def test_unsolicited_reports_are_ignored(self):
        noisy = reply(0x23, 0x10) + ALL + reply(0x22, 0x00)
        self.assertEqual(sb.tally([noisy]).unanswered, 0)

    def test_the_whole_stream_counts_frames(self):
        self.assertEqual(sb.tally_whole(ALL + ODD + ALL, 3).unanswered, 0)
        self.assertEqual(sb.tally_whole(ALL + ODD, 3).unanswered, 6)
        self.assertEqual(sb.tally_whole(ALL + ODD + ALL + reply(0x23, 1), 3).unanswered, 0)


if __name__ == "__main__":
    unittest.main()
