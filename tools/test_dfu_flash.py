#!/usr/bin/env python3
"""End-to-end smoke test for the flashing runner, without a board.

Three bugs shipped in this script before this test existed, all of the same
shape: a runtime failure under `set -u` that neither `bash -n` nor shellcheck
can see, because the script is syntactically fine and every variable *is*
assigned -- just not always before it is used. macOS's bash 3.2 makes it worse,
since expanding an empty array counts as unbound there.

So this runs the real script against a stub `adafruit-nrfutil`, checks it
survives to the end, and asserts it would have invoked the tool correctly.
Requires a built ELF; skipped when there is not one.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ELF = os.path.join(REPO, "target/thumbv7em-none-eabihf/release/blink")

STUB = """#!/usr/bin/env bash
# Stub adafruit-nrfutil: record the invocation, and for genpkg create the file
# the real tool would have produced so the rest of the script can proceed.
#
# NRFUTIL_FAIL_SERIAL: how many `dfu serial` calls to fail before succeeding.
# That is how the touch race is reproduced -- the real tool reboots the board
# and then cannot reopen the port in time.
set -euo pipefail
printf '%s\\n' "$*" >> "$NRFUTIL_LOG"
if [[ "${1:-}" == "dfu" && "${2:-}" == "genpkg" ]]; then
    for arg in "$@"; do :; done
    : > "$arg"
fi
if [[ "$*" == *"dfu serial"* ]]; then
    fail="${NRFUTIL_FAIL_SERIAL:-0}"
    done_file="$NRFUTIL_LOG.serial"
    n=0
    [[ -f "$done_file" ]] && n="$(cat "$done_file")"
    n=$((n + 1))
    echo "$n" > "$done_file"
    # The real tool exits 0 either way: a failed transfer is reported only in
    # its text, and success only by its last line. The stub does the same, so
    # the script is tested against the tool it actually drives.
    if (( n <= fail )); then
        echo "Failed to upgrade target. Error is: Target is not in DFU mode."
        exit 0
    fi
    echo "Device programmed."
fi
exit 0
"""


# Stub Python, for the one place the script shells out to it: reopening the
# port at 115200 so the host stops caching the touch rate. One script under
# several names. It answers the script's `-c 'import serial'` probe yes only if
# its name is in PY_WITH_SERIAL, and records the reopen -- its name, its
# arguments and the program on stdin, since the baud rate is in the program.
# Anything else goes to the real interpreter: the script's own Python tools
# run through `env python3`, which this shadows. The one named python-nrfutil
# stands in for the interpreter on adafruit-nrfutil's `#!` line, and so runs
# the (bash) stub tool when asked to.
PYTHON_STUB = """#!/usr/bin/env bash
me="$(basename "$0")"
if [[ "${1:-}" == "-c" && "${2:-}" == "import serial" ]]; then
    [[ " ${PY_WITH_SERIAL:-} " == *" $me "* ]]
    exit $?
fi
if [[ "${1:-}" == "-" ]]; then
    program="$(cat)"
    printf '%s %s\\n%s\\n' "$me" "$*" "$program" >> "$STTY_LOG"
    [[ -z "${PY_REOPEN_FAIL:-}" ]]
    exit $?
fi
if [[ "$me" == python-nrfutil ]]; then exec bash "$@"; fi
exec "$PY_REAL" "$@"
"""

# ioreg, as the script reads it: IOREG_BEFORE until the first `dfu serial`
# has run, IOREG_AFTER from then on -- which is how a board is shown leaving
# its application for its bootloader.
IOREG_STUB = """#!/usr/bin/env bash
if [[ -n "${IOREG_AFTER:-}" && -f "$NRFUTIL_LOG.serial" ]]; then
    cat "$IOREG_AFTER"
elif [[ -n "${IOREG_BEFORE:-}" ]]; then
    cat "$IOREG_BEFORE"
fi
"""

# The bench's two boards as `ioreg -r -c IOUSBHostDevice -l` showed them on
# 2026-10-24, trimmed to the lines the script reads plus the ones that look
# like them: 7F23 running an application, 354B in its bootloader. The nested,
# repeated keys and the "USB Device Info" dictionary are real, and are what a
# parser that took the wrong line would trip on.
SERIAL_7F23 = "65C11224153B1DFD"
SERIAL_354B = "D3304814DC467229"


def app_device(serial, port, indent=""):
    return f"""{indent}+-o oxinode qspi bring-up@01100000  <class IOUSBHostDevice, id 0x100050485, registered, matched, active, busy 0 (231 ms), retain 25>
{indent}  |   "idProduct" = 6
{indent}  |   "USB Product Name" = "oxinode qspi bring_up"
{indent}  |   "locationID" = 17825792
{indent}  |   "kUSBSerialNumberString" = "{serial}"
{indent}  |   "idVendor" = 4617
{indent}  |   "USB Serial Number" = "{serial}"
{indent}  | |   "idProduct" = 6
{indent}  | |   "idVendor" = 4617
{indent}  | |   "USB Serial Number" = "{serial}"
{indent}      |   "idProduct" = 6
{indent}      |   "idVendor" = 4617
{indent}            "IOCalloutDevice" = "{port}"
{indent}            "IODialinDevice" = "{port.replace("/cu.", "/tty.")}"
"""


def bootloader_device(serial, port, indent=""):
    return f"""{indent}+-o muzi Base@03100000  <class IOUSBHostDevice, id 0x1000504db, registered, matched, active, busy 0 (248 ms), retain 29>
{indent}  |   "idProduct" = 129
{indent}  |   "USB Product Name" = "muzi Base"
{indent}  |   "locationID" = 51380224
{indent}  |   "kUSBSerialNumberString" = "{serial}"
{indent}  |   "idVendor" = 9114
{indent}  |   "USB Serial Number" = "{serial}"
{indent}  | |   "idProduct" = 129
{indent}  | |   "idVendor" = 9114
{indent}  |   |   "idProduct" = 129
{indent}  |   |   "idVendor" = 9114
{indent}  |         "IOCalloutDevice" = "{port}"
{indent}  |         "IODialinDevice" = "{port.replace("/cu.", "/tty.")}"
{indent}      |   "USB Device Info" = {{"bcdDevice"=256,"idProduct"=129,"kUSBSerialNumberString"="{serial}","USB Product Name"="muzi Base","idVendor"=9114,"bInterfaceClass"=8}}
{indent}      |   "idVendor" = 9114
"""


def hub(*children):
    """A hub with no serial number of its own, its children nested under it --
    so a port's nearest device is not the first one in the tree."""
    head = """+-o USB2.1 Hub@03000000  <class IOUSBHostDevice, id 0x100050400, registered, matched, active, busy 0 (12 ms), retain 20>
  |   "idProduct" = 9488
  |   "idVendor" = 1507
"""
    return head + "".join(c for c in children)


@unittest.skipUnless(
    os.path.isfile(ELF), "no release ELF built; run `cargo build --release` first"
)
class TestDfuFlash(unittest.TestCase):
    def setUp(self):
        # Ports as files in a directory of their own, not the bench's real
        # /dev/cu.usbmodem1101 and 3101: what is plugged in decides nothing,
        # and the script finds them present without waiting.
        ports = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, ports, ignore_errors=True)
        for name in ("cu.usbmodem1101", "cu.usbmodem3101", "tty.usbmodem3101"):
            open(os.path.join(ports, name), "w").close()
        self.port_a = os.path.join(ports, "cu.usbmodem1101")
        self.port_b = os.path.join(ports, "cu.usbmodem3101")

    def run_script(
        self, nrfutil_shebang=None, ioreg_before=None, ioreg_after=None, **env_extra
    ):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)

        def write(name, text, mode=0o755):
            path = os.path.join(tmp, name)
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)
            os.chmod(path, mode)
            return path

        stub = STUB
        if nrfutil_shebang:
            stub = nrfutil_shebang + "\n" + stub.split("\n", 1)[1]
        write("adafruit-nrfutil", stub)

        # Every interpreter the script might try is a stub, so what the host
        # happens to have installed decides nothing.
        for name in ("reopen-python", "python3.11", "python3", "python-nrfutil"):
            write(name, PYTHON_STUB)
        self.stty_log = os.path.join(tmp, "reopen.log")

        ioreg = write("ioreg-stub", IOREG_STUB)

        log = os.path.join(tmp, "calls.log")
        env = dict(os.environ)
        env["PATH"] = tmp + os.pathsep + env["PATH"]
        env["NRFUTIL_LOG"] = log
        env["STTY_LOG"] = self.stty_log
        env["PY_REAL"] = sys.executable
        env["PY_WITH_SERIAL"] = "reopen-python"
        env["OXINODE_PYTHON"] = os.path.join(tmp, "reopen-python")
        env["OXINODE_DFU_PORT"] = "/dev/null"
        env["OXINODE_DFU_IOREG"] = ioreg
        if ioreg_before is not None:
            env["IOREG_BEFORE"] = write("ioreg-before.txt", ioreg_before, 0o644)
        if ioreg_after is not None:
            env["IOREG_AFTER"] = write("ioreg-after.txt", ioreg_after, 0o644)
        env.update(env_extra)
        for key, value in list(env_extra.items()):
            if value is None:
                env.pop(key, None)

        proc = subprocess.run(
            [os.path.join(REPO, "tools/dfu-flash.sh"), ELF],
            cwd=REPO,
            env=env,
            capture_output=True,
            text=True,
        )
        calls = []
        if os.path.exists(log):
            with open(log, encoding="utf-8") as f:
                calls = [line.strip() for line in f if line.strip()]
        return proc, calls

    def test_a_single_candidate_port_is_chosen_automatically(self):
        ports = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, ports, ignore_errors=True)
        only = os.path.join(ports, "cu.usbmodemONE")
        open(only, "w").close()
        proc, calls = self.run_script(
            OXINODE_DFU_PORT=None,
            OXINODE_DFU_PORT_GLOB=os.path.join(ports, "cu.usbmodem*"),
            OXINODE_DFU_IN_DFU="1",
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(any(only in c for c in calls), calls)

    def test_no_candidate_port_fails_with_a_reason(self):
        """Replaces a version that skipped itself whenever a board was plugged
        in -- which, on the machine that does the flashing, was always. It had
        therefore been testing nothing for as long as it had existed. Pointing
        the glob at an empty directory makes the case deterministic.
        """
        ports = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, ports, ignore_errors=True)
        proc, _ = self.run_script(
            OXINODE_DFU_PORT=None,
            OXINODE_DFU_PORT_GLOB=os.path.join(ports, "cu.usbmodem*"),
            OXINODE_DFU_IN_DFU="1",
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("no /dev/cu.usbmodem", proc.stderr)

    def test_two_boards_attached_refuses_rather_than_guessing(self):
        """The hazard this guards is not hypothetical.

        A second Base Duo running someone else's firmware enumerates on the
        same glob. The old code took the first match, which is alphabetical and
        therefore arbitrary -- and flashing the wrong one overwrites firmware
        that was not ours to overwrite.
        """
        ports = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, ports, ignore_errors=True)
        first = os.path.join(ports, "cu.usbmodem1101")
        second = os.path.join(ports, "cu.usbmodem3101")
        open(first, "w").close()
        open(second, "w").close()
        proc, calls = self.run_script(
            OXINODE_DFU_PORT=None,
            OXINODE_DFU_PORT_GLOB=os.path.join(ports, "cu.usbmodem*"),
            OXINODE_DFU_IN_DFU="1",
        )
        self.assertNotEqual(proc.returncode, 0, "should refuse, not choose")
        self.assertIn("more than one", proc.stderr)
        # Both are named, so the operator can tell which is which.
        self.assertIn(first, proc.stderr)
        self.assertIn(second, proc.stderr)
        # And nothing was flashed.
        self.assertFalse(
            any("dfu serial" in c for c in calls), f"attempted a flash: {calls}"
        )

    def test_a_lost_touch_race_is_retried_rather_than_reported(self):
        """The failure this fixes cost three trips to the reset button.

        `--touch 1200` reboots the board into its bootloader and then reopens
        the port -- but the board has to re-enumerate first, and the tool can
        get there before the bootloader does. It gives up, having already
        removed the application it would have touched, and the operator has to
        re-run the same command with OXINODE_DFU_IN_DFU=1 by hand.

        Since the bootloader announces itself by mounting a drive, the script
        can see that state and finish the job.
        """
        proc, calls = self.run_script(
            OXINODE_DFU_IN_DFU="0",
            OXINODE_DFU_BOOTLOADER="1",
            OXINODE_DFU_RETRY_DELAY="0",
            NRFUTIL_FAIL_SERIAL="1",
        )
        self.assertEqual(proc.returncode, 0, f"stderr:\n{proc.stderr}")
        serial = [c for c in calls if "dfu serial" in c]
        self.assertEqual(len(serial), 2, f"expected one retry, got {calls}")
        # The first attempt touches; the retry must not, because by then there
        # is no application left to reboot.
        self.assertIn("--touch", serial[0])
        self.assertNotIn("--touch", serial[1])

    def test_no_bootloader_means_no_retry(self):
        """A failure with no bootloader in sight is a real failure. Retrying it
        would just be three more of the same error, two seconds apart."""
        proc, calls = self.run_script(
            OXINODE_DFU_IN_DFU="0",
            OXINODE_DFU_BOOTLOADER="0",
            OXINODE_DFU_RETRY_DELAY="0",
            NRFUTIL_FAIL_SERIAL="9",
        )
        self.assertNotEqual(proc.returncode, 0)
        serial = [c for c in calls if "dfu serial" in c]
        self.assertEqual(len(serial), 1, f"should not retry: {calls}")
        self.assertIn("Double-tap the reset button", proc.stderr)

    def test_a_bootloader_that_never_answers_gives_up_and_says_how_to_recover(self):
        """The state this board actually gets into: the bootloader is running
        and mounts its drive, but its serial DFU service does not answer. There
        is no software route out of it, so the message has to say so."""
        proc, calls = self.run_script(
            OXINODE_DFU_IN_DFU="0",
            OXINODE_DFU_BOOTLOADER="1",
            OXINODE_DFU_RETRY_DELAY="0",
            NRFUTIL_FAIL_SERIAL="99",
        )
        self.assertNotEqual(proc.returncode, 0)
        serial = [c for c in calls if "dfu serial" in c]
        self.assertEqual(len(serial), 4, f"one attempt plus three retries: {calls}")
        self.assertIn("Double-tap the reset button", proc.stderr)

    def test_runs_to_completion_in_dfu_mode(self):
        proc, calls = self.run_script(OXINODE_DFU_IN_DFU="1")
        self.assertEqual(proc.returncode, 0, f"stderr:\n{proc.stderr}")
        self.assertNotIn("unbound variable", proc.stderr)
        self.assertEqual(len(calls), 2, f"expected genpkg then serial, got {calls}")

    def test_builds_a_package_for_the_right_chip(self):
        _proc, calls = self.run_script(OXINODE_DFU_IN_DFU="1")
        genpkg = calls[0]
        self.assertIn("dfu genpkg", genpkg)
        # 0x0052 is the nRF52 device type; the bootloader rejects a package
        # built for anything else.
        self.assertIn("--dev-type 0x0052", genpkg)
        self.assertIn("--application", genpkg)
        self.assertIn(".hex", genpkg)

    def test_sends_the_package_over_the_named_port(self):
        _proc, calls = self.run_script(OXINODE_DFU_IN_DFU="1")
        serial = calls[1]
        self.assertIn("dfu serial", serial)
        self.assertIn("-p /dev/null", serial)
        self.assertIn("-b 115200", serial)
        # Single bank: the whole application region is the target, with no room
        # reserved to stage a second copy.
        self.assertIn("--singlebank", serial)
        self.assertIn(".zip", serial)

    def test_no_touch_when_the_board_is_already_in_the_bootloader(self):
        # Asking for a 1200-baud touch here makes the tool wait for a
        # re-enumeration that never comes, because there is no app to reset.
        _proc, calls = self.run_script(OXINODE_DFU_IN_DFU="1")
        self.assertNotIn("--touch", calls[1])

    def test_the_cached_baud_rate_is_reset_after_a_successful_flash(self):
        """The 1200-baud touch lives in the host's terminal settings, not on
        the board, and it is re-applied the next time anything opens that
        device path. Leaving it there means the next `cat` of the log port is
        a second touch, and the freshly flashed image reboots into DFU.

        The stub `stty` records that it was asked to set 115200.
        """
        proc, _ = self.run_script(OXINODE_DFU_IN_DFU="1", OXINODE_DFU_SETTLE="0")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(os.path.exists(self.stty_log), "the port was never reopened")
        with open(self.stty_log, encoding="utf-8") as f:
            calls = f.read()
        self.assertIn("115200", calls, calls)

    def test_touch_when_an_application_is_running(self):
        # The empty-array case that broke on bash 3.2 is the *other* branch, so
        # both are exercised.
        proc, calls = self.run_script(OXINODE_DFU_IN_DFU="0")
        self.assertEqual(proc.returncode, 0, f"stderr:\n{proc.stderr}")
        self.assertIn("--touch 1200", calls[1])


    # -- Finding a Python that can reopen the port -------------------------

    def reopens(self):
        if not os.path.exists(self.stty_log):
            return []
        with open(self.stty_log, encoding="utf-8") as f:
            return [line.split()[0] for line in f if line.startswith(("python", "reopen"))]

    def test_no_python_with_pyserial_says_so_rather_than_skipping(self):
        """What happened on the bench: `python3` was radioconda's, without
        pyserial, and the ImportError went to /dev/null. The cached 1200 baud
        stayed, and the board was touched back into its bootloader by the next
        open of its port, with nothing anywhere to say why."""
        proc, _ = self.run_script(
            OXINODE_PYTHON=None, PY_WITH_SERIAL="", OXINODE_DFU_IN_DFU="1",
            OXINODE_DFU_SETTLE="0",
        )
        # The flash itself happened, so it is still a success...
        self.assertEqual(proc.returncode, 0, proc.stderr)
        # ...but a loud one, before the flash and again after it, naming what
        # was tried.
        self.assertEqual(
            proc.stderr.count("no Python with pyserial found"), 2, proc.stderr
        )
        self.assertIn("python3.11 python3", proc.stderr)
        self.assertEqual(self.reopens(), [])

    def test_an_interpreter_without_pyserial_is_passed_over(self):
        proc, _ = self.run_script(
            PY_WITH_SERIAL="python3.11", OXINODE_DFU_IN_DFU="1", OXINODE_DFU_SETTLE="0"
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self.reopens(), ["python3.11"])
        self.assertNotIn("no Python with pyserial", proc.stderr)

    def test_oxinode_python_comes_first(self):
        proc, _ = self.run_script(
            PY_WITH_SERIAL="reopen-python python3.11 python3",
            OXINODE_DFU_IN_DFU="1", OXINODE_DFU_SETTLE="0",
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self.reopens(), ["reopen-python"])

    def test_the_flashers_own_interpreter_is_tried(self):
        """adafruit-nrfutil needs pyserial itself, so the interpreter on its
        `#!` line has it -- on the bench, the venv's python3.11, when neither
        `python3` on PATH nor anything else did."""
        proc, calls = self.run_script(
            nrfutil_shebang="#!/usr/bin/env python-nrfutil",
            OXINODE_PYTHON=None, PY_WITH_SERIAL="python-nrfutil",
            OXINODE_DFU_IN_DFU="1", OXINODE_DFU_SETTLE="0",
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(any("dfu serial" in c for c in calls), calls)
        self.assertEqual(self.reopens(), ["python-nrfutil"])

    def test_a_reopen_that_fails_is_reported(self):
        proc, _ = self.run_script(
            PY_REOPEN_FAIL="1", OXINODE_DFU_IN_DFU="1", OXINODE_DFU_SETTLE="0"
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("could not reopen /dev/null at 115200", proc.stderr)

    # -- Which board is in its bootloader ------------------------------------
    #
    # The bench case: two boards attached, one of them in its bootloader. The
    # old check asked whether *any* bootloader drive was mounted.

    def two_boards(self):
        return app_device(SERIAL_7F23, self.port_a) + bootloader_device(
            SERIAL_354B, self.port_b
        )

    def test_another_boards_bootloader_does_not_skip_the_touch(self):
        proc, calls = self.run_script(
            ioreg_before=self.two_boards(), OXINODE_DFU_PORT=self.port_a,
            OXINODE_DFU_SETTLE="0",
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        serial = [c for c in calls if "dfu serial" in c]
        self.assertIn("--touch 1200", serial[0])
        self.assertIn(f"USB serial {SERIAL_7F23}, running an application", proc.stdout)

    def test_this_boards_bootloader_skips_the_touch(self):
        for port in (self.port_b, self.port_b.replace("/cu.", "/tty.")):
            with self.subTest(port=port):
                proc, calls = self.run_script(
                    ioreg_before=self.two_boards(), OXINODE_DFU_PORT=port,
                    OXINODE_DFU_SETTLE="0",
                )
                self.assertEqual(proc.returncode, 0, proc.stderr)
                serial = [c for c in calls if "dfu serial" in c]
                self.assertNotIn("--touch", serial[0])
                self.assertIn(f"USB serial {SERIAL_354B}, in its bootloader", proc.stdout)

    def test_a_board_behind_a_hub_is_told_apart(self):
        """A port belongs to the nearest device above it, not the first one in
        the tree: here that is a hub with no serial of its own."""
        tree = app_device(SERIAL_7F23, self.port_a) + hub(
            bootloader_device(SERIAL_354B, self.port_b, indent="  | ")
        )
        for port, touch in ((self.port_b, False), (self.port_a, True)):
            with self.subTest(port=port):
                _proc, calls = self.run_script(
                    ioreg_before=tree, OXINODE_DFU_PORT=port, OXINODE_DFU_SETTLE="0"
                )
                serial = [c for c in calls if "dfu serial" in c]
                self.assertEqual("--touch" in serial[0], touch, serial)

    def test_a_lost_race_is_retried_when_this_board_reaches_its_bootloader(self):
        """The retry, decided per board: 7F23 is touched, the tool loses the
        race, and 7F23 comes back as the bootloader under the same serial."""
        after = bootloader_device(SERIAL_7F23, self.port_a) + bootloader_device(
            SERIAL_354B, self.port_b
        )
        proc, calls = self.run_script(
            ioreg_before=self.two_boards(), ioreg_after=after,
            OXINODE_DFU_PORT=self.port_a, OXINODE_DFU_RETRY_DELAY="0",
            OXINODE_DFU_SETTLE="0", NRFUTIL_FAIL_SERIAL="1",
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        serial = [c for c in calls if "dfu serial" in c]
        self.assertEqual(len(serial), 2, calls)
        self.assertIn("--touch", serial[0])
        self.assertNotIn("--touch", serial[1])

    def test_the_other_boards_bootloader_is_not_a_reason_to_retry(self):
        """7F23's touch fails and 7F23 stays an application. 354B sitting in
        its bootloader says nothing about that, and retrying without a touch
        would only fail against 7F23's running image three more times."""
        proc, calls = self.run_script(
            ioreg_before=self.two_boards(), ioreg_after=self.two_boards(),
            OXINODE_DFU_PORT=self.port_a, OXINODE_DFU_RETRY_DELAY="0",
            OXINODE_DFU_SETTLE="0", NRFUTIL_FAIL_SERIAL="9",
        )
        self.assertNotEqual(proc.returncode, 0)
        serial = [c for c in calls if "dfu serial" in c]
        self.assertEqual(len(serial), 1, calls)

    def test_a_port_with_no_usb_device_behind_it_is_not_in_its_bootloader(self):
        """No serial, nothing to match -- not "some bootloader is up"."""
        proc, calls = self.run_script(
            ioreg_before=self.two_boards(), OXINODE_DFU_PORT="/dev/null",
            OXINODE_DFU_SETTLE="0",
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        serial = [c for c in calls if "dfu serial" in c]
        self.assertIn("--touch 1200", serial[0])
        self.assertNotIn("USB serial", proc.stdout)


if __name__ == "__main__":
    unittest.main()
