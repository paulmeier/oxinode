#!/usr/bin/env bash
# Build release firmware and lay out the files that go on a GitHub Release.
#
# CI calls this; so can you. Keeping the packaging here rather than inside a
# workflow means a release can be reproduced and inspected locally instead of
# only ever existing as the output of a green tick.
#
#   tools/package.sh                  # version from Cargo.toml, marked -dev
#   tools/package.sh --version v0.1.0 # must match Cargo.toml's version
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version=""
outdir="dist"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) version="$2"; shift 2 ;;
        --out)     outdir="$2";  shift 2 ;;
        *) echo "usage: package.sh [--version vX.Y.Z] [--out DIR]" >&2; exit 2 ;;
    esac
done

# The version in Cargo.toml is the authority. A tag that disagrees with it
# produces a release whose contents do not match its name, which is the kind of
# thing nobody notices until they are trying to work out which firmware is on a
# board months later.
crate_version="$(tools/crate_version.py Cargo.toml)"

if [[ -n "$version" ]]; then
    if [[ "${version#v}" != "$crate_version" ]]; then
        cat >&2 <<MSG
package: version mismatch.
    tag         $version  (${version#v})
    Cargo.toml  $crate_version

Bump the [package] version in Cargo.toml to ${version#v}, commit that, and tag
the commit that carries it.
MSG
        exit 1
    fi
else
    version="v${crate_version}-dev"
    echo "package: no --version given, building as $version"
fi

profile_dir="target/thumbv7em-none-eabihf/release"
base="$(tools/layout.py memory.x --field FLASH.origin)"

echo "package: building $version (load address $base)"
cargo build --release --locked

rm -rf "$outdir"
mkdir -p "$outdir"

for bin in blink usb-cdc radio; do
    elf="$profile_dir/$bin"
    rust-objcopy -O binary "$elf" "$elf.bin"
    tools/uf2conv.py "$elf.bin" -o "$elf.uf2" -b "$base"
    tools/check_layout.py "$elf" --uf2 "$elf.uf2"

    cp "$elf.uf2" "$outdir/oxinode-$bin-$version.uf2"
    # The ELF ships too. Without a debug probe, a fault address reported over
    # serial is the only forensic evidence there is, and turning one into a line
    # number needs the exact binary that produced the image -- not a rebuild.
    cp "$elf" "$outdir/oxinode-$bin-$version.elf"
done

( cd "$outdir"
  if command -v sha256sum >/dev/null; then
      sha256sum ./*.uf2 ./*.elf > SHA256SUMS
  else
      shasum -a 256 ./*.uf2 ./*.elf > SHA256SUMS
  fi )

echo
echo "package: $outdir"
ls -lh "$outdir" | tail -n +2 | awk '{printf "  %-40s %s\n", $9, $5}'
