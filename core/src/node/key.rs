//! The board's Reticulum identity, and the record it is kept in.
//!
//! One identity for the board's life. Every destination the node has is
//! derived from it -- `lxmf.delivery` is the one peers keep conversations
//! with -- so a board that made a new one at every boot, as the spike did,
//! would be a stranger to everyone it had spoken to.
//!
//! # A record of its own
//!
//! It is kept in the bootloader's reserved forty kilobytes, which no reflash
//! touches, on the page after the device record rather than in it:
//!
//! * **An older image still reads its device record.** A new version of the
//!   device record would be one an older release does not know, and the
//!   device record's rule is that an unknown record is no record at all --
//!   so flashing an older release would make a provisioned board come up
//!   unprovisioned, and its next save would write the identity away.
//! * **`rnodeconf --eeprom-wipe` leaves it alone.** The wipe is how a
//!   provisioned RNode is re-provisioned, and it has always taken the
//!   EEPROM, the signature and the bonds with it. The identity is not the
//!   RNode's: it is what peers know the board by. Re-provisioning the modem
//!   is not a reason for every conversation with it to end.
//! * **The RNode never writes it.** An image without the node does not read
//!   or write this page, so it cannot lose it either.
//!
//! # The private key does not leave
//!
//! [`NodeKey`] holds the 64 bytes and offers them in exactly one form, the
//! record [`NodeKey::record`] writes, which is for the flash and nothing
//! else. Its `Debug` says nothing about them. `tools/test_secrets.py` holds
//! the firmware to that: the record is written in one place, and nothing
//! outside this module asks the stack for a private key.

use rns_crypto::identity::Identity;

use super::Rng;
use crate::rnode::store::{crc32, crc32_continue};

/// Identifies the record in an otherwise erased page. Nothing erased (`0xff`)
/// or zeroed flash produces.
pub const MAGIC: [u8; 4] = *b"OXK1";
/// The record layout's version. See the device record's rule: a byte's
/// meaning never changes at a version that has been written to a board.
pub const VERSION: u8 = 1;
/// An X25519 private key and an Ed25519 seed, in Reticulum's order.
pub const KEY_LEN: usize = 64;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
/// Three bytes of nothing, so the header is eight and the record is a whole
/// number of words for the flash controller.
const OFF_CRC: usize = 8;
const OFF_KEY: usize = 12;
/// The size of the record in flash.
pub const RECORD_LEN: usize = OFF_KEY + KEY_LEN;
const _: () = assert!(RECORD_LEN % 4 == 0);

/// The hashes a person can see. Defined with the screens, which show them
/// in every image, the RNode's included.
pub use crate::screens::NodeHashes as Hashes;

/// The board's private key. See the module docs for what it will not do.
#[derive(Clone, PartialEq, Eq)]
pub struct NodeKey([u8; KEY_LEN]);

impl core::fmt::Debug for NodeKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("NodeKey(..)")
    }
}

impl NodeKey {
    /// A new identity, from `rng`: the node's ChaCha20, seeded from the
    /// chip's hardware RNG.
    pub fn generate(rng: &mut dyn Rng) -> Self {
        // An identity the stack has just made always holds its private key,
        // and a key of zeros in its place would be worse than stopping.
        let identity = Identity::new(rng);
        Self(
            identity
                .get_private_key()
                .expect("a new identity has its private key"),
        )
    }

    /// From the 64 bytes Python's `Identity.from_bytes` takes, for tests
    /// against identities Python made.
    #[cfg(test)]
    pub(crate) fn from_private(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// The stack's identity, to sign and decrypt with.
    pub fn identity(&self) -> Identity {
        Identity::from_private_key(&self.0)
    }

    /// The identity hash and the LXMF address.
    pub fn hashes(&self) -> Hashes {
        let identity = *self.identity().hash();
        Hashes {
            identity,
            lxmf: rns_core::destination::destination_hash("lxmf", &["delivery"], Some(&identity)),
        }
    }

    /// The record for the flash: the only form the key's bytes take outside
    /// this type.
    pub fn record(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&MAGIC);
        out[OFF_VERSION] = VERSION;
        out[OFF_KEY..].copy_from_slice(&self.0);
        let crc = record_crc(&out);
        out[OFF_CRC..OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// What a page holds: a key, nothing ever written, or something else.
    pub fn read(bytes: &[u8]) -> Stored {
        let Some(bytes) = bytes.get(..RECORD_LEN) else {
            return Stored::Damaged;
        };
        if bytes.iter().all(|&b| b == 0xff) {
            return Stored::Erased;
        }
        let stored = u32::from_le_bytes([
            bytes[OFF_CRC],
            bytes[OFF_CRC + 1],
            bytes[OFF_CRC + 2],
            bytes[OFF_CRC + 3],
        ]);
        if bytes[OFF_MAGIC..OFF_MAGIC + 4] != MAGIC
            || bytes[OFF_VERSION] != VERSION
            || record_crc(bytes) != stored
        {
            return Stored::Damaged;
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes[OFF_KEY..]);
        Stored::Key(Self(key))
    }
}

/// What [`NodeKey::read`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Stored {
    Key(NodeKey),
    /// The page has never been written: a board's first boot with a node.
    Erased,
    /// Written, and not a record this firmware can trust: torn, from another
    /// layout, or something else's. The key in it, if there was one, is gone
    /// either way, and the board makes a new identity -- but the log says
    /// which of the two happened.
    Damaged,
}

/// The CRC over the whole record except the four bytes that hold it, as the
/// device record does it.
fn record_crc(record: &[u8]) -> u32 {
    crc32_continue(crc32(&record[..OFF_CRC]), &record[OFF_CRC + 4..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Entropy;

    const VECTORS: &str = include_str!("vectors/vectors.txt");

    fn vector(name: &str) -> [u8; 16] {
        let line = VECTORS
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .unwrap();
        core::array::from_fn(|i| u8::from_str_radix(&line[2 * i..2 * i + 2], 16).unwrap())
    }

    /// The identity `tools/node_vectors.py` made with Python's
    /// `Identity.from_bytes(bytes(range(64)))`.
    fn python() -> NodeKey {
        NodeKey::from_private(core::array::from_fn(|i| i as u8))
    }

    #[test]
    fn the_hashes_are_the_ones_python_computes_for_the_same_key() {
        let hashes = python().hashes();
        assert_eq!(hashes.identity, vector("identity"));
        assert_eq!(hashes.lxmf, vector("destination"));
    }

    #[test]
    fn a_key_survives_its_record() {
        let key = NodeKey::generate(&mut Entropy::from_seed([3; 32]));
        assert_eq!(NodeKey::read(&key.record()), Stored::Key(key));
    }

    #[test]
    fn erased_flash_is_a_first_boot_not_damage() {
        assert_eq!(NodeKey::read(&[0xff; RECORD_LEN]), Stored::Erased);
    }

    #[test]
    fn any_changed_byte_is_damage() {
        let record = python().record();
        for i in 0..RECORD_LEN {
            let mut torn = record;
            torn[i] ^= 0x01;
            assert_eq!(NodeKey::read(&torn), Stored::Damaged, "byte {i}");
        }
    }

    #[test]
    fn a_short_page_and_a_zeroed_page_are_damage() {
        assert_eq!(
            NodeKey::read(&python().record()[..RECORD_LEN - 1]),
            Stored::Damaged
        );
        assert_eq!(NodeKey::read(&[0; RECORD_LEN]), Stored::Damaged);
    }

    /// A record whose checksum is right for what it holds, and which is not
    /// this layout: a later version, or the device record's magic. The CRC
    /// cannot tell these apart from a key; only the header can.
    #[test]
    fn a_consistent_record_of_another_layout_is_not_a_key() {
        let resealed = |change: fn(&mut [u8; RECORD_LEN])| {
            let mut record = python().record();
            change(&mut record);
            let crc = record_crc(&record);
            record[OFF_CRC..OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
            NodeKey::read(&record)
        };
        assert_eq!(resealed(|_| {}), Stored::Key(python()), "the control reads");
        assert_eq!(resealed(|r| r[OFF_VERSION] = VERSION + 1), Stored::Damaged);
        assert_eq!(
            resealed(|r| r[..4].copy_from_slice(b"OXN1")),
            Stored::Damaged
        );
    }

    #[test]
    fn a_generated_key_is_a_working_identity_and_its_own() {
        let a = NodeKey::generate(&mut Entropy::from_seed([1; 32]));
        let b = NodeKey::generate(&mut Entropy::from_seed([2; 32]));
        assert_ne!(a.hashes(), b.hashes(), "the seed decides the key");
        // It signs, and what it signs verifies against its public half.
        let identity = a.identity();
        let signature = identity.sign(b"hello").unwrap();
        let public = Identity::from_public_key(&identity.get_public_key().unwrap()).unwrap();
        assert!(public.verify(&signature, b"hello"));
        assert!(!public.verify(&signature, b"hellp"));
    }

    #[test]
    fn debug_says_nothing_about_the_key() {
        let key = python();
        let shown = format!("{key:?} {:?}", Stored::Key(key.clone()));
        assert_eq!(shown, "NodeKey(..) Key(NodeKey(..))");
    }
}
