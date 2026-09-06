//! nRF52 GPIO pin-selection (`PSEL`) encoding.
//!
//! Every nRF52 peripheral that owns pins does so through a `PSEL.*` register
//! rather than a pin-mux: the peripheral is told which GPIO to drive, and the
//! GPIO itself has no idea. That makes `PSEL` the one place where "did the
//! driver actually claim the pin I meant?" can be answered from software, which
//! matters a great deal on a board whose SPI never reaches copper and cannot be
//! probed.
//!
//! The layout is the same in every peripheral (nRF52840 product specification,
//! `PSEL` register description):
//!
//! | bits | meaning |
//! |---|---|
//! | 4:0 | pin number within the port |
//! | 5 | port number |
//! | 30:6 | reserved |
//! | 31 | 1 = disconnected |
//!
//! So `P1.13` is `(1 << 5) | 13` = 45. The firmware reads these registers back
//! after configuring a peripheral and compares against [`psel`]; the encoding
//! lives here so that comparison is tested on the host rather than trusted.

/// The value a `PSEL` register holds when no pin is assigned.
pub const DISCONNECTED: u32 = 1 << 31;

/// Encode `Pport.pin` as a `PSEL` register value.
///
/// Panics on a pin or port the nRF52840 does not have, which in a `const`
/// context is a compile error.
pub const fn psel(port: u8, pin: u8) -> u32 {
    assert!(port <= 1, "nRF52840 has ports P0 and P1 only");
    assert!(pin <= 31, "a port has 32 pins");
    ((port as u32) << 5) | pin as u32
}

/// Decode a `PSEL` register value back to `(port, pin)`, or `None` if the
/// register says no pin is connected.
///
/// Only the bits the hardware defines are examined. A register that reads back
/// as all-ones — the usual signature of a peripheral that is powered down or an
/// address that decoded to nothing — has bit 31 set and so reports `None`
/// rather than pretending to be `P1.31`.
pub const fn psel_decode(value: u32) -> Option<(u8, u8)> {
    if value & DISCONNECTED != 0 {
        None
    } else {
        Some((((value >> 5) & 1) as u8, (value & 0x1f) as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_zero_is_just_the_pin_number() {
        assert_eq!(psel(0, 0), 0);
        assert_eq!(psel(0, 17), 17);
        assert_eq!(psel(0, 31), 31);
    }

    #[test]
    fn port_one_sets_bit_five() {
        assert_eq!(psel(1, 0), 32);
        assert_eq!(psel(1, 31), 63);
    }

    /// The four pins this board's radio SPI actually uses, spelled out so a
    /// transposed digit in the firmware shows up as a failing test and not as a
    /// radio that never answers.
    #[test]
    fn the_radio_spi_pins_encode_as_expected() {
        assert_eq!(psel(1, 12), 44); // NSS
        assert_eq!(psel(1, 13), 45); // SCK
        assert_eq!(psel(1, 14), 46); // MOSI
        assert_eq!(psel(1, 15), 47); // MISO
    }

    #[test]
    fn decoding_inverts_encoding() {
        for port in 0..=1u8 {
            for pin in 0..=31u8 {
                assert_eq!(psel_decode(psel(port, pin)), Some((port, pin)));
            }
        }
    }

    #[test]
    fn the_disconnected_bit_wins() {
        assert_eq!(psel_decode(DISCONNECTED), None);
        // A peripheral that reads back as all-ones is not P1.31.
        assert_eq!(psel_decode(u32::MAX), None);
        assert_eq!(psel_decode(DISCONNECTED | psel(1, 13)), None);
    }

    #[test]
    fn reserved_bits_are_ignored_rather_than_folded_in() {
        // Bits 30:6 are reserved; whatever they read as must not shift the
        // answer, or a future silicon revision turns into a phantom pin.
        assert_eq!(psel_decode(0x7fff_ffc0 | psel(1, 13)), Some((1, 13)));
    }
}
