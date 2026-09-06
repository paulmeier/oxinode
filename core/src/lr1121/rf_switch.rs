//! Which of the LR1121's own DIOs drive the antenna switch, and what each one
//! does in each mode.
//!
//! `SetDioAsRfSwitch` is the equivalent of RadioLib's `LR11X0_DIO_AS_RF_SWITCH`.
//! It tells the chip which of *its* pins (DIO5, DIO6, DIO7, DIO8, DIO10) are
//! wired to an external switch, and what level each should take in standby, RX,
//! TX, high-power TX, high-frequency TX, GNSS and WiFi.
//!
//! The values are board-specific and there is no way to discover them: they are
//! a property of the copper, not of the chip. Getting them wrong does not
//! produce an error — the chip reports a clean `TxDone` while nothing reaches
//! the antenna. That is why this is built here, from a written-down table, with
//! tests, rather than assembled inline at the call site.
//!
//! It is also why step 5 cannot validate itself. Only an SDR can say whether
//! anything left the connector.

/// Bit for each switchable DIO, as `SetDioAsRfSwitch` numbers them.
pub mod dio {
    /// DIO5.
    pub const DIO5: u8 = 1 << 0;
    /// DIO6.
    pub const DIO6: u8 = 1 << 1;
    /// DIO7.
    pub const DIO7: u8 = 1 << 2;
    /// DIO8.
    pub const DIO8: u8 = 1 << 3;
    /// DIO10.
    pub const DIO10: u8 = 1 << 4;
}

/// One `SetDioAsRfSwitch` configuration: which DIOs are switches, and the state
/// each takes per mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfSwitchMasks {
    /// Which DIOs are wired to the switch at all. A DIO not set here is left
    /// alone in every mode.
    pub enable: u8,
    /// Levels in standby.
    pub standby: u8,
    /// Levels in RX.
    pub rx: u8,
    /// Levels in low-power TX.
    pub tx: u8,
    /// Levels in high-power TX.
    pub tx_hp: u8,
    /// Levels in high-frequency TX — the 2.4 GHz path.
    pub tx_hf: u8,
    /// Levels while scanning GNSS.
    pub gnss: u8,
    /// Levels while scanning WiFi, and in high-frequency RX.
    pub wifi: u8,
}

impl RfSwitchMasks {
    /// Pack into the 64-bit argument `SetDioAsRfSwitch` takes.
    ///
    /// Written out here rather than delegated to `lr11xx`'s `RfSwitchConfig`
    /// because that type **does not map bits 16..=23** — the high-frequency TX
    /// state. Its builder has no field for it, so a config assembled through
    /// the crate leaves that byte at zero with no way to say otherwise.
    ///
    /// On this board that is harmless, and harmless by coincidence rather than
    /// by design: the 2.4 GHz output has its own u.FL connector and does not go
    /// through the switch, so its correct state *is* all-low. A board that
    /// routed 2.4 GHz through the same switch could not be configured with that
    /// crate at all. Building the word here keeps the coincidence visible
    /// instead of load-bearing.
    pub const fn to_raw(&self) -> u64 {
        ((self.enable as u64) << 56)
            | ((self.standby as u64) << 48)
            | ((self.rx as u64) << 40)
            | ((self.tx as u64) << 32)
            | ((self.tx_hp as u64) << 24)
            | ((self.tx_hf as u64) << 16)
            | ((self.gnss as u64) << 8)
            | (self.wifi as u64)
    }

    /// Whether every per-mode state only names DIOs that are enabled.
    ///
    /// A state bit for a DIO that is not a switch is meaningless, and much more
    /// likely to be a typo than an intention.
    pub const fn states_are_within_enable(&self) -> bool {
        let outside =
            (self.standby | self.rx | self.tx | self.tx_hp | self.tx_hf | self.gnss | self.wifi)
                & !self.enable;
        outside == 0
    }
}

/// The muzi.works Base Duo, from the Rev 01 schematic.
///
/// Only DIO5 and DIO6 are used:
///
/// | mode | DIO5 | DIO6 |
/// |---|---|---|
/// | standby | low | low |
/// | RX | **high** | low |
/// | TX | low | **high** |
/// | TX high power | low | **high** |
/// | TX high frequency (2.4 GHz) | low | low |
/// | GNSS | low | low |
/// | WiFi | low | low |
///
/// The 2.4 GHz row is not an oversight. TX-HF is the same low/low as standby
/// because the 2.4 GHz path does not go through this switch: the schematic
/// gives the sub-GHz output its own SMA connector and the 2.4 GHz output a
/// separate u.FL, so the switch only ever arbitrates the sub-GHz RX/TX pair.
pub const BASE_DUO: RfSwitchMasks = RfSwitchMasks {
    enable: dio::DIO5 | dio::DIO6,
    standby: 0,
    rx: dio::DIO5,
    tx: dio::DIO6,
    tx_hp: dio::DIO6,
    tx_hf: 0,
    gnss: 0,
    wifi: 0,
};

const _: () = assert!(
    BASE_DUO.states_are_within_enable(),
    "a mode drives a DIO that is not enabled as a switch"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole configuration as one number, computed by hand from the table
    /// in [`BASE_DUO`]'s documentation. If the packing changes, or a mode's
    /// mask is edited, this is what notices.
    #[test]
    fn the_base_duo_config_packs_to_a_known_word() {
        assert_eq!(BASE_DUO.to_raw(), 0x0300_0102_0200_0000);
    }

    /// Each byte in its own place, so a shifted field is a specific failure
    /// rather than a mysterious one.
    #[test]
    fn each_mode_occupies_its_own_byte() {
        let raw = BASE_DUO.to_raw();
        let byte = |shift: u32| ((raw >> shift) & 0xff) as u8;
        assert_eq!(byte(56), dio::DIO5 | dio::DIO6, "enable");
        assert_eq!(byte(48), 0, "standby");
        assert_eq!(byte(40), dio::DIO5, "rx");
        assert_eq!(byte(32), dio::DIO6, "tx");
        assert_eq!(byte(24), dio::DIO6, "tx_hp");
        assert_eq!(byte(16), 0, "tx_hf");
        assert_eq!(byte(8), 0, "gnss");
        assert_eq!(byte(0), 0, "wifi");
    }

    /// RX and TX must not drive the same pin high, or the switch is being asked
    /// to point both ways at once.
    #[test]
    fn receive_and_transmit_are_opposites() {
        assert_ne!(BASE_DUO.rx, BASE_DUO.tx);
        assert_eq!(BASE_DUO.rx & BASE_DUO.tx, 0);
    }

    /// High-power TX takes the same path as ordinary TX on this board. If that
    /// ever stops being true it is a hardware change, not a tuning decision.
    #[test]
    fn high_power_transmit_uses_the_same_path() {
        assert_eq!(BASE_DUO.tx_hp, BASE_DUO.tx);
    }

    /// The 2.4 GHz path bypasses the switch, so its state is the same as
    /// standby. Asserted explicitly because "same as standby" looks exactly
    /// like "nobody filled this row in".
    #[test]
    fn the_high_frequency_path_bypasses_the_switch() {
        assert_eq!(BASE_DUO.tx_hf, BASE_DUO.standby);
        assert_eq!(BASE_DUO.tx_hf, 0);
    }

    #[test]
    fn only_dio5_and_dio6_are_switches_on_this_board() {
        assert_eq!(BASE_DUO.enable, 0b0000_0011);
        assert_eq!(BASE_DUO.enable & (dio::DIO7 | dio::DIO8 | dio::DIO10), 0);
    }

    #[test]
    fn a_state_outside_enable_is_caught() {
        let bad = RfSwitchMasks {
            tx: dio::DIO7,
            ..BASE_DUO
        };
        assert!(!bad.states_are_within_enable());
        assert!(BASE_DUO.states_are_within_enable());
    }

    /// Packing must be reversible field by field for an arbitrary
    /// configuration, not just for the one this board happens to use — every
    /// mask distinct so a swapped pair of fields cannot hide.
    #[test]
    fn packing_keeps_every_field_distinct() {
        let m = RfSwitchMasks {
            enable: 0x1f,
            standby: 0x01,
            rx: 0x02,
            tx: 0x04,
            tx_hp: 0x08,
            tx_hf: 0x10,
            gnss: 0x03,
            wifi: 0x05,
        };
        assert_eq!(m.to_raw(), 0x1f01_0204_0810_0305);
        assert!(m.states_are_within_enable());
    }

    /// Nothing enabled means nothing switched, and a zero word is what an
    /// uninitialised config looks like — so it must not be mistaken for a
    /// valid one by anything downstream.
    #[test]
    fn an_empty_config_is_all_zeroes() {
        let m = RfSwitchMasks {
            enable: 0,
            standby: 0,
            rx: 0,
            tx: 0,
            tx_hp: 0,
            tx_hf: 0,
            gnss: 0,
            wifi: 0,
        };
        assert_eq!(m.to_raw(), 0);
        assert_ne!(BASE_DUO.to_raw(), 0);
    }
}
