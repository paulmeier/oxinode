//! Where the device's provisioning lives, and why it survives a reflash.
//!
//! The record goes in the **40 KB the bootloader reserves for application
//! data**, immediately above the application region. That is not a spare
//! corner of flash we found; it is the region the Adafruit bootloader sets
//! aside with `DFU_APP_DATA_RESERVED` and then refuses to write through, on
//! both of its flashing paths — serial DFU rejects any image that would reach
//! it, and the UF2 drive silently drops blocks addressed above it.
//!
//! So a firmware update cannot touch it, which is exactly the property
//! provisioning needs: `rnodeconf` writes an identity and a signature into a
//! board once, and flashing a new oxinode image afterwards must not take them
//! away. The 40 KB was costing us nothing and this is what it is for.
//!
//! The address is derived from `memory.x` rather than written down again —
//! [`boot::APP_FLASH_END`] is the end of the application region, which is the
//! start of the reserved one by definition.
//!
//! # What this costs while it runs
//!
//! Erasing a page on the nRF52840 stalls the CPU for something like 85 ms, and
//! writing the record another few. Nothing else in the executor runs during
//! that, USB included. That is why [`Action::Persist`] means "eventually" and
//! this is called once the writes stop, rather than 155 times while they are
//! arriving.
//!
//! [`Action::Persist`]: oxinode_core::rnode::protocol::Action::Persist

use embassy_nrf::nvmc::{Nvmc, PAGE_SIZE};
use embassy_nrf::peripherals::NVMC;
use embassy_nrf::Peri;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use oxinode_core::rnode::store::{DeviceStore, RECORD_LEN};

use crate::boot;

/// The page the record lives in: the first of the bootloader's reserved forty
/// kilobytes.
pub const RECORD_ADDR: u32 = boot::APP_FLASH_END;

/// Where the bootloader's own code starts, from its board build. Recorded here
/// only so the assertion below has something to check against; nothing writes
/// anywhere near it.
const BOOTLOADER_START: u32 = 0x000F_4000;

/// Why a record could not be written.
///
/// Kept as four cases rather than one, because they mean different things to
/// whoever is reading the log: an erase or write failure is the flash
/// controller refusing, and a verify failure is a write that was accepted and
/// did not take — which is the one that would otherwise be discovered weeks
/// later as a board that forgets its provisioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum StoreError {
    /// The page erase failed.
    Erase,
    /// The write failed.
    Write,
    /// The record could not be read back.
    Read,
    /// It read back as something other than what was written.
    Verify,
}

/// Reads and writes the persistent device record.
pub struct Storage<'d> {
    nvmc: Nvmc<'d>,
}

impl<'d> Storage<'d> {
    pub fn new(nvmc: Peri<'d, NVMC>) -> Self {
        Self {
            nvmc: Nvmc::new(nvmc),
        }
    }

    /// Read the record back, or the state of a device that has never been
    /// provisioned.
    ///
    /// There is no third answer. A record that fails its checksum, or was
    /// written by a layout this firmware does not know, is discarded — see
    /// `oxinode_core::rnode::store` for why partial trust is the wrong thing
    /// to extend to a record that decides what frequency the radio comes up on.
    pub fn load(&mut self) -> DeviceStore {
        let mut buf = [0u8; RECORD_LEN];
        if self.nvmc.read(RECORD_ADDR, &mut buf).is_err() {
            defmt::error!("store: could not read the record page");
            return DeviceStore::new();
        }
        match DeviceStore::decode(&buf) {
            Some(store) => {
                defmt::info!(
                    "store: loaded from {=u32:#x}, provisioned={=bool}, configured={=bool}",
                    RECORD_ADDR,
                    store.rom.is_provisioned(),
                    store.rom.stored_config().is_some()
                );
                store
            }
            None => {
                // The overwhelmingly common case, and not an error: a board
                // that nobody has run `rnodeconf --rom` against.
                defmt::info!("store: nothing stored at {=u32:#x}", RECORD_ADDR);
                DeviceStore::new()
            }
        }
    }

    /// Write the record out, erasing its page first.
    ///
    /// Blocking, and long enough to notice: see the module docs.
    pub fn save(&mut self, store: &DeviceStore) -> Result<(), StoreError> {
        let record = store.encode();
        self.nvmc
            .erase(RECORD_ADDR, RECORD_ADDR + PAGE_SIZE as u32)
            .map_err(|_| StoreError::Erase)?;
        self.nvmc
            .write(RECORD_ADDR, &record)
            .map_err(|_| StoreError::Write)?;

        // Read it back before claiming it is stored. This is the one layer of
        // the project that cannot be unit tested, and a write that did not
        // take would otherwise be discovered by a host, later, as a board that
        // forgets its provisioning across a power cycle.
        let mut check = [0u8; RECORD_LEN];
        self.nvmc
            .read(RECORD_ADDR, &mut check)
            .map_err(|_| StoreError::Read)?;
        if DeviceStore::decode(&check).as_ref() != Some(store) {
            return Err(StoreError::Verify);
        }
        defmt::info!(
            "store: {=usize} bytes written to {=u32:#x}",
            RECORD_LEN,
            RECORD_ADDR
        );
        Ok(())
    }
}

// The record page must be page-aligned, because that is the unit of erase, and
// a misaligned erase would take out the top of the application instead.
const _: () = assert!(RECORD_ADDR as usize % PAGE_SIZE == 0);
// It must sit inside the reserved region and nowhere near the bootloader.
const _: () = assert!(RECORD_ADDR + PAGE_SIZE as u32 <= BOOTLOADER_START);
// And a record has to fit in the page we erase for it.
const _: () = assert!(RECORD_LEN <= PAGE_SIZE);
