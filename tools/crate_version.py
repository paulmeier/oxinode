#!/usr/bin/env python3
"""Print the firmware package's version from Cargo.toml.

Not `cargo metadata`: that wants a populated registry and fails on a cold
checkout, and this runs before the first build during packaging.
"""

import argparse
import re
import sys


def crate_version(text: str) -> str:
    try:
        import tomllib

        return tomllib.loads(text)["package"]["version"]
    except ImportError:  # Python < 3.11
        pass

    # Only look inside [package]; [profile.release] and the dependency tables
    # have `version` keys of their own.
    if "[package]" not in text:
        raise ValueError("no [package] section")
    section = text.split("[package]", 1)[1].split("\n[", 1)[0]
    m = re.search(r'^\s*version\s*=\s*"([^"]+)"', section, re.M)
    if not m:
        raise ValueError("no version in [package]")
    return m.group(1)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("manifest", nargs="?", default="Cargo.toml")
    args = ap.parse_args()
    with open(args.manifest, "r", encoding="utf-8") as f:
        print(crate_version(f.read()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
