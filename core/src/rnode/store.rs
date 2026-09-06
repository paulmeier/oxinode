//! Everything about this device that has to survive a power cycle, and the
//! record it is written to flash as.
//!
//! Three things persist: the [`Eeprom`] image, the device signature
//! `rnodeconf --sign` produces, and the firmware hash `--firmware-hash` sets.
//! The EEPROM is the only one the host can read back byte for byte; the other
//! two are answered through their own commands. They are stored together
//! because they are written together, by the same tool, in the same minute.
//!
//! # Why there is a checksum over it
//!
//! The record lands in flash, and flash writes are not atomic: the board can
//! lose power between erasing the page and finishing the write. Without a
//! checksum the next boot reads whatever survived and believes it.
//!
//! For the identity block that would be survivable — the host's own MD5 would
//! catch it and say so. For the *stored radio configuration* it would not: a
//! torn record can leave a plausible-looking frequency, and a device in TNC
//! mode brings its radio up from that at boot with nobody watching. A checksum
//! is the difference between coming up unprovisioned and transmitting on a
//! frequency nobody chose.
//!
//! So a record whose magic, version or CRC does not check out is not repaired
//! and not partially trusted. It is discarded, and the device is exactly as it
//! was before it was ever provisioned.

use super::eeprom::{Eeprom, DEVICE_SIGNATURE_LEN, HASH_LEN, SIZE as EEPROM_SIZE};

/// Identifies our record in an otherwise erased flash page. Chosen to be
/// nothing that erased flash (`0xff`) or zeroed flash could produce.
pub const MAGIC: [u8; 4] = *b"OXN1";

/// The record layout's version. A future field goes in by incrementing this
/// and teaching [`DeviceStore::decode`] the old shape — never by changing what
/// a byte means at a version that has already been written to a board.
pub const VERSION: u8 = 1;

/// Bit 0 of the flags byte: a device signature is present.
const FLAG_DEVICE_SIGNATURE: u8 = 1 << 0;
/// Bit 1: a target firmware hash is present.
const FLAG_FIRMWARE_HASH: u8 = 1 << 1;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_FLAGS: usize = 5;
const OFF_CRC: usize = 6;
const OFF_BODY: usize = 10;
const OFF_EEPROM: usize = OFF_BODY;
const OFF_SIGNATURE: usize = OFF_EEPROM + EEPROM_SIZE;
const OFF_FW_HASH: usize = OFF_SIGNATURE + DEVICE_SIGNATURE_LEN;

/// The size of one stored record.
pub const RECORD_LEN: usize = OFF_FW_HASH + HASH_LEN;

/// The persistent device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceStore {
    /// The EEPROM image, as the host reads and writes it.
    pub rom: Eeprom,
    /// A signature over [`super::eeprom::device_hash`], made on the host.
    pub device_signature: Option<[u8; DEVICE_SIGNATURE_LEN]>,
    /// The hash the running firmware is expected to have.
    pub target_firmware_hash: Option<[u8; HASH_LEN]>,
}

impl Default for DeviceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceStore {
    /// A device that has never been provisioned.
    pub const fn new() -> Self {
        Self {
            rom: Eeprom::new(),
            device_signature: None,
            target_firmware_hash: None,
        }
    }

    /// Erase everything: `CMD_ROM_WIPE`.
    ///
    /// The signature goes with the EEPROM, and that is not tidiness. It
    /// attests to the identity being erased, so keeping it would leave a
    /// signature over a device that no longer exists — and the next
    /// provisioning would inherit it.
    pub fn wipe(&mut self) {
        *self = Self::new();
    }

    /// Serialise for storage.
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&MAGIC);
        out[OFF_VERSION] = VERSION;

        let mut flags = 0u8;
        if let Some(sig) = self.device_signature {
            flags |= FLAG_DEVICE_SIGNATURE;
            out[OFF_SIGNATURE..OFF_SIGNATURE + DEVICE_SIGNATURE_LEN].copy_from_slice(&sig);
        }
        if let Some(hash) = self.target_firmware_hash {
            flags |= FLAG_FIRMWARE_HASH;
            out[OFF_FW_HASH..OFF_FW_HASH + HASH_LEN].copy_from_slice(&hash);
        }
        out[OFF_FLAGS] = flags;
        out[OFF_EEPROM..OFF_EEPROM + EEPROM_SIZE].copy_from_slice(self.rom.as_bytes());

        // Over the flags and the body, so a flipped presence bit is caught too.
        let crc = crc32(&out[OFF_FLAGS..OFF_FLAGS + 1]);
        let crc = crc32_continue(crc, &out[OFF_BODY..]);
        out[OFF_CRC..OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Read a record back, or `None` if there is not one there.
    ///
    /// `None` covers erased flash, a record written by a different version,
    /// and a write that was interrupted. All three mean the same thing to the
    /// caller — start from [`DeviceStore::new`] — and telling them apart would
    /// invite treating one of them as recoverable.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < RECORD_LEN {
            return None;
        }
        if bytes[OFF_MAGIC..OFF_MAGIC + 4] != MAGIC || bytes[OFF_VERSION] != VERSION {
            return None;
        }
        let stored = u32::from_le_bytes([
            bytes[OFF_CRC],
            bytes[OFF_CRC + 1],
            bytes[OFF_CRC + 2],
            bytes[OFF_CRC + 3],
        ]);
        let crc = crc32(&bytes[OFF_FLAGS..OFF_FLAGS + 1]);
        let crc = crc32_continue(crc, &bytes[OFF_BODY..RECORD_LEN]);
        if crc != stored {
            return None;
        }

        let flags = bytes[OFF_FLAGS];
        let mut rom = [0u8; EEPROM_SIZE];
        rom.copy_from_slice(&bytes[OFF_EEPROM..OFF_EEPROM + EEPROM_SIZE]);

        let device_signature = (flags & FLAG_DEVICE_SIGNATURE != 0).then(|| {
            let mut sig = [0u8; DEVICE_SIGNATURE_LEN];
            sig.copy_from_slice(&bytes[OFF_SIGNATURE..OFF_SIGNATURE + DEVICE_SIGNATURE_LEN]);
            sig
        });
        let target_firmware_hash = (flags & FLAG_FIRMWARE_HASH != 0).then(|| {
            let mut hash = [0u8; HASH_LEN];
            hash.copy_from_slice(&bytes[OFF_FW_HASH..OFF_FW_HASH + HASH_LEN]);
            hash
        });

        Some(Self {
            rom: Eeprom::from_bytes(rom),
            device_signature,
            target_firmware_hash,
        })
    }
}

/// CRC-32/ISO-HDLC, computed a bit at a time.
///
/// No lookup table: this runs over 350 bytes a handful of times in the life of
/// a board, and a 1 KB table to make that faster would cost more flash than
/// the whole record.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_continue(0, data)
}

/// Continue a CRC over another slice. `crc` is the finalised value so far.
pub fn crc32_continue(crc: u32, data: &[u8]) -> u32 {
    let mut value = !crc;
    for &byte in data {
        value ^= byte as u32;
        for _ in 0..8 {
            let mask = (value & 1).wrapping_neg();
            value = (value >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !value
}

// The record has to fit in one flash page, because the storage layer erases a
// page to write one and a record spanning two would be two erases -- and the
// window between them is exactly the failure this design is avoiding.
const _: () = assert!(RECORD_LEN <= 4096);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::DEFAULT;

    fn populated() -> DeviceStore {
        let mut store = DeviceStore::new();
        store.rom.write(super::super::eeprom::addr::PRODUCT, 0xF0);
        store.rom.write(super::super::eeprom::addr::MODEL, 0xFF);
        store.rom.save_config(&DEFAULT);
        store.device_signature = Some([0xA5; DEVICE_SIGNATURE_LEN]);
        store.target_firmware_hash = Some([0x5A; HASH_LEN]);
        store
    }

    /// The check vector everyone uses. A CRC that agrees with this is the one
    /// the rest of the world calls CRC-32.
    #[test]
    fn the_crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// Feeding it in pieces must give the same answer as feeding it whole,
    /// because the record is checksummed as two ranges with a gap.
    #[test]
    fn a_crc_computed_in_pieces_matches_one_computed_whole() {
        let whole = crc32(b"123456789");
        let pieces = crc32_continue(crc32(b"1234"), b"56789");
        assert_eq!(whole, pieces);
    }

    #[test]
    fn a_populated_store_round_trips() {
        let store = populated();
        let decoded = DeviceStore::decode(&store.encode()).expect("valid record");
        assert_eq!(decoded, store);
    }

    #[test]
    fn an_empty_store_round_trips() {
        let store = DeviceStore::new();
        assert_eq!(DeviceStore::decode(&store.encode()), Some(store));
    }

    /// Erased flash is not a record. This is the state a board is in before it
    /// has ever been provisioned, and it must not decode as anything.
    #[test]
    fn erased_flash_is_not_a_record() {
        assert_eq!(DeviceStore::decode(&[0xFF; RECORD_LEN]), None);
        assert_eq!(DeviceStore::decode(&[0x00; RECORD_LEN]), None);
        assert_eq!(DeviceStore::decode(&[]), None);
    }

    /// The point of the checksum. Every single byte of the record is covered,
    /// so no interrupted write can produce something that decodes.
    #[test]
    fn flipping_any_byte_of_the_record_makes_it_undecodable() {
        let encoded = populated().encode();
        for i in 0..RECORD_LEN {
            let mut corrupt = encoded;
            corrupt[i] ^= 0x01;
            assert_eq!(
                DeviceStore::decode(&corrupt),
                None,
                "byte {i} was changed and the record still decoded"
            );
        }
    }

    /// A truncated write -- power lost part way through -- is the realistic
    /// version of the above, and must also be rejected rather than padded.
    #[test]
    fn a_truncated_record_is_rejected() {
        let encoded = populated().encode();
        for cut in [0, 1, 9, OFF_EEPROM, RECORD_LEN - 1] {
            assert_eq!(DeviceStore::decode(&encoded[..cut]), None, "cut at {cut}");
        }
    }

    /// A record from a future layout is not read as if it were this one.
    #[test]
    fn a_record_of_another_version_is_not_decoded() {
        let mut encoded = populated().encode();
        encoded[OFF_VERSION] = VERSION + 1;
        assert_eq!(DeviceStore::decode(&encoded), None);
    }

    /// Presence is a flag rather than a sentinel value, so an all-zero
    /// signature is a signature and an absent one is absent. A device that
    /// used "all zero means missing" would refuse a real signature that
    /// happened to be zero, and more usefully, would report a wiped board as
    /// having one.
    #[test]
    fn an_absent_signature_and_a_zero_signature_are_different_things() {
        let mut with_zero = DeviceStore::new();
        with_zero.device_signature = Some([0; DEVICE_SIGNATURE_LEN]);
        let decoded = DeviceStore::decode(&with_zero.encode()).unwrap();
        assert_eq!(decoded.device_signature, Some([0; DEVICE_SIGNATURE_LEN]));

        let without = DeviceStore::new();
        assert_eq!(
            DeviceStore::decode(&without.encode())
                .unwrap()
                .device_signature,
            None
        );
    }

    /// A wipe leaves nothing behind -- including the signature, which attested
    /// to the identity that was just erased.
    #[test]
    fn a_wipe_takes_the_signature_with_it() {
        let mut store = populated();
        store.wipe();
        assert_eq!(store, DeviceStore::new());
        assert_eq!(store.device_signature, None);
        assert_eq!(store.target_firmware_hash, None);
        assert!(!store.rom.is_provisioned());
    }
}
