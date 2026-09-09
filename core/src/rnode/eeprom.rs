//! The device EEPROM: what `rnodeconf` writes into a board to make it a
//! provisioned RNode, and what it reads back to decide whether to trust it.
//!
//! # What the host does with these bytes
//!
//! `rnodeconf` asks for the whole image with `CMD_ROM_READ` and then, in
//! `parse_eeprom`:
//!
//! 1. reads [`addr::INFO_LOCK`]; unless it holds [`INFO_LOCK_BYTE`] the device
//!    is simply "not provisioned" and nothing else is looked at;
//! 2. takes the eleven-byte identity block — product, model, hardware revision,
//!    four bytes of serial, four of manufacture time — and **MD5**s it;
//! 3. compares that against the sixteen bytes at [`addr::CHECKSUM`], and exits
//!    on a mismatch;
//! 4. verifies the 128-byte RSA signature at [`addr::SIGNATURE`] over those
//!    sixteen checksum bytes, against a list of vendor keys plus any local
//!    signing key on the machine;
//! 5. if [`addr::CONF_OK`] holds [`CONF_OK_BYTE`], reads the stored radio
//!    configuration and reports the device as being in TNC mode.
//!
//! So the device does not have to *do* anything with most of this. It has to
//! store it faithfully, hand it back byte for byte, and — this is the part that
//! is easy to skip — mean the same thing by "provisioned" as the host does.
//! [`Eeprom::status`] therefore checks the checksum too, so that a board whose
//! provisioning is half-written says so in its own log rather than looking
//! fine until a host disagrees.
//!
//! # Why the image is 256 bytes
//!
//! Because the wire protocol addresses it with **one byte**: `CMD_ROM_WRITE`
//! carries `[address, value]`. 256 is therefore not a chosen capacity, it is
//! the whole addressable space, and it means [`Eeprom::write`] has no failure
//! mode. The host's own reader caps a `CMD_ROM_READ` frame at 1024 bytes, so
//! there is room to spare in the other direction.
//!
//! # What this board reports itself as
//!
//! [`PRODUCT_HMBRW`] `0xf0` "Hombrew RNode", model [`MODEL_FF`], board
//! [`BOARD_HMBRW`]. That is the honest answer — oxinode is not any of the
//! boards in `rnodeconf`'s table — and it is also the *safe* one: for an nRF52
//! device whose model is `0xff` and whose board is not a RAK4631, `rnodeconf`
//! refuses `--update` with "No firmware found for this board", instead of
//! offering to flash somebody else's firmware onto an LR1121.
//!
//! One consequence to know about rather than be surprised by: `rnodeconf -i`
//! prints "Max TX power: 14 dBm" and "(Band capabilities unknown)" for model
//! `0xff`, because those come from its own table and not from the device. The
//! module is rated 20 dBm sub-GHz, and `oxinode_core::lr1121::config` is what
//! actually enforces that.

use crate::hash::{md5, sha256};
use crate::lr1121::config::RadioConfig;

/// The addressable size of the EEPROM, and the size of the image the device
/// hands back. See the module docs for why it is exactly 256.
pub const SIZE: usize = 256;

/// Byte offsets, as `rnodeconf`'s `ROM` class defines them.
pub mod addr {
    /// Product family. One byte.
    pub const PRODUCT: u8 = 0x00;
    /// Model within the family. One byte; indexes the host's capability table.
    pub const MODEL: u8 = 0x01;
    /// Hardware revision. One byte.
    pub const HW_REV: u8 = 0x02;
    /// Serial number, four bytes big-endian.
    pub const SERIAL: u8 = 0x03;
    /// Manufacture time, four bytes big-endian, seconds since the Unix epoch.
    pub const MADE: u8 = 0x07;
    /// MD5 of the eleven bytes from [`PRODUCT`] to the end of [`MADE`].
    pub const CHECKSUM: u8 = 0x0B;
    /// RSA signature over [`CHECKSUM`], 128 bytes.
    pub const SIGNATURE: u8 = 0x1B;
    /// Holds [`super::INFO_LOCK_BYTE`] when the block above is complete.
    pub const INFO_LOCK: u8 = 0x9B;

    /// Stored spreading factor.
    pub const CONF_SF: u8 = 0x9C;
    /// Stored coding rate.
    pub const CONF_CR: u8 = 0x9D;
    /// Stored transmit power in dBm.
    pub const CONF_TXP: u8 = 0x9E;
    /// Stored bandwidth in hertz, four bytes big-endian.
    pub const CONF_BW: u8 = 0x9F;
    /// Stored centre frequency in hertz, four bytes big-endian.
    pub const CONF_FREQ: u8 = 0xA3;
    /// Holds [`super::CONF_OK_BYTE`] when the block above is valid.
    pub const CONF_OK: u8 = 0xA7;

    /// Bluetooth enabled.
    pub const CONF_BT: u8 = 0xB0;
    /// Display present, intensity, address, blanking, rotation.
    pub const CONF_DSET: u8 = 0xB1;
    /// See [`CONF_DSET`].
    pub const CONF_DINT: u8 = 0xB2;
    /// See [`CONF_DSET`].
    pub const CONF_DADR: u8 = 0xB3;
    /// See [`CONF_DSET`].
    pub const CONF_DBLK: u8 = 0xB4;
    /// See [`CONF_DSET`].
    pub const CONF_DROT: u8 = 0xB8;
    /// Neopixel settings. This board has none.
    pub const CONF_PSET: u8 = 0xB5;
    /// See [`CONF_PSET`].
    pub const CONF_PINT: u8 = 0xB6;
    /// Bluetooth set.
    pub const CONF_BSET: u8 = 0xB7;
    /// Interference avoidance disabled.
    pub const CONF_DIA: u8 = 0xB9;
    /// WiFi mode. This board has no WiFi.
    pub const CONF_WIFI: u8 = 0xBA;
    /// WiFi channel. See [`CONF_WIFI`].
    pub const CONF_WCHN: u8 = 0xBB;
}

/// Written to [`addr::INFO_LOCK`] once the identity block is complete.
pub const INFO_LOCK_BYTE: u8 = 0x73;
/// Written to [`addr::CONF_OK`] once a radio configuration is stored.
pub const CONF_OK_BYTE: u8 = 0x73;

/// Length of the identity block that is checksummed.
pub const INFO_LEN: usize = 11;
/// Length of the stored checksum.
pub const CHECKSUM_LEN: usize = 16;
/// Length of the stored EEPROM signature.
pub const SIGNATURE_LEN: usize = 128;
/// Length of the device signature `rnodeconf --sign` produces. This is an RNS
/// `Identity` signature — Ed25519 — and not the RSA one in the EEPROM.
pub const DEVICE_SIGNATURE_LEN: usize = 64;
/// Length of a device or firmware hash.
pub const HASH_LEN: usize = 32;

/// `ROM.PRODUCT_HMBRW`. See the module docs for why this and not `PRODUCT_RNODE`.
pub const PRODUCT_HMBRW: u8 = 0xF0;
/// `ROM.MODEL_FF`, the model with no declared capabilities.
pub const MODEL_FF: u8 = 0xFF;
/// `ROM.BOARD_HMBRW`, answered to `CMD_BOARD`.
pub const BOARD_HMBRW: u8 = 0x32;

/// What the host will make of this image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// [`addr::INFO_LOCK`] does not hold [`INFO_LOCK_BYTE`]. The host reports
    /// the device as not provisioned and stops looking.
    Unprovisioned,
    /// Locked, but the stored checksum is not the MD5 of the identity block.
    /// The host logs "EEPROM checksum mismatch" and exits.
    ChecksumMismatch,
    /// Locked and self-consistent. Whether the host *trusts* it is a separate
    /// question, answered by the signature, which only the host can check.
    Provisioned,
}

/// The radio configuration a provisioned device stores for TNC mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredConfig {
    pub frequency_hz: u32,
    pub bandwidth_hz: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power_dbm: i8,
}

/// The device EEPROM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eeprom {
    bytes: [u8; SIZE],
}

impl Default for Eeprom {
    fn default() -> Self {
        Self::new()
    }
}

impl Eeprom {
    /// An erased image.
    ///
    /// `0xff` rather than zero, because that is what erased flash reads as and
    /// this image is stored in flash. A wipe and a never-written device should
    /// be the same thing; making them differ would mean the storage layer has
    /// two "empty" states and only one of them round-trips.
    pub const fn new() -> Self {
        Self {
            bytes: [0xFF; SIZE],
        }
    }

    /// Adopt an image read back from storage.
    pub const fn from_bytes(bytes: [u8; SIZE]) -> Self {
        Self { bytes }
    }

    /// The image, for storage or for `CMD_ROM_READ`.
    pub const fn as_bytes(&self) -> &[u8; SIZE] {
        &self.bytes
    }

    /// Read one byte. Total: every `u8` addresses a byte that exists.
    pub const fn read(&self, addr: u8) -> u8 {
        self.bytes[addr as usize]
    }

    /// Write one byte. Total, for the same reason as [`Eeprom::read`].
    pub const fn write(&mut self, addr: u8, value: u8) {
        self.bytes[addr as usize] = value;
    }

    /// Erase the image.
    pub const fn wipe(&mut self) {
        self.bytes = [0xFF; SIZE];
    }

    /// The eleven bytes the checksum covers.
    pub fn info_block(&self) -> [u8; INFO_LEN] {
        let mut out = [0u8; INFO_LEN];
        out.copy_from_slice(&self.bytes[addr::PRODUCT as usize..][..INFO_LEN]);
        out
    }

    /// The MD5 the host will compute over [`Eeprom::info_block`].
    pub fn computed_checksum(&self) -> [u8; CHECKSUM_LEN] {
        md5::digest(&self.info_block())
    }

    /// The sixteen bytes actually stored at [`addr::CHECKSUM`].
    pub fn stored_checksum(&self) -> [u8; CHECKSUM_LEN] {
        let mut out = [0u8; CHECKSUM_LEN];
        out.copy_from_slice(&self.bytes[addr::CHECKSUM as usize..][..CHECKSUM_LEN]);
        out
    }

    /// The stored RSA signature. Only the host can verify it; the device just
    /// keeps it.
    pub fn signature(&self) -> &[u8] {
        &self.bytes[addr::SIGNATURE as usize..][..SIGNATURE_LEN]
    }

    /// What the host will make of this image. See [`Status`].
    pub fn status(&self) -> Status {
        if self.read(addr::INFO_LOCK) != INFO_LOCK_BYTE {
            Status::Unprovisioned
        } else if self.stored_checksum() != self.computed_checksum() {
            Status::ChecksumMismatch
        } else {
            Status::Provisioned
        }
    }

    /// Whether the host would call this device provisioned.
    pub fn is_provisioned(&self) -> bool {
        self.status() == Status::Provisioned
    }

    /// Product, model and hardware revision, whatever the lock byte says.
    pub const fn identity(&self) -> (u8, u8, u8) {
        (
            self.read(addr::PRODUCT),
            self.read(addr::MODEL),
            self.read(addr::HW_REV),
        )
    }

    /// The serial number, big-endian.
    pub const fn serial(&self) -> u32 {
        self.be32(addr::SERIAL)
    }

    /// Manufacture time, seconds since the Unix epoch.
    pub const fn made(&self) -> u32 {
        self.be32(addr::MADE)
    }

    /// The stored radio configuration, if [`addr::CONF_OK`] says there is one.
    ///
    /// Nothing here validates it against the radio's limits. That is
    /// deliberate: this reports what is *stored*, and whether the hardware can
    /// do it is `lr1121::config`'s question, asked in one place rather than two.
    pub const fn stored_config(&self) -> Option<StoredConfig> {
        if self.read(addr::CONF_OK) != CONF_OK_BYTE {
            return None;
        }
        Some(StoredConfig {
            frequency_hz: self.be32(addr::CONF_FREQ),
            bandwidth_hz: self.be32(addr::CONF_BW),
            spreading_factor: self.read(addr::CONF_SF),
            coding_rate: self.read(addr::CONF_CR),
            tx_power_dbm: self.read(addr::CONF_TXP) as i8,
        })
    }

    /// Store a radio configuration and mark it valid: `CMD_CONF_SAVE`.
    pub fn save_config(&mut self, config: &RadioConfig) {
        self.write(addr::CONF_SF, config.spreading_factor);
        self.write(addr::CONF_CR, config.coding_rate);
        self.write(addr::CONF_TXP, config.tx_power_dbm as u8);
        self.write_be32(addr::CONF_BW, config.bandwidth_hz);
        self.write_be32(addr::CONF_FREQ, config.frequency_hz);
        self.write(addr::CONF_OK, CONF_OK_BYTE);
    }

    /// Forget the stored configuration: `CMD_CONF_DELETE`.
    ///
    /// Only the validity byte is cleared. The values are left where they are
    /// because clearing them would be a second thing that could half-happen,
    /// and the byte that decides is the one the host reads.
    pub const fn delete_config(&mut self) {
        self.write(addr::CONF_OK, 0xFF);
    }

    const fn be32(&self, at: u8) -> u32 {
        let i = at as usize;
        (self.bytes[i] as u32) << 24
            | (self.bytes[i + 1] as u32) << 16
            | (self.bytes[i + 2] as u32) << 8
            | self.bytes[i + 3] as u32
    }

    fn write_be32(&mut self, at: u8, value: u32) {
        let bytes = value.to_be_bytes();
        for (i, b) in bytes.iter().enumerate() {
            self.write(at.wrapping_add(i as u8), *b);
        }
    }
}

/// The thirty-two bytes answered to `CMD_DEV_HASH`.
///
/// **This one is oxinode's own definition.** Nothing on the host pins it:
/// `rnodeconf --sign` asks the device for a hash, signs whatever it is given
/// with the machine's device key, and hands the signature back to be stored.
/// So the only requirements are that it be thirty-two bytes, deterministic,
/// and specific to this device — otherwise a signature made for one board
/// would validate another.
///
/// It is therefore SHA-256 over the identity block and the MCU's factory
/// device ID: the provisioning *and* the physical chip. Reprovisioning changes
/// it, which is right — the old signature attested to the old identity.
pub fn device_hash(eeprom: &Eeprom, mcu_id: u64) -> [u8; HASH_LEN] {
    let mut input = [0u8; INFO_LEN + 8];
    input[..INFO_LEN].copy_from_slice(&eeprom.info_block());
    input[INFO_LEN..].copy_from_slice(&mcu_id.to_be_bytes());
    sha256::digest(&input)
}

// Every address the host knows about has to be inside the image, or a write it
// makes would land nowhere. The highest one it defines is the WiFi channel.
const _: () = assert!((addr::CONF_WCHN as usize) < SIZE);
// The identity block, its checksum and the signature must tile without
// overlapping, and the lock byte must sit above all of them -- the host writes
// it last precisely so that it is only set once everything below it is there.
const _: () = assert!(addr::CHECKSUM as usize == addr::PRODUCT as usize + INFO_LEN);
const _: () = assert!(addr::SIGNATURE as usize == addr::CHECKSUM as usize + CHECKSUM_LEN);
const _: () = assert!(addr::INFO_LOCK as usize == addr::SIGNATURE as usize + SIGNATURE_LEN);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::DEFAULT;

    /// Product `0xf0`, model `0xff`, hardware revision 1, serial 1,
    /// manufactured 2025-09-04 — and the MD5 of those eleven bytes as computed
    /// by an independent implementation (Python's `hashlib`), so that this
    /// pins the *host's* arithmetic rather than agreeing with our own.
    const GOLDEN_INFO: [u8; INFO_LEN] = [
        0xF0, 0xFF, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68, 0xB9, 0xB1, 0x40,
    ];
    const GOLDEN_CHECKSUM: [u8; CHECKSUM_LEN] = [
        0x41, 0x9E, 0x21, 0xE3, 0x9C, 0x5B, 0x75, 0x3F, 0x1F, 0x34, 0x41, 0x8C, 0xEE, 0xB5, 0x92,
        0x5E,
    ];

    /// Provision an image the way `rnodeconf --rom` does: identity, checksum,
    /// signature, then the lock byte last.
    fn provisioned() -> Eeprom {
        let mut rom = Eeprom::new();
        for (i, b) in GOLDEN_INFO.iter().enumerate() {
            rom.write(addr::PRODUCT + i as u8, *b);
        }
        for (i, b) in GOLDEN_CHECKSUM.iter().enumerate() {
            rom.write(addr::CHECKSUM + i as u8, *b);
        }
        for i in 0..SIGNATURE_LEN {
            rom.write(addr::SIGNATURE + i as u8, i as u8);
        }
        rom.write(addr::INFO_LOCK, INFO_LOCK_BYTE);
        rom
    }

    /// An erased image is a device that has never been provisioned, and that
    /// is the state a board ships in.
    #[test]
    fn a_new_image_is_erased_and_unprovisioned() {
        let rom = Eeprom::new();
        assert_eq!(rom.status(), Status::Unprovisioned);
        assert!(!rom.is_provisioned());
        assert!(rom.as_bytes().iter().all(|&b| b == 0xFF));
        assert_eq!(rom.stored_config(), None);
    }

    /// The whole point of the module: a device provisioned the way the host
    /// provisions it agrees with the host about what it is.
    #[test]
    fn an_image_written_the_way_rnodeconf_writes_one_is_provisioned() {
        let rom = provisioned();
        assert_eq!(rom.status(), Status::Provisioned);
        assert_eq!(rom.identity(), (PRODUCT_HMBRW, MODEL_FF, 1));
        assert_eq!(rom.serial(), 1);
        assert_eq!(rom.made(), 1_757_000_000);
        assert_eq!(rom.stored_checksum(), GOLDEN_CHECKSUM);
        assert_eq!(rom.computed_checksum(), GOLDEN_CHECKSUM);
    }

    /// The checksum is over the identity block and nothing else, so a change to
    /// any of those eleven bytes must invalidate it — including the ones a
    /// careless implementation might treat as padding.
    #[test]
    fn changing_any_identity_byte_breaks_the_checksum() {
        for at in addr::PRODUCT..addr::PRODUCT + INFO_LEN as u8 {
            let mut rom = provisioned();
            rom.write(at, rom.read(at) ^ 0x01);
            assert_eq!(
                rom.status(),
                Status::ChecksumMismatch,
                "byte {at:#04x} is inside the checksum but changing it did not matter"
            );
        }
    }

    /// The lock byte is written last for a reason: until it is there, a
    /// half-written provisioning must not look like a whole one. The host stops
    /// at that byte and so does this.
    #[test]
    fn without_the_lock_byte_nothing_else_is_looked_at() {
        let mut rom = provisioned();
        rom.write(addr::INFO_LOCK, 0xFF);
        assert_eq!(rom.status(), Status::Unprovisioned);
        // Even with a deliberately wrong checksum, which would otherwise be a
        // mismatch rather than an absence.
        rom.write(addr::CHECKSUM, 0x00);
        assert_eq!(rom.status(), Status::Unprovisioned);
    }

    /// A mismatch is a different thing from an absence, and the host reports
    /// them differently — "not provisioned" versus "EEPROM checksum mismatch".
    /// A device that collapsed them would be less use than the host it serves.
    #[test]
    fn a_wrong_checksum_is_a_mismatch_and_not_an_absence() {
        let mut rom = provisioned();
        rom.write(addr::CHECKSUM + 3, 0x00);
        assert_eq!(rom.status(), Status::ChecksumMismatch);
        assert!(!rom.is_provisioned());
    }

    /// The signature is stored and handed back untouched. The device cannot
    /// check it — that needs the vendor's public key, which lives on the host —
    /// so the one thing it must not do is alter it.
    #[test]
    fn the_signature_round_trips_byte_for_byte() {
        let rom = provisioned();
        let sig = rom.signature();
        assert_eq!(sig.len(), SIGNATURE_LEN);
        for (i, b) in sig.iter().enumerate() {
            assert_eq!(*b, i as u8);
        }
    }

    /// Every one-byte address writes a byte that exists and reads back. This
    /// is what makes `CMD_ROM_WRITE` have no failure mode, and it is only true
    /// because the image is exactly as large as the address space.
    #[test]
    fn every_address_the_wire_can_carry_round_trips() {
        let mut rom = Eeprom::new();
        for addr in 0..=u8::MAX {
            rom.write(addr, addr ^ 0x5A);
        }
        for addr in 0..=u8::MAX {
            assert_eq!(rom.read(addr), addr ^ 0x5A, "{addr:#04x}");
        }
    }

    /// A wipe returns the device to the state it left the factory in, which is
    /// the same state erased flash reads as.
    #[test]
    fn a_wipe_is_indistinguishable_from_a_new_image() {
        let mut rom = provisioned();
        rom.save_config(&DEFAULT);
        rom.wipe();
        assert_eq!(rom, Eeprom::new());
        assert_eq!(rom.status(), Status::Unprovisioned);
    }

    /// TNC mode: a stored configuration comes back as it went in, and the
    /// values are big-endian because that is how the host reads them.
    #[test]
    fn a_stored_configuration_round_trips() {
        let mut rom = Eeprom::new();
        let mut config = DEFAULT;
        config.frequency_hz = 915_000_000;
        config.bandwidth_hz = 125_000;
        config.spreading_factor = 8;
        config.coding_rate = 5;
        config.tx_power_dbm = 17;
        rom.save_config(&config);

        assert_eq!(
            rom.stored_config(),
            Some(StoredConfig {
                frequency_hz: 915_000_000,
                bandwidth_hz: 125_000,
                spreading_factor: 8,
                coding_rate: 5,
                tx_power_dbm: 17,
            })
        );
        // Byte order, as the host reads it.
        assert_eq!(
            &rom.as_bytes()[addr::CONF_FREQ as usize..][..4],
            &[0x36, 0x89, 0xCA, 0xC0]
        );
        assert_eq!(
            &rom.as_bytes()[addr::CONF_BW as usize..][..4],
            &125_000u32.to_be_bytes()
        );
    }

    /// Transmit power is signed on the wire and signed here, so the low-power
    /// PA's negative settings survive a save and a reload. Storing it unsigned
    /// would turn −9 dBm into 247 dBm on the next boot.
    #[test]
    fn a_negative_transmit_power_survives_storage() {
        let mut rom = Eeprom::new();
        let mut config = DEFAULT;
        config.tx_power_dbm = -9;
        rom.save_config(&config);
        assert_eq!(rom.stored_config().unwrap().tx_power_dbm, -9);
    }

    /// Deleting a configuration clears the byte the host reads, and leaves the
    /// values alone. Two things that can half-happen are worse than one.
    #[test]
    fn deleting_a_configuration_clears_only_the_validity_byte() {
        let mut rom = Eeprom::new();
        rom.save_config(&DEFAULT);
        assert!(rom.stored_config().is_some());
        rom.delete_config();
        assert_eq!(rom.stored_config(), None);
        assert_eq!(rom.read(addr::CONF_SF), DEFAULT.spreading_factor);
    }

    /// Provisioning and configuration are independent: a device can be in TNC
    /// mode without being provisioned, and provisioned without a stored
    /// configuration. The host treats them as separate gates and so does this.
    #[test]
    fn provisioning_and_configuration_are_separate_gates() {
        let mut rom = Eeprom::new();
        rom.save_config(&DEFAULT);
        assert_eq!(rom.status(), Status::Unprovisioned);
        assert!(rom.stored_config().is_some());

        let rom = provisioned();
        assert_eq!(rom.status(), Status::Provisioned);
        assert_eq!(rom.stored_config(), None);
    }

    /// The device hash is specific to the physical chip as well as to the
    /// provisioning, so a signature made for one board does not validate
    /// another that happens to carry the same serial number — which two people
    /// provisioning with their own `rnodeconf` counters certainly will.
    #[test]
    fn the_device_hash_separates_two_boards_with_the_same_provisioning() {
        let rom = provisioned();
        let a = device_hash(&rom, 0x0011_2233_4455_6677);
        let b = device_hash(&rom, 0x0011_2233_4455_6678);
        assert_ne!(a, b);
        assert_eq!(a.len(), HASH_LEN);
        // Deterministic: the same board asked twice signs to the same thing.
        assert_eq!(a, device_hash(&rom, 0x0011_2233_4455_6677));
    }

    /// Reprovisioning changes the hash, because the old signature attested to
    /// the old identity.
    #[test]
    fn reprovisioning_changes_the_device_hash() {
        let rom = provisioned();
        let before = device_hash(&rom, 1);
        let mut after = rom;
        after.write(addr::SERIAL + 3, 0x02);
        assert_ne!(before, device_hash(&after, 1));
    }
}
