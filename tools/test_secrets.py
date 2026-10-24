#!/usr/bin/env python3
"""The node's private key stays where it is put.

The board's Reticulum identity is a 64-byte private key that lives on its own
flash page (`oxinode_core::node::key`). `NodeKey` gives its bytes out in one
form, the record written to that page, and its `Debug` says nothing. These
tests hold the rest of the source to the same rule, by reading it:

* nothing outside the key module asks the stack for a private key;
* in the firmware, the record is made in one place, the code that writes it;
* in the two firmware files that hold a key, no log line is handed one.

A grep is not a proof, and these do not pretend to be: they are the tripwire
for the ordinary way a secret leaks, which is somebody logging it while
debugging and forgetting to take the line out.
"""

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
KEY_MODULE = ROOT / "core" / "src" / "node" / "key.rs"
# The firmware files that hold a `NodeKey`: the one that makes or loads it,
# and the one that writes it.
HOLDERS = [ROOT / "src" / "node.rs", ROOT / "src" / "store.rs"]
DEFMT = re.compile(r"defmt::\w+!\(")
STRING = re.compile(r'"(?:\\.|[^"\\])*"')


def rust_files(*dirs):
    for d in dirs:
        yield from sorted((ROOT / d).rglob("*.rs"))


def log_arguments(text):
    """The argument text of every defmt macro in `text`, string literals
    removed, with the line each starts on."""
    for m in DEFMT.finditer(text):
        i, depth = m.end(), 1
        while depth and i < len(text):
            depth += {"(": 1, ")": -1}.get(text[i], 0)
            i += 1
        yield text.count("\n", 0, m.start()) + 1, STRING.sub('""', text[m.end() : i - 1])


def leaks(text):
    """Log lines in `text` that are handed a key, by line number."""
    return [
        line
        for line, args in log_arguments(text)
        if re.search(r"\bkey\b|\bNodeKey\b|\.record\(", args)
    ]


class TheKeyStaysPut(unittest.TestCase):
    def test_only_the_key_module_asks_for_a_private_key(self):
        users = [
            str(f.relative_to(ROOT))
            for f in rust_files("src", "core/src")
            if f != KEY_MODULE and "get_private_key" in f.read_text()
        ]
        self.assertEqual(users, [])

    def test_the_firmware_makes_the_record_only_to_write_it(self):
        makers = [
            str(f.relative_to(ROOT))
            for f in rust_files("src")
            if re.search(r"\.record\(\)", f.read_text())
        ]
        self.assertEqual(makers, ["src/store.rs"])

    def test_no_log_line_in_a_file_that_holds_a_key_is_handed_one(self):
        for f in HOLDERS:
            with self.subTest(str(f.relative_to(ROOT))):
                self.assertEqual(leaks(f.read_text()), [])


class TheScanBites(unittest.TestCase):
    """The scan itself, on lines written to be caught and lines that are not."""

    def test_a_logged_key_is_caught(self):
        for line in [
            'defmt::info!("key {=[u8]:02x}", &key.record()[..]);',
            'defmt::info!("{}", defmt::Debug2Format(&key));',
            'defmt::warn!("node: {=[u8]}",\n    &key.record()[12..]\n);',
            'defmt::error!("x {}", NodeKey::read(&buf));',
        ]:
            with self.subTest(line):
                self.assertEqual(leaks(line), [1])

    def test_the_word_in_a_message_and_the_public_hashes_are_not(self):
        for line in [
            'defmt::warn!("store: {}", defmt::Debug2Format(&e));',
            'defmt::warn!("node: the identity page does not hold a key it can trust");',
            'defmt::info!("node: identity {=[u8]:02x}", &hashes.identity[..]);',
            'defmt::info!("node: {=str}", if a { "key" } else { "no key" });',
        ]:
            with self.subTest(line):
                self.assertEqual(leaks(line), [])

    def test_the_holders_exist(self):
        # A rename would otherwise make the test above pass by reading nothing.
        for f in HOLDERS + [KEY_MODULE]:
            self.assertTrue(f.is_file(), f)
        self.assertIn("NodeKey", HOLDERS[0].read_text())
        self.assertIn("save_key", HOLDERS[1].read_text())


if __name__ == "__main__":
    unittest.main()
