//! The BLE side of the RNode host protocol.
//!
//! Sideband (and Reticulum's Android `RNodeInterface`) reach an RNode over
//! Bluetooth by connecting to a **Nordic UART Service** peripheral and pushing
//! the *same* KISS byte stream that the USB serial port carries. There is no
//! separate Bluetooth protocol: BLE is a second pipe, not a second feature.
//!
//! Everything in this module is an interoperability constant taken from
//! Reticulum's own client behaviour. Getting any of it wrong means Sideband
//! either never sees the board or connects and then gives up, so it is pinned
//! here and tested rather than written into the BLE stack by hand.
//!
//! What this module does *not* contain is a BLE stack. That is phase 8, after
//! the RNode protocol exists to be carried and the display exists to show a
//! pairing passkey on. See the README for the controller choice and the
//! peripheral conflicts it brings.

use crate::serial::hex_u16;

/// Nordic UART Service.
///
/// Held as a `u128` in canonical (big-endian, as-written) order. BLE puts
/// 128-bit UUIDs on the wire least-significant byte first, which is what
/// [`uuid_le_bytes`] produces.
pub const NUS_SERVICE: u128 = 0x6e40_0001_b5a3_f393_e0a9_e50e_24dc_ca9e;

/// NUS RX: the host writes to this characteristic, so from our side it is the
/// **receive** path. Named from the client's point of view, which is how every
/// NUS implementation names it, confusingly.
pub const NUS_RX_CHARACTERISTIC: u128 = 0x6e40_0002_b5a3_f393_e0a9_e50e_24dc_ca9e;

/// NUS TX: we notify on this characteristic, so it is our **transmit** path.
pub const NUS_TX_CHARACTERISTIC: u128 = 0x6e40_0003_b5a3_f393_e0a9_e50e_24dc_ca9e;

/// A 128-bit UUID in BLE wire order.
pub const fn uuid_le_bytes(uuid: u128) -> [u8; 16] {
    uuid.to_le_bytes()
}

/// Prefix Reticulum scans for when no specific device was configured.
///
/// `RNodeInterface` falls back to matching any peripheral whose name
/// `startswith("RNode ")`. A device advertising anything else is simply never
/// offered to the user, however correct the rest of its GATT server is.
pub const NAME_PREFIX: &str = "RNode ";

/// Length of the name produced by [`advertised_name`]: `"RNode "` (6) plus 4 hex digits.
pub const NAME_LEN: usize = NAME_PREFIX.len() + 4;

/// A legacy advertising packet holds 31 bytes: 3 for the mandatory flags
/// structure and 2 for the name's own header. A name longer than what is left
/// gets truncated, and a truncated name fails the host's prefix match, so this
/// is checked at build time rather than left to a test.
const _: () = assert!(NAME_LEN <= 31 - 3 - 2);

/// Largest attribute Reticulum will read or write, and the BLE ceiling anyway.
pub const MAX_GATT_ATTR_LEN: usize = 512;

/// ATT MTU before negotiation. Every BLE connection starts here.
pub const DEFAULT_ATT_MTU: u16 = 23;

/// ATT MTU Reticulum asks for.
pub const PREFERRED_ATT_MTU: u16 = 512;

/// Build the advertised device name for a board, e.g. `RNode 4F2A`.
///
/// The stock firmware derives its suffix from a hash of the Bluetooth MAC. We
/// use the chip's factory device ID instead, which we already have to hand for
/// the USB serial number and which needs no hash. Nothing on the host side
/// depends on how the suffix is produced -- only that the name starts with
/// [`NAME_PREFIX`] and stays the same across reboots, so that a paired device
/// keeps working.
pub fn advertised_name(device_id: u64) -> [u8; NAME_LEN] {
    // Fold all 64 bits into the 16 that fit, so two boards are unlikely to
    // collide just because their IDs share a low word.
    let tag = ((device_id >> 48) ^ (device_id >> 32) ^ (device_id >> 16) ^ device_id) as u16;

    let mut out = [0u8; NAME_LEN];
    out[..NAME_PREFIX.len()].copy_from_slice(NAME_PREFIX.as_bytes());
    out[NAME_PREFIX.len()..].copy_from_slice(&hex_u16(tag));
    out
}

/// How many bytes of KISS stream fit in one notification at a given ATT MTU.
///
/// A notification spends 3 bytes on the ATT opcode and handle. Below the
/// default MTU there is nothing useful left, and above 512 the attribute length
/// ceiling binds instead.
pub const fn notification_payload_len(att_mtu: u16) -> usize {
    let usable = (att_mtu as usize).saturating_sub(3);
    if usable > MAX_GATT_ATTR_LEN {
        MAX_GATT_ATTR_LEN
    } else {
        usable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nordic_uart_service_uuids() {
        // Pinned against Reticulum's RNodeInterface. These are the whole
        // contract: a typo here is a device Sideband can see but not talk to.
        assert_eq!(
            format_uuid(NUS_SERVICE),
            "6e400001-b5a3-f393-e0a9-e50e24dcca9e"
        );
        assert_eq!(
            format_uuid(NUS_RX_CHARACTERISTIC),
            "6e400002-b5a3-f393-e0a9-e50e24dcca9e"
        );
        assert_eq!(
            format_uuid(NUS_TX_CHARACTERISTIC),
            "6e400003-b5a3-f393-e0a9-e50e24dcca9e"
        );
    }

    #[test]
    fn characteristics_differ_only_in_the_service_field() {
        // They are the same base UUID; mixing up RX and TX gives a peripheral
        // that advertises correctly and then silently drops everything.
        assert_eq!(NUS_RX_CHARACTERISTIC - NUS_SERVICE, 1u128 << 96);
        assert_eq!(NUS_TX_CHARACTERISTIC - NUS_RX_CHARACTERISTIC, 1u128 << 96);
    }

    #[test]
    fn uuids_go_on_the_wire_least_significant_byte_first() {
        let bytes = uuid_le_bytes(NUS_SERVICE);
        assert_eq!(bytes[0], 0x9e);
        assert_eq!(bytes[15], 0x6e);
        // Reversing gets the canonical order back.
        let mut canonical = bytes;
        canonical.reverse();
        assert_eq!(canonical[0], 0x6e);
        assert_eq!(canonical[3], 0x01);
    }

    fn format_uuid(uuid: u128) -> String {
        let b = uuid.to_be_bytes();
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }

    fn name(id: u64) -> String {
        String::from_utf8(advertised_name(id).to_vec()).unwrap()
    }

    #[test]
    fn advertised_name_matches_reticulums_discovery_filter() {
        // `RNodeInterface` scans for names starting with "RNode ". Anything
        // else never reaches the user's device list.
        for id in [0u64, 1, 0xDEAD_BEEF_CAFE_F00D, u64::MAX] {
            let n = name(id);
            assert!(n.starts_with(NAME_PREFIX), "{n:?} would not be discovered");
            assert_eq!(n.len(), NAME_LEN);
            assert!(n.is_ascii());
        }
    }

    #[test]
    fn advertised_name_shape() {
        assert_eq!(name(0), "RNode 0000");
        assert_eq!(name(0x0000_0000_0000_1A2B), "RNode 1A2B");
        // Every 16-bit lane is folded in, so a board is not identified by its
        // low word alone.
        assert_eq!(name(0x1A2B_0000_0000_0000), "RNode 1A2B");
        assert_eq!(name(0x0000_00F0_0000_0000), "RNode 00F0");
        // XOR folding cancels: two lanes carrying the same bits annihilate.
        // Harmless here -- factory device IDs are not built from repeated
        // words -- but it is the reason this is a name and not an identity.
        assert_eq!(name(0x0001_0000_0000_0001), "RNode 0000");
    }

    #[test]
    fn advertised_name_is_stable() {
        // Bonding survives reboots only if the name does.
        assert_eq!(name(0x1234_5678_9ABC_DEF0), name(0x1234_5678_9ABC_DEF0));
    }

    #[test]
    fn notification_payload_at_the_default_mtu() {
        // The 20 bytes every BLE connection starts with, before negotiation.
        assert_eq!(notification_payload_len(DEFAULT_ATT_MTU), 20);
    }

    #[test]
    fn notification_payload_is_capped_at_the_attribute_ceiling() {
        assert_eq!(notification_payload_len(PREFERRED_ATT_MTU), 509);
        assert_eq!(notification_payload_len(1024), MAX_GATT_ATTR_LEN);
    }

    #[test]
    fn notification_payload_never_underflows() {
        // A misbehaving peer proposing a nonsense MTU must not wrap round to a
        // huge length and index off the end of a buffer.
        for mtu in [0, 1, 2, 3, 4] {
            assert_eq!(
                notification_payload_len(mtu),
                mtu.saturating_sub(3) as usize
            );
        }
    }
}
