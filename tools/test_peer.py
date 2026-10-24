#!/usr/bin/env python3
"""Tests for what the bench peer claims about announces and LXMF's framing.

The peer checks the board's announces and messages with its own reading of
the bytes, so that reading is what is pinned here: against the announce stock
Reticulum wrote for `oxinode_core::node`'s tests, against the sizes LXMF
works to, and against messages built by hand. Where Reticulum and LXMF are
installed (`python3.11 -m unittest tools/test_peer.py` on the bench Mac), the
last class also holds every reading to theirs: the signature over the
announce, a message LXMF packed, and LXMF's own reading of the app data.
"""

import importlib.util
import os
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import peer  # noqa: E402

VECTORS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "core", "src", "node", "vectors")
HAVE_LXMF = importlib.util.find_spec("RNS") is not None and importlib.util.find_spec("LXMF") is not None


def vector_bytes(name: str) -> bytes:
    with open(os.path.join(VECTORS, name), "rb") as f:
        return f.read()


def vectors() -> dict:
    out = {}
    with open(os.path.join(VECTORS, "vectors.txt")) as f:
        for line in f:
            if line.strip() and not line.startswith("#"):
                key, value = line.split()
                out[key] = bytes.fromhex(value)
    return out


def announce() -> peer.Announce:
    return peer.parse_announce(peer.parse_packet(vector_bytes("announce.bin")))


def message_bytes(payload, destination=b"\x11" * 16, source=b"\x22" * 16, signature=b"\x33" * 64) -> bytes:
    return destination + source + signature + peer.packb(payload)


class Sizes(unittest.TestCase):
    def test_an_encrypted_packet_carries_383_bytes(self):
        # 464 less the 48 of the token and the 32 of the ephemeral key, in
        # whole AES blocks, less one
        self.assertEqual(peer.ENCRYPTED_MDU, 383)

    def test_lxmf_overhead_and_the_opportunistic_limit(self):
        self.assertEqual(peer.LXMF_OVERHEAD, 112)
        self.assertEqual(peer.OPPORTUNISTIC_MAX_CONTENT, 295)

    def test_the_content_size_is_lxmfs(self):
        # Past 255 bytes the content is bin16, three bytes of header, and the
        # count comes out at exactly the content's length.
        for n in (256, 295, 296):
            self.assertEqual(peer.content_size(1.5, b"", b"x" * n, {}), n)
        # Shorter, bin8 saves a byte.
        self.assertEqual(peer.content_size(1.5, b"", b"x" * 100, {}), 99)
        # A title and fields count too.
        self.assertEqual(peer.content_size(1.5, b"tt", b"x" * 100, {}), 101)
        self.assertLessEqual(peer.content_size(1.5, b"", b"x" * 295, {}), peer.OPPORTUNISTIC_MAX_CONTENT)
        self.assertGreater(peer.content_size(1.5, b"", b"x" * 296, {}), peer.OPPORTUNISTIC_MAX_CONTENT)


class Msgpack(unittest.TestCase):
    def test_bytes_as_umsgpack_writes_them(self):
        self.assertEqual(peer.packb(None), b"\xc0")
        self.assertEqual(peer.packb(True), b"\xc3")
        self.assertEqual(peer.packb(5), b"\x05")
        self.assertEqual(peer.packb(-1), b"\xff")
        self.assertEqual(peer.packb(200), b"\xcc\xc8")
        self.assertEqual(peer.packb(70000), b"\xce\x00\x01\x11\x70")
        self.assertEqual(peer.packb(-200), b"\xd1\xff\x38")
        self.assertEqual(peer.packb(1.5), b"\xcb" + struct.pack(">d", 1.5))
        self.assertEqual(peer.packb("abc"), b"\xa3abc")
        self.assertEqual(peer.packb(b"ab"), b"\xc4\x02ab")
        self.assertEqual(peer.packb(b"x" * 300)[:3], b"\xc5\x01\x2c")
        self.assertEqual(peer.packb([1, None]), b"\x92\x01\xc0")
        self.assertEqual(peer.packb({1: b""}), b"\x81\x01\xc4\x00")
        self.assertEqual(peer.packb(list(range(16)))[:3], b"\xdc\x00\x10")

    def test_round_trips(self):
        for value in (
            None,
            False,
            0,
            127,
            128,
            65535,
            2**40,
            -32,
            -33,
            -(2**40),
            3.25,
            "",
            "é" * 40,
            b"",
            b"\x00" * 70000,
            [1, [2, [3]]],
            list(range(20)),
            {1: b"a", "k": [None]},
            {i: i for i in range(20)},
        ):
            self.assertEqual(peer.unpackb(peer.packb(value)), value)

    def test_float32_and_the_signed_forms_read(self):
        self.assertEqual(peer.unpackb(b"\xca" + struct.pack(">f", 0.5)), 0.5)
        self.assertEqual(peer.unpackb(b"\xd0\x80"), -128)
        self.assertEqual(peer.unpackb(b"\xcd\x01\x00"), 256)
        self.assertEqual(peer.unpackb(b"\xd9\x02hi"), "hi")

    def test_truncated_trailing_and_unknown_are_errors(self):
        for bad in (b"", b"\xc4\x05ab", b"\x92\x01", b"\xcb\x00", b"\x01\x02", b"\xc1", b"\xd4\x00\x00"):
            with self.assertRaises(ValueError):
                peer.unpackb(bad)

    def test_array_spans_cover_the_bytes(self):
        b = peer.packb([1.5, b"t", b"c", {}, b"stamp"])
        header, spans = peer.array_spans(b)
        self.assertEqual(header, 1)
        self.assertEqual([s[0] for s in spans], [1.5, b"t", b"c", {}, b"stamp"])
        self.assertEqual(spans[0][1], 1)
        self.assertEqual(spans[-1][2], len(b))
        for (_, _, end), (_, start, _) in zip(spans, spans[1:]):
            self.assertEqual(end, start)
        with self.assertRaises(ValueError):
            peer.array_spans(b"\x81\x01\x02")


class Announces(unittest.TestCase):
    def test_the_vector_reads_as_python_wrote_it(self):
        a = announce()
        v = vectors()
        self.assertEqual(a.destination, v["destination"])
        self.assertEqual(a.public_key, v["public_key"])
        self.assertEqual(a.identity_hash, v["identity"])
        self.assertEqual(a.name_hash, v["name_hash"])
        self.assertEqual(a.app_data, v["app_data"])
        self.assertEqual(a.ratchet, b"")
        self.assertEqual(a.hops, 0)
        self.assertEqual(len(a.signature), peer.SIGNATURE_LEN)

    def test_the_vector_checks_out(self):
        a = announce()
        self.assertEqual(peer.name_hash(peer.ASPECT), a.name_hash)
        self.assertEqual(peer.announce_problems(a), [])
        self.assertEqual(peer.lxmf_app_data(a.app_data), (peer.DISPLAY_NAME, None))

    def test_the_random_hash_ends_in_the_clock(self):
        # Written on 2026-10-23; a clock between 2026 and 2030 is a sane read
        # of the five bytes, and anything else is reading the wrong five.
        self.assertTrue(1767225600 <= announce().emitted < 1893456000, announce().emitted)

    def test_the_signature_covers_destination_key_names_random_and_app_data(self):
        a = announce()
        raw = vector_bytes("announce.bin")
        data = raw[2 + 16 + 1 :]
        self.assertEqual(a.signed_data, a.destination + data[:84] + a.app_data)

    def test_an_announce_of_its_own_accord_is_not_a_path_response(self):
        self.assertFalse(announce().path_response)

    def test_a_path_response_is_told_by_its_context(self):
        raw = bytearray(vector_bytes("announce.bin"))
        raw[2 + 16] = peer.CONTEXT_PATH_RESPONSE
        a = peer.parse_announce(peer.parse_packet(bytes(raw)))
        self.assertTrue(a.path_response)
        # The context is not signed, so the rest reads and checks the same.
        self.assertEqual(a._replace(path_response=False), announce())
        self.assertEqual(peer.announce_problems(a), [])

    def test_a_second_header_moves_the_destination_along(self):
        raw = vector_bytes("announce.bin")
        flags = raw[0] | 0b0100_0000
        transport = b"\xee" * 16
        a = peer.parse_announce(peer.parse_packet(bytes([flags, 3]) + transport + raw[2:]))
        self.assertEqual(a, announce()._replace(hops=3))

    def test_a_ratchet_sits_before_the_signature_and_is_signed(self):
        raw = vector_bytes("announce.bin")
        head, data = raw[: 2 + 16 + 1], raw[2 + 16 + 1 :]
        ratchet = bytes(range(32))
        flagged = bytes([raw[0] | 0b0010_0000]) + head[1:] + data[:84] + ratchet + data[84:]
        a = peer.parse_announce(peer.parse_packet(flagged))
        plain = announce()
        self.assertEqual(a.ratchet, ratchet)
        self.assertEqual(a.signature, plain.signature)
        self.assertEqual(a.app_data, plain.app_data)
        self.assertIn(ratchet + plain.app_data, a.signed_data)

    def test_a_changed_key_does_not_make_the_destination(self):
        raw = bytearray(vector_bytes("announce.bin"))
        raw[2 + 16 + 1 + 5] ^= 1
        a = peer.parse_announce(peer.parse_packet(bytes(raw)))
        self.assertEqual(peer.announce_problems(a), ["the destination hash is not the one the key and name make"])

    def test_another_aspect_is_named(self):
        a = announce()._replace(name_hash=peer.name_hash("nomadnetwork.node"))
        problems = peer.announce_problems(a)
        self.assertTrue(problems[0].startswith("name hash"), problems)

    def test_app_data_lxmf_cannot_read_is_a_problem(self):
        for bad in (b"\x92\xc4\x05ab", b"\x92\x01\xc0", b"\x91\xc4\x01a\x00", b"\xff\xfe"):
            a = announce()._replace(app_data=bad)
            self.assertTrue(any(p.startswith("app data") for p in peer.announce_problems(a)), bad)

    def test_app_data_forms(self):
        self.assertEqual(peer.lxmf_app_data(b""), (None, None))
        self.assertEqual(peer.lxmf_app_data(b"Old style"), ("Old style", None))
        self.assertEqual(peer.lxmf_app_data(peer.packb([b"Board\x00 ", 8])), ("Board", 8))
        self.assertEqual(peer.lxmf_app_data(peer.packb([None, None])), (None, None))
        self.assertEqual(peer.lxmf_app_data(peer.packb([b"n", None, [0]])), ("n", None))

    def test_a_floods_names_are_read_back_as_given(self):
        self.assertEqual(peer.flood_name(1), "flood 01")
        self.assertEqual(peer.flood_name(20), "flood 20")
        for name in ("flood 01", "Zoë"):
            self.assertEqual(peer.lxmf_app_data(peer.lxmf_name_app_data(name)), (name, None))
        # The board's own reading in core/src/node.rs packs the same bytes.
        self.assertEqual(peer.lxmf_name_app_data("Bench peer"), vectors()["app_data"])

    def test_what_is_not_an_announce_or_is_short_is_refused(self):
        raw = vector_bytes("announce.bin")
        with self.assertRaises(ValueError):
            peer.parse_announce(peer.parse_packet(bytes([raw[0] & ~0b11]) + raw[1:]))
        with self.assertRaises(ValueError):
            peer.parse_announce(peer.parse_packet(raw[:100]))
        for short in (b"", b"\x01", raw[:18]):
            with self.assertRaises(ValueError):
                peer.parse_packet(short)


class Messages(unittest.TestCase):
    def test_the_fields_are_where_lxmf_puts_them(self):
        lxmf = message_bytes([1.5, b"title", b"content", {1: b"x"}])
        m = peer.parse_lxmf(lxmf)
        self.assertEqual((m.destination, m.source, m.signature), (b"\x11" * 16, b"\x22" * 16, b"\x33" * 64))
        self.assertEqual((m.timestamp, m.title, m.content, m.fields, m.stamp), (1.5, b"title", b"content", {1: b"x"}, None))

    def test_the_hash_covers_destination_source_and_payload(self):
        lxmf = message_bytes([1.5, b"", b"hi", {}])
        m = peer.parse_lxmf(lxmf)
        self.assertEqual(m.hashed_part, lxmf[:32] + lxmf[96:])
        self.assertEqual(m.signed_part, m.hashed_part + m.message_hash)

    def test_a_stamp_is_not_hashed(self):
        plain = peer.parse_lxmf(message_bytes([1.5, b"", b"hi", {}]))
        stamped = peer.parse_lxmf(message_bytes([1.5, b"", b"hi", {}, b"\x07" * 32]))
        self.assertEqual(stamped.stamp, b"\x07" * 32)
        self.assertEqual(stamped.message_hash, plain.message_hash)

    def test_a_payload_that_is_not_lxmfs_is_refused(self):
        for payload in (
            [1.5, b"", b"hi"],
            [1.5, b"", b"hi", {}, b"", b""],
            [1, b"", b"hi", {}],
            [1.5, "title", b"hi", {}],
            [1.5, b"", "hi", {}],
            [1.5, b"", b"hi", []],
            {1: 2},
        ):
            with self.assertRaises(ValueError, msg=payload):
                peer.parse_lxmf(message_bytes(payload))
        with self.assertRaises(ValueError):
            peer.parse_lxmf(b"\x00" * peer.PAYLOAD_OFFSET)


class Verdicts(unittest.TestCase):
    def test_a_run_that_saw_everything_passes(self):
        self.assertEqual(peer.shortfalls(peer.Expect(1, 2, 1), peer.Seen(3, 0, 2, 1, 0)), [])

    def test_each_shortfall_is_named(self):
        missing = peer.shortfalls(peer.Expect(1, 2, 1), peer.Seen(0, 1, 1, 0, 1))
        self.assertEqual(len(missing), 5)
        self.assertIn("0 of 1 announces from the board", missing)
        self.assertIn("1 of 2 delivery proofs", missing)
        self.assertIn("0 of 1 messages from the board", missing)

    def test_a_bad_announce_fails_a_run_that_expected_none(self):
        self.assertEqual(len(peer.shortfalls(peer.Expect(0, 0, 0), peer.Seen(0, 1, 0, 0, 0))), 1)

    def test_a_message_proved_twice_counts_once(self):
        with tempfile.TemporaryDirectory() as d:
            run = peer.Run(d, None)
            self.assertTrue(run.prove(1))
            self.assertFalse(run.prove(1))
            self.assertTrue(run.prove(2))
            self.assertEqual(run.seen().proofs, 2)
            run.events.close()

    def test_hashes_as_reticulum_prints_them(self):
        h = bytes(range(16))
        self.assertEqual(peer.parse_hash("<" + h.hex() + ">"), h)
        self.assertEqual(peer.parse_hash(h.hex()), h)
        for bad in ("abc", "zz" * 16, "00" * 15, "00" * 32):
            with self.assertRaises(ValueError):
                peer.parse_hash(bad)


def board_announce(name: str = "board_announce.bin") -> peer.Announce:
    """The board's own announce, as `oxinode_core::node` builds it and its
    test `the_vectors_are_what_the_board_makes` holds it to."""
    return peer.parse_announce(peer.parse_packet(vector_bytes(name)))


class TheBoardsAnnounce(unittest.TestCase):
    """What the board puts on the air, read the way the peer reads it on the
    bench, so a run of the peer against a board starts from bytes already
    known to pass."""

    def test_it_checks_out(self):
        self.assertEqual(peer.announce_problems(board_announce()), [])

    def test_it_carries_the_boards_name(self):
        self.assertEqual(peer.lxmf_app_data(board_announce().app_data), ("oxinode 7F23", None))

    def test_its_emission_time_is_the_one_it_was_given(self):
        # 2026-10-24 00:00:00 UTC, the time the vector was made with.
        self.assertEqual(board_announce().emitted, 1_792_800_000)

    def test_it_is_not_a_path_response_and_its_twin_is(self):
        self.assertFalse(board_announce().path_response)
        response = board_announce("board_path_response.bin")
        self.assertTrue(response.path_response)
        self.assertEqual(peer.announce_problems(response), [])
        self.assertEqual(response.signature, board_announce().signature, "nothing signed differs")

    def test_it_is_one_hop_from_nowhere(self):
        self.assertEqual(board_announce().hops, 0)


@unittest.skipUnless(HAVE_LXMF, "Reticulum and LXMF are not installed for this Python")
class AgainstReticulum(unittest.TestCase):
    """The same readings, held to the stack's own where it is installed."""

    @classmethod
    def setUpClass(cls):
        import LXMF
        import RNS

        cls.RNS, cls.LXMF = RNS, LXMF

    def test_the_sizes_are_lxmfs(self):
        M = self.LXMF.LXMessage
        self.assertEqual(peer.ENCRYPTED_MDU, self.RNS.Packet.ENCRYPTED_MDU)
        self.assertEqual(peer.LXMF_OVERHEAD, M.LXMF_OVERHEAD)
        self.assertEqual(peer.OPPORTUNISTIC_MAX_CONTENT, M.ENCRYPTED_PACKET_MAX_CONTENT)
        self.assertEqual(peer.ax.MDU, self.RNS.Reticulum.MDU)
        self.assertEqual(peer.CONTEXT_PATH_RESPONSE, self.RNS.Packet.PATH_RESPONSE)

    def test_a_floods_app_data_is_lxmfs(self):
        self.assertEqual(
            self.LXMF.display_name_from_app_data(peer.lxmf_name_app_data(peer.flood_name(3))),
            "flood 03",
        )

    def test_the_signature_verifies_over_what_is_said_to_be_signed(self):
        a = announce()
        identity = self.RNS.Identity(create_keys=False)
        identity.load_public_key(a.public_key)
        self.assertTrue(identity.validate(a.signature, a.signed_data))
        self.assertFalse(identity.validate(a.signature, a.signed_data + b"\x00"))

    def test_the_boards_signature_verifies(self):
        for name in ("board_announce.bin", "board_path_response.bin"):
            a = board_announce(name)
            identity = self.RNS.Identity(create_keys=False)
            identity.load_public_key(a.public_key)
            self.assertTrue(identity.validate(a.signature, a.signed_data), name)
            self.assertFalse(identity.validate(a.signature, a.signed_data[:-1] + b"\x00"), name)

    def test_lxmf_reads_the_boards_app_data_the_same(self):
        a = board_announce()
        self.assertEqual(self.LXMF.display_name_from_app_data(a.app_data), "oxinode 7F23")
        self.assertIsNone(self.LXMF.stamp_cost_from_app_data(a.app_data))

    def test_lxmf_reads_the_app_data_the_same(self):
        for app_data in (vectors()["app_data"], b"Old style", peer.packb([b"Board", 8]), peer.packb([None, None])):
            self.assertEqual(
                peer.lxmf_app_data(app_data),
                (self.LXMF.display_name_from_app_data(app_data), self.LXMF.stamp_cost_from_app_data(app_data)),
            )

    def test_umsgpack_writes_what_packb_writes(self):
        from RNS.vendor import umsgpack

        for value in ([1.5, b"t", "s", {1: [None, True]}], 70000, -200, b"x" * 300, list(range(20))):
            self.assertEqual(peer.packb(value), umsgpack.packb(value))

    def _message(self, content="hello from the bench", title="t", fields=None):
        RNS, LXMF = self.RNS, self.LXMF
        source = RNS.Destination(RNS.Identity.from_bytes(bytes(range(64))), RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery")
        destination = RNS.Destination(RNS.Identity(), RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery")
        m = LXMF.LXMessage(destination, source, content, title=title, fields=fields or {}, desired_method=LXMF.LXMessage.OPPORTUNISTIC)
        m.pack()
        return m, source

    def test_a_message_lxmf_packed_reads_and_hashes_the_same(self):
        m, source = self._message(fields={0x0F: b"\x01"})
        parsed = peer.parse_lxmf(m.packed)
        self.assertEqual(parsed.message_hash, m.hash)
        self.assertEqual((parsed.title, parsed.content, parsed.fields), (b"t", b"hello from the bench", {0x0F: b"\x01"}))
        self.assertEqual(parsed.timestamp, m.timestamp)
        self.assertTrue(source.identity.validate(parsed.signature, parsed.signed_part))

    def test_the_limit_is_where_lxmf_stops_sending_opportunistically(self):
        for n, method in ((295, self.LXMF.LXMessage.OPPORTUNISTIC), (296, self.LXMF.LXMessage.DIRECT)):
            m, _ = self._message(content="x" * n, title="")
            self.assertEqual(m.desired_method, method, n)
            self.assertEqual(peer.content_size(m.timestamp, b"", b"x" * n, {}), n)


if __name__ == "__main__":
    unittest.main()
