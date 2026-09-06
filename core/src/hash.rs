//! The two digests the provisioning protocol needs.
//!
//! * [`md5`] — because that is what checksums an RNode's EEPROM. It is not a
//!   choice; the host computes MD5 over the device's identity block and refuses
//!   the device if the sixteen bytes stored on it disagree.
//! * [`sha256`] — for the device hash, which is oxinode's own definition (see
//!   [`crate::rnode::eeprom::device_hash`]) rather than something a host pins.
//!
//! Neither is used for anything security-bearing here. MD5 is a checksum in the
//! literal sense: it detects a mangled EEPROM, and the *signature* over it —
//! made and verified on the host — is what carries any trust. Saying so
//! explicitly matters, because "MD5" in 2026 otherwise reads as a mistake.
//!
//! Both are one-shot over a slice. Everything hashed in this project is under a
//! hundred bytes, so a streaming API would be unused surface.

pub mod md5;
pub mod sha256;
