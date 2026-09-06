//! The `GetVersion` reply, and telling a real one from a bus that is not
//! working.
//!
//! `GetVersion` (opcode `0x0101`) is the first thing oxinode ever says to the
//! radio, and the first answer it gets back is also the first evidence that any
//! of the preceding steps did what they claimed. That makes reading it
//! carefully worth more than it looks: the reply arrives as four bytes over a
//! bus that, on this board, cannot be probed, so *the bytes are the only
//! witness*. A response of four zeros and a response from a healthy chip travel
//! by exactly the same route.
//!
//! So the decoding is separated from the judgement. [`Version`] says what the
//! bytes mean if they mean anything; [`VersionVerdict`] says whether to believe
//! them.

/// Opcode for `GetVersion`.
///
/// Sent most-significant byte first, like every LR11xx opcode.
pub const GET_VERSION: u16 = 0x0101;

/// The `use_case` byte oxinode expects from this board.
///
/// The Base Duo's Meshtastic variant defines both `USE_SX1262` and
/// `USE_LR1121`, because Elecrow ships two footprint-compatible modules and one
/// PCB accepts either. The Rev 01 schematic populates the LR1121, so this is
/// an assertion about the board in hand rather than a configuration choice.
pub const EXPECTED_USE_CASE: u8 = 0x03;

/// `use_case` values the LR11x0 family defines.
pub mod use_case {
    /// LR1110.
    pub const LR1110: u8 = 0x01;
    /// LR1120.
    pub const LR1120: u8 = 0x02;
    /// LR1121 — what this board carries.
    pub const LR1121: u8 = 0x03;
    /// The chip is running its own bootloader, not its transceiver firmware.
    pub const BOOTLOADER: u8 = 0xDF;
}

/// The transceiver-firmware version oxinode was actually brought up against,
/// as `(major, minor)`.
///
/// **Measured on the bench board: 1.1.** The LR11x0 family carries firmware in
/// its own flash and behaviour genuinely differs between images, so when the
/// radio does something the datasheet does not explain, this is the first thing
/// to look at.
///
/// This replaced a list of "known" versions carried over from the phase-3 plan
/// — `0x0307`, `0x0401`, `0x0402`, attributed to RadioLib. That list was wrong
/// for the job. It is family-wide rather than per-part, it could not be checked
/// against RadioLib from here, and the first real board to answer reported 1.1
/// and was duly announced as running firmware "no reference implementation
/// documents". A check that calls healthy hardware suspect is worse than no
/// check.
///
/// A difference from this value is not a fault. It means the board in hand is
/// not the board the behaviour in this repository was observed on, which is
/// worth one line in a log and nothing more.
pub const OBSERVED_FIRMWARE: (u8, u8) = (0x01, 0x01);

/// A decoded `GetVersion` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    /// Silicon revision.
    pub hardware: u8,
    /// Which part of the family this is; see [`use_case`].
    pub use_case: u8,
    /// Transceiver firmware, major.
    pub fw_major: u8,
    /// Transceiver firmware, minor.
    pub fw_minor: u8,
}

impl Version {
    /// Split the four reply bytes.
    ///
    /// The order is hardware, use case, firmware major, firmware minor — the
    /// same order as the big-endian `u32` the datasheet describes, so a reply
    /// read into a `[u8; 4]` needs no swapping.
    pub const fn decode(raw: [u8; 4]) -> Self {
        Self {
            hardware: raw[0],
            use_case: raw[1],
            fw_major: raw[2],
            fw_minor: raw[3],
        }
    }

    /// Whether this is the firmware version oxinode was developed against.
    ///
    /// See [`OBSERVED_FIRMWARE`]: a `false` here is a note, never a fault.
    pub const fn firmware_matches_bench(&self) -> bool {
        self.fw_major == OBSERVED_FIRMWARE.0 && self.fw_minor == OBSERVED_FIRMWARE.1
    }
}

/// Whether to believe a `GetVersion` reply, and what it means if so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionVerdict {
    /// An LR1121. What this board is supposed to have.
    Expected(Version),
    /// A real LR11x0 reply, from the wrong member of the family.
    WrongPart(Version),
    /// The chip answered, from its own bootloader rather than its transceiver
    /// firmware.
    Bootloader(Version),
    /// Every byte was zero.
    AllZeros,
    /// Every byte was `0xFF`.
    AllOnes,
    /// The chip answered something, but not a `use_case` the family defines.
    Unrecognised(Version),
}

impl VersionVerdict {
    /// Read a reply.
    ///
    /// The all-zeros and all-ones cases are checked *first* and deliberately do
    /// not reach [`Version::decode`]. Both are decodable — all zeros parses as
    /// hardware 0, use case 0, firmware 0.0 — and both are far more likely to
    /// be a bus that is not working than a chip that is. Letting them through
    /// as `Unrecognised` would be technically true and practically useless.
    pub const fn of(raw: [u8; 4]) -> Self {
        if raw[0] == 0x00 && raw[1] == 0x00 && raw[2] == 0x00 && raw[3] == 0x00 {
            return Self::AllZeros;
        }
        if raw[0] == 0xFF && raw[1] == 0xFF && raw[2] == 0xFF && raw[3] == 0xFF {
            return Self::AllOnes;
        }
        let version = Version::decode(raw);
        match version.use_case {
            use_case::LR1121 => Self::Expected(version),
            use_case::LR1110 | use_case::LR1120 => Self::WrongPart(version),
            use_case::BOOTLOADER => Self::Bootloader(version),
            _ => Self::Unrecognised(version),
        }
    }

    /// The decoded reply, where there is one worth having.
    pub const fn version(&self) -> Option<Version> {
        match self {
            Self::Expected(v)
            | Self::WrongPart(v)
            | Self::Bootloader(v)
            | Self::Unrecognised(v) => Some(*v),
            Self::AllZeros | Self::AllOnes => None,
        }
    }

    /// Whether the radio is the part this firmware is for.
    pub const fn is_expected(&self) -> bool {
        matches!(self, Self::Expected(_))
    }

    /// One line for a log.
    pub const fn summary(&self) -> &'static str {
        match self {
            Self::Expected(_) => "LR1121, as expected",
            Self::WrongPart(_) => "an LR11x0, but not the LR1121 this board should carry",
            Self::Bootloader(_) => {
                "the chip is in its own bootloader, not its transceiver firmware"
            }
            Self::AllZeros => "every byte zero: nothing is driving MISO",
            Self::AllOnes => "every byte 0xFF: MISO is idle high, nothing is answering",
            Self::Unrecognised(_) => "an answer, but not one the LR11x0 family defines",
        }
    }

    /// What to look at next.
    ///
    /// Every one of these points somewhere specific, because on this board the
    /// alternative is a person staring at a module they cannot probe.
    pub const fn next_step(&self) -> &'static str {
        match self {
            Self::Expected(_) => "nothing -- proceed to the TCXO",
            Self::WrongPart(_) => {
                "the board populated a different module. Elecrow's nRFLR1262 is \
                 footprint-compatible and puts the interrupt on P1.06 instead of \
                 P1.08, so the pin map is wrong too"
            }
            Self::Bootloader(_) => {
                "the transceiver firmware did not start. A longer wait after \
                 reset will not help -- BUSY already fell, so the chip finished \
                 booting into the wrong image"
            }
            Self::AllZeros => {
                "SPI, not the radio. MISO on the wrong pin, the peripheral not \
                 clocking, or a chip held in reset. Note that BUSY answering the \
                 reset in step 2 rules out the last of those"
            }
            Self::AllOnes => {
                "SPI, not the radio -- and specifically the opposite of \
                 all-zeros: the bus is idle rather than driven low, so the chip \
                 is not selected. Check NSS, and that the command phase and the \
                 read phase are separate NSS cycles"
            }
            Self::Unrecognised(_) => {
                "most likely a framing error rather than an exotic part: an \
                 off-by-one in the reply shifts the use case into a byte that \
                 means nothing"
            }
        }
    }
}

/// The status byte that precedes every reply.
///
/// Worth decoding even though `lr11xx` does it too: when the reply itself is
/// nonsense, this byte often says *why*, and it is available before any of the
/// crate's own error handling gets a chance to discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat1 {
    /// At least one interrupt is active.
    pub interrupt: bool,
    /// How the previous command went; see [`command_status`].
    pub command_status: u8,
}

/// `command_status` values, from bits 1..=3 of [`Stat1`].
pub mod command_status {
    /// The last command could not be executed.
    pub const FAIL: u8 = 0;
    /// The last command could not be processed — wrong opcode or arguments.
    pub const PROCESSING_ERROR: u8 = 1;
    /// The last command was processed.
    pub const OK: u8 = 2;
    /// The last command was processed and data is being returned.
    pub const DATA: u8 = 3;
}

impl Stat1 {
    /// Split the status byte.
    pub const fn decode(raw: u8) -> Self {
        Self {
            interrupt: raw & 0x01 != 0,
            command_status: (raw >> 1) & 0x07,
        }
    }

    /// Whether this status accompanies a reply that actually carries data.
    pub const fn carries_data(&self) -> bool {
        self.command_status == command_status::DATA
    }

    /// One line for a log.
    pub const fn summary(&self) -> &'static str {
        match self.command_status {
            command_status::FAIL => "previous command could not be executed",
            command_status::PROCESSING_ERROR => "previous command not processed: opcode or args",
            command_status::OK => "previous command ok, no data",
            command_status::DATA => "previous command ok, data follows",
            _ => "reserved command status",
        }
    }
}

/// The second status byte, which says what mode the chip is in.
///
/// Decoded here as well as in `lr11xx` for the same reason [`Stat1`] is: the
/// raw probe runs before the crate is trusted with anything, and `chip_mode`
/// is one of step 3's failure signatures — a chip sitting in its own bootloader
/// says so here as well as in the `use_case` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat2 {
    /// The chip is executing from flash rather than from ROM.
    pub flash: bool,
    /// Current mode; see [`chip_mode`].
    pub chip_mode: u8,
    /// What caused the last reset; see [`reset_status`].
    ///
    /// Cleared by the first `GetStatus` after a reset, and by nothing else.
    pub reset_status: u8,
}

/// `chip_mode` values, from bits 1..=3 of [`Stat2`].
pub mod chip_mode {
    /// Sleep.
    pub const SLEEP: u8 = 0;
    /// Standby on the RC oscillator — where a healthy chip lands after reset.
    pub const STANDBY_RC: u8 = 1;
    /// Standby on the external oscillator.
    pub const STANDBY_XOSC: u8 = 2;
    /// Frequency synthesis.
    pub const FS: u8 = 3;
    /// Receiving.
    pub const RX: u8 = 4;
    /// Transmitting.
    pub const TX: u8 = 5;
    /// WiFi or GNSS scanning.
    pub const WIFI_GNSS: u8 = 6;
}

/// `reset_status` values, from bits 4..=7 of [`Stat2`].
pub mod reset_status {
    /// No reset recorded — the field has been read and cleared.
    pub const CLEARED: u8 = 0;
    /// Power-on or brown-out.
    pub const ANALOG: u8 = 1;
    /// The NRESET pin.
    pub const EXTERNAL: u8 = 2;
    /// A system reset.
    pub const SYSTEM: u8 = 3;
    /// The watchdog.
    pub const WATCHDOG: u8 = 4;
    /// Woken by NSS toggling.
    pub const WAKEUP: u8 = 5;
    /// RTC restart.
    pub const RTC: u8 = 6;
}

impl Stat2 {
    /// Split the second status byte.
    pub const fn decode(raw: u8) -> Self {
        Self {
            flash: raw & 0x01 != 0,
            chip_mode: (raw >> 1) & 0x07,
            reset_status: (raw >> 4) & 0x0f,
        }
    }

    /// Whether the chip is where a healthy one sits after a reset.
    pub const fn is_standby_rc(&self) -> bool {
        self.chip_mode == chip_mode::STANDBY_RC
    }

    /// One line for a log.
    pub const fn mode_summary(&self) -> &'static str {
        match self.chip_mode {
            chip_mode::SLEEP => "sleep",
            chip_mode::STANDBY_RC => "standby (RC)",
            chip_mode::STANDBY_XOSC => "standby (XOSC)",
            chip_mode::FS => "frequency synthesis",
            chip_mode::RX => "rx",
            chip_mode::TX => "tx",
            chip_mode::WIFI_GNSS => "wifi/gnss scan",
            _ => "reserved mode",
        }
    }

    /// One line for a log.
    pub const fn reset_summary(&self) -> &'static str {
        match self.reset_status {
            reset_status::CLEARED => "cleared",
            reset_status::ANALOG => "power-on or brown-out",
            reset_status::EXTERNAL => "NRESET pin",
            reset_status::SYSTEM => "system",
            reset_status::WATCHDOG => "watchdog",
            reset_status::WAKEUP => "NSS wakeup",
            reset_status::RTC => "RTC restart",
            _ => "reserved reset cause",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reply oxinode expects to see from this board, taken apart by hand:
    /// hardware rev, then use case, then firmware major and minor.
    const LR1121_REPLY: [u8; 4] = [0x22, 0x03, 0x04, 0x01];

    #[test]
    fn decoding_keeps_the_datasheet_order() {
        let v = Version::decode(LR1121_REPLY);
        assert_eq!(v.hardware, 0x22);
        assert_eq!(v.use_case, 0x03);
        assert_eq!(v.fw_major, 0x04);
        assert_eq!(v.fw_minor, 0x01);
    }

    /// A transposed use case and firmware major would still decode, still look
    /// plausible, and be wrong. Pin the positions.
    #[test]
    fn the_use_case_is_the_second_byte_not_the_third() {
        let v = Version::decode([0x00, 0xAA, 0xBB, 0x00]);
        assert_eq!(v.use_case, 0xAA);
        assert_eq!(v.fw_major, 0xBB);
    }

    #[test]
    fn this_boards_part_is_the_expected_one() {
        assert_eq!(EXPECTED_USE_CASE, use_case::LR1121);
        assert!(VersionVerdict::of(LR1121_REPLY).is_expected());
        assert_eq!(
            VersionVerdict::of(LR1121_REPLY),
            VersionVerdict::Expected(Version::decode(LR1121_REPLY))
        );
    }

    #[test]
    fn the_other_family_members_are_the_wrong_part() {
        for uc in [use_case::LR1110, use_case::LR1120] {
            let verdict = VersionVerdict::of([0x22, uc, 0x04, 0x01]);
            assert!(matches!(verdict, VersionVerdict::WrongPart(_)), "{uc:#04x}");
            assert!(!verdict.is_expected());
        }
    }

    #[test]
    fn the_bootloader_use_case_is_called_out_separately() {
        let verdict = VersionVerdict::of([0x22, use_case::BOOTLOADER, 0x00, 0x00]);
        assert!(matches!(verdict, VersionVerdict::Bootloader(_)));
    }

    /// The whole point of the type. All zeros decodes perfectly well into a
    /// `Version` -- hardware 0, use case 0, firmware 0.0 -- and reporting that
    /// as "an unrecognised part" would send someone looking at the radio when
    /// the bus is what is broken.
    #[test]
    fn a_silent_bus_is_not_an_exotic_chip() {
        assert_eq!(VersionVerdict::of([0x00; 4]), VersionVerdict::AllZeros);
        assert_eq!(VersionVerdict::of([0xFF; 4]), VersionVerdict::AllOnes);
        assert_eq!(VersionVerdict::of([0x00; 4]).version(), None);
        assert_eq!(VersionVerdict::of([0xFF; 4]).version(), None);
    }

    /// ...but only when *every* byte matches. A reply that happens to start
    /// with a zero byte is still a reply.
    #[test]
    fn a_partly_zero_reply_is_still_decoded() {
        let verdict = VersionVerdict::of([0x00, use_case::LR1121, 0x04, 0x01]);
        assert!(verdict.is_expected());
        assert_eq!(verdict.version().unwrap().hardware, 0x00);

        let verdict = VersionVerdict::of([0xFF, use_case::LR1121, 0xFF, 0xFF]);
        assert!(verdict.is_expected());
    }

    #[test]
    fn an_undefined_use_case_is_unrecognised() {
        let verdict = VersionVerdict::of([0x22, 0x7E, 0x04, 0x01]);
        assert!(matches!(verdict, VersionVerdict::Unrecognised(_)));
        assert_eq!(verdict.version().unwrap().use_case, 0x7E);
    }

    #[test]
    fn every_verdict_points_somewhere_different() {
        let all = [
            VersionVerdict::of(LR1121_REPLY),
            VersionVerdict::of([0x22, use_case::LR1120, 0x04, 0x01]),
            VersionVerdict::of([0x22, use_case::BOOTLOADER, 0x00, 0x00]),
            VersionVerdict::of([0x00; 4]),
            VersionVerdict::of([0xFF; 4]),
            VersionVerdict::of([0x22, 0x7E, 0x04, 0x01]),
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(!a.summary().is_empty());
            assert!(!a.next_step().is_empty());
            for b in &all[i + 1..] {
                assert_ne!(a.summary(), b.summary());
                assert_ne!(a.next_step(), b.next_step());
            }
        }
    }

    /// The bench board really did answer this. If the constant drifts, the log
    /// line it drives starts crying wolf on the only hardware anyone has.
    #[test]
    fn the_bench_board_matches_its_own_recorded_firmware() {
        let v = Version::decode([0x22, use_case::LR1121, 0x01, 0x01]);
        assert_eq!(OBSERVED_FIRMWARE, (0x01, 0x01));
        assert!(v.firmware_matches_bench());
    }

    #[test]
    fn a_different_firmware_version_is_noticed() {
        let v = Version::decode([0x22, use_case::LR1121, 0x04, 0x02]);
        assert!(!v.firmware_matches_bench());
    }

    /// A different firmware version is a note, not a fault. If this ever gates
    /// anything, a board shipped with a newer image refuses to come up -- which
    /// is exactly what the list this replaced would have done.
    #[test]
    fn a_different_firmware_version_does_not_change_the_verdict() {
        let verdict = VersionVerdict::of([0x22, use_case::LR1121, 0x09, 0x09]);
        assert!(verdict.is_expected());
    }

    /// The byte this board actually returned during the command phase, 0x13.
    /// Taken apart by hand so a shifted field shows up here rather than as a
    /// confusing log line six steps later.
    #[test]
    fn stat2_decodes_the_byte_this_board_returned() {
        let s = Stat2::decode(0x13);
        assert!(s.flash);
        assert_eq!(s.chip_mode, chip_mode::STANDBY_RC);
        assert_eq!(s.reset_status, reset_status::ANALOG);
        assert!(s.is_standby_rc());
    }

    #[test]
    fn stat2_fields_do_not_bleed_into_each_other() {
        // Reset cause in the high nibble, mode in bits 1..=3, flash in bit 0:
        // EXTERNAL (2) << 4, FS (3) << 1, flash clear.
        let s = Stat2::decode(0b0010_0110);
        assert!(!s.flash);
        assert_eq!(s.chip_mode, chip_mode::FS);
        assert_eq!(s.reset_status, reset_status::EXTERNAL);

        // ...and the same nibbles with flash set must not shift anything.
        let s = Stat2::decode(0b0010_0111);
        assert!(s.flash);
        assert_eq!(s.chip_mode, chip_mode::FS);
        assert_eq!(s.reset_status, reset_status::EXTERNAL);
    }

    #[test]
    fn stat2_names_every_defined_value() {
        let modes = [
            chip_mode::SLEEP,
            chip_mode::STANDBY_RC,
            chip_mode::STANDBY_XOSC,
            chip_mode::FS,
            chip_mode::RX,
            chip_mode::TX,
            chip_mode::WIFI_GNSS,
        ];
        for (i, a) in modes.iter().enumerate() {
            let a = Stat2::decode(a << 1);
            assert_ne!(a.mode_summary(), "reserved mode");
            for b in &modes[i + 1..] {
                assert_ne!(a.mode_summary(), Stat2::decode(b << 1).mode_summary());
            }
        }
        assert_eq!(Stat2::decode(0b0000_1110).mode_summary(), "reserved mode");

        for cause in 0..=6u8 {
            assert_ne!(
                Stat2::decode(cause << 4).reset_summary(),
                "reserved reset cause"
            );
        }
        assert_eq!(Stat2::decode(0x70).reset_summary(), "reserved reset cause");
    }

    #[test]
    fn stat1_splits_the_interrupt_bit_from_the_command_status() {
        // command status in bits 1..=3, interrupt in bit 0.
        let s = Stat1::decode(0b0000_0110);
        assert_eq!(s.command_status, command_status::DATA);
        assert!(!s.interrupt);
        assert!(s.carries_data());

        let s = Stat1::decode(0b0000_0111);
        assert_eq!(s.command_status, command_status::DATA);
        assert!(s.interrupt);
    }

    /// Bits above 3 are not ours; a chip that sets them must not shift the
    /// command status.
    #[test]
    fn stat1_ignores_the_bits_above_the_command_status() {
        let s = Stat1::decode(0b1111_0110);
        assert_eq!(s.command_status, command_status::DATA);
    }

    /// The two signatures an unresponsive bus produces, and they are not
    /// symmetric. All zeros decodes as a defined status -- "could not be
    /// executed" -- which reads as a plausible chip answer. All ones decodes to
    /// 7, which the family does not define at all, so an idle-high bus gives
    /// itself away in this byte before the reply is even looked at.
    #[test]
    fn a_dead_bus_gives_itself_away_asymmetrically() {
        assert_eq!(Stat1::decode(0x00).command_status, command_status::FAIL);
        assert!(!Stat1::decode(0x00).carries_data());

        assert_eq!(Stat1::decode(0xFF).command_status, 7);
        assert!(!Stat1::decode(0xFF).carries_data());
        assert_eq!(Stat1::decode(0xFF).summary(), "reserved command status");
    }

    #[test]
    fn every_command_status_says_something() {
        let mut seen: [&str; 8] = [""; 8];
        for raw in 0..8u8 {
            let s = Stat1::decode(raw << 1);
            assert!(!s.summary().is_empty());
            seen[raw as usize] = s.summary();
        }
        for defined in 0..4usize {
            for other in defined + 1..4 {
                assert_ne!(seen[defined], seen[other]);
            }
        }
    }
}
