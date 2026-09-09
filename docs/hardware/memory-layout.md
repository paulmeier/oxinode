# Memory layout

The nRF52840 has 1 MB of flash and 256 KB of RAM. What oxinode may use of
each is decided by the bootloader that ships on the board, and `memory.x` is
the single source of truth for it.

## Flash

```
0x00000000 ┌────────────────────────────────┐
           │ Nordic MBR                     │   4 KB
0x00001000 ├────────────────────────────────┤
           │ SoftDevice S140 6.1.1 (unused) │ 148 KB
0x00026000 ├────────────────────────────────┤ ◄── oxinode is linked here
           │ application                    │ 784 KB
0x000EA000 ├────────────────────────────────┤ ◄── the bootloader writes no higher
           │ app data reserved by the       │  40 KB   device record at 0xEA000
           │ bootloader (DFU_APP_DATA_...)  │
0x000F4000 ├────────────────────────────────┤
           │ Adafruit UF2 bootloader        │  38 KB
0x000FD800 ├────────────────────────────────┤
           │ bootloader config (CF2)        │   2 KB
0x000FE000 ├────────────────────────────────┤
           │ MBR parameter page             │   4 KB
0x000FF000 ├────────────────────────────────┤
           │ bootloader settings            │   4 KB
0x00100000 └────────────────────────────────┘
```

```
MEMORY
{
  FLASH : ORIGIN = 0x00026000, LENGTH = 784K
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
```

### Why the image is linked at `0x26000`

The board ships with the S140 SoftDevice occupying `0x1000` to `0x26000` and
the bootloader at the top of flash. oxinode never enables the SoftDevice (its
Bluetooth stack is a library linked into the image, see
[Bluetooth](../architecture/bluetooth.md)), but it must still link above it:
the MBR hands control to the SoftDevice, which forwards to whatever is at
`0x26000`. Linking at `0x0` would work exactly once and cost the bootloader,
which is the only recovery path on a board with no debug probe.

Because the image boots from behind the SoftDevice, `boot::relocate_vector_table`
points `VTOR` at the application's own vector table before any interrupt is
enabled.

### Why the application stops at `0xEA000`, not `0xF4000`

The bootloader's code starts at `0xF4000`, and it would be natural to give the
application everything below it. The nRF52840 build of this bootloader sets
`DFU_APP_DATA_RESERVED` to ten flash pages (40 KB, "to match circuitpython for
840"), and both of its flashing paths enforce the resulting `USER_FLASH_END`
of `0xEA000`:

- serial DFU rejects any image larger than
  `DFU_IMAGE_MAX_SIZE_FULL = (0xF4000 - 0x26000) - 0xA000 = 0xC4000`;
- the UF2 drive's `write_block()` silently drops any block that fails
  `in_app_space()`, which is `addr < USER_FLASH_END`.

So the real ceiling is **784 KB**. An over-large image simply cannot be
flashed, which is milder than overwriting the bootloader but still a build
that looks fine and then does not work. Two nearby numbers are worth not being
misled by: 824 KB (`0xF4000 - 0x26000`) is where the bootloader's code begins,
and 815 104 (`0xED000 - 0x26000`) is what Meshtastic declares, taken verbatim
from the Adafruit Arduino BSP's linker script, which reserves 28 KB where the
bootloader reserves 40. Trust the bootloader.

### The device record

The 40 KB the bootloader reserves is exactly what provisioning needs: a region
a reflash cannot reach. oxinode puts its device record (the RNode EEPROM
image, the stored radio configuration, the Bluetooth bonds) at `0xEA000`, the
first page of it. The address is derived, not written twice: `build.rs`
exports `APP_FLASH_END` from `memory.x`, and the end of the application region
is the start of the reserved one by definition. Growing the application region
would relocate the record and lose every provisioned board's identity, so do
not move `FLASH`'s length without moving the record. See
[Provisioning and storage](../architecture/provisioning.md).

## RAM

RAM is the full 256 KB because the SoftDevice is never enabled; a SoftDevice
present in flash but disabled reserves no RAM. The Bluetooth controller
library takes its memory from the image's own statics.

The product image has no allocator. Free RAM as shown on the System screen is
the gap between the end of static data (the linker's `__sheap`) and the stack
pointer, which is the only honest number on a board where the stack is the
one thing that grows.

## How the layout is kept honest

`memory.x` is parsed twice: once in Rust for the build (`build.rs`, through
`oxinode_core::linker_script`, which is unit tested) and once in Python for
the host tools (`tools/layout.py`), because the firmware build cannot shell
out to Python and the tools should not have to link Rust. Both suites pin the
same expected addresses against the real file, so a divergence fails one of
them.

Every built image is checked against the layout on every build and again
before every flash by `tools/check_layout.py`: every loadable byte inside
FLASH, RAM usage inside RAM, the vector table's initial stack pointer pointing
into RAM, and the reset vector pointing into FLASH with the Thumb bit set.
Those last two are the words the CPU reads first, and they are exactly what is
wrong when a linker script is wrong. The checker's own tests feed it
deliberately broken images and assert that it rejects each one.

The image checks at build time do not depend on `memory.x` being right about
the bootloader. `tools/verify_flash.py` closes that loop by reading the
board's own `CURRENT.UF2` and comparing its vector table against the built
ELF.
