//! RSSI calibration: the gain-tune table each front end needs before the
//! number it reports means anything.
//!
//! The chip estimates received power from which gain step its AGC settled on
//! plus a correction per step, and the corrections depend on what sits
//! between the die and the antenna — the matching network, a switch, a
//! balun. `SetRssiCalibration` (`0x0229`) loads seventeen half-dB tunes and a
//! global offset. The LR1121 user manual (UM.LR1121.W.APP rev 1.2, §7.2.15)
//! says the chip boots calibrated *for the 868–915 MHz band on Semtech's
//! evaluation kit*, and that an uncalibrated table is not only a wrong number:
//! it is a wrong gain selection in LoRa, and so lost packets and less
//! resistance to interference.
//!
//! The 2.4 GHz front end is different silicon behind a different pin, and the
//! manual gives it a different table (Table 7-21, "above 2 GHz"). Semtech's
//! own reference code (`SWSD003`, `smtc_shield_lr11xx_common.c`) carries the
//! same three tables and picks one by frequency on every radio
//! initialisation. oxinode does the same on every configuration, so a modem
//! that moves from one band to the other is not left with the other band's
//! table.
//!
//! What the tables are *not* is a calibration of this module. The manual's
//! values are for Semtech's evaluation board; the procedure for a board of
//! one's own (§7.2.15, a generator into the connector, one tone per gain
//! step) needs a signal generator at the band's frequency. Until that runs,
//! these are the best available tables rather than measured ones, and the
//! radio page records what applying them did to the numbers.
//!
//! # Packed here, not in `lr11xx`
//!
//! `lr11xx` 0.1 has a `RssiCalibration` bitfield for this command and its
//! layout does not match the manual: it places `g8` at bits 68..=71 where
//! the manual's byte 4 puts `g9`, gives `g12` two bits instead of four, and
//! transposes `g9`/`g10`/`g11`/`g13` among themselves. The manual's layout
//! (Table 7-19) and Semtech's driver (`lr11xx_radio_set_rssi_calibration` in
//! `SWDR001`) agree with each other and are what [`RssiTable::to_bytes`]
//! produces; the word is handed to the crate raw, the same way the PA and
//! RF-switch words already are.

use super::pa::Band;

/// One `SetRssiCalibration` argument set.
///
/// Seventeen gain tunes and an offset, in the manual's units: a tune is a
/// 4-bit signed value in half-dB steps (Table 7-20 — `0..=7` is `0..=+3.5 dB`
/// and `8..=15` is `−4..=−0.5 dB`), and the offset is a 12-bit field the
/// manual calls signed, also in half-dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RssiTable {
    /// The global offset added to every tune. Twelve bits; the upper four of
    /// the field on the wire are unused.
    pub gain_offset: u16,
    /// The gain tunes, in the manual's order: `g4`..`g13` at indices `0..=9`,
    /// then the boosted-LNA steps `g13hp1`..`g13hp7` at `10..=16`. Each is a
    /// nibble; the upper four bits of each entry are ignored when packed.
    pub tunes: [u8; 17],
}

/// How many bytes `SetRssiCalibration` takes after the opcode.
pub const ARGUMENT_LEN: usize = 11;

impl RssiTable {
    /// The command's eleven argument bytes, in wire order (Table 7-19).
    ///
    /// Two tunes per byte, the odd-numbered gain in the high nibble:
    /// `G5:G4`, `G7:G6`, `G9:G8`, `G11:G10`, `G13:G12`, `hp2:hp1`,
    /// `hp4:hp3`, `hp6:hp5`, then `hp7` alone in a low nibble, then the offset
    /// big-endian.
    pub const fn to_bytes(&self) -> [u8; ARGUMENT_LEN] {
        let t = &self.tunes;
        [
            pair(t[1], t[0]),
            pair(t[3], t[2]),
            pair(t[5], t[4]),
            pair(t[7], t[6]),
            pair(t[9], t[8]),
            pair(t[11], t[10]),
            pair(t[13], t[12]),
            pair(t[15], t[14]),
            t[16] & 0x0F,
            ((self.gain_offset >> 8) & 0x0F) as u8,
            (self.gain_offset & 0xFF) as u8,
        ]
    }

    /// The same bytes as one big-endian integer, for a driver that takes the
    /// argument as an 88-bit word.
    pub const fn to_raw(&self) -> u128 {
        let bytes = self.to_bytes();
        let mut raw: u128 = 0;
        let mut i = 0;
        while i < ARGUMENT_LEN {
            raw = (raw << 8) | bytes[i] as u128;
            i += 1;
        }
        raw
    }
}

/// Two nibbles into one byte, high first.
const fn pair(high: u8, low: u8) -> u8 {
    ((high & 0x0F) << 4) | (low & 0x0F)
}

/// What a tune nibble means, in half-decibels (Table 7-20).
///
/// `0..=7` is `0..=+7` half-dB; `8..=15` is `−8..=−1`. Nothing in the
/// firmware needs this to send the table — the nibbles go as they are — but a
/// log line or a test that says what a table *does* needs the sign.
pub const fn tune_half_db(nibble: u8) -> i8 {
    let n = (nibble & 0x0F) as i8;
    if n < 8 {
        n
    } else {
        n - 16
    }
}

/// The manual's recommended table below 600 MHz. Not a band this board has;
/// kept so the set is the manual's and a reader can check it against
/// Table 7-21 in one place.
pub const BELOW_600_MHZ: RssiTable = RssiTable {
    gain_offset: 0,
    tunes: [12, 12, 14, 0, 1, 3, 4, 4, 3, 6, 6, 6, 6, 6, 6, 6, 6],
};

/// The manual's recommended table from 600 MHz to 2 GHz: the sub-GHz path.
///
/// This is also, by the manual's own description, the band the chip boots
/// calibrated for. It is sent anyway, because a modem that has been on
/// 2.4 GHz has the other table loaded and nothing else puts this one back.
pub const FROM_600_MHZ_TO_2_GHZ: RssiTable = RssiTable {
    gain_offset: 0,
    tunes: [2, 2, 2, 3, 3, 4, 5, 4, 4, 6, 5, 5, 6, 6, 6, 7, 6],
};

/// The manual's recommended table above 2 GHz: the 2.4 GHz path.
///
/// The offset is the manual's figure verbatim. Read as the 12-bit signed
/// half-dB value the manual describes it as, 2030 is over a thousand dB,
/// which it plainly is not; read modulo 256 it is −18 half-dB, −9 dB. Which
/// arithmetic the chip does is not documented, so the effect on a reported
/// RSSI is measured rather than derived — see the radio page.
pub const ABOVE_2_GHZ: RssiTable = RssiTable {
    gain_offset: 2030,
    tunes: [6, 7, 6, 4, 3, 4, 14, 12, 14, 12, 12, 12, 12, 8, 8, 9, 9],
};

/// The table for a band.
///
/// Semtech's reference code chooses by frequency with edges at 600 MHz and
/// 2 GHz; both of this board's bands fall on one side of each edge, so the
/// band is enough.
pub const fn table_for(band: Band) -> &'static RssiTable {
    match band {
        Band::SubGhz => &FROM_600_MHZ_TO_2_GHZ,
        Band::HighFrequency => &ABOVE_2_GHZ,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 7-19, byte by byte, with every nibble distinct so a transposed
    /// pair cannot pass.
    #[test]
    fn byte_layout_follows_table_7_19() {
        let table = RssiTable {
            gain_offset: 0x0ABC,
            tunes: [
                0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA, // g4..g13
                0xB, 0xC, 0xD, 0xE, 0xF, 0x0, 0x1, // g13hp1..g13hp7
            ],
        };
        assert_eq!(
            table.to_bytes(),
            [
                0x21, // G5:G4
                0x43, // G7:G6
                0x65, // G9:G8
                0x87, // G11:G10
                0xA9, // G13:G12
                0xCB, // hp2:hp1
                0xED, // hp4:hp3
                0x0F, // hp6:hp5
                0x01, // hp7
                0x0A, 0xBC, // offset, big-endian
            ]
        );
    }

    /// The bytes Semtech's driver would send for the manual's 2.4 GHz row,
    /// worked by hand from Table 7-21.
    #[test]
    fn above_2_ghz_packs_as_semtech_sends_it() {
        assert_eq!(
            ABOVE_2_GHZ.to_bytes(),
            [0x76, 0x46, 0x43, 0xCE, 0xCE, 0xCC, 0x8C, 0x98, 0x09, 0x07, 0xEE]
        );
        assert_eq!(ABOVE_2_GHZ.gain_offset, 2030);
    }

    #[test]
    fn sub_ghz_table_is_the_manuals_600_mhz_to_2_ghz_row() {
        assert_eq!(
            FROM_600_MHZ_TO_2_GHZ.to_bytes(),
            [0x22, 0x32, 0x43, 0x45, 0x64, 0x55, 0x66, 0x76, 0x06, 0x00, 0x00]
        );
    }

    #[test]
    fn below_600_mhz_row_is_the_manuals() {
        assert_eq!(
            BELOW_600_MHZ.to_bytes(),
            [0xCC, 0x0E, 0x31, 0x44, 0x63, 0x66, 0x66, 0x66, 0x06, 0x00, 0x00]
        );
    }

    #[test]
    fn raw_word_is_the_bytes_big_endian() {
        let raw = ABOVE_2_GHZ.to_raw();
        let bytes = ABOVE_2_GHZ.to_bytes();
        for (i, b) in bytes.iter().enumerate() {
            let shift = 8 * (ARGUMENT_LEN - 1 - i);
            assert_eq!(((raw >> shift) & 0xFF) as u8, *b, "byte {i}");
        }
        // Eleven bytes: nothing above bit 87.
        assert_eq!(raw >> 88, 0);
    }

    #[test]
    fn stray_high_bits_do_not_leak_between_nibbles() {
        let table = RssiTable {
            gain_offset: 0xFFFF,
            tunes: [0xFF; 17],
        };
        let bytes = table.to_bytes();
        assert_eq!(&bytes[..8], &[0xFF; 8]);
        assert_eq!(bytes[8], 0x0F);
        assert_eq!(&bytes[9..], &[0x0F, 0xFF]);
    }

    /// Table 7-20.
    #[test]
    fn tune_sign_follows_table_7_20() {
        assert_eq!(tune_half_db(0), 0);
        assert_eq!(tune_half_db(1), 1); // +0.5 dB
        assert_eq!(tune_half_db(7), 7); // +3.5 dB
        assert_eq!(tune_half_db(8), -8); // −4 dB
        assert_eq!(tune_half_db(14), -2); // −1 dB
        assert_eq!(tune_half_db(15), -1); // −0.5 dB
    }

    #[test]
    fn each_band_gets_its_side_of_the_2_ghz_edge() {
        assert_eq!(*table_for(Band::SubGhz), FROM_600_MHZ_TO_2_GHZ);
        assert_eq!(*table_for(Band::HighFrequency), ABOVE_2_GHZ);
    }
}
