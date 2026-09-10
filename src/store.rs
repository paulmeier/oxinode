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
//! # Two ways of writing it
//!
//! The record's layout, and the decision *what* to write, live in
//! `oxinode_core::rnode::store`, which is pure and host-tested. This module
//! only knows how to get the bytes into the chip, and it knows two ways,
//! behind one trait ([`RecordFlash`]):
//!
//! * **Direct**, through the NVMC. Erasing a page stalls the CPU for about
//!   85 ms and writing the record another few, during which nothing else in
//!   the executor runs — USB included, and the Bluetooth controller, which
//!   cannot hold a connection through it. This is the path for an image
//!   without a controller, and for the product image if its controller failed
//!   to start.
//! * **Scheduled**, through the controller's own timeslot API
//!   (`nrf_mpsl::Flash`). The erase is done in 10 ms partial slices and the
//!   write a few words at a time, each inside a timeslot the controller fits
//!   between its radio events, so a phone stays connected through a
//!   provisioning run. The cost is that the write is asynchronous and takes
//!   longer end to end — a few hundred milliseconds with a connection up —
//!   and that a timeslot the scheduler cannot fit comes back as an error
//!   rather than a stall, which is why [`Storage::save`] retries.
//!
//! Either way the write is asynchronous from the caller's side, and the caller
//! awaits it to completion: nothing queues a second write while the first is
//! in flight, because the modem loop that owns the storage does not run again
//! until [`Storage::save`] has returned. That is also why [`Action::Persist`]
//! means "eventually" and this is called once the writes stop, rather than 155
//! times while they are arriving.
//!
//! [`Action::Persist`]: oxinode_core::rnode::protocol::Action::Persist

use embassy_nrf::nvmc::{Nvmc, PAGE_SIZE};
use embassy_nrf::peripherals::NVMC;
use embassy_nrf::Peri;
use embassy_time::{Duration, Instant, Timer};
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

/// How many times a write is attempted before it is given up on.
///
/// The direct path does not fail in practice. The scheduled one can: a
/// timeslot the controller could not fit — a connection with a very short
/// interval, or a request that lost to a radio event — comes back as an error
/// from the scheduler, and the right answer is to ask again rather than to
/// lose a provisioning. Each attempt starts over from the erase, because a
/// word that has been written twice since its last erase may not be written
/// a third time.
const ATTEMPTS: u8 = 4;

/// How long to wait between attempts: long enough for a connection event to
/// have come and gone.
const RETRY_AFTER: Duration = Duration::from_millis(100);

/// Why a record could not be written.
///
/// Kept as four cases rather than one, because they mean different things to
/// whoever is reading the log: an erase or write failure is the flash
/// controller (or the scheduler in front of it) refusing, and a verify failure
/// is a write that was accepted and did not take — which is the one that would
/// otherwise be discovered weeks later as a board that forgets its
/// provisioning.
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

/// A way of getting the record's bytes into the chip.
///
/// Reads never need scheduling: the flash is memory-mapped and a read is a
/// copy. Erases and writes are where the two implementations differ, and both
/// are asynchronous so that a caller is written once for either.
#[allow(async_fn_in_trait)]
pub trait RecordFlash {
    /// What the log calls this path.
    fn name(&self) -> &'static str;

    /// Copy `buf.len()` bytes out of the flash at `addr`.
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), StoreError>;

    /// Erase the page at `addr`, which must be page-aligned.
    async fn erase_page(&mut self, addr: u32) -> Result<(), StoreError>;

    /// Write `data` at `addr`; both word-aligned.
    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), StoreError>;
}

/// The NVMC, driven directly. Blocking, and long enough to notice.
impl RecordFlash for Nvmc<'_> {
    fn name(&self) -> &'static str {
        "nvmc"
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), StoreError> {
        ReadNorFlash::read(self, addr, buf).map_err(|_| StoreError::Read)
    }

    async fn erase_page(&mut self, addr: u32) -> Result<(), StoreError> {
        NorFlash::erase(self, addr, addr + PAGE_SIZE as u32).map_err(|e| {
            defmt::warn!("store: nvmc erase failed: {}", defmt::Debug2Format(&e));
            StoreError::Erase
        })
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), StoreError> {
        NorFlash::write(self, addr, data).map_err(|e| {
            defmt::warn!("store: nvmc write failed: {}", defmt::Debug2Format(&e));
            StoreError::Write
        })
    }
}

/// The controller's flash scheduler: every erase slice and every run of words
/// goes inside a timeslot the controller has fitted between its radio events.
#[cfg(feature = "ble")]
impl RecordFlash for nrf_mpsl::Flash<'_> {
    fn name(&self) -> &'static str {
        "mpsl"
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), StoreError> {
        nrf_mpsl::Flash::read(self, addr, buf).map_err(|_| StoreError::Read)
    }

    async fn erase_page(&mut self, addr: u32) -> Result<(), StoreError> {
        nrf_mpsl::Flash::erase(self, addr, addr + PAGE_SIZE as u32)
            .await
            .map_err(|e| {
                defmt::warn!("store: scheduled erase failed: {}", e);
                StoreError::Erase
            })
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), StoreError> {
        nrf_mpsl::Flash::write(self, addr, data).await.map_err(|e| {
            defmt::warn!("store: scheduled write failed: {}", e);
            StoreError::Write
        })
    }
}

/// Whichever of the two paths an image ended up with.
///
/// The product image decides at boot: if the Bluetooth controller came up the
/// record goes through its scheduler, and if it did not — the board is USB
/// only until it is reset, and says so — the NVMC is driven directly, exactly
/// as an image without a controller drives it. The choice cannot be made at
/// compile time because the failure cannot.
pub enum Flash<'d> {
    /// The NVMC, driven directly.
    Direct(Nvmc<'d>),
    /// The controller's timeslot scheduler.
    #[cfg(feature = "ble")]
    Scheduled(nrf_mpsl::Flash<'d>),
}

impl<'d> Flash<'d> {
    /// Drive the NVMC directly.
    pub fn direct(nvmc: Peri<'d, NVMC>) -> Self {
        Self::Direct(Nvmc::new(nvmc))
    }

    /// Write through the controller's scheduler. `mpsl` must have been
    /// started with timeslot support — see `ble::bring_up`.
    #[cfg(feature = "ble")]
    pub fn scheduled(
        mpsl: &'d nrf_mpsl::MultiprotocolServiceLayer<'d>,
        nvmc: Peri<'d, NVMC>,
    ) -> Self {
        Self::Scheduled(nrf_mpsl::Flash::take(mpsl, nvmc))
    }
}

impl RecordFlash for Flash<'_> {
    fn name(&self) -> &'static str {
        match self {
            Self::Direct(f) => f.name(),
            #[cfg(feature = "ble")]
            Self::Scheduled(f) => f.name(),
        }
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), StoreError> {
        match self {
            Self::Direct(f) => RecordFlash::read(f, addr, buf),
            #[cfg(feature = "ble")]
            Self::Scheduled(f) => RecordFlash::read(f, addr, buf),
        }
    }

    async fn erase_page(&mut self, addr: u32) -> Result<(), StoreError> {
        match self {
            Self::Direct(f) => RecordFlash::erase_page(f, addr).await,
            #[cfg(feature = "ble")]
            Self::Scheduled(f) => RecordFlash::erase_page(f, addr).await,
        }
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), StoreError> {
        match self {
            Self::Direct(f) => RecordFlash::write(f, addr, data).await,
            #[cfg(feature = "ble")]
            Self::Scheduled(f) => RecordFlash::write(f, addr, data).await,
        }
    }
}

/// Reads and writes the persistent device record, through whichever path the
/// image has.
pub struct Storage<F: RecordFlash> {
    flash: F,
}

impl<F: RecordFlash> Storage<F> {
    pub fn new(flash: F) -> Self {
        Self { flash }
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
        if self.flash.read(RECORD_ADDR, &mut buf).is_err() {
            defmt::error!("store: could not read the record page");
            return DeviceStore::new();
        }
        match DeviceStore::decode(&buf) {
            Some(store) => {
                defmt::info!(
                    "store: loaded from {=u32:#x}, provisioned={=bool}, configured={=bool}, writes via {=str}",
                    RECORD_ADDR,
                    store.rom.is_provisioned(),
                    store.rom.stored_config().is_some(),
                    self.flash.name()
                );
                store
            }
            None => {
                // The overwhelmingly common case, and not an error: a board
                // that nobody has run `rnodeconf --rom` against.
                defmt::info!(
                    "store: nothing stored at {=u32:#x}, writes via {=str}",
                    RECORD_ADDR,
                    self.flash.name()
                );
                DeviceStore::new()
            }
        }
    }

    /// Write the record out, erasing its page first, and do not return until
    /// it has landed or been given up on.
    ///
    /// Retried from the erase on any failure — see [`ATTEMPTS`]. Never cancel
    /// this: a scheduled write in flight has handed the scheduler a pointer
    /// into `store`'s encoding, and the modem loop relies on the write having
    /// finished before it looks at the record again.
    pub async fn save(&mut self, store: &DeviceStore) -> Result<(), StoreError> {
        let record = store.encode();
        let started = Instant::now();
        let mut attempt = 1;
        loop {
            match self.attempt(store, &record).await {
                Ok(()) => {
                    defmt::info!(
                        "store: {=usize} bytes written to {=u32:#x} via {=str} in {=u64} ms{=str}",
                        RECORD_LEN,
                        RECORD_ADDR,
                        self.flash.name(),
                        started.elapsed().as_millis(),
                        if attempt > 1 { ", after a retry" } else { "" }
                    );
                    return Ok(());
                }
                Err(e) if attempt < ATTEMPTS => {
                    defmt::warn!(
                        "store: attempt {=u8} of {=u8} failed: {}; retrying",
                        attempt,
                        ATTEMPTS,
                        e
                    );
                    attempt += 1;
                    Timer::after(RETRY_AFTER).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One erase, write and read-back.
    async fn attempt(
        &mut self,
        store: &DeviceStore,
        record: &[u8; RECORD_LEN],
    ) -> Result<(), StoreError> {
        self.flash.erase_page(RECORD_ADDR).await?;
        self.flash.write(RECORD_ADDR, record).await?;

        // Read it back before claiming it is stored. This is the one layer of
        // the project that cannot be unit tested, and a write that did not
        // take would otherwise be discovered by a host, later, as a board that
        // forgets its provisioning across a power cycle.
        let mut check = [0u8; RECORD_LEN];
        self.flash.read(RECORD_ADDR, &mut check)?;
        if DeviceStore::decode(&check).as_ref() != Some(store) {
            return Err(StoreError::Verify);
        }
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
// The scheduled path writes whole words, and the record is padded to one.
const _: () = assert!(RECORD_LEN % 4 == 0);
