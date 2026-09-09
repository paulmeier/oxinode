# Releasing

Releases are built only from tags matching `v*`. There is no manual trigger
and no branch build that publishes.

```bash
# 1. Bump [package] version in Cargo.toml and commit it.
# 2. Tag that commit and push the tag.
git tag v0.1.0
git push origin v0.1.0
```

The tag must match the version in `Cargo.toml`; `tools/package.sh` refuses
to build otherwise, because a release whose contents disagree with its name
is a problem nobody notices until they are trying to work out which firmware
is on a board months later. Tags with a suffix (`v0.2.0-rc1`) are published
as pre-releases.

The release workflow runs the full check suite before it publishes anything.
A broken release is worse than a missing one, because it gets flashed.

## What a release carries

| File | |
|---|---|
| `oxinode-rnode-vX.Y.Z.uf2` | the product image |
| `oxinode-radio-vX.Y.Z.uf2` | the radio diagnostic |
| `oxinode-usb-cdc-vX.Y.Z.uf2` | the USB echo image |
| `oxinode-blink-vX.Y.Z.uf2` | the blink image |
| `*.elf` | the exact binaries the images were built from |
| `SHA256SUMS` | over every asset |

The ELFs ship because with no debug probe, a fault address reported over
serial is the only forensic evidence available, and resolving one needs the
exact binary, not a rebuild. They are also what `defmt-print` needs to decode
that release's log.

## Reproducing a release locally

Both scripts run locally, so a release can be inspected without waiting on
CI:

```bash
tools/package.sh --version v0.1.0    # writes dist/
tools/release_notes.sh v0.1.0        # the release body, to stdout
```

Without `--version`, `package.sh` builds the current tree as `vX.Y.Z-dev`.

## CI

`.github/workflows/ci.yml` runs `tools/test.sh` on every push to `main` and
every pull request, then packages the images and uploads them as a build
artifact kept for fourteen days. `.github/workflows/docs.yml` builds this
documentation with `mkdocs build --strict` on the same events and publishes
it to GitHub Pages from `main`. `.github/workflows/release.yml` runs on tags.
