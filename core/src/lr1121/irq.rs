//! Which LR1121 interrupts are routed to which pin, and what the bits mean.
//!
//! The LR1121 has two interrupt outputs. On the Base Duo **only one of them
//! reaches the MCU**: the module brings `LR_DIO9` out on pin 11, its datasheet
//! requires that pin be jumpered to an MCU GPIO, and the schematic does so on a
//! net named `IRQ_JUMPER` landing on nRF **P1.08**. `LR_DIO7` and `LR_DIO8` are
//! exposed and unconnected; DIO11 does not leave the module at all.
//!
//! So [`DIO11_MASK`] is empty, and that is a fact about this board rather than a
//! default nobody got round to filling in.
//!
//! DIO3 is not in this picture because it is driving the oscillator supply — the
//! reason DIO9 has to be jumpered in the first place.

/// Bit positions in the LR1121's interrupt word.
pub mod bit {
    /// Packet transmission completed.
    pub const TX_DONE: u32 = 1 << 2;
    /// Packet received.
    pub const RX_DONE: u32 = 1 << 3;
    /// Preamble detected.
    pub const PREAMBLE_DETECTED: u32 = 1 << 4;
    /// Valid sync word, or LoRa header detected.
    pub const SYNC_WORD_HEADER_VALID: u32 = 1 << 5;
    /// LoRa header CRC error.
    pub const HEADER_ERR: u32 = 1 << 6;
    /// Packet received with a bad CRC.
    pub const CRC_ERR: u32 = 1 << 7;
    /// Channel activity detection finished.
    pub const CAD_DONE: u32 = 1 << 8;
    /// Channel activity detected.
    pub const CAD_DETECTED: u32 = 1 << 9;
    /// RX or TX timeout.
    pub const TIMEOUT: u32 = 1 << 10;
    /// The host sent a command the chip would not execute.
    pub const CMD_ERROR: u32 = 1 << 22;
    /// Something else went wrong; `GetErrors` says what.
    pub const ERROR: u32 = 1 << 23;
}

/// What DIO9 — the only interrupt line that reaches the MCU — is asked to
/// signal.
///
/// Deliberately excludes `PREAMBLE_DETECTED`, `SYNC_WORD_HEADER_VALID` and
/// `CAD_DETECTED`. Those fire constantly on a busy band and would wake the MCU
/// for events it has nothing useful to do about; the ones kept are the ones
/// that end an operation or report a fault.
///
/// `CMD_ERROR` and `ERROR` are in the mask for bring-up as much as for
/// operation. They are the only interrupts that can be raised deliberately
/// without transmitting anything, which is what makes step 6 testable before
/// step 7b exists.
pub const DIO9_MASK: u32 = bit::TX_DONE
    | bit::RX_DONE
    | bit::HEADER_ERR
    | bit::CRC_ERR
    | bit::CAD_DONE
    | bit::TIMEOUT
    | bit::CMD_ERROR
    | bit::ERROR;

/// What DIO11 is asked to signal: nothing, because it does not leave the module.
pub const DIO11_MASK: u32 = 0;

/// Every interrupt named in [`bit`], for clearing.
pub const ALL_NAMED: u32 = bit::TX_DONE
    | bit::RX_DONE
    | bit::PREAMBLE_DETECTED
    | bit::SYNC_WORD_HEADER_VALID
    | bit::HEADER_ERR
    | bit::CRC_ERR
    | bit::CAD_DONE
    | bit::CAD_DETECTED
    | bit::TIMEOUT
    | bit::CMD_ERROR
    | bit::ERROR;

/// Every bit in [`ALL_NAMED`] with a name, for logging what actually fired.
pub const NAMES: [(u32, &str); 11] = [
    (bit::TX_DONE, "tx_done"),
    (bit::RX_DONE, "rx_done"),
    (bit::PREAMBLE_DETECTED, "preamble"),
    (bit::SYNC_WORD_HEADER_VALID, "sync/header"),
    (bit::HEADER_ERR, "header_err"),
    (bit::CRC_ERR, "crc_err"),
    (bit::CAD_DONE, "cad_done"),
    (bit::CAD_DETECTED, "cad_detected"),
    (bit::TIMEOUT, "timeout"),
    (bit::CMD_ERROR, "cmd_error"),
    (bit::ERROR, "error"),
];

const _: () = assert!(
    DIO11_MASK == 0,
    "DIO11 does not reach the MCU on this board; asking the chip to signal on \
     it would raise interrupts nobody can see"
);
const _: () = assert!(
    DIO9_MASK & bit::CMD_ERROR != 0,
    "cmd_error is the only interrupt that can be provoked without transmitting, \
     so removing it removes the ability to test the interrupt path at all"
);
const _: () = assert!(
    DIO9_MASK & (bit::PREAMBLE_DETECTED | bit::CAD_DETECTED) == 0,
    "these fire constantly on a busy band and wake the MCU for nothing"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The positions are the chip's, not ours. A shifted bit would route the
    /// wrong interrupt and be very hard to see afterwards.
    #[test]
    fn the_bit_positions_are_the_chips() {
        assert_eq!(bit::TX_DONE, 0x0000_0004);
        assert_eq!(bit::RX_DONE, 0x0000_0008);
        assert_eq!(bit::TIMEOUT, 0x0000_0400);
        assert_eq!(bit::CMD_ERROR, 0x0040_0000);
        assert_eq!(bit::ERROR, 0x0080_0000);
    }

    #[test]
    fn the_dio9_mask_carries_what_ends_an_operation() {
        for (b, name) in [
            (bit::TX_DONE, "tx_done"),
            (bit::RX_DONE, "rx_done"),
            (bit::TIMEOUT, "timeout"),
            (bit::CMD_ERROR, "cmd_error"),
            (bit::ERROR, "error"),
        ] {
            assert_ne!(DIO9_MASK & b, 0, "{name} missing from the DIO9 mask");
        }
    }

    /// The mask must not name a bit outside the word the chip defines, or the
    /// command is being sent arguments it has no meaning for.
    #[test]
    fn the_mask_only_names_bits_that_exist() {
        assert_eq!(DIO9_MASK & !ALL_NAMED, 0);
    }

    #[test]
    fn every_named_bit_is_distinct_and_a_single_bit() {
        let mut seen = 0u32;
        for (b, name) in NAMES {
            assert_eq!(b.count_ones(), 1, "{name} is not a single bit");
            assert_eq!(seen & b, 0, "{name} collides with an earlier bit");
            seen |= b;
        }
        assert_eq!(seen, ALL_NAMED);
    }

    #[test]
    fn names_cover_the_dio9_mask() {
        let named: u32 = NAMES.iter().map(|(b, _)| *b).fold(0, |a, b| a | b);
        assert_eq!(DIO9_MASK & !named, 0, "an interrupt could fire unnamed");
    }
}
