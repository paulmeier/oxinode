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
    if (( n <= fail )); then
        echo "Target is not in DFU mode." >&2
        exit 1
    fi
fi
exit 0
"""


@unittest.skipUnless(
    os.path.isfile(ELF), "no release ELF built; run `cargo build --release` first"
)
class TestDfuFlash(unittest.TestCase):
    def run_script(self, **env_extra):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)

        stub = os.path.join(tmp, "adafruit-nrfutil")
        with open(stub, "w", encoding="utf-8") as f:
            f.write(STUB)
        os.chmod(stub, 0o755)

        log = os.path.join(tmp, "calls.log")
        env = dict(os.environ)
        env["PATH"] = tmp + os.pathsep + env["PATH"]
        env["NRFUTIL_LOG"] = log
        env["OXINODE_DFU_PORT"] = "/dev/null"
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

    def test_touch_when_an_application_is_running(self):
        # The empty-array case that broke on bash 3.2 is the *other* branch, so
        # both are exercised.
        proc, calls = self.run_script(OXINODE_DFU_IN_DFU="0")
        self.assertEqual(proc.returncode, 0, f"stderr:\n{proc.stderr}")
        self.assertIn("--touch 1200", calls[1])



if __name__ == "__main__":
    unittest.main()
