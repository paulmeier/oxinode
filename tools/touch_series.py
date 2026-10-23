#!/usr/bin/env python3
"""Reflash a board over and over by 1200-baud touch, each time with a phone's
Bluetooth link up, and count the times serial DFU does not answer.

Issue #23: a touch on the product image taken while a phone was connected was
twice seen, on 2026-09-07, to leave the board in its bootloader with serial DFU
not answering, recoverable only by a double-tap. This is that recipe, kept so
the measurement is repeatable rather than remembered:

    tools/touch_series.py --serial 65C11224153B1DFD --rounds 10
    tools/touch_series.py --serial 65C11224153B1DFD --rounds 10 --no-link

Each round waits for the board's log to show a phone's encrypted link in the
current boot, lets the link carry traffic for a few seconds, then runs
`dfu-flash.sh`, which takes the touch and flashes the ELF over serial DFU. The
board comes back up running the same image, the phone reconnects, and the next
round waits for that: a new boot in the log, and a new link in it. The series
starts with `CMD_RESET`, so the first round waits for the same, rather than for
a link that came up before the log was being read. `--no-link` waits for no
link, for a series with no phone to compare against.

A round flashes on the first try, after `dfu-flash.sh`'s retry (the known race
with the bootloader's enumeration, which is not this), or fails; the series
stops at the first failure, since a wedged board needs a double-tap before
anything else can happen. Exits 1 if a round failed.

The `defmt` log, every round's flasher output and a summary go to
target/touch-series/<time>. Needs pyserial, from the Python Reticulum runs
under, as `air_exchange.py` does; the board needs the RNode image, for its log.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import time
from typing import List, NamedTuple, Optional, Sequence

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from air_exchange import DEFAULT_ELF, OXINODE_VID_PID, LogCapture, PortInfo, find_interpreter, list_ports  # noqa: E402

# A decoded log line starts with the board's uptime in seconds.
_TIMESTAMP = re.compile(r"^(\d+\.\d+) ")

# What the RNode image logs once a phone's link is encrypted; see src/nus.rs.
LINK_UP = "nus: link encrypted"

# CMD_RESET with its confirm byte, as a KISS frame; see core/src/rnode/command.rs.
# The board restarts its application and leaves GPREGRET alone, so it is not a touch.
FEND = 0xC0
CMD_RESET = 0x55
CONFIRM_BYTE = 0xF8
RESET_FRAME = bytes([FEND, CMD_RESET, CONFIRM_BYTE, FEND])

# What `dfu-flash.sh` prints before its retry.
RETRIED = "the board is in its bootloader; retrying"

FIRST_TRY = "first try"
AFTER_RETRY = "after retry"
FAILED = "failed"


class Boot(NamedTuple):
    linked: bool


def boots(lines: Sequence[str]) -> List[Boot]:
    """The boots a decoded log shows, in order, and whether a link came up in each.

    A boot starts wherever the uptime goes backwards. Lines without an uptime
    (the reader's own notes) belong to no boot. The lines before the first
    reset are a boot too, though the log may have joined it partway.
    """
    out: List[Boot] = []
    last = None
    linked = False
    for line in lines:
        m = _TIMESTAMP.match(line)
        if not m:
            continue
        t = float(m.group(1))
        if last is not None and t < last:
            out.append(Boot(linked))
            linked = False
        last = t
        linked = linked or LINK_UP in line
    if last is not None:
        out.append(Boot(linked))
    return out


def outcome(returncode: int, output: str) -> str:
    """How one `dfu-flash.sh` run went."""
    if returncode != 0:
        return FAILED
    return AFTER_RETRY if RETRIED in output else FIRST_TRY


def summary(outcomes: Sequence[str]) -> str:
    first = outcomes.count(FIRST_TRY)
    retried = outcomes.count(AFTER_RETRY)
    failed = outcomes.count(FAILED)
    return (
        f"{len(outcomes)} touches: {first + retried} flashed "
        f"({first} first try, {retried} after the retry), {failed} failed"
    )


def port_for(serial: str, ports: Sequence[PortInfo]) -> Optional[str]:
    """The KISS port of the RNode-image board with this USB serial number."""
    for info in ports:
        if info.serial_number == serial and (info.vid, info.pid) == OXINODE_VID_PID:
            return info.device
    return None


def reset_board(interpreter: str, port: str) -> None:
    """Send CMD_RESET, at 115200 so the open is not itself a touch."""
    script = (
        "import sys, time, serial\n"
        "s = serial.Serial(sys.argv[1], 115200, timeout=0.1)\n"
        "time.sleep(0.2)\n"
        f"s.write(bytes({list(RESET_FRAME)}))\n"
        "s.flush()\n"
        "time.sleep(0.2)\n"
        "try:\n"
        "    s.close()\n"
        "except OSError:\n"
        "    pass  # the port went with the reset\n"
    )
    subprocess.run([interpreter, "-c", script, port], check=True)


def read_lines(path: str) -> List[str]:
    with open(path, "r", errors="replace") as f:
        return f.read().splitlines()


def wait_for(condition, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return True
        time.sleep(0.5)
    return condition()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--serial", required=True, help="USB serial number of the board")
    ap.add_argument("--rounds", type=int, default=10)
    ap.add_argument("--elf", default=DEFAULT_ELF, help="the image to flash each round, and to decode the log with")
    ap.add_argument("--no-link", action="store_true", help="do not wait for a phone's link")
    ap.add_argument("--settle", type=float, default=5.0, help="seconds of link before the touch (default 5)")
    ap.add_argument("--timeout", type=float, default=60.0, help="seconds to wait for a boot or a link (default 60)")
    ap.add_argument("--out", help="where the logs go (default target/touch-series/<time>)")
    args = ap.parse_args(argv)

    if not os.path.exists(args.elf):
        sys.exit(f"touch_series: no ELF at {args.elf}")
    if not shutil.which("defmt-print"):
        sys.exit("touch_series: defmt-print is not installed (cargo install defmt-print)")
    interpreter = find_interpreter()
    port = port_for(args.serial, list_ports(interpreter))
    if port is None:
        sys.exit(f"touch_series: no board with the RNode image and USB serial {args.serial} is attached")

    here = os.path.dirname(os.path.abspath(__file__))
    out_dir = args.out or os.path.join("target", "touch-series", time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(out_dir, exist_ok=True)
    log_path = os.path.join(out_dir, "defmt.log")
    capture = LogCapture(args.serial, args.elf, log_path)
    env = dict(os.environ, OXINODE_DFU_PORT=port, OXINODE_PYTHON=interpreter)

    def log_boots() -> List[Boot]:
        return boots(read_lines(log_path))

    notes = [
        f"touch series, {time.strftime('%Y-%m-%d %H:%M:%S %Z')}",
        f"{args.serial} on {port}, flashing {args.elf}",
        "no phone link required" if args.no_link else "a phone link required before each touch",
        "",
    ]
    outcomes: List[str] = []
    stopped = None
    try:
        time.sleep(1.0)  # the boot ring drains a moment after the reader attaches
        seen = len(log_boots())
        reset_board(interpreter, port)
        for n in range(1, args.rounds + 1):
            if not wait_for(lambda: len(log_boots()) > seen, args.timeout):
                stopped = f"round {n}: the log never showed the board coming back"
                break
            if not args.no_link and not wait_for(lambda: log_boots()[-1:] == [Boot(True)], args.timeout):
                stopped = f"round {n}: no phone link in {args.timeout:.0f} s"
                break
            time.sleep(args.settle)
            seen = len(log_boots())
            flash = subprocess.run(
                [os.path.join(here, "dfu-flash.sh"), args.elf],
                env=env, capture_output=True, text=True,
            )
            text = flash.stdout + flash.stderr
            with open(os.path.join(out_dir, f"round{n}.txt"), "w") as f:
                f.write(text)
            result = outcome(flash.returncode, text)
            outcomes.append(result)
            line = f"round {n}: {result}"
            print(line, flush=True)
            notes.append(f"{time.strftime('%H:%M:%S')} {line}")
            if result == FAILED:
                stopped = f"round {n} failed; see round{n}.txt. A wedged board needs a double-tap."
                break
    finally:
        time.sleep(0.5)
        capture.stop()

    lines = [summary(outcomes)] + ([stopped] if stopped else [])
    with open(os.path.join(out_dir, "summary.txt"), "w") as f:
        f.write("\n".join(notes + [""] + lines) + "\n")
    print()
    print("\n".join(lines))
    print(f"\nlogs in {out_dir}")
    if FAILED in outcomes:
        return 1
    return 2 if stopped else 0


if __name__ == "__main__":
    sys.exit(main())
