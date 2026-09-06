#!/usr/bin/env python3
"""Tests for the Cargo.toml version reader.

The release workflow refuses to publish when the tag and this value disagree, so
reading the wrong key here would either block every release or, worse, wave
through a mislabelled one.
"""

import os
import unittest

import crate_version

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class TestRealManifest(unittest.TestCase):
    def test_reads_the_projects_own_cargo_toml(self):
        with open(os.path.join(REPO, "Cargo.toml"), encoding="utf-8") as f:
            version = crate_version.crate_version(f.read())
        # Shape, not value: the version is meant to change.
        self.assertRegex(version, r"^\d+\.\d+\.\d+")


class TestParsing(unittest.TestCase):
    def test_simple(self):
        self.assertEqual(
            crate_version.crate_version('[package]\nname = "oxinode"\nversion = "1.2.3"\n'),
            "1.2.3",
        )

    def test_ignores_version_keys_in_other_sections(self):
        # `[profile.release]` has no version, but dependency tables do, and
        # `[build-dependencies]` sits right after `[package]` in this manifest.
        manifest = (
            '[package]\n'
            'name = "oxinode"\n'
            'version = "0.4.2"\n'
            '\n'
            '[dependencies]\n'
            'cortex-m = { version = "0.7.7" }\n'
            'embassy-nrf = "0.11"\n'
        )
        self.assertEqual(crate_version.crate_version(manifest), "0.4.2")

    def test_does_not_read_a_dependency_when_package_has_no_version(self):
        manifest = '[package]\nname = "x"\n\n[dependencies]\nfoo = { version = "9.9.9" }\n'
        with self.assertRaises(Exception):
            crate_version.crate_version(manifest)

    def test_prerelease_versions(self):
        self.assertEqual(
            crate_version.crate_version('[package]\nversion = "0.1.0-rc.1"\n'), "0.1.0-rc.1"
        )

    def test_missing_package_section(self):
        with self.assertRaises(Exception):
            crate_version.crate_version('[workspace]\nmembers = ["core"]\n')


if __name__ == "__main__":
    unittest.main()
