/* Flash layout for the muzi.works Base Duo (nRF52840, 1 MB flash / 256 KB RAM)
 * as it ships from the factory, running the Adafruit nRF52 UF2 bootloader.
 *
 * Read off the board's own INFO_UF2.TXT:
 *     UF2 Bootloader 0.9.2-37-gf7fad14
 *     Model: muzi Base   Board-ID: muzi-Base-Board
 *     SoftDevice: S140 6.1.1
 *
 *   0x00000000 +--------------------------------+
 *              | Nordic MBR                     |  4 KB
 *   0x00001000 +--------------------------------+
 *              | SoftDevice S140 6.1.1          |  148 KB
 *   0x00026000 +--------------------------------+  <-- our application
 *              | application (oxinode)          |  784 KB
 *   0x000EA000 +--------------------------------+  <-- the bootloader writes no higher
 *              | app data reserved by the       |  40 KB
 *              | bootloader (DFU_APP_DATA_...)  |
 *   0x000F4000 +--------------------------------+
 *              | Adafruit UF2 bootloader        |  38 KB
 *   0x000FD800 +--------------------------------+
 *              | bootloader config (CF2)        |  2 KB
 *   0x000FE000 +--------------------------------+
 *              | MBR parameter page             |  4 KB
 *   0x000FF000 +--------------------------------+
 *              | bootloader settings            |  4 KB
 *   0x00100000 +--------------------------------+
 *
 * We link at 0x26000 rather than 0x0 so the UF2 bootloader survives: it is the
 * only recovery path on a board with no debug probe attached. Do not "reclaim"
 * the SoftDevice region by moving FLASH down -- the MBR hands control to the
 * SoftDevice at 0x1000, which in turn forwards to whatever lives at 0x26000.
 *
 * The ceiling is 0xEA000, not the 0xF4000 where the bootloader's code starts.
 * The nRF52840 build of this bootloader sets DFU_APP_DATA_RESERVED to 10 pages
 * (40 KB, "to match circuitpython for 840"), and BOTH of its flashing paths
 * refuse to go above the resulting USER_FLASH_END:
 *
 *   - serial DFU, which is how we flash, rejects any image larger than
 *     DFU_IMAGE_MAX_SIZE_FULL = (0xF4000 - 0x26000) - 0xA000 = 0xC4000;
 *   - the UF2 drive's write_block() silently discards blocks failing
 *     in_app_space(), which is addr < USER_FLASH_END = 0xEA000.
 *
 * We never put a filesystem in that 40 KB, but the reservation is compiled into
 * the bootloader on the board, so it bounds us regardless. Linking above it
 * produces an image that cannot be flashed, not one that overwrites something.
 *
 * Note that the Adafruit Arduino BSP's nrf52840_s140_v6.ld -- which is what
 * Meshtastic builds this board against, and which claims 0xED000 -- disagrees
 * with the bootloader that has to accept its output. Trust the bootloader.
 *
 * RAM is the full 256 KB because we never call sd_softdevice_enable(); a
 * SoftDevice that is present in flash but disabled reserves no RAM. If oxinode
 * ever enables S140 (it has no reason to -- we want no BLE), this must shrink
 * and gain a matching ORIGIN offset.
 *
 * build.rs parses ORIGIN(FLASH) out of this file, so keep that line's shape.
 */

MEMORY
{
  FLASH : ORIGIN = 0x00026000, LENGTH = 784K
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
