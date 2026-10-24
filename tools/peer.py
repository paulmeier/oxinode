#!/usr/bin/env python3
"""A Reticulum peer on the bench: the other end of every messaging test.

Stock Python Reticulum and LXMF, in a configuration directory of their own,
driving the second bench board as an ordinary `RNodeInterface`. The board
under test is a node with no host attached; this is what it talks to, and
what says whether it said the right thing.

  * It hears every announce on the air and checks the ones for an
    `lxmf.delivery` destination itself: the name hash, the destination hash
    against the key, the Ed25519 signature, and the display name and stamp
    cost in the app data. Each frame is recorded, repeats included, with the
    signal it came with.
  * It sends opportunistic LXMF messages to the board once it has heard the
    board announce, one at a time, and reports the delivery proof and how
    long it took to come back. With `--repeat`, each message delivered is
    sent once more as a new packet, as LXMF retries one whose proof was
    lost: the same message hash, which the board must prove again and not
    keep twice.
  * It prints every LXMF message the board sends it, with LXMF's own
    signature check, and checks the framing of each against its own reading.
  * It announces its own `lxmf.delivery` destination, under one identity that
    is kept between runs, when it starts and again when it first hears the
    board, so a board that came up later still learns it and can answer.
    With `--no-announce` it never does, so its messages come from a sender
    the board has not heard -- give it an `--identity` the board has not met
    either -- and the board must ask for its path to learn its key.
    Reticulum answers that path request on its own.
  * With `--flood N`, it then announces N more `lxmf.delivery` destinations
    under throwaway identities, named `flood 01` and on, as fast as the
    RNode will send them: more peers than the board keeps, and more packets
    than it queues, to see what it loses and that it stays responsive.

    python3.11 tools/peer.py --rnode /dev/cu.usbmodem3101 --board-log 65C11224153B1DFD

The run ends when what it was asked to check has happened -- and, if the
board sent it a message, `--linger` seconds after the last one, so that
message's proof goes out and the board logs it delivered -- or after
`--seconds`, and exits non-zero if anything is missing: an announce from the
board, a proof for a message sent, or a message from the board. So it is a
regression run as well as a tool to watch with.

Everything goes to `target/peer/<time>/`: `events.log` (what is printed),
`summary.txt`, Reticulum's own log, and each oxinode's `defmt` log through
`log_reader.py`, as `air_exchange.py` keeps them.

What it claims about the framing of an announce and of an LXMF message is in
the functions above the peer, which need neither a board nor RNS, so
`test_peer.py` can hold them to bytes stock Reticulum wrote.
"""

import argparse
import hashlib
import os
import shutil
import struct
import sys
import threading
import time
from typing import Dict, List, NamedTuple, Optional, Tuple

import air_exchange as ax

# --------------------------------------------------------------- Reticulum's framing

# `RNS.Identity.TRUNCATED_HASHLENGTH`, `NAME_HASH_LENGTH`, `KEYSIZE`,
# `SIGLENGTH` and `RATCHETSIZE`, in bytes; the random hash is ten bytes,
# five random and five of the announcing node's clock in seconds.
HASH_LEN = 16
NAME_HASH_LEN = 10
RANDOM_HASH_LEN = 10
PUBLIC_KEY_LEN = 64
SIGNATURE_LEN = 64
RATCHET_LEN = 32

# The flags byte: header type, context flag, transport type, destination type,
# packet type, from the top bit down. `RNS.Packet.unpack`.
HEADER_2 = 1
PACKET_ANNOUNCE = 1
DESTINATION_SINGLE = 0
# The context byte after the destination. An announce sent in answer to a
# path request carries `RNS.Packet.PATH_RESPONSE`; it is not the node
# announcing of its own accord, and is not counted as one.
CONTEXT_PATH_RESPONSE = 0x0B

# `RNS.Identity.TOKEN_OVERHEAD` (a 16-byte IV and a 32-byte HMAC), the
# ephemeral key that opens a packet to a single destination, and AES's block.
TOKEN_OVERHEAD = 48
EPHEMERAL_KEY_LEN = 32
AES_BLOCK = 16
# `RNS.Packet.ENCRYPTED_MDU`: what an encrypted packet may carry.
ENCRYPTED_MDU = (ax.MDU - TOKEN_OVERHEAD - EPHEMERAL_KEY_LEN) // AES_BLOCK * AES_BLOCK - 1

# ------------------------------------------------------------------ LXMF's framing

# An LXMF message: the destination hash, the source hash, an Ed25519
# signature, then a msgpack array of timestamp, title, content and fields,
# with a stamp as a fifth element when the receiver asks for one. Sent
# opportunistically, the destination hash is left off: the packet carries it.
SIGNATURE_OFFSET = 2 * HASH_LEN
PAYLOAD_OFFSET = SIGNATURE_OFFSET + SIGNATURE_LEN
# `LXMessage.TIMESTAMP_SIZE` and `STRUCT_OVERHEAD`, and what they add up to.
TIMESTAMP_SIZE = 8
STRUCT_OVERHEAD = 8
LXMF_OVERHEAD = 2 * HASH_LEN + SIGNATURE_LEN + TIMESTAMP_SIZE + STRUCT_OVERHEAD
# `LXMessage.ENCRYPTED_PACKET_MAX_CONTENT`: past this, LXMF quietly sends the
# message over a link instead, which the board does not take until #67.
ENCRYPTED_PACKET_MDU = ENCRYPTED_MDU + TIMESTAMP_SIZE
OPPORTUNISTIC_MAX_CONTENT = ENCRYPTED_PACKET_MDU - LXMF_OVERHEAD + HASH_LEN

ASPECT = "lxmf.delivery"
# Seconds between hearing the board's first announce and answering it.
ANSWER_DELAY = 2.0
DISPLAY_NAME = "Bench peer"


def name_hash(full_name: str) -> bytes:
    return hashlib.sha256(full_name.encode("utf-8")).digest()[:NAME_HASH_LEN]


def identity_hash(public_key: bytes) -> bytes:
    return hashlib.sha256(public_key).digest()[:HASH_LEN]


def destination_hash(name: bytes, identity: bytes) -> bytes:
    return hashlib.sha256(name + identity).digest()[:HASH_LEN]


# ------------------------------------------------------------- msgpack, the subset
#
# Only what LXMF and its announces use. Written here, rather than borrowed from
# RNS's vendored umsgpack, so the tests run where Reticulum is not installed.


def _unpack(b: bytes, i: int) -> Tuple[object, int]:
    def take(n: int) -> bytes:
        if i + 1 + n > len(b):
            raise ValueError(f"msgpack truncated at byte {i}")
        return b[i + 1 : i + 1 + n]

    if i >= len(b):
        raise ValueError(f"msgpack truncated at byte {i}")
    t = b[i]
    if t <= 0x7F:
        return t, i + 1
    if t >= 0xE0:
        return t - 0x100, i + 1
    if 0x80 <= t <= 0x8F:
        return _unpack_map(b, i + 1, t & 0x0F)
    if 0x90 <= t <= 0x9F:
        return _unpack_array(b, i + 1, t & 0x0F)
    if 0xA0 <= t <= 0xBF:
        n = t & 0x1F
        return take(n).decode("utf-8"), i + 1 + n
    if t == 0xC0:
        return None, i + 1
    if t in (0xC2, 0xC3):
        return t == 0xC3, i + 1
    sized = {0xC4: (1, bytes), 0xC5: (2, bytes), 0xC6: (4, bytes), 0xD9: (1, str), 0xDA: (2, str), 0xDB: (4, str)}
    if t in sized:
        width, kind = sized[t]
        n = int.from_bytes(take(width), "big")
        i += width
        body = take(n)
        return (body if kind is bytes else body.decode("utf-8")), i + 1 + n
    fixed = {0xCA: ">f", 0xCB: ">d", 0xCC: ">B", 0xCD: ">H", 0xCE: ">I", 0xCF: ">Q", 0xD0: ">b", 0xD1: ">h", 0xD2: ">i", 0xD3: ">q"}
    if t in fixed:
        n = struct.calcsize(fixed[t])
        return struct.unpack(fixed[t], take(n))[0], i + 1 + n
    if t in (0xDC, 0xDD, 0xDE, 0xDF):
        width = 2 if t in (0xDC, 0xDE) else 4
        n = int.from_bytes(take(width), "big")
        if t in (0xDC, 0xDD):
            return _unpack_array(b, i + 1 + width, n)
        return _unpack_map(b, i + 1 + width, n)
    raise ValueError(f"msgpack type 0x{t:02x} at byte {i} is not one LXMF uses")


def _unpack_array(b: bytes, i: int, n: int) -> Tuple[object, int]:
    out = []
    for _ in range(n):
        value, i = _unpack(b, i)
        out.append(value)
    return out, i


def _unpack_map(b: bytes, i: int, n: int) -> Tuple[object, int]:
    out = {}
    for _ in range(n):
        key, i = _unpack(b, i)
        value, i = _unpack(b, i)
        out[key] = value
    return out, i


def unpackb(b: bytes) -> object:
    value, end = _unpack(b, 0)
    if end != len(b):
        raise ValueError(f"{len(b) - end} bytes after the msgpack value")
    return value


def array_spans(b: bytes) -> Tuple[int, List[Tuple[object, int, int]]]:
    """A top-level msgpack array as its header length and each element with
    the bytes it spans, so a message can be re-hashed without re-packing."""
    t = b[0] if b else None
    if t is not None and 0x90 <= t <= 0x9F:
        header, n = 1, t & 0x0F
    elif t == 0xDC and len(b) >= 3:
        header, n = 3, int.from_bytes(b[1:3], "big")
    elif t == 0xDD and len(b) >= 5:
        header, n = 5, int.from_bytes(b[1:5], "big")
    else:
        raise ValueError("not a msgpack array")
    spans = []
    i = header
    for _ in range(n):
        value, end = _unpack(b, i)
        spans.append((value, i, end))
        i = end
    if i != len(b):
        raise ValueError(f"{len(b) - i} bytes after the msgpack array")
    return header, spans


def packb(value: object) -> bytes:
    """msgpack as RNS's umsgpack writes it, for the types LXMF sends."""
    if value is None:
        return b"\xc0"
    if value is True or value is False:
        return b"\xc3" if value else b"\xc2"
    if isinstance(value, int):
        if 0 <= value <= 0x7F:
            return bytes([value])
        if -32 <= value < 0:
            return struct.pack(">b", value)
        if value >= 0:
            for t, fmt in ((0xCC, ">B"), (0xCD, ">H"), (0xCE, ">I"), (0xCF, ">Q")):
                if value < 1 << (8 * struct.calcsize(fmt)):
                    return bytes([t]) + struct.pack(fmt, value)
        else:
            for t, fmt in ((0xD0, ">b"), (0xD1, ">h"), (0xD2, ">i"), (0xD3, ">q")):
                if value >= -(1 << (8 * struct.calcsize(fmt) - 1)):
                    return bytes([t]) + struct.pack(fmt, value)
        raise ValueError(f"{value} does not fit msgpack")
    if isinstance(value, float):
        return b"\xcb" + struct.pack(">d", value)
    if isinstance(value, (bytes, str)):
        body = value if isinstance(value, bytes) else value.encode("utf-8")
        n = len(body)
        if isinstance(value, str) and n < 32:
            return bytes([0xA0 | n]) + body
        widths = (0xC4, 0xC5, 0xC6) if isinstance(value, bytes) else (0xD9, 0xDA, 0xDB)
        for t, width in zip(widths, (1, 2, 4)):
            if n < 1 << (8 * width):
                return bytes([t]) + n.to_bytes(width, "big") + body
    if isinstance(value, (list, tuple)):
        return _sized(len(value), 0x90, 0xDC, 0xDD) + b"".join(packb(v) for v in value)
    if isinstance(value, dict):
        return _sized(len(value), 0x80, 0xDE, 0xDF) + b"".join(packb(k) + packb(v) for k, v in value.items())
    raise ValueError(f"cannot pack {type(value).__name__}")


def _sized(n: int, fix: int, t16: int, t32: int) -> bytes:
    if n < 16:
        return bytes([fix | n])
    if n < 1 << 16:
        return bytes([t16]) + n.to_bytes(2, "big")
    return bytes([t32]) + n.to_bytes(4, "big")


# ------------------------------------------------------------------- an announce


class Packet(NamedTuple):
    header_type: int
    context_flag: int
    packet_type: int
    destination_type: int
    hops: int
    destination: bytes
    context: int
    data: bytes


def parse_packet(raw: bytes) -> Packet:
    """A Reticulum packet as it came off the air, as far as an announce needs.
    With a second header the transport id comes before the destination."""
    if len(raw) < 2:
        raise ValueError("shorter than a packet header")
    flags, hops = raw[0], raw[1]
    header_type = (flags >> 6) & 1
    start = 2 + (HASH_LEN if header_type == HEADER_2 else 0)
    if len(raw) < start + HASH_LEN + 1:
        raise ValueError("shorter than a packet header")
    return Packet(
        header_type=header_type,
        context_flag=(flags >> 5) & 1,
        packet_type=flags & 0b11,
        destination_type=(flags >> 2) & 0b11,
        hops=hops,
        destination=raw[start : start + HASH_LEN],
        context=raw[start + HASH_LEN],
        data=raw[start + HASH_LEN + 1 :],
    )


class Announce(NamedTuple):
    destination: bytes
    public_key: bytes
    name_hash: bytes
    random_hash: bytes
    ratchet: bytes
    signature: bytes
    app_data: bytes
    hops: int
    path_response: bool

    @property
    def identity_hash(self) -> bytes:
        return identity_hash(self.public_key)

    @property
    def emitted(self) -> int:
        """The announcing node's clock when it made the announce, in seconds."""
        return int.from_bytes(self.random_hash[5:], "big")

    @property
    def signed_data(self) -> bytes:
        """What the signature covers. `RNS.Identity.validate_announce`."""
        return self.destination + self.public_key + self.name_hash + self.random_hash + self.ratchet + self.app_data


def parse_announce(packet: Packet) -> Announce:
    """The fields of an announce. With the context flag set it carries a
    ratchet key between the random hash and the signature."""
    if packet.packet_type != PACKET_ANNOUNCE:
        raise ValueError("not an announce")
    ratchet_len = RATCHET_LEN if packet.context_flag else 0
    fixed = PUBLIC_KEY_LEN + NAME_HASH_LEN + RANDOM_HASH_LEN + ratchet_len + SIGNATURE_LEN
    d = packet.data
    if len(d) < fixed:
        raise ValueError(f"an announce is at least {fixed} bytes of data, not {len(d)}")
    i = PUBLIC_KEY_LEN
    nh = d[i : i + NAME_HASH_LEN]
    i += NAME_HASH_LEN
    rh = d[i : i + RANDOM_HASH_LEN]
    i += RANDOM_HASH_LEN
    ratchet = d[i : i + ratchet_len]
    i += ratchet_len
    return Announce(
        destination=packet.destination,
        public_key=d[:PUBLIC_KEY_LEN],
        name_hash=nh,
        random_hash=rh,
        ratchet=ratchet,
        signature=d[i : i + SIGNATURE_LEN],
        app_data=d[i + SIGNATURE_LEN :],
        hops=packet.hops,
        path_response=packet.context == CONTEXT_PATH_RESPONSE,
    )


def lxmf_app_data(app_data: bytes) -> Tuple[Optional[str], Optional[int]]:
    """The display name and stamp cost an `lxmf.delivery` announce carries.

    Since LXMF 0.5.0 that is a msgpack array, name first as bytes, then the
    stamp cost; before, the app data was the name in UTF-8. LXMF tells the
    two apart by the first byte, and so does this. Raises ValueError on app
    data LXMF could not read.
    """
    if not app_data:
        return None, None
    if 0x90 <= app_data[0] <= 0x9F or app_data[0] == 0xDC:
        items = unpackb(app_data)
        name = items[0] if items else None
        cost = items[1] if len(items) > 1 else None
        if name is not None and not isinstance(name, bytes):
            raise ValueError(f"the display name is a {type(name).__name__}, not bytes")
        if cost is not None and not isinstance(cost, int):
            raise ValueError(f"the stamp cost is a {type(cost).__name__}, not an integer")
        text = None if name is None else name.decode("utf-8").replace("\x00", "").strip()
        return text, cost
    return app_data.decode("utf-8"), None


def flood_name(n: int) -> str:
    """The display name of the n'th of a flood's announces, from 1."""
    return f"flood {n:02d}"


def lxmf_name_app_data(name: str) -> bytes:
    """LXMF's app data for a display name and no stamp cost, as
    `LXMRouter.get_announce_app_data` packs it."""
    return packb([name.encode("utf-8"), None])


def announce_problems(a: Announce) -> List[str]:
    """What is wrong with an `lxmf.delivery` announce, short of the signature,
    which needs Ed25519 and so Reticulum. Empty when nothing is."""
    problems = []
    if a.name_hash != name_hash(ASPECT):
        problems.append(f"name hash {a.name_hash.hex()} is not {ASPECT}'s")
    if a.destination != destination_hash(a.name_hash, a.identity_hash):
        problems.append("the destination hash is not the one the key and name make")
    try:
        lxmf_app_data(a.app_data)
    except (ValueError, IndexError, UnicodeDecodeError) as e:
        problems.append(f"app data LXMF cannot read: {e}")
    return problems


# ----------------------------------------------------------------- an LXMF message


class Message(NamedTuple):
    destination: bytes
    source: bytes
    signature: bytes
    timestamp: float
    title: bytes
    content: bytes
    fields: Dict[object, object]
    stamp: Optional[bytes]
    hashed_part: bytes

    @property
    def message_hash(self) -> bytes:
        """The message's id, which the signature covers along with the hashed part."""
        return hashlib.sha256(self.hashed_part).digest()

    @property
    def signed_part(self) -> bytes:
        return self.hashed_part + self.message_hash


def parse_lxmf(lxmf: bytes) -> Message:
    """An LXMF message with its destination hash in front. The stamp is not
    part of what is hashed, so a fifth element is cut out of the payload
    before hashing, as `LXMessage.unpack_from_bytes` does."""
    if len(lxmf) <= PAYLOAD_OFFSET:
        raise ValueError(f"an LXMF message is more than {PAYLOAD_OFFSET} bytes, not {len(lxmf)}")
    payload = lxmf[PAYLOAD_OFFSET:]
    _, spans = array_spans(payload)
    if len(spans) not in (4, 5):
        raise ValueError(f"an LXMF payload has four or five elements, not {len(spans)}")
    timestamp, title, content, fields = (s[0] for s in spans[:4])
    if not isinstance(timestamp, float):
        raise ValueError(f"the timestamp is a {type(timestamp).__name__}, not a float")
    for name, value in (("title", title), ("content", content)):
        if not isinstance(value, bytes):
            raise ValueError(f"the {name} is a {type(value).__name__}, not bytes")
    if not isinstance(fields, dict):
        raise ValueError(f"the fields are a {type(fields).__name__}, not a map")
    if len(spans) == 5:
        hashed_payload = b"\x94" + payload[spans[0][1] : spans[3][2]]
        stamp = spans[4][0]
    else:
        hashed_payload = payload
        stamp = None
    return Message(
        destination=lxmf[:HASH_LEN],
        source=lxmf[HASH_LEN:SIGNATURE_OFFSET],
        signature=lxmf[SIGNATURE_OFFSET:PAYLOAD_OFFSET],
        timestamp=timestamp,
        title=title,
        content=content,
        fields=fields,
        stamp=stamp,
        hashed_part=lxmf[:SIGNATURE_OFFSET] + hashed_payload,
    )


def content_size(timestamp: float, title: bytes, content: bytes, fields: Dict[object, object]) -> int:
    """What LXMF counts against the opportunistic limit: the packed payload
    less the timestamp and the structure around it."""
    return len(packb([timestamp, title, content, fields])) - TIMESTAMP_SIZE - STRUCT_OVERHEAD


# ------------------------------------------------------------------- what to check


class Expect(NamedTuple):
    announces: int
    proofs: int
    messages: int
    # Proofs for messages sent a second time (`--repeat`).
    repeat_proofs: int = 0


class Seen(NamedTuple):
    announces: int
    bad_announces: int
    proofs: int
    messages: int
    bad_messages: int
    repeat_proofs: int = 0


def shortfalls(expect: Expect, seen: Seen) -> List[str]:
    """What the run was asked to see and did not, or saw wrong. Empty is a pass."""
    out = []
    if seen.announces < expect.announces:
        out.append(f"{seen.announces} of {expect.announces} announces from the board")
    if seen.bad_announces:
        out.append(f"{seen.bad_announces} announces from the board that did not check out")
    if seen.proofs < expect.proofs:
        out.append(f"{seen.proofs} of {expect.proofs} delivery proofs")
    if seen.repeat_proofs < expect.repeat_proofs:
        out.append(f"{seen.repeat_proofs} of {expect.repeat_proofs} proofs for messages sent again")
    if seen.messages < expect.messages:
        out.append(f"{seen.messages} of {expect.messages} messages from the board")
    if seen.bad_messages:
        out.append(f"{seen.bad_messages} messages from the board that did not check out")
    return out


# How long the run goes on after the last message from the board, by
# default, before it may end: Reticulum proves a message as it arrives, but
# the proof still has to wait for a clear channel and go out through the
# RNode, and the board then has to hear it and say so in its log. A run that
# ended on the message itself left the board's last reply unproved.
LINGER_S = 8.0


def may_end(met: bool, last_message_at: Optional[float], now: float, linger: float) -> bool:
    """Whether a run that has seen everything it expected may end now: not
    until `linger` seconds after the last message from the board, if any came,
    so that message's proof goes out and is heard."""
    if not met:
        return False
    return last_message_at is None or now - last_message_at >= linger


def parse_hash(text: str) -> bytes:
    """A destination hash as Reticulum prints it, with or without its brackets."""
    h = text.strip().strip("<>")
    try:
        b = bytes.fromhex(h)
    except ValueError:
        raise ValueError(f"{text!r} is not a hash") from None
    if len(b) != HASH_LEN:
        raise ValueError(f"a destination hash is {HASH_LEN} bytes, not {len(b)}")
    return b


# ------------------------------------------------------------------------ the peer


class Run:
    """Everything the peer saw, and the lock that guards it: Reticulum calls
    back from its own threads."""

    def __init__(self, out_dir: str, board: Optional[bytes]):
        self.t0 = time.time()
        self.lock = threading.Lock()
        self.events = open(os.path.join(out_dir, "events.log"), "a")
        self.board = board
        self.board_identity = None
        self.announces = 0
        self.bad_announces = 0
        self.random_hashes = set()
        # The messages proved, by number: LXMF reports a message delivered
        # once per proof, and a message sent twice can be proved twice.
        self.proved = set()
        # The same, for each message sent a second time as a new packet.
        self.proved_again = set()
        self.messages = 0
        self.bad_messages = 0
        # When the last message from the board came, by the clock.
        self.last_message_at: Optional[float] = None
        # When to announce again, in answer to the board's first announce.
        self.answer_at: Optional[float] = None

    def say(self, msg: str):
        line = f"[{time.time() - self.t0:8.3f}] {msg}"
        with self.lock:
            print(line, flush=True)
            self.events.write(line + "\n")
            self.events.flush()

    def prove(self, number: int) -> bool:
        """Note a proof for message `number`; whether it is the first."""
        with self.lock:
            first = number not in self.proved
            self.proved.add(number)
            return first

    def prove_again(self, number: int) -> bool:
        """Note a proof for message `number` sent again; whether it is the first."""
        with self.lock:
            first = number not in self.proved_again
            self.proved_again.add(number)
            return first

    def seen(self) -> Seen:
        with self.lock:
            return Seen(
                self.announces,
                self.bad_announces,
                len(self.proved),
                self.messages,
                self.bad_messages,
                len(self.proved_again),
            )


def hexrep(b: bytes) -> str:
    return f"<{b.hex()}>"


def peer_main(args, out_dir: str) -> int:
    import RNS
    import LXMF

    expect = Expect(
        args.expect_announces,
        args.send if args.expect_proofs is None else args.expect_proofs,
        args.expect_messages,
        args.send if args.repeat else 0,
    )
    run = Run(out_dir, parse_hash(args.board) if args.board else None)

    configdir = os.path.join(out_dir, "reticulum")
    os.makedirs(configdir, exist_ok=True)
    radio = {key: getattr(args, key) for key in ax.DEFAULT_RADIO}
    with open(os.path.join(configdir, "config"), "w") as f:
        f.write(ax.config_text(args.rnode, radio))
    rns_log = open(os.path.join(out_dir, "reticulum.log"), "a")
    errors: List[str] = []

    def to_log(msg):
        rns_log.write(msg + "\n")
        rns_log.flush()
        if "[Error]" in msg:
            errors.append(msg)

    RNS.Reticulum(configdir=configdir, loglevel=RNS.LOG_DEBUG, logdest=to_log)

    os.makedirs(os.path.dirname(os.path.abspath(args.identity)), exist_ok=True)
    if os.path.exists(args.identity):
        identity = RNS.Identity.from_file(args.identity)
    else:
        identity = RNS.Identity()
        identity.to_file(args.identity)
    router = LXMF.LXMRouter(identity=identity, storagepath=os.path.join(out_dir, "lxmf"))
    delivery = router.register_delivery_identity(identity, display_name=args.name)
    run.say(f"peer {hexrep(delivery.hash)} {args.name!r}, identity {hexrep(identity.hash)} from {args.identity}")

    try:
        iface = ax.wait_online(RNS, args.timeout)
    except TimeoutError:
        # Reticulum retries a refused configuration for ever, and says why
        # only in its log: a locked radio, or settings the board cannot do.
        reason = f"the RNode on {args.rnode} did not come up in {args.timeout:.0f} s"
        if errors:
            reason += "; Reticulum's errors: " + " / ".join(e.split("]", 2)[-1].strip() for e in errors[-4:])
        run.say(reason)
        with open(os.path.join(out_dir, "summary.txt"), "w") as f:
            f.write(f"result: FAIL: {reason}\n")
        return 1
    run.say(f"RNode up on {args.rnode}: " + ", ".join(f"{k} {v}" for k, v in radio.items()))

    # Every frame, as it comes off the air, before Reticulum filters repeats
    # out. The signal is read here for the reason air_exchange gives: the
    # interface forgets it once the packet is queued.
    hand_up = iface.process_incoming

    def heard(data):
        try:
            frame(bytes(data), iface.r_stat_rssi, iface.r_stat_snr)
        except Exception as e:  # a report, never a reason to drop the frame
            run.say(f"could not read a frame of {len(data)} bytes: {e!r}")
        hand_up(data)

    def frame(raw: bytes, rssi, snr):
        packet = parse_packet(raw)
        if packet.packet_type != PACKET_ANNOUNCE:
            return
        a = parse_announce(packet)
        if a.name_hash != name_hash(ASPECT):
            run.say(f"announce {hexrep(a.destination)}, not {ASPECT} (name hash {a.name_hash.hex()}), rssi {rssi} snr {snr}")
            return
        if a.destination == delivery.hash:
            return
        problems = announce_problems(a)
        signer = RNS.Identity(create_keys=False)
        signer.load_public_key(a.public_key)
        if not signer.validate(a.signature, a.signed_data):
            problems.append("the signature does not verify")
        name, cost = lxmf_app_data(a.app_data) if not any(p.startswith("app data") for p in problems) else (None, None)
        with run.lock:
            if run.board is None and not problems:
                run.board = a.destination
            ours = a.destination == run.board
            repeat = a.random_hash in run.random_hashes
            run.random_hashes.add(a.random_hash)
            # A bad path response is as bad as a bad announce; a good one is
            # not the board announcing, so it does not count towards one.
            if ours and not repeat and problems:
                run.bad_announces += 1
            elif ours and not repeat and not a.path_response:
                run.announces += 1
                # A board that came up after the peer missed its
                # announce, and cannot answer a peer it has not heard.
                if run.announces == 1:
                    run.answer_at = time.time() + ANSWER_DELAY
        who = "board" if ours else "other"
        what = ("repeat of a path response" if repeat else "path response") if a.path_response else ("repeat of an announce" if repeat else "announce")
        detail = (
            f"{what} from {who} {hexrep(a.destination)} name={name!r} stamp_cost={cost} "
            f"hops={a.hops} ratchet={'yes' if a.ratchet else 'no'} emitted={a.emitted} "
            f"{len(raw)} bytes rssi {rssi} snr {snr}"
        )
        run.say(detail + (": " + "; ".join(problems) if problems else ", signature ok"))

    iface.process_incoming = heard

    def on_lxmf(message):
        problems = []
        try:
            parsed = parse_lxmf(message.packed)
            if parsed.message_hash != message.hash:
                problems.append("its hash is not the one LXMF computed")
            if parsed.destination != delivery.hash:
                problems.append(f"it is for {hexrep(parsed.destination)}")
        except (ValueError, IndexError) as e:
            problems.append(f"framing: {e}")
        if not message.signature_validated:
            reason = {LXMF.LXMessage.SOURCE_UNKNOWN: "source unknown", LXMF.LXMessage.SIGNATURE_INVALID: "signature invalid"}
            problems.append("LXMF did not validate it: " + reason.get(message.unverified_reason, str(message.unverified_reason)))
        with run.lock:
            ours = message.source_hash == run.board
            if ours:
                run.last_message_at = time.time()
                if problems:
                    run.bad_messages += 1
                else:
                    run.messages += 1
        run.say(
            f"LXMF from {'board' if ours else 'other'} {hexrep(message.source_hash)}: "
            f"title={message.title_as_string()!r} content={message.content_as_string()!r} "
            f"timestamp={message.timestamp:.3f} {message.packed_size} bytes packed "
            f"signature_validated={message.signature_validated} method={message.method} "
            f"rssi={getattr(message, 'rssi', None)} snr={getattr(message, 'snr', None)}"
            + (": " + "; ".join(problems) if problems else "")
        )

    router.register_delivery_callback(on_lxmf)

    class BoardAnnounces:
        """Tells the peer when Reticulum has accepted the board's announce,
        so its identity can be recalled to send to."""

        aspect_filter = ASPECT

        def received_announce(self, destination_hash, announced_identity, app_data, *_, **__):
            with run.lock:
                if destination_hash == run.board and run.board_identity is None:
                    run.board_identity = announced_identity

    RNS.Transport.register_announce_handler(BoardAnnounces())

    sent = 0
    in_flight = None  # (message, time it was handed to the router, number)
    again = None  # (message, number, when to send it again), with --repeat
    next_send: Optional[float] = None  # set once the board can be sent to
    next_announce = time.time()
    deadline = time.time() + args.seconds

    def resolved(number, start):
        def on_delivered(message):
            what = "delivered: proof" if run.prove(number) else "proved again"
            run.say(f"message {number} {what} after {time.time() - start:.3f} s")

        def on_failed(message):
            run.say(f"message {number} failed after {time.time() - start:.3f} s, state 0x{message.state:02x}")

        return on_delivered, on_failed

    try:
        while time.time() < deadline:
            now = time.time()
            if args.no_announce:
                next_announce = float("inf")
            if now >= next_announce:
                router.announce(delivery.hash)
                run.say(f"announced {hexrep(delivery.hash)}")
                next_announce = now + args.announce_every if args.announce_every > 0 else float("inf")
                for n in range(1, args.flood + 1):
                    throwaway = RNS.Destination(RNS.Identity(), RNS.Destination.IN, RNS.Destination.SINGLE, "lxmf", "delivery")
                    throwaway.announce(app_data=lxmf_name_app_data(flood_name(n)))
                    run.say(f"flood: announced {hexrep(throwaway.hash)} as {flood_name(n)!r}")
                args.flood = 0
            with run.lock:
                answer, board_identity = run.answer_at is not None and now >= run.answer_at, run.board_identity
                if answer:
                    run.answer_at = None
            if answer and not args.no_announce:
                router.announce(delivery.hash)
                run.say(f"announced {hexrep(delivery.hash)} in answer to the board's first announce")
            if board_identity is not None and next_send is None:
                # Not straight away: a board that has just announced may be
                # about to send again, and the two would meet on the air.
                next_send = now + args.settle
            if in_flight and in_flight[0].state in (LXMF.LXMessage.DELIVERED, LXMF.LXMessage.FAILED, LXMF.LXMessage.REJECTED, LXMF.LXMessage.CANCELLED):
                if args.repeat and in_flight[0].state == LXMF.LXMessage.DELIVERED:
                    again = (in_flight[0], in_flight[2], now + args.gap)
                in_flight = None
                next_send = now + args.gap
            if again is not None and now >= again[2]:
                # What LXMF does when a proof is lost: the packed message in a
                # new packet, encrypted afresh, with the same message hash.
                message, number, _ = again
                again = None
                receipt = message._LXMessage__as_packet().send()

                def proved_again(_receipt, number=number, start=now):
                    what = "proof" if run.prove_again(number) else "proved again"
                    run.say(f"message {number} sent again: {what} after {time.time() - start:.3f} s")

                if receipt:
                    receipt.set_delivery_callback(proved_again)
                run.say(f"message {number} sent again as a new packet, id {message.hash.hex()}")
                next_send = now + args.gap
            if sent < args.send and in_flight is None and next_send is not None and now >= next_send:
                sent += 1
                destination = RNS.Destination(board_identity, RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery")
                text = args.text.format(n=sent, time=time.strftime("%H:%M:%S"))
                message = LXMF.LXMessage(destination, delivery, text, title=args.title, desired_method=LXMF.LXMessage.OPPORTUNISTIC)
                on_delivered, on_failed = resolved(sent, now)
                message.register_delivery_callback(on_delivered)
                message.register_failed_callback(on_failed)
                router.handle_outbound(message)
                run.say(f"message {sent} sent to {hexrep(destination.hash)}: {text!r}, {message.packed_size} bytes packed, id {message.hash.hex()}")
                in_flight = (message, now, sent)
            met = not shortfalls(expect, run.seen()) and sent >= args.send and in_flight is None and again is None
            with run.lock:
                last_message_at = run.last_message_at
            if not args.listen and may_end(met, last_message_at, now, args.linger):
                break
            time.sleep(0.1)
    except KeyboardInterrupt:
        run.say("interrupted")

    seen = run.seen()
    missing = shortfalls(expect, seen)
    lines = [
        f"peer run, {time.strftime('%Y-%m-%d %H:%M:%S %Z', time.localtime(run.t0))}, {time.time() - run.t0:.1f} s",
        f"rnode: {args.rnode}; radio: " + ", ".join(f"{k} {v}" for k, v in radio.items()),
        f"peer: {hexrep(delivery.hash)}; board: {hexrep(run.board) if run.board else 'not heard'}",
        f"announces from the board: {seen.announces} (expected {expect.announces}), {seen.bad_announces} bad",
        f"messages sent: {sent}; proofs: {seen.proofs} (expected {expect.proofs})"
        + (f"; sent again: proofs {seen.repeat_proofs} (expected {expect.repeat_proofs})" if args.repeat else ""),
        f"messages from the board: {seen.messages} (expected {expect.messages}), {seen.bad_messages} bad",
        "result: " + ("pass" if not missing else "FAIL: " + "; ".join(missing)),
    ]
    with open(os.path.join(out_dir, "summary.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print()
    print("\n".join(lines))
    print(f"\nlogs in {out_dir}", flush=True)
    run.events.close()
    return 0 if not missing else 1


# ------------------------------------------------------------------- the host side


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rnode", required=True, help="KISS port of the bench board the peer speaks through")
    ap.add_argument("--board", help="the board's lxmf.delivery hash (default: the first one heard that checks out)")
    ap.add_argument("--board-log", help="USB serial number of the board under test, to keep its log")
    ap.add_argument("--rnode-log", help="the RNode's USB serial number, or 'none' (default: looked up from --rnode)")
    ap.add_argument("--board-elf", default="target/node/thumbv7em-none-eabihf/release/rnode", help="the ELF the board under test runs, for its log")
    ap.add_argument("--rnode-elf", default=ax.DEFAULT_ELF, help="the ELF the RNode runs, for its log")
    ap.add_argument("--expect-announces", type=int, default=1, help="announces from the board to see (default 1)")
    ap.add_argument("--send", type=int, default=0, help="opportunistic messages to send the board, one at a time (default 0)")
    ap.add_argument("--expect-proofs", type=int, help="delivery proofs to see (default: one per message sent)")
    ap.add_argument("--repeat", action="store_true", help="send each delivered message again as a new packet, and expect a proof for that too")
    ap.add_argument("--no-announce", action="store_true", help="never announce: the board has to ask for the peer's key")
    ap.add_argument("--expect-messages", type=int, default=0, help="messages from the board to see (default 0)")
    ap.add_argument("--text", default="message {n} from the bench peer at {time}", help="what to send; {n} and {time} are filled in")
    ap.add_argument("--title", default="", help="the messages' title (default none)")
    ap.add_argument("--settle", type=float, default=5.0, help="seconds between the board's first announce and the first message (default 5)")
    ap.add_argument("--gap", type=float, default=10.0, help="seconds between one message resolving and the next going (default 10)")
    ap.add_argument("--flood", type=int, default=0, help="announce this many throwaway lxmf.delivery destinations, back to back, after the peer's own first announce (default 0)")
    ap.add_argument("--announce-every", type=float, default=0.0, help="seconds between the peer's own announces (default 0: once, at the start)")
    ap.add_argument("--seconds", type=float, default=120.0, help="the longest the run lasts (default 120)")
    ap.add_argument("--listen", action="store_true", help="run all --seconds, even once everything expected has happened")
    ap.add_argument("--linger", type=float, default=LINGER_S, help=f"seconds to go on after the last message from the board, so its proof goes out (default {LINGER_S:g})")
    ap.add_argument("--name", default=DISPLAY_NAME, help=f"the peer's display name (default {DISPLAY_NAME!r})")
    ap.add_argument("--identity", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), ".peer", "identity"), help="the peer's identity, made on first use and kept")
    ap.add_argument("--timeout", type=float, default=30.0, help="seconds to wait for the RNode to come up (default 30)")
    ap.add_argument("--out", help="where the logs go (default target/peer/<time>)")
    for key, value in ax.DEFAULT_RADIO.items():
        ap.add_argument(f"--{key}", type=int, default=value, help=f"(default {value})")
    args = ap.parse_args()

    if args.board:
        try:
            parse_hash(args.board)
        except ValueError as e:
            ap.error(str(e))
    try:
        args.text.format(n=1, time="00:00:00")
    except (KeyError, IndexError, ValueError) as e:
        ap.error(f"--text: {e!r}")
    longest = content_size(time.time(), args.title.encode("utf-8"), args.text.format(n=args.send, time="00:00:00").encode("utf-8"), {})
    if args.send and longest > OPPORTUNISTIC_MAX_CONTENT:
        ap.error(f"a message of {longest} bytes of content does not go opportunistically; the most is {OPPORTUNISTIC_MAX_CONTENT}")

    # Reticulum and LXMF live in whichever Python has them; run there.
    import importlib.util

    if importlib.util.find_spec("RNS") is None or importlib.util.find_spec("LXMF") is None:
        interpreter = ax.find_interpreter()
        if interpreter == sys.executable or os.environ.get("OXINODE_PEER_REEXEC"):
            sys.exit("peer: this Python has no RNS or no LXMF; run it with the Python Reticulum is installed in")
        os.environ["OXINODE_PEER_REEXEC"] = "1"
        os.execv(interpreter, [interpreter, os.path.abspath(__file__)] + sys.argv[1:])

    out_dir = args.out or os.path.join("target", "peer", time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(out_dir, exist_ok=True)

    logs = []
    if args.board_log and args.board_log.lower() != "none":
        logs.append(("board", args.board_log, args.board_elf))
    rnode_log = args.rnode_log
    if rnode_log is None:
        rnode_log = ax.usb_serial_for(args.rnode, ax.list_ports(sys.executable))
    if rnode_log and rnode_log.lower() != "none":
        logs.append(("rnode", rnode_log, args.rnode_elf))
    captures = []
    for name, serial, elf in logs:
        if not os.path.exists(elf):
            sys.exit(f"peer: no ELF at {elf} to decode the {name}'s log with")
    if logs and not shutil.which("defmt-print"):
        sys.exit("peer: defmt-print is not installed (cargo install defmt-print)")
    for name, serial, elf in logs:
        captures.append(ax.LogCapture(serial, elf, os.path.join(out_dir, f"{name}.defmt.log")))
    if captures:
        time.sleep(1.0)  # the boot ring drains a moment after the reader attaches

    code = 1
    try:
        code = peer_main(args, out_dir)
    finally:
        time.sleep(0.5)
        for c in captures:
            c.stop()
        sys.stdout.flush()
        # Reticulum's and LXMF's threads do not all end on their own.
        os._exit(code)
    return code


if __name__ == "__main__":
    sys.exit(main())
