#!/usr/bin/env python3
"""Exchange packets between two RNodes over the air, through Reticulum.

The one test that settles whether the air side of an oxinode is a stock
RNode's is an exchange with a stock RNode: `rnsd` on each, a packet under
254 bytes each way and a 400-byte packet each way, so the split at 254 and the
reassembly are exercised in both directions, and the logs of both sides kept.
This runs that exchange on one host, with both boards attached to it.

Each side is a Reticulum instance of its own (a private config directory, no
shared instance, one `RNodeInterface`), so what goes on the air is exactly what
`rnsd` would put there. The two are separate processes because Reticulum is a
singleton per process. For every size, in each direction, a receiver comes up
and listens on a plain destination, a sender comes up and sends one packet of
random bytes to it, and the receiver reports what it got with the signal it
came with. The sender and receiver compare hashes; the packet's size on the
air is asserted from `Packet.raw`, so a 400-byte exchange is 400 bytes on the
air, not 400 bytes of payload.

Every oxinode's `defmt` log is captured from its log port for the whole run,
so each frame's header byte and sequence nibble, and every join, are in the
notes alongside Reticulum's own log of the interface. A stock RNode has no
such port; its side of the story is Reticulum's log.

    tools/air_exchange.py --a /dev/cu.usbmodem101 --b /dev/cu.usbmodem3101

Everything it needs to know about Reticulum's framing is in the constants at
the top, and the functions above the peer are pure so `test_air_exchange.py`
can check the plan, the sizes and the config without a board or RNS.
"""

import argparse
import hashlib
import importlib.util
import os
import secrets
import shutil
import subprocess
import sys
import time
from typing import Dict, List, NamedTuple, Optional, Sequence

# --------------------------------------------------------------- Reticulum's framing

# A plain packet with a single-hash header: flags, hops, the 16-byte
# destination hash, then the payload. `RNS.Reticulum.HEADER_MINSIZE`.
HEADER_1_OVERHEAD = 2 + 16 + 1
# `RNS.Reticulum.MTU`, and its MDU: the MTU less the largest header and the
# smallest interface access code. What a plain packet may carry.
MTU = 500
MDU = MTU - (2 + 1 + 32) - 1
RAW_MIN = HEADER_1_OVERHEAD + 1
RAW_MAX = HEADER_1_OVERHEAD + MDU

# Where an oxinode splits: a frame is 255 bytes with one of header, so a packet
# of more than 254 goes as two frames. The default sizes sit either side.
FRAME_PAYLOAD = 254
DEFAULT_SIZES = (200, 400)

APP_NAME = "oxinode"
ASPECTS = ("air", "exchange")

DEFAULT_RADIO = {
    "frequency": 915000000,
    "bandwidth": 125000,
    "txpower": 14,
    "spreadingfactor": 8,
    "codingrate": 5,
}

DEFAULT_ELF = "target/thumbv7em-none-eabihf/release/rnode"


def data_len_for_raw(raw_len: int) -> int:
    """How much payload makes a plain packet of `raw_len` bytes on the air."""
    if not RAW_MIN <= raw_len <= RAW_MAX:
        raise ValueError(
            f"a plain packet is {RAW_MIN} to {RAW_MAX} bytes on the air, not {raw_len}"
        )
    return raw_len - HEADER_1_OVERHEAD


def log_port_for(port: str) -> Optional[str]:
    """The log port that goes with an oxinode's KISS port, where that is knowable.

    On macOS the two CDC ports of one board enumerate as `...XXX1` and
    `...XXX3`. Anywhere else the pairing is not knowable from the name.
    """
    if port.startswith("/dev/cu.usbmodem") and port.endswith("1"):
        return port[:-1] + "3"
    return None


def config_text(port: str, radio: Dict[str, int]) -> str:
    """A Reticulum config with one RNode on `port` and nothing else.

    No shared instance, so two of these can run on one host; no transport,
    so nothing but the one packet is ever sent.
    """
    lines = [
        "[reticulum]",
        "  enable_transport = No",
        "  share_instance = No",
        "  panic_on_interface_error = No",
        "",
        "[interfaces]",
        "  [[air]]",
        "    type = RNodeInterface",
        "    interface_enabled = True",
        f"    port = {port}",
    ]
    for key in ("frequency", "bandwidth", "txpower", "spreadingfactor", "codingrate"):
        lines.append(f"    {key} = {radio[key]}")
    return "\n".join(lines) + "\n"


class Side(NamedTuple):
    name: str
    port: str
    log_port: Optional[str]


class Exchange(NamedTuple):
    sender: str
    receiver: str
    raw_len: int


def plan(sizes: Sequence[int], names: Sequence[str] = ("A", "B")) -> List[Exchange]:
    """Every size, both ways: the small one each way, then the large one."""
    a, b = names
    out = []
    for size in sizes:
        out.append(Exchange(a, b, size))
        out.append(Exchange(b, a, size))
    return out


def parse_report(line: str) -> Dict[str, object]:
    """`RECEIVED raw=400 data=381 sha256=... rssi=-40 snr=9.5` as a dict."""
    words = line.split()
    out: Dict[str, object] = {"kind": words[0]}
    for word in words[1:]:
        key, _, value = word.partition("=")
        if key in ("raw", "data"):
            out[key] = int(value)
        elif key in ("rssi", "snr"):
            out[key] = None if value == "None" else float(value)
        else:
            out[key] = value
    return out


def verdict(sent: Dict[str, object], received: Optional[Dict[str, object]]) -> str:
    """One word on an exchange: `ok`, `lost`, or what disagreed."""
    if received is None or received.get("kind") != "RECEIVED":
        return "lost"
    if received.get("raw") != sent.get("raw"):
        return f"size {received.get('raw')} != {sent.get('raw')}"
    if received.get("sha256") != sent.get("sha256"):
        return "payload differs"
    return "ok"


def interpreter_from_shebang(text: str) -> Optional[str]:
    """The interpreter a script's first line names, or None."""
    first = text.splitlines()[0] if text else ""
    if not first.startswith("#!"):
        return None
    words = first[2:].split()
    if not words:
        return None
    if os.path.basename(words[0]) == "env" and len(words) > 1:
        return words[1]
    return words[0]


def find_interpreter() -> str:
    """A Python that can import RNS: this one, or the one `rnsd` runs under."""
    if importlib.util.find_spec("RNS") is not None:
        return sys.executable
    rnsd = shutil.which("rnsd")
    if rnsd:
        with open(rnsd, "r", errors="replace") as f:
            head = f.read(512)
        named = interpreter_from_shebang(head)
        if named:
            found = shutil.which(named) if not os.path.isabs(named) else named
            if found and os.path.exists(found):
                return found
    sys.exit(
        "air_exchange: this Python has no RNS module and no rnsd is on PATH; "
        "run this with the Python that has Reticulum installed"
    )


# ------------------------------------------------------------------------ the peer
#
# One side of one exchange, in a process of its own. Speaks a few words on
# stdout -- READY, SENT ..., RECEIVED ..., TIMEOUT -- and takes SEND <hex> and
# DONE on stdin. Reticulum's own log goes to a file, so the words are the only
# thing on stdout.


def wait_online(RNS, timeout: float):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for iface in list(RNS.Transport.interfaces):
            if getattr(iface, "online", False):
                return iface
        time.sleep(0.1)
    raise TimeoutError("the RNode did not come up")


def peer_main(role: str, configdir: str, log_path: str, timeout: float) -> int:
    import RNS

    log = open(log_path, "a")

    def to_log(msg):
        log.write(msg + "\n")
        log.flush()

    def say(line: str):
        sys.stdout.write(line + "\n")
        sys.stdout.flush()

    def take(prefix: str) -> Optional[str]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = sys.stdin.readline()
            if not line:
                return None
            if line.startswith(prefix):
                return line.strip()
        return None

    RNS.Reticulum(configdir=configdir, loglevel=RNS.LOG_DEBUG, logdest=to_log)
    code = 1
    try:
        if role == "receive":
            dest = RNS.Destination(None, RNS.Destination.IN, RNS.Destination.PLAIN, APP_NAME, *ASPECTS)
            got = []
            dest.set_packet_callback(lambda data, packet: got.append((data, packet)))
            iface = wait_online(RNS, timeout)
            # The interface reports the signal (STAT_RSSI, STAT_SNR ahead of
            # the data) and then forgets it as soon as it has queued the
            # packet, before the packet is stamped with it. So it is read
            # here, as the frame comes in, which is the reading the RNode
            # sent with this packet.
            signal: Dict[str, object] = {}
            hand_up = iface.process_incoming

            def with_signal(data):
                signal["rssi"] = iface.r_stat_rssi
                signal["snr"] = iface.r_stat_snr
                hand_up(data)

            iface.process_incoming = with_signal
            say("READY")
            deadline = time.monotonic() + timeout
            while not got and time.monotonic() < deadline:
                time.sleep(0.05)
            if got:
                data, packet = got[0]
                rssi = packet.rssi if packet.rssi is not None else signal.get("rssi")
                snr = packet.snr if packet.snr is not None else signal.get("snr")
                say(
                    f"RECEIVED raw={len(packet.raw)} data={len(data)} "
                    f"sha256={hashlib.sha256(data).hexdigest()} "
                    f"rssi={rssi} snr={snr}"
                )
                code = 0
            else:
                say("TIMEOUT")
        else:
            dest = RNS.Destination(None, RNS.Destination.OUT, RNS.Destination.PLAIN, APP_NAME, *ASPECTS)
            wait_online(RNS, timeout)
            say("READY")
            order = take("SEND ")
            if order is None:
                say("TIMEOUT")
            else:
                data = bytes.fromhex(order.split(" ", 1)[1])
                packet = RNS.Packet(dest, data)
                packet.send()
                say(
                    f"SENT raw={len(packet.raw)} data={len(data)} "
                    f"sha256={hashlib.sha256(data).hexdigest()}"
                )
                code = 0
        take("DONE")
    except Exception as e:  # reported as a word, so the parent can go on
        say(f"ERROR {e!r}")
    finally:
        log.close()
        try:
            RNS.Reticulum.exit_handler()
        except Exception:
            pass
        os._exit(code)
    return code


# ------------------------------------------------------------------- the host side


class LogCapture:
    """`defmt-print` on an oxinode's log port, for the whole run."""

    def __init__(self, port: str, elf: str, path: str):
        flag = "-f" if sys.platform == "darwin" else "-F"
        subprocess.run(["stty", flag, port, "115200"], check=True)
        fd = os.open(port, os.O_RDONLY | os.O_NOCTTY)
        self.file = open(path, "wb")
        try:
            self.proc = subprocess.Popen(
                ["defmt-print", "-e", elf], stdin=fd, stdout=self.file, stderr=subprocess.STDOUT
            )
        finally:
            os.close(fd)

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.file.close()


class Peer:
    def __init__(self, interpreter: str, role: str, configdir: str, log_path: str, timeout: float):
        self.proc = subprocess.Popen(
            [
                interpreter,
                os.path.abspath(__file__),
                "--peer",
                role,
                "--config",
                configdir,
                "--peer-log",
                log_path,
                "--timeout",
                str(timeout),
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(log_path + ".stderr", "wb"),
            text=True,
        )
        self.timeout = timeout

    def expect(self, *prefixes: str) -> Optional[str]:
        deadline = time.monotonic() + self.timeout + 5
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                return None
            line = line.strip()
            if line.startswith(prefixes) or line.startswith("ERROR"):
                return line
        return None

    def tell(self, line: str):
        try:
            self.proc.stdin.write(line + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError):
            pass

    def finish(self):
        self.tell("DONE")
        try:
            self.proc.wait(10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Result(NamedTuple):
    exchange: Exchange
    sent: Dict[str, object]
    received: Optional[Dict[str, object]]
    verdict: str


def run_exchange(
    index: int,
    ex: Exchange,
    sides: Dict[str, Side],
    radio: Dict[str, int],
    out_dir: str,
    interpreter: str,
    timeout: float,
) -> Result:
    tag = f"{index:02d}-{ex.sender}-to-{ex.receiver}-{ex.raw_len}"
    dirs = {}
    for who in (ex.receiver, ex.sender):
        d = os.path.join(out_dir, f"{tag}.{who}.reticulum")
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, "config"), "w") as f:
            f.write(config_text(sides[who].port, radio))
        dirs[who] = d

    data = secrets.token_bytes(data_len_for_raw(ex.raw_len))
    receiver = Peer(interpreter, "receive", dirs[ex.receiver], os.path.join(out_dir, f"{tag}.{ex.receiver}.rns.log"), timeout)
    sender: Optional[Peer] = None
    sent: Dict[str, object] = {"kind": "SENT", "raw": ex.raw_len, "sha256": hashlib.sha256(data).hexdigest()}
    received: Optional[Dict[str, object]] = None
    try:
        ready = receiver.expect("READY")
        if ready != "READY":
            return Result(ex, sent, None, f"receiver did not come up: {ready}")
        sender = Peer(interpreter, "send", dirs[ex.sender], os.path.join(out_dir, f"{tag}.{ex.sender}.rns.log"), timeout)
        ready = sender.expect("READY")
        if ready != "READY":
            return Result(ex, sent, None, f"sender did not come up: {ready}")
        sender.tell("SEND " + data.hex())
        said = sender.expect("SENT", "TIMEOUT")
        if not said or not said.startswith("SENT"):
            return Result(ex, sent, None, f"sender did not send: {said}")
        sent = parse_report(said)
        if sent.get("raw") != ex.raw_len:
            return Result(ex, sent, None, f"Reticulum made a {sent.get('raw')}-byte packet, not {ex.raw_len}")
        heard = receiver.expect("RECEIVED", "TIMEOUT")
        received = parse_report(heard) if heard else None
        return Result(ex, sent, received, verdict(sent, received))
    finally:
        if sender:
            sender.finish()
        receiver.finish()


def summary_lines(results: Sequence[Result]) -> List[str]:
    lines = [f"{'#':>2}  {'direction':<10} {'bytes':>5}  {'rssi':>6}  {'snr':>5}  result"]
    for i, r in enumerate(results, 1):
        rssi = "" if not r.received or r.received.get("rssi") is None else f"{r.received['rssi']:.0f}"
        snr = "" if not r.received or r.received.get("snr") is None else f"{r.received['snr']:.1f}"
        lines.append(
            f"{i:>2}  {r.exchange.sender + ' -> ' + r.exchange.receiver:<10} {r.exchange.raw_len:>5}  {rssi:>6}  {snr:>5}  {r.verdict}"
        )
    return lines


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--a", help="KISS port of the first RNode")
    ap.add_argument("--b", help="KISS port of the second RNode")
    ap.add_argument("--a-name", default="A", help="what to call the first (default A)")
    ap.add_argument("--b-name", default="B", help="what to call the second (default B)")
    ap.add_argument("--a-log", help="the first's log port, or 'none' (default: derived for an oxinode on macOS)")
    ap.add_argument("--b-log", help="the second's log port, or 'none'")
    ap.add_argument("--elf", default=DEFAULT_ELF, help="the ELF the oxinodes were built from, for the log")
    ap.add_argument("--sizes", default=",".join(str(s) for s in DEFAULT_SIZES), help="packet sizes on the air, comma separated (default 200,400)")
    ap.add_argument("--timeout", type=float, default=30.0, help="seconds to wait for an RNode, or a packet (default 30)")
    ap.add_argument("--out", help="where the logs go (default target/air-exchange/<time>)")
    for key, value in DEFAULT_RADIO.items():
        ap.add_argument(f"--{key}", type=int, default=value, help=f"(default {value})")
    ap.add_argument("--peer", choices=("send", "receive"), help=argparse.SUPPRESS)
    ap.add_argument("--config", help=argparse.SUPPRESS)
    ap.add_argument("--peer-log", help=argparse.SUPPRESS)
    args = ap.parse_args()

    if args.peer:
        return peer_main(args.peer, args.config, args.peer_log, args.timeout)

    if not args.a or not args.b:
        ap.error("--a and --b are both needed")
    try:
        sizes = [int(s) for s in args.sizes.split(",") if s]
        for size in sizes:
            data_len_for_raw(size)
    except ValueError as e:
        ap.error(str(e))
    radio = {key: getattr(args, key) for key in DEFAULT_RADIO}

    def log_port(given: Optional[str], port: str) -> Optional[str]:
        if given:
            return None if given.lower() == "none" else given
        return log_port_for(port)

    sides = {
        args.a_name: Side(args.a_name, args.a, log_port(args.a_log, args.a)),
        args.b_name: Side(args.b_name, args.b, log_port(args.b_log, args.b)),
    }
    out_dir = args.out or os.path.join("target", "air-exchange", time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(out_dir, exist_ok=True)
    interpreter = find_interpreter()

    captures = []
    for side in sides.values():
        if side.log_port:
            if not os.path.exists(args.elf):
                sys.exit(f"air_exchange: no ELF at {args.elf} to decode {side.name}'s log with")
            if not shutil.which("defmt-print"):
                sys.exit("air_exchange: defmt-print is not installed (cargo install defmt-print)")
            captures.append(LogCapture(side.log_port, args.elf, os.path.join(out_dir, f"{side.name}.defmt.log")))
    if captures:
        time.sleep(1.0)  # the boot ring drains a moment after DTR

    results: List[Result] = []
    notes = [f"air exchange, {time.strftime('%Y-%m-%d %H:%M:%S %Z')}"]
    for name, side in sides.items():
        notes.append(f"{name}: {side.port}" + (f", log {side.log_port}" if side.log_port else ", no log port"))
    notes.append("radio: " + ", ".join(f"{k} {v}" for k, v in radio.items()))
    notes.append("")
    try:
        for i, ex in enumerate(plan(sizes, (args.a_name, args.b_name)), 1):
            print(f"[{i}] {ex.sender} -> {ex.receiver}, {ex.raw_len} bytes ...", flush=True)
            notes.append(f"[{i}] {time.strftime('%H:%M:%S')} {ex.sender} -> {ex.receiver}, {ex.raw_len} bytes")
            r = run_exchange(i, ex, sides, radio, out_dir, interpreter, args.timeout)
            results.append(r)
            print(f"    {r.verdict}", flush=True)
    finally:
        time.sleep(0.5)
        for c in captures:
            c.stop()

    lines = summary_lines(results)
    with open(os.path.join(out_dir, "summary.txt"), "w") as f:
        f.write("\n".join(notes + lines) + "\n")
    print()
    print("\n".join(lines))
    print(f"\nlogs in {out_dir}")
    return 0 if all(r.verdict == "ok" for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
