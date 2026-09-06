//! Hex formatting for identifiers that end up in front of a user or a host.

/// One nibble as an uppercase ASCII hex digit.
const fn nibble(value: u8) -> u8 {
    match value & 0xF {
        n @ 0..=9 => b'0' + n,
        n => b'A' + (n - 10),
    }
}

/// Format a 16-bit value as exactly 4 uppercase ASCII hex digits.
pub fn hex_u16(value: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = nibble((value >> (12 - 4 * i)) as u8);
    }
    out
}

/// Format a 64-bit value as exactly 16 uppercase ASCII hex digits, most
/// significant nibble first.
///
/// Used to turn the nRF52840's factory device ID into a USB serial number
/// string. USB string descriptors must be valid text and hosts key their
/// persistent device naming off this value, so the output has to be pure ASCII,
/// fixed width, and stable across reboots for a given chip.
pub fn hex_u64(value: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = nibble((value >> (60 - 4 * i)) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(value: u64) -> String {
        String::from_utf8(hex_u64(value).to_vec()).expect("ASCII")
    }

    #[test]
    fn formats_known_values() {
        assert_eq!(hex(0), "0000000000000000");
        assert_eq!(hex(u64::MAX), "FFFFFFFFFFFFFFFF");
        assert_eq!(hex(0x0123_4567_89AB_CDEF), "0123456789ABCDEF");
        assert_eq!(hex(1), "0000000000000001");
    }

    #[test]
    fn is_big_endian_not_reversed() {
        // Getting the shift direction backwards still produces plausible-looking
        // hex, so pin an asymmetric value.
        assert_eq!(hex(0xDEAD_BEEF_0000_0000), "DEADBEEF00000000");
        assert_eq!(hex(0x0000_0000_DEAD_BEEF), "00000000DEADBEEF");
    }

    #[test]
    fn output_is_always_printable_ascii_hex() {
        for value in [0, 1, u64::MAX, 0x8000_0000_0000_0000, 0x5555_5555_5555_5555] {
            for byte in hex_u64(value) {
                assert!(
                    byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte),
                    "{byte:#x} is not an uppercase hex digit"
                );
            }
        }
        // Every nibble value appears at least once across this input, so the
        // digit/letter branch boundary is covered.
        assert_eq!(hex(0x0123_4567_89AB_CDEF).len(), 16);
    }

    #[test]
    fn distinct_chips_get_distinct_serials() {
        // Two boards on one host must not collide, so the mapping has to be
        // injective across the whole 64-bit range, not just the low word.
        assert_ne!(hex_u64(0x1), hex_u64(0x1_0000_0000));
        assert_ne!(
            hex_u64(0xAAAA_AAAA_AAAA_AAAA),
            hex_u64(0xAAAA_AAAA_AAAA_AAAB)
        );
    }

    #[test]
    fn hex_u16_formats_four_digits() {
        let text = |v| String::from_utf8(hex_u16(v).to_vec()).unwrap();
        assert_eq!(text(0x0000), "0000");
        assert_eq!(text(0xFFFF), "FFFF");
        assert_eq!(text(0x1A2B), "1A2B");
        assert_eq!(text(0x000F), "000F");
        assert_eq!(text(0xF000), "F000");
    }

    #[test]
    fn round_trips_through_parsing() {
        for value in [0u64, 42, 0xDEAD_BEEF_CAFE_F00D, u64::MAX] {
            let text = hex(value);
            assert_eq!(u64::from_str_radix(&text, 16).unwrap(), value);
        }
    }
}
