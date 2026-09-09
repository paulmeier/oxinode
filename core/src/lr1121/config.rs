//! The radio's settable parameters, their validation, and the chip's encodings.
//!
//! This is the seam the RNode protocol layer sits on. An RNode host sets
//! frequency, bandwidth, spreading factor, coding rate and transmit power one
//! at a time, in any order, and expects the modem to be running with whatever
//! the last complete set was. Two things follow, and both are why this module
//! has the shape it does.
//!
//! # An invalid configuration is a normal thing to hold
//!
//! A host moving from 915 MHz to 868 MHz sets one of them first, and in between
//! holds a configuration outside the band its antenna is cut for. Rejecting the
//! *set* would break a host doing nothing wrong; applying it would transmit
//! outside the band. So the mutable value and the thing that may be given to
//! the chip are deliberately different types: [`RadioConfig`] can hold anything
//! and [`ValidConfig`] can only be constructed by passing [`RadioConfig::check`].
//!
//! There is no way to reach the radio except through a `ValidConfig`, so
//! "someone forgot to validate" is not a bug that can be written.
//!
//! # The wire's units are not the chip's units
//!
//! Bandwidth travels as hertz and arrives as a four-bit code. Coding rate
//! travels as the denominator of 4/n — 5 to 8 — and arrives as 1 to 4.
//! Frequency travels as the frequency somebody wants and has to arrive 73 ppm
//! higher, for the reason in [`super::reference`]. Every one of those is a
//! place where a wrong answer produces a radio that works perfectly at settings
//! nobody chose, which on hardware with no probe is the expensive kind of bug.
//! They are all pure functions, and they are all tested here.

use super::lora;
use super::pa;
use super::reference;

/// Lowest spreading factor an RNode host can ask for.
///
/// The LR1121 goes down to SF5 and [`RadioConfig::check`] allows it, because
/// what the chip can do and what a protocol can express are different
/// questions. This constant is here so the protocol layer has the protocol's
/// answer without having to invent it.
pub const RNODE_SF_MIN: u8 = 7;
/// Highest spreading factor an RNode host can ask for. The same as the chip's.
pub const RNODE_SF_MAX: u8 = 12;

/// Lowest coding rate, as the denominator of 4/n. `4/5`, the least redundancy.
pub const CR_MIN: u8 = 5;
/// Highest coding rate, as the denominator of 4/n. `4/8`, the most.
pub const CR_MAX: u8 = 8;

/// Shortest preamble that still lets a receiver find the packet.
///
/// Below about six symbols a receiver has no chance to lock, and the LR1121
/// will accept the setting anyway. Not a chip limit; a physics one.
pub const PREAMBLE_MIN: u16 = 6;

/// The private-network sync word, which is what RNode uses.
///
/// `0x34` would announce this as LoRaWAN. It is not a security feature — a
/// receiver set to the other value simply does not correlate — but it is the
/// difference between two radios hearing each other and not.
pub const SYNC_WORD_PRIVATE: u8 = 0x12;

/// The largest payload a LoRa packet can carry.
pub const MAX_PAYLOAD: u8 = 255;

/// Everything the radio's PHY layer is told, in the units a host uses.
///
/// Plain data, `Copy`, and deliberately without invariants: this is the type a
/// host mutates one field at a time. [`RadioConfig::check`] is what turns it
/// into something the radio may be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioConfig {
    /// Centre frequency in hertz — the frequency *wanted*, not the one the
    /// chip is commanded with. See [`RadioConfig::commanded_frequency_hz`].
    pub frequency_hz: u32,
    /// Modulation bandwidth in hertz. Must be one of [`lora::BANDWIDTHS`].
    pub bandwidth_hz: u32,
    /// Spreading factor, 5 to 12.
    pub spreading_factor: u8,
    /// Coding rate as the denominator of 4/n, 5 to 8 — the host's units.
    pub coding_rate: u8,
    /// Transmit power in dBm at the connector.
    pub tx_power_dbm: i8,
    /// Preamble length in symbols.
    pub preamble_symbols: u16,
    /// Sync word.
    pub sync_word: u8,
    /// Whether to correct for the module's 73 ppm reference error.
    ///
    /// A field rather than a constant because the right answer depends on who
    /// is listening: corrected is right for another RNode and wrong for another
    /// nRFLR1121. See [`super::reference`].
    pub correct_reference: bool,
    /// Implicit header: the receiver is told the length rather than reading it.
    pub implicit_header: bool,
    /// Whether packets carry a CRC.
    pub crc: bool,
    /// Whether to invert the IQ signals, which is how some networks separate
    /// gateway traffic from node traffic.
    pub invert_iq: bool,
}

/// A sensible starting point, and what the firmware boots with.
///
/// Every value is a choice worth stating:
///
/// * **915.000 MHz** — the middle of US915, far enough from both edges that a
///   tuning error reads as an offset rather than as silence.
/// * **125 kHz, SF8, CR 4/5** — an ordinary LoRa configuration rather than an
///   extreme one, and fast enough that a bench test is not spent waiting.
/// * **14 dBm** — the top of the low-power PA, which is the only PA that has
///   ever been measured on this board.
/// * **corrected** — because an RNode's peers are other RNodes.
pub const DEFAULT: RadioConfig = RadioConfig {
    frequency_hz: pa::CW_TEST_HZ,
    bandwidth_hz: 125_000,
    spreading_factor: 8,
    coding_rate: 5,
    tx_power_dbm: pa::LP_MAX_DBM,
    preamble_symbols: 8,
    sync_word: SYNC_WORD_PRIVATE,
    correct_reference: true,
    implicit_header: false,
    crc: true,
    invert_iq: false,
};

/// Why a configuration may not be given to the radio.
///
/// One variant per reason, rather than a single "invalid". A host that is told
/// *which* limit it hit can correct itself; one that is told "no" cannot, and
/// the protocol layer has to answer a host over a serial line with no other
/// diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The frequency is outside the band this board's antenna is cut for.
    FrequencyOutOfBand,
    /// The bandwidth is not one the LR1121 offers.
    UnsupportedBandwidth,
    /// The spreading factor is outside 5–12.
    SpreadingFactorOutOfRange,
    /// The coding rate is outside 5–8.
    CodingRateOutOfRange,
    /// No PA on this chip produces that power.
    PowerUnreachable,
    /// The power is above what the *module* is rated for, whatever the die
    /// would accept.
    PowerAboveModuleRating,
    /// The preamble is too short for a receiver to lock onto.
    PreambleTooShort,
}

impl ConfigError {
    /// A line fit for a log or a serial console.
    ///
    /// `oxinode-core` has no dependencies and so cannot derive `defmt::Format`;
    /// the rest of this crate solves that the same way.
    pub const fn message(self) -> &'static str {
        match self {
            Self::FrequencyOutOfBand => "frequency is outside the 902-928 MHz band",
            Self::UnsupportedBandwidth => "bandwidth is not one of 62.5/125/250/500 kHz",
            Self::SpreadingFactorOutOfRange => "spreading factor is outside 5-12",
            Self::CodingRateOutOfRange => "coding rate is outside 5-8",
            Self::PowerUnreachable => "no PA on this chip produces that power",
            Self::PowerAboveModuleRating => "power is above the module's 20 dBm rating",
            Self::PreambleTooShort => "preamble is too short to lock onto",
        }
    }
}

impl RadioConfig {
    /// Whether this configuration may be given to the radio, and if not, why.
    ///
    /// The order the checks run in is the order a host is most likely to get
    /// them wrong, so the first complaint is the useful one.
    ///
    /// Note what is *not* checked: whether a host could express this over the
    /// RNode protocol. SF5 and SF6 are outside [`RNODE_SF_MIN`] and are still
    /// valid here, because what the chip can do and what a protocol can say are
    /// separate questions and conflating them would make the bench console
    /// unable to reach hardware that works.
    pub const fn check(&self) -> Result<(), ConfigError> {
        if !pa::is_in_us915(self.frequency_hz) {
            return Err(ConfigError::FrequencyOutOfBand);
        }
        if bandwidth_code(self.bandwidth_hz).is_none() {
            return Err(ConfigError::UnsupportedBandwidth);
        }
        if self.spreading_factor < lora::SF_MIN || self.spreading_factor > lora::SF_MAX {
            return Err(ConfigError::SpreadingFactorOutOfRange);
        }
        if self.coding_rate < CR_MIN || self.coding_rate > CR_MAX {
            return Err(ConfigError::CodingRateOutOfRange);
        }
        // The module's rating is checked before the die's, so a power the die
        // would accept and the module would not is named for what it is rather
        // than reported as unreachable.
        if self.tx_power_dbm > pa::MODULE_MAX_SUB_GHZ_DBM {
            return Err(ConfigError::PowerAboveModuleRating);
        }
        if pa::pa_config_for(self.tx_power_dbm).is_none() {
            return Err(ConfigError::PowerUnreachable);
        }
        if self.preamble_symbols < PREAMBLE_MIN {
            return Err(ConfigError::PreambleTooShort);
        }
        Ok(())
    }

    /// The frequency to command, which is not the frequency wanted.
    ///
    /// See [`super::reference`]. With the correction off this is the identity,
    /// which is what the bench needs in order to talk to another board carrying
    /// the same error.
    pub const fn commanded_frequency_hz(&self) -> u32 {
        if self.correct_reference {
            reference::command_for(self.frequency_hz)
        } else {
            self.frequency_hz
        }
    }

    /// The chip's bandwidth code.
    pub const fn bandwidth_code(&self) -> Option<u8> {
        bandwidth_code(self.bandwidth_hz)
    }

    /// The chip's coding-rate code, 1–4, from the host's 5–8.
    pub const fn coding_rate_code(&self) -> Option<u8> {
        coding_rate_code(self.coding_rate)
    }

    /// Whether the low-data-rate optimisation is required at these settings.
    pub const fn low_data_rate_optimize(&self) -> bool {
        lora::low_data_rate_optimize(self.spreading_factor, self.bandwidth_hz)
    }

    /// How long one symbol lasts, in microseconds.
    pub const fn symbol_time_us(&self) -> u32 {
        lora::symbol_time_us(self.spreading_factor, self.bandwidth_hz)
    }

    /// How long a packet of `payload_len` bytes occupies the air, in
    /// microseconds.
    ///
    /// This is the transmit timeout, so a wrong answer is either a spurious
    /// error or a hang.
    pub const fn airtime_us(&self, payload_len: u8) -> u32 {
        let Some(cr) = self.coding_rate_code() else {
            return 0;
        };
        lora::airtime_us(
            self.spreading_factor,
            self.bandwidth_hz,
            cr,
            self.preamble_symbols,
            payload_len,
            !self.implicit_header,
            self.crc,
        )
    }

    /// Payload bits per second, ignoring the preamble and header.
    ///
    /// `SF × BW / 2^SF × 4/(4+n)`, the usual figure. Rounded down, in integers.
    /// Not used to decide anything — it is what a status line reports and what
    /// makes "SF12 at 62.5 kHz" mean something to a person.
    pub const fn bitrate_bps(&self) -> u32 {
        let Some(cr) = self.coding_rate_code() else {
            return 0;
        };
        let sf = self.spreading_factor as u64;
        let numerator = sf * self.bandwidth_hz as u64 * 4;
        let denominator = (1u64 << sf) * (4 + cr as u64);
        (numerator / denominator) as u32
    }

    /// Which PA this power selects, or `None` if none reaches it.
    pub const fn pa_config(&self) -> Option<pa::PaConfigWord> {
        pa::pa_config_for(self.tx_power_dbm)
    }

    /// Whether an RNode host could express this configuration.
    ///
    /// Separate from [`RadioConfig::check`] on purpose — see its note. The
    /// protocol layer needs this to know whether a locally-set configuration
    /// can be reported back honestly.
    pub const fn is_rnode_representable(&self) -> bool {
        self.spreading_factor >= RNODE_SF_MIN
            && self.spreading_factor <= RNODE_SF_MAX
            && self.tx_power_dbm >= 0
    }
}

/// One radio parameter with a value, as the panel sets it.
///
/// The five things an RNode host sets, one at a time -- see the module docs.
/// The panel's editor changes one of them per confirmation, and this is what
/// it hands back; [`RadioConfig::apply`] is the one place a setting lands in
/// a configuration, so the panel and the host cannot disagree about which
/// field a setting means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Hertz.
    Frequency(u32),
    /// Hertz.
    Bandwidth(u32),
    /// 7 to 12, as a host would ask.
    SpreadingFactor(u8),
    /// The denominator of 4/n, 5 to 8.
    CodingRate(u8),
    /// dBm at the connector.
    TxPower(i8),
}

impl Setting {
    /// The parameter's name, for a log.
    pub const fn name(self) -> &'static str {
        match self {
            Setting::Frequency(_) => "frequency",
            Setting::Bandwidth(_) => "bandwidth",
            Setting::SpreadingFactor(_) => "spreading factor",
            Setting::CodingRate(_) => "coding rate",
            Setting::TxPower(_) => "tx power",
        }
    }
}

impl RadioConfig {
    /// Land one setting in this configuration, unvalidated.
    ///
    /// Unvalidated on purpose, for the reason the host's setters are: an
    /// intermediate state is a normal thing to hold. Whether the result may
    /// reach the radio is [`RadioConfig::check`]'s question, asked afterwards.
    pub const fn apply(&mut self, setting: Setting) {
        match setting {
            Setting::Frequency(hz) => self.frequency_hz = hz,
            Setting::Bandwidth(hz) => self.bandwidth_hz = hz,
            Setting::SpreadingFactor(sf) => self.spreading_factor = sf,
            Setting::CodingRate(cr) => self.coding_rate = cr,
            Setting::TxPower(dbm) => self.tx_power_dbm = dbm,
        }
    }

    /// The same configuration with one setting changed.
    pub const fn with(mut self, setting: Setting) -> Self {
        self.apply(setting);
        self
    }
}

/// A configuration that has passed [`RadioConfig::check`].
///
/// The only way to make one, and the only thing the modem will accept. That is
/// the entire point: it moves "did anybody validate this?" from a question
/// about code review to a question the compiler answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidConfig(RadioConfig);

impl ValidConfig {
    /// Validate, or say why not.
    pub const fn new(config: RadioConfig) -> Result<Self, ConfigError> {
        match config.check() {
            Ok(()) => Ok(Self(config)),
            Err(e) => Err(e),
        }
    }

    /// The configuration inside.
    pub const fn get(&self) -> &RadioConfig {
        &self.0
    }

    /// The chip's bandwidth code. Infallible here, because validation
    /// established it exists.
    pub const fn bandwidth_code(&self) -> u8 {
        match self.0.bandwidth_code() {
            Some(code) => code,
            // Unreachable: `check` rejects a bandwidth with no code. Returning
            // the 125 kHz code rather than panicking keeps this a `const fn`
            // with no panic path in the firmware, and the compile-time
            // assertions below are what actually hold the invariant.
            None => 0x04,
        }
    }

    /// The chip's coding-rate code. Infallible, for the same reason.
    pub const fn coding_rate_code(&self) -> u8 {
        match self.0.coding_rate_code() {
            Some(code) => code,
            None => 0x01,
        }
    }

    /// The PA word this power selects. Infallible, for the same reason.
    pub const fn pa_config(&self) -> pa::PaConfigWord {
        match self.0.pa_config() {
            Some(word) => word,
            None => pa::LOW_POWER,
        }
    }
}

impl core::ops::Deref for ValidConfig {
    type Target = RadioConfig;
    fn deref(&self) -> &RadioConfig {
        &self.0
    }
}

/// The LR1121's code for a bandwidth in hertz, or `None` if it has none.
pub const fn bandwidth_code(hz: u32) -> Option<u8> {
    // A loop rather than a match so the table in `lora` stays the one source of
    // truth; `const fn` allows `while` but not `for`.
    let mut i = 0;
    while i < lora::BANDWIDTHS.len() {
        let (code, bw) = lora::BANDWIDTHS[i];
        if bw == hz {
            return Some(code);
        }
        i += 1;
    }
    None
}

/// The chip's coding-rate code, 1–4, from the denominator of 4/n, 5–8.
///
/// The offset is trivial and the mapping is not: `lr11xx` also defines codes
/// 5–7 for the *long* interleaver at the same rates, so an implementation that
/// passed the host's number through would select long-interleaver 4/5 when it
/// meant short-interleaver 4/8 — a working radio that no other radio can hear.
pub const fn coding_rate_code(cr: u8) -> Option<u8> {
    if cr < CR_MIN || cr > CR_MAX {
        return None;
    }
    Some(cr - 4)
}

/// The denominator of 4/n from the chip's short-interleaver code. The inverse
/// of [`coding_rate_code`], for reporting a configuration back.
pub const fn coding_rate_from_code(code: u8) -> Option<u8> {
    if code < 1 || code > 4 {
        return None;
    }
    Some(code + 4)
}

// The default has to be valid, or the firmware cannot boot into it. Checked
// when the crate compiles rather than when its tests run, because a build that
// cannot produce a working radio should not exist.
const _: () = assert!(DEFAULT.check().is_ok());
// ...and it has to be something a host could also ask for, or the firmware
// boots into a state it cannot describe.
const _: () = assert!(DEFAULT.is_rnode_representable());
// The default power must come from the PA that has actually been measured.
const _: () = assert!(pa::low_power_pa_accepts(DEFAULT.tx_power_dbm));
// The codes are the chip's, and the two directions must agree. A silent
// off-by-one here transmits at the wrong redundancy and reports the right one.
const _: () = assert!(matches!(coding_rate_code(5), Some(1)));
const _: () = assert!(matches!(coding_rate_code(8), Some(4)));
const _: () = assert!(coding_rate_code(4).is_none() && coding_rate_code(9).is_none());
const _: () = assert!(matches!(coding_rate_from_code(1), Some(5)));
const _: () = assert!(matches!(coding_rate_from_code(4), Some(8)));
// `ValidConfig`'s infallible accessors lean on validation having run. These
// assert the fallbacks are unreachable rather than merely unlikely, by pinning
// the property that makes them so.
const _: () = assert!(bandwidth_code(DEFAULT.bandwidth_hz).is_some());
const _: () = assert!(DEFAULT.pa_config().is_some());
// The RNode subset is a subset. If it ever stopped being one, `check` would be
// rejecting configurations a host is entitled to ask for.
const _: () = assert!(RNODE_SF_MIN >= lora::SF_MIN && RNODE_SF_MAX <= lora::SF_MAX);

#[cfg(test)]
mod tests {
    use super::*;

    /// Each rejection has to name its own reason. A host on the far end of a
    /// serial line has nothing else to go on.
    #[test]
    fn every_limit_is_reported_as_itself() {
        let cases = [
            (
                RadioConfig {
                    frequency_hz: 868_000_000,
                    ..DEFAULT
                },
                ConfigError::FrequencyOutOfBand,
            ),
            (
                RadioConfig {
                    bandwidth_hz: 100_000,
                    ..DEFAULT
                },
                ConfigError::UnsupportedBandwidth,
            ),
            (
                RadioConfig {
                    spreading_factor: 13,
                    ..DEFAULT
                },
                ConfigError::SpreadingFactorOutOfRange,
            ),
            (
                RadioConfig {
                    spreading_factor: 4,
                    ..DEFAULT
                },
                ConfigError::SpreadingFactorOutOfRange,
            ),
            (
                RadioConfig {
                    coding_rate: 9,
                    ..DEFAULT
                },
                ConfigError::CodingRateOutOfRange,
            ),
            (
                RadioConfig {
                    coding_rate: 4,
                    ..DEFAULT
                },
                ConfigError::CodingRateOutOfRange,
            ),
            (
                RadioConfig {
                    tx_power_dbm: 21,
                    ..DEFAULT
                },
                ConfigError::PowerAboveModuleRating,
            ),
            (
                RadioConfig {
                    tx_power_dbm: -30,
                    ..DEFAULT
                },
                ConfigError::PowerUnreachable,
            ),
            (
                RadioConfig {
                    preamble_symbols: 4,
                    ..DEFAULT
                },
                ConfigError::PreambleTooShort,
            ),
        ];
        for (config, expected) in cases {
            assert_eq!(config.check(), Err(expected), "{config:?}");
            assert_eq!(ValidConfig::new(config), Err(expected));
        }
    }

    /// The band edges are inside the band, and one hertz outside either is not.
    /// This is the check that keeps a carrier off an antenna cut for elsewhere.
    #[test]
    fn the_band_edges_are_inclusive() {
        for hz in [pa::US915_MIN_HZ, pa::US915_MAX_HZ] {
            assert!(RadioConfig {
                frequency_hz: hz,
                ..DEFAULT
            }
            .check()
            .is_ok());
        }
        for hz in [pa::US915_MIN_HZ - 1, pa::US915_MAX_HZ + 1] {
            assert_eq!(
                RadioConfig {
                    frequency_hz: hz,
                    ..DEFAULT
                }
                .check(),
                Err(ConfigError::FrequencyOutOfBand)
            );
        }
    }

    /// Every bandwidth the chip offers must validate and encode; nothing else
    /// may. A bandwidth that validated without a code would reach
    /// `ValidConfig::bandwidth_code`'s unreachable fallback and transmit at
    /// 125 kHz whatever it was told.
    #[test]
    fn exactly_the_chips_bandwidths_are_accepted() {
        for (code, hz) in lora::BANDWIDTHS {
            let config = RadioConfig {
                bandwidth_hz: hz,
                ..DEFAULT
            };
            let valid = ValidConfig::new(config).expect("a chip bandwidth must validate");
            assert_eq!(valid.bandwidth_code(), code, "{hz} Hz");
        }
        for hz in [0, 1, 62_499, 100_000, 125_001, 1_000_000] {
            assert_eq!(bandwidth_code(hz), None, "{hz} Hz");
        }
    }

    /// The whole range the chip supports, including the two spreading factors
    /// an RNode host cannot ask for. Rejecting those would make the bench
    /// console unable to reach hardware that works.
    #[test]
    fn the_chips_spreading_factors_are_all_valid_and_two_are_not_rnode() {
        for sf in lora::SF_MIN..=lora::SF_MAX {
            let config = RadioConfig {
                spreading_factor: sf,
                ..DEFAULT
            };
            assert!(config.check().is_ok(), "SF{sf}");
            assert_eq!(
                config.is_rnode_representable(),
                sf >= RNODE_SF_MIN,
                "SF{sf}"
            );
        }
    }

    /// Round-tripping the coding rate. The dangerous failure is not rejection,
    /// it is passing 5–8 straight through: those are the chip's *long
    /// interleaver* codes, so the radio would work and nothing else would hear
    /// it.
    #[test]
    fn the_coding_rate_is_translated_and_not_passed_through() {
        for cr in CR_MIN..=CR_MAX {
            let code = coding_rate_code(cr).expect("in range");
            assert!((1..=4).contains(&code), "4/{cr} -> {code}");
            assert_ne!(code, cr, "4/{cr} must not pass through unchanged");
            assert_eq!(coding_rate_from_code(code), Some(cr));
        }
    }

    /// A longer coding rate must cost airtime, and the arithmetic must be
    /// reaching `lora` rather than quietly using a fixed code.
    #[test]
    fn airtime_responds_to_every_parameter_that_affects_it() {
        let base = DEFAULT.airtime_us(16);
        assert!(base > 0);
        let cases = [
            RadioConfig {
                coding_rate: 8,
                ..DEFAULT
            },
            RadioConfig {
                spreading_factor: 9,
                ..DEFAULT
            },
            RadioConfig {
                preamble_symbols: 16,
                ..DEFAULT
            },
        ];
        for config in cases {
            assert!(config.airtime_us(16) > base, "{config:?}");
        }
        // ...and falls with bandwidth, which is the one that goes the other way.
        assert!(
            RadioConfig {
                bandwidth_hz: 250_000,
                ..DEFAULT
            }
            .airtime_us(16)
                < base
        );
        // A longer payload always costs more, at every supported setting.
        for sf in lora::SF_MIN..=lora::SF_MAX {
            let config = RadioConfig {
                spreading_factor: sf,
                ..DEFAULT
            };
            assert!(config.airtime_us(255) > config.airtime_us(1), "SF{sf}");
        }
    }

    /// The airtime here and the airtime `lora` computes are the same number.
    /// They are reached by different routes — one through the host's coding
    /// rate, one through the chip's — and a mismatch would mean the translation
    /// is wrong in exactly the way nothing else would notice.
    #[test]
    fn airtime_agrees_with_the_module_it_delegates_to() {
        let config = RadioConfig {
            spreading_factor: lora::bench::SF,
            bandwidth_hz: lora::bench::BANDWIDTH_HZ,
            coding_rate: 5,
            preamble_symbols: lora::bench::PREAMBLE,
            ..DEFAULT
        };
        assert_eq!(
            config.airtime_us(lora::bench::PAYLOAD_LEN),
            lora::bench::AIRTIME_US
        );
    }

    /// SF8 at 125 kHz with CR 4/5 is 3125 bps: 8 × 125000 / 256 × 4/5.
    /// Worked by hand, because a bitrate is the number a person sanity-checks a
    /// configuration against.
    #[test]
    fn the_bitrate_matches_the_hand_calculation() {
        assert_eq!(DEFAULT.bitrate_bps(), 3_125);
        // SF12 at 125 kHz, CR 4/5: 12 × 125000 / 4096 × 4/5 = 292.97 bps, and
        // this rounds down. Pinned as 292 rather than 293 so that the
        // truncation is a recorded decision instead of a surprise the first
        // time somebody compares a status line against a calculator.
        assert_eq!(
            RadioConfig {
                spreading_factor: 12,
                ..DEFAULT
            }
            .bitrate_bps(),
            292
        );
        // Slower spreading factors and longer coding rates both cost rate.
        assert!(
            RadioConfig {
                coding_rate: 8,
                ..DEFAULT
            }
            .bitrate_bps()
                < DEFAULT.bitrate_bps()
        );
    }

    /// The correction is off by default in the *value* only when asked. What
    /// matters is that the field actually reaches the commanded frequency, in
    /// both positions — a flag that is read nowhere is worse than no flag.
    #[test]
    fn the_reference_correction_reaches_the_commanded_frequency() {
        let corrected = RadioConfig {
            correct_reference: true,
            ..DEFAULT
        };
        let raw = RadioConfig {
            correct_reference: false,
            ..DEFAULT
        };
        assert_eq!(raw.commanded_frequency_hz(), raw.frequency_hz);
        assert_eq!(
            corrected.commanded_frequency_hz(),
            reference::command_for(DEFAULT.frequency_hz)
        );
        assert!(corrected.commanded_frequency_hz() - raw.commanded_frequency_hz() > 60_000);
    }

    /// A corrected frequency may leave the band even though the wanted one is
    /// inside it — 928 MHz corrected is 928.068 MHz. That is 68 kHz outside a
    /// 26 MHz band and it is what the hardware would actually radiate, so the
    /// fact is recorded here rather than discovered later.
    #[test]
    fn correcting_at_the_top_of_the_band_pushes_the_carrier_just_outside_it() {
        let top = RadioConfig {
            frequency_hz: pa::US915_MAX_HZ,
            correct_reference: true,
            ..DEFAULT
        };
        assert!(top.check().is_ok());
        assert!(!pa::is_in_us915(top.commanded_frequency_hz()));
        // Small enough to be a guard-band question rather than a band-plan one.
        assert!(top.commanded_frequency_hz() - pa::US915_MAX_HZ < 100_000);
    }

    /// Powers above the low-power PA select the high-power one, with the
    /// supply that power needs. Nothing in the config layer may quietly clamp:
    /// a host that asks for 20 dBm and gets 14 has no way to find out.
    #[test]
    fn the_power_selects_a_pa_and_is_never_silently_clamped() {
        for dbm in pa::LP_MIN_DBM..=pa::MODULE_MAX_SUB_GHZ_DBM {
            let config = RadioConfig {
                tx_power_dbm: dbm,
                ..DEFAULT
            };
            let valid = ValidConfig::new(config).expect("within the module's rating");
            assert_eq!(valid.tx_power_dbm, dbm);
            assert_eq!(valid.pa_config().pa_sel, u8::from(dbm > pa::LP_MAX_DBM));
        }
        assert_eq!(
            ValidConfig::new(RadioConfig {
                tx_power_dbm: pa::MODULE_MAX_SUB_GHZ_DBM + 1,
                ..DEFAULT
            }),
            Err(ConfigError::PowerAboveModuleRating)
        );
    }

    /// The low-data-rate optimisation is a property of the configuration, not
    /// a setting, and it has to track. Left off where it is required, a
    /// receiver decodes nothing and every other parameter looks right.
    #[test]
    fn the_low_data_rate_optimisation_tracks_the_symbol_time() {
        for sf in lora::SF_MIN..=lora::SF_MAX {
            for (_, bw) in lora::BANDWIDTHS {
                let config = RadioConfig {
                    spreading_factor: sf,
                    bandwidth_hz: bw,
                    ..DEFAULT
                };
                assert_eq!(
                    config.low_data_rate_optimize(),
                    config.symbol_time_us() > 16_000,
                    "SF{sf} BW{bw}"
                );
            }
        }
    }

    /// Every combination the chip supports must validate and produce sane
    /// derived values. This is the sweep that catches a division by zero or an
    /// overflow at a corner nobody would think to try.
    #[test]
    fn every_supported_combination_validates_and_derives() {
        for sf in lora::SF_MIN..=lora::SF_MAX {
            for (_, bw) in lora::BANDWIDTHS {
                for cr in CR_MIN..=CR_MAX {
                    for dbm in [pa::LP_MIN_DBM, 0, pa::LP_MAX_DBM, 20] {
                        let config = RadioConfig {
                            spreading_factor: sf,
                            bandwidth_hz: bw,
                            coding_rate: cr,
                            tx_power_dbm: dbm,
                            ..DEFAULT
                        };
                        let valid = ValidConfig::new(config)
                            .unwrap_or_else(|e| panic!("{config:?}: {}", e.message()));
                        assert!(valid.bitrate_bps() > 0);
                        assert!(valid.airtime_us(255) > 0);
                        assert!(valid.airtime_us(255) < 30_000_000);
                        assert!((0x03..=0x06).contains(&valid.bandwidth_code()));
                        assert!((1..=4).contains(&valid.coding_rate_code()));
                    }
                }
            }
        }
    }

    /// The only way to make a `ValidConfig` is to pass validation, and what
    /// comes out is what went in. If these two ever diverged, the modem would
    /// be programming something other than what was asked for.
    #[test]
    fn a_valid_config_carries_the_configuration_unchanged() {
        let config = RadioConfig {
            frequency_hz: 903_500_000,
            bandwidth_hz: 500_000,
            spreading_factor: 5,
            coding_rate: 7,
            tx_power_dbm: -3,
            preamble_symbols: 12,
            sync_word: 0x2b,
            correct_reference: false,
            implicit_header: true,
            crc: false,
            invert_iq: true,
        };
        let valid = ValidConfig::new(config).expect("valid");
        assert_eq!(*valid.get(), config);
        assert_eq!(valid.sync_word, 0x2b);
        assert_eq!(valid.coding_rate_code(), 3);
        assert_eq!(valid.bandwidth_code(), 0x06);
    }

    /// A setting lands in exactly the field it names, and nowhere else. A
    /// setting that landed in the wrong field would be a panel that changes
    /// the bandwidth when asked for the spreading factor.
    #[test]
    fn a_setting_changes_exactly_the_field_it_names() {
        type Read = fn(&RadioConfig) -> i64;
        let cases: [(Setting, Read); 5] = [
            (Setting::Frequency(903_000_000), |c| c.frequency_hz as i64),
            (Setting::Bandwidth(500_000), |c| c.bandwidth_hz as i64),
            (Setting::SpreadingFactor(12), |c| c.spreading_factor as i64),
            (Setting::CodingRate(8), |c| c.coding_rate as i64),
            (Setting::TxPower(-3), |c| c.tx_power_dbm as i64),
        ];
        for (setting, read) in cases {
            let changed = DEFAULT.with(setting);
            assert_ne!(
                read(&changed),
                read(&DEFAULT),
                "{setting:?} changed nothing"
            );
            // Every other field is untouched.
            let mut back = changed;
            match setting {
                Setting::Frequency(_) => back.frequency_hz = DEFAULT.frequency_hz,
                Setting::Bandwidth(_) => back.bandwidth_hz = DEFAULT.bandwidth_hz,
                Setting::SpreadingFactor(_) => back.spreading_factor = DEFAULT.spreading_factor,
                Setting::CodingRate(_) => back.coding_rate = DEFAULT.coding_rate,
                Setting::TxPower(_) => back.tx_power_dbm = DEFAULT.tx_power_dbm,
            }
            assert_eq!(back, DEFAULT, "{setting:?} touched another field");
            assert!(!setting.name().is_empty());
        }
    }

    /// Applying does not validate, so an editor can hold a refused value
    /// and say so; refusing happens in `check`, once, for everybody.
    #[test]
    fn applying_a_setting_never_validates() {
        let out_of_band = DEFAULT.with(Setting::Frequency(868_000_000));
        assert_eq!(out_of_band.frequency_hz, 868_000_000);
        assert_eq!(out_of_band.check(), Err(ConfigError::FrequencyOutOfBand));
        let too_hot = DEFAULT.with(Setting::TxPower(22));
        assert_eq!(too_hot.check(), Err(ConfigError::PowerAboveModuleRating));
    }

    /// Every error has a message, and none of them is empty. This is the only
    /// text a host or an operator ever sees for a rejected configuration.
    #[test]
    fn every_error_says_something() {
        for e in [
            ConfigError::FrequencyOutOfBand,
            ConfigError::UnsupportedBandwidth,
            ConfigError::SpreadingFactorOutOfRange,
            ConfigError::CodingRateOutOfRange,
            ConfigError::PowerUnreachable,
            ConfigError::PowerAboveModuleRating,
            ConfigError::PreambleTooShort,
        ] {
            assert!(e.message().len() > 10, "{e:?}");
        }
    }
}
