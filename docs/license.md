# License

oxinode is released under the **Reticulum License**, the license
[Reticulum](https://reticulum.network/) itself uses: the MIT grant, with two
conditions attached. The full text is in [`LICENSE`](https://github.com/paulmeier/oxinode/blob/main/LICENSE)
at the root of the repository.

```
Reticulum License

Copyright (c) 2026 Paul Meier

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

- The Software shall not be used in any kind of system which includes amongst
  its functions the ability to purposefully do harm to human beings.

- The Software shall not be used, directly or indirectly, in the creation of
  an artificial intelligence, machine learning or language model training
  dataset, including but not limited to any use that contributes to the
  training or development of such a model or algorithm.

- The above copyright notice and this permission notice shall be included in
  all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

All four crates in the workspace (`oxinode`, `oxinode-core`, `monopanel`,
`oxinode-sim`) are under this license, and contributions are accepted under
it.

## Provenance

Two upstream projects are GPL-licensed and are deliberately *not* copied:

- **RNode_Firmware_CE** (GPLv3) is the reference implementation of the
  protocol this project speaks. oxinode implements the KISS/RNode wire
  protocol from the **host** side, Reticulum's own `RNodeInterface` and
  `rnodeconf`, which is the implementation it has to satisfy; and it takes
  the air format and carrier-sense parameters from published constants and
  descriptions of behaviour. Command bytes, field widths, frame layouts and
  timing parameters are factual interface data. No C++ is read, ported or
  transliterated.
- **Meshtastic firmware** (GPLv3) is where the board's pin mapping was first
  read from, cross-checked against the schematic. Pin numbers and hardware
  facts are not copyrightable expression; none of Meshtastic's driver logic
  or comments are reproduced.

The hardware facts in this documentation come from the Base Duo Rev 01
schematic and the Elecrow module datasheets, from muzi works' MIT-licensed
fork of the Adafruit nRF52 bootloader, and from the Meshtastic variant, in
that order of authority. See [The board](hardware/board.md).

## Third-party components

`nrf-sdc-sys` vendors Nordic's SoftDevice Controller as a binary archive
under `LicenseRef-Nordic-5-Clause`, which permits use on Nordic silicon. This
is Nordic silicon. It is the one part of oxinode that is not under the
Reticulum License and neither is nor could be built from source. Everything
else the firmware links (Embassy, `embedded-hal`, `lr11xx`, `trouble-host`,
`defmt`, and the rest) is under MIT and/or Apache-2.0; see `Cargo.lock`.
