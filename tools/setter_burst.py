#!/usr/bin/env python3
"""Write RNode setters to a board back to back and count the ones it answers.

Issue #36: a synthetic host that wrote six setters with no reads between them
once lost roughly one four-byte frame in ten. This is that host, kept so the
measurement is repeatable rather than remembered:

    tools/setter_burst.py /dev/cu.usbmodem1101 --rounds 200
    tools/setter_burst.py /dev/cu.usbmodem1101 --rounds 100 --no-read
    tools/setter_burst.py /dev/cu.usbmodem1101 --rounds 100 --reopen

Each round writes six one-byte setters (spreading factor, coding rate, TX
power, twice each with different values) in one `write()`, and an RNode
answers every setter with the value it took. Alternate rounds use different
values, so a reply that arrives late is not mistaken for the next round's. By default the replies are read
after each round; `--no-read` reads nothing until every round is written, and
`--reopen` opens the port afresh for each round. Exits 1 if any setter went
unanswered.

The radio stays off throughout, so nothing is transmitted. Needs pyserial (the
Python Reticulum runs under has it).
"""

import argparse
import sys
import time
from typing import Iterable, List, NamedTuple, Sequence, Tuple

FEND = 0xC0
FESC = 0xDB
TFEND = 0xDC
TFESC = 0xDD

CMD_SF = 0x03
CMD_TXPOWER = 0x04
CMD_CR = 0x05

# Two of each, so a lost frame cannot be hidden by a later one with the same
# command and value; and a second set for odd rounds, so a late reply to one
# round cannot stand in for a lost one in the next.
SETTERS: Tuple[Tuple[int, int], ...] = (
    (CMD_SF, 7),
    (CMD_CR, 5),
    (CMD_TXPOWER, 2),
    (CMD_SF, 8),
    (CMD_CR, 6),
    (CMD_TXPOWER, 3),
)
SETTERS_ODD: Tuple[Tuple[int, int], ...] = (
    (CMD_SF, 9),
    (CMD_CR, 7),
    (CMD_TXPOWER, 4),
    (CMD_SF, 10),
    (CMD_CR, 8),
    (CMD_TXPOWER, 5),
)


def setters_for(round_number: int) -> Tuple[Tuple[int, int], ...]:
    return SETTERS_ODD if round_number % 2 else SETTERS


def burst(setters: Sequence[Tuple[int, int]] = SETTERS) -> bytes:
    """One round: every setter as a KISS frame, back to back."""
    return b"".join(bytes([FEND, cmd, value, FEND]) for cmd, value in setters)


def frames(stream: bytes) -> List[bytes]:
    """The complete KISS frames in `stream`, unescaped. Bytes before the first
    FEND and a frame still open at the end are not frames."""
    out: List[bytes] = []
    current = None
    escaped = False
    for b in stream:
        if b == FEND:
            if current:
                out.append(bytes(current))
            current = bytearray()
            escaped = False
        elif current is None:
            continue
        elif escaped:
            current.append({TFEND: FEND, TFESC: FESC}.get(b, b))
            escaped = False
        elif b == FESC:
            escaped = True
        else:
            current.append(b)
    return out


class Tally(NamedTuple):
    rounds: int
    rounds_with_a_loss: int
    unanswered: int
    by_position: Tuple[int, ...]


def tally(replies_per_round: Iterable[bytes]) -> Tally:
    """Which setters each round's replies answer, round `n` having been sent
    `setters_for(n)`.

    A reply answers a setter when it carries the same command and value.
    Replies to other commands (the board's unsolicited reports) are ignored.
    """
    rounds = lossy = unanswered = 0
    positions = [0] * len(SETTERS)
    for n, stream in enumerate(replies_per_round):
        rounds += 1
        setters = setters_for(n)
        got = [f[:2] for f in frames(stream) if len(f) >= 2]
        missing = [i for i, (cmd, value) in enumerate(setters) if bytes([cmd, value]) not in got]
        if missing:
            lossy += 1
            unanswered += len(missing)
            for i in missing:
                positions[i] += 1
    return Tally(rounds, lossy, unanswered, tuple(positions))


def tally_whole(stream: bytes, rounds: int) -> Tally:
    """For `--no-read`: one stream answering every round, counted by frame
    rather than by round, since which round a reply belongs to is not knowable."""
    wanted = {bytes([cmd, value]) for cmd, value in SETTERS + SETTERS_ODD}
    got = [f[:2] for f in frames(stream) if len(f) >= 2 and f[:2] in wanted]
    expected = rounds * len(SETTERS)
    missing = max(expected - len(got), 0)
    return Tally(rounds, 1 if missing else 0, missing, ())


def read_for(port, seconds: float) -> bytes:
    deadline = time.monotonic() + seconds
    buf = b""
    while time.monotonic() < deadline:
        buf += port.read(4096)
    return buf


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("port", help="the board's KISS port")
    ap.add_argument("--rounds", type=int, default=200)
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--no-read", action="store_true", help="read nothing until every round is written")
    mode.add_argument("--reopen", action="store_true", help="open the port afresh for each round")
    ap.add_argument("--wait", type=float, default=0.4, help="seconds to collect each round's replies (default 0.4)")
    args = ap.parse_args(argv)

    try:
        import serial
    except ImportError:
        sys.exit("setter_burst: needs pyserial; run it with the Python Reticulum runs under")

    def open_port():
        # A read never blocks past the collection window, or the window is
        # longer than it says.
        return serial.Serial(args.port, 115200, timeout=min(0.05, args.wait))

    if args.no_read:
        port = open_port()
        time.sleep(0.3)
        port.reset_input_buffer()
        for n in range(args.rounds):
            port.write(burst(setters_for(n)))
        port.timeout = 0.05
        result = tally_whole(read_for(port, 1.0 + args.rounds * 0.01), args.rounds)
        port.close()
        mode_name = "no reads until the end"
    else:
        streams = []
        port = None if args.reopen else open_port()
        if port:
            time.sleep(0.3)
            port.reset_input_buffer()
        for n in range(args.rounds):
            p = open_port() if args.reopen else port
            p.write(burst(setters_for(n)))
            streams.append(read_for(p, args.wait))
            if args.reopen:
                p.close()
        if port:
            port.close()
        result = tally(streams)
        mode_name = "reopened each round" if args.reopen else "read after each round"

    total = result.rounds * len(SETTERS)
    print(
        f"{args.port}, {mode_name}: {result.rounds} rounds, {result.rounds_with_a_loss} with a loss, "
        f"{result.unanswered}/{total} setters unanswered"
        + (f", by position {list(result.by_position)}" if result.by_position else "")
    )
    return 1 if result.unanswered else 0


if __name__ == "__main__":
    sys.exit(main())
