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
set -euo pipefail
printf '%s\\n' "$*" >> "$NRFUTIL_LOG"
if [[ "${1:-}" == "dfu" && "${2:-}" == "genpkg" ]]; then
    for arg in "$@"; do :; done
    : > "$arg"
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

    def test_fails_clearly_with_no_port(self):
        proc, calls = self.run_script(OXINODE_DFU_PORT="", OXINODE_DFU_IN_DFU="1")
        if proc.returncode == 0:
            self.skipTest("a board is plugged in; the no-port path cannot be tested")
        self.assertIn("no serial port found", proc.stderr)
        self.assertEqual(calls, [], "must not invoke the flasher with no port")


if __name__ == "__main__":
    unittest.main()
