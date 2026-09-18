//! Editing one radio parameter from the panel: the value model.
//!
//! The navigator stops at two levels on purpose and leaves room for one
//! exception, and this is it: open a field, change it, confirm or cancel. What
//! is here is the *value* side of that -- which fields there are, what each
//! steps through, how a frequency is typed a digit at a time, and what happens
//! at confirmation. The words on the screen are [`crate::screens`]'s, and the
//! key handling that gets here is [`crate::ui::Nav`]'s.
//!
//! # Refused, not clamped
//!
//! An editor is handed a copy of the whole configuration when it opens, and
//! confirming puts the candidate into that copy and asks
//! [`ValidConfig::new`] -- the same function that decides whether a host's
//! configuration may reach the chip. A candidate that fails stays in the
//! editor with the reason under it, unapplied; nothing rounds it to the nearest
//! legal value. That is the modem's rule, and the panel is not an exception to
//! it: a person who asked for 21 dBm and was given 20 without being told has
//! no way to find out.
//!
//! The sets a stepper walks are therefore deliberately wider than what the
//! radio accepts. Power runs to 22 dBm because that is what an RNode host can
//! ask for and the refusal has to be reachable to be honest; bandwidth lists
//! the ten LoRa bandwidths a host offers and the three the 2.4 GHz path has,
//! six of which this chip does not have at all and the rest of which it has
//! on one band or the other. A stepper that only ever produced legal values
//! would never show the refusal path, and the first time it mattered would be
//! a value nobody had tested.
//!
//! One set serves both bands, on purpose. The band is decided by the
//! frequency, and a person moving the board from 915 MHz to 2478 MHz edits
//! the frequency first and then the bandwidth -- the same order a host sets
//! them in, through the same mixed state, refused for the same reason until
//! the bandwidth follows. A stepper that hid the other band's values would
//! have to guess which band the person was on their way to.
//!
//! # Cancel restores
//!
//! Nothing is applied until it is confirmed. The original configuration is
//! kept in the editor untouched, and cancelling hands nothing back at all --
//! the caller's configuration was never modified, so there is nothing to
//! restore. The test for it is the whole point of the design: it asserts about
//! what the editor *returns*, and cancel returns nothing.

use crate::lr1121::config::{ConfigError, RadioConfig, Setting, ValidConfig};
use crate::lr1121::pa;

/// Which parameter an editor is for.
///
/// In the order the Radio screen lists them, which is the order a host sets
/// them and the order the screen's own rows go in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Frequency,
    Bandwidth,
    SpreadingFactor,
    CodingRate,
    TxPower,
}

impl Field {
    /// Every field, in menu order.
    pub const ALL: [Field; 5] = [
        Field::Frequency,
        Field::Bandwidth,
        Field::SpreadingFactor,
        Field::CodingRate,
        Field::TxPower,
    ];

    /// The word in the title bar while it is being edited.
    pub const fn title(self) -> &'static str {
        match self {
            Field::Frequency => "Frequency",
            Field::Bandwidth => "Bandwidth",
            Field::SpreadingFactor => "Spread Factor",
            Field::CodingRate => "Coding Rate",
            Field::TxPower => "TX Power",
        }
    }

    /// The menu item that opens it.
    pub const fn label(self) -> &'static str {
        match self {
            Field::Frequency => "Frequency",
            Field::Bandwidth => "Bandwidth",
            Field::SpreadingFactor => "Spread Factor",
            Field::CodingRate => "Coding Rate",
            Field::TxPower => "TX Power",
        }
    }

    /// Its name, for a log.
    pub const fn name(self) -> &'static str {
        match self {
            Field::Frequency => "frequency",
            Field::Bandwidth => "bandwidth",
            Field::SpreadingFactor => "spreading factor",
            Field::CodingRate => "coding rate",
            Field::TxPower => "tx power",
        }
    }

    /// The value this field has in a configuration, as the editor carries it.
    ///
    /// Wide, because a 2.4 GHz frequency in hertz does not fit a signed
    /// 32-bit value, and the editor's arithmetic must not wrap it.
    pub const fn value_in(self, config: &RadioConfig) -> i64 {
        match self {
            Field::Frequency => config.frequency_hz as i64,
            Field::Bandwidth => config.bandwidth_hz as i64,
            Field::SpreadingFactor => config.spreading_factor as i64,
            Field::CodingRate => config.coding_rate as i64,
            Field::TxPower => config.tx_power_dbm as i64,
        }
    }

    /// A value of this field as a [`Setting`].
    ///
    /// The narrowing casts are safe by construction for the steppers, which
    /// only produce members of their sets. The digit editor is the exception:
    /// seven digits of kilohertz reach 9999.999 MHz, which is past what a
    /// `u32` of hertz holds, and a plain cast would wrap 5200 MHz to 905 MHz
    /// -- a refused value quietly becoming a legal one. So a frequency above
    /// the field's range saturates instead. That is not a clamp of a
    /// configuration: `u32::MAX` is in neither band and is refused, which is
    /// what the value it stands for would have been.
    pub const fn setting(self, value: i64) -> Setting {
        match self {
            Field::Frequency => Setting::Frequency(if value > u32::MAX as i64 {
                u32::MAX
            } else {
                value as u32
            }),
            Field::Bandwidth => Setting::Bandwidth(value as u32),
            Field::SpreadingFactor => Setting::SpreadingFactor(value as u8),
            Field::CodingRate => Setting::CodingRate(value as u8),
            Field::TxPower => Setting::TxPower(value as i8),
        }
    }

    /// The fixed set a stepper walks, ascending; empty for the digit editor.
    pub const fn steps(self) -> &'static [i64] {
        match self {
            Field::Frequency => &[],
            Field::Bandwidth => &BANDWIDTH_STEPS,
            Field::SpreadingFactor => &SF_STEPS,
            Field::CodingRate => &CR_STEPS,
            Field::TxPower => &POWER_STEPS,
        }
    }
}

/// The bandwidths a LoRa host offers and the three the 2.4 GHz path has, in
/// hertz, in one ascending set. Six of them this chip cannot do on either
/// band, and they are here so the refusal is reachable -- see the module
/// docs. The odd 2.4 GHz values are exact: 1625 kHz over a power of two.
pub const BANDWIDTH_STEPS: [i64; 13] = [
    7_800, 10_400, 15_600, 20_800, 31_250, 41_700, 62_500, 125_000, 203_125, 250_000, 406_250,
    500_000, 812_500,
];
/// The spreading factors an RNode host can ask for.
pub const SF_STEPS: [i64; 6] = [7, 8, 9, 10, 11, 12];
/// The coding rates, as the denominator of 4/n.
pub const CR_STEPS: [i64; 4] = [5, 6, 7, 8];
/// Powers from the bottom of the lowest PA to the top of what a host can ask
/// for, one decibel apart. The bottom is the high-frequency PA's -18 dBm,
/// which no sub-GHz PA reaches; the top is 22 dBm, above the module's rating
/// on either band. Both ends are refused when confirmed, which is the point.
pub const POWER_STEPS: [i64; 41] = {
    let mut steps = [0i64; 41];
    let mut i = 0;
    while i < steps.len() {
        steps[i] = pa::HF_MIN_DBM as i64 + i as i64;
        i += 1;
    }
    steps
};

/// How many digits the frequency editor shows: `MMMM.kkk`, megahertz to the
/// kilohertz. Four megahertz digits because 2478 MHz needs them; a sub-GHz
/// frequency shows a blank where its leading zero would be. Anything under a
/// kilohertz is kept from the original value and not shown, because no
/// channel plan is drawn in hertz.
pub const FREQ_DIGITS: usize = 7;
/// How many of those digits are megahertz: where the point goes.
pub const FREQ_POINT: usize = 4;

/// Who has the radio, when it is not the panel.
///
/// A host that configured the radio believes what it set, and Reticulum's
/// interface code checks its configuration against the device exactly once,
/// when it brings the interface up. Changing the radio underneath it would
/// leave the host transmitting on a channel it thinks is a different one.
/// So while a host has the line, the live configuration is its.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// The KISS port is open on USB.
    Usb,
    /// A phone is connected.
    Bluetooth,
}

impl Lock {
    /// The first line of the notice.
    pub const fn headline(self) -> &'static str {
        match self {
            Lock::Usb => "USB has the radio.",
            Lock::Bluetooth => "Phone has the radio.",
        }
    }
}

/// An editor for one field: the configuration it opened on, the candidate,
/// and the last refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Editor {
    field: Field,
    /// The configuration as it was when the editor opened. Never modified;
    /// this is what cancel leaves in place.
    original: RadioConfig,
    /// The value being edited, in the field's own units.
    candidate: i64,
    /// The frequency editor's digits, most significant first, and which one
    /// the cursor is under. Unused by the steppers.
    digits: [u8; FREQ_DIGITS],
    cursor: usize,
    /// Why the last confirmation was refused, until the value changes.
    refused: Option<ConfigError>,
}

/// What confirming did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// The candidate passed validation: apply this and close the editor.
    Apply(Setting),
    /// The candidate was refused. The editor stays open and says why.
    Refused(ConfigError),
}

impl Editor {
    /// Open an editor on `field`, starting from what `config` has.
    pub fn open(field: Field, config: RadioConfig) -> Self {
        let candidate = field.value_in(&config);
        let mut digits = [0u8; FREQ_DIGITS];
        if field == Field::Frequency {
            // Kilohertz, seven digits, most significant first.
            let mut khz = config.frequency_hz / 1_000;
            for slot in digits.iter_mut().rev() {
                *slot = (khz % 10) as u8;
                khz /= 10;
            }
        }
        Editor {
            field,
            original: config,
            candidate,
            digits,
            cursor: 0,
            refused: None,
        }
    }

    pub const fn field(&self) -> Field {
        self.field
    }

    /// The value as it stands, in the field's units.
    pub const fn candidate(&self) -> i64 {
        self.candidate
    }

    /// The value the field had when the editor opened.
    pub const fn original(&self) -> i64 {
        self.field.value_in(&self.original)
    }

    /// The configuration the editor opened on, untouched.
    pub const fn original_config(&self) -> &RadioConfig {
        &self.original
    }

    /// Whether the candidate differs from what the editor opened on.
    pub const fn changed(&self) -> bool {
        self.candidate != self.original()
    }

    /// Why the last confirmation was refused, if it was and nothing has
    /// changed since.
    pub const fn refused(&self) -> Option<ConfigError> {
        self.refused
    }

    /// The frequency editor's digits, `MMMM.kkk` without the point.
    pub const fn digits(&self) -> &[u8; FREQ_DIGITS] {
        &self.digits
    }

    /// Which digit the cursor is under, from the left.
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether this editor walks a fixed set (else it is the digit editor).
    pub const fn is_stepper(&self) -> bool {
        !self.field.steps().is_empty()
    }

    /// Up: the next value in the set, or the digit under the cursor plus one.
    pub fn up(&mut self) {
        self.refused = None;
        if self.is_stepper() {
            // The first member above the candidate, so a value that is not in
            // the set -- one a host set -- steps onto the set rather than
            // past it.
            if let Some(&next) = self.field.steps().iter().find(|&&v| v > self.candidate) {
                self.candidate = next;
            }
        } else {
            self.digits[self.cursor] = (self.digits[self.cursor] + 1) % 10;
            self.candidate = self.frequency_from_digits();
        }
    }

    /// Down: the previous value in the set, or the digit under the cursor
    /// minus one.
    pub fn down(&mut self) {
        self.refused = None;
        if self.is_stepper() {
            if let Some(&prev) = self
                .field
                .steps()
                .iter()
                .rev()
                .find(|&&v| v < self.candidate)
            {
                self.candidate = prev;
            }
        } else {
            self.digits[self.cursor] = (self.digits[self.cursor] + 9) % 10;
            self.candidate = self.frequency_from_digits();
        }
    }

    /// Move the cursor one digit left. Steppers have no cursor.
    pub fn left(&mut self) {
        if !self.is_stepper() {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    /// Move the cursor one digit right.
    pub fn right(&mut self) {
        if !self.is_stepper() && self.cursor + 1 < FREQ_DIGITS {
            self.cursor += 1;
        }
    }

    /// The digits as hertz, with the sub-kilohertz part of the original kept.
    ///
    /// Wide arithmetic: seven digits of kilohertz is up to ten of hertz, and
    /// the top of that range is more than a `u32` holds. See
    /// [`Field::setting`] for what happens to it at confirmation.
    fn frequency_from_digits(&self) -> i64 {
        let mut khz: i64 = 0;
        for &d in &self.digits {
            khz = khz * 10 + d as i64;
        }
        khz * 1_000 + (self.original.frequency_hz % 1_000) as i64
    }

    /// The configuration the candidate would produce.
    pub const fn candidate_config(&self) -> RadioConfig {
        self.original.with(self.field.setting(self.candidate))
    }

    /// Confirm: validate the candidate the way a host's configuration is
    /// validated, and either hand it back or refuse it.
    pub fn confirm(&mut self) -> Confirm {
        match ValidConfig::new(self.candidate_config()) {
            Ok(_) => {
                self.refused = None;
                Confirm::Apply(self.field.setting(self.candidate))
            }
            Err(e) => {
                self.refused = Some(e);
                Confirm::Refused(e)
            }
        }
    }
}

// Every stepper set is ascending, or `up` and `down` would skip members.
const fn ascending(steps: &[i64]) -> bool {
    let mut i = 1;
    while i < steps.len() {
        if steps[i] <= steps[i - 1] {
            return false;
        }
        i += 1;
    }
    true
}
const _: () = assert!(ascending(&BANDWIDTH_STEPS));
const _: () = assert!(ascending(&SF_STEPS));
const _: () = assert!(ascending(&CR_STEPS));
const _: () = assert!(ascending(&POWER_STEPS));
// The power set has to reach past the module's rating on both bands, or the
// refusal path is unreachable from the panel; and it has to reach the bottom
// of every PA, so every legal power on either band is reachable.
const _: () = assert!(POWER_STEPS[0] <= pa::LP_MIN_DBM as i64);
const _: () = assert!(POWER_STEPS[0] <= pa::HF_MIN_DBM as i64);
const _: () = assert!(POWER_STEPS[POWER_STEPS.len() - 1] == pa::HP_MAX_DBM as i64);
const _: () = assert!(POWER_STEPS[POWER_STEPS.len() - 1] > pa::MODULE_MAX_SUB_GHZ_DBM as i64);
const _: () = assert!(POWER_STEPS[POWER_STEPS.len() - 1] > pa::MODULE_MAX_2G4_DBM as i64);
// Seven digits of kilohertz reach 9999.999 MHz, which covers both bands; the
// point sits after the megahertz.
const _: () = assert!(9_999_999_000 > pa::ISM_2G4_MAX_HZ as u64);
const _: () = assert!(FREQ_POINT < FREQ_DIGITS && FREQ_DIGITS - FREQ_POINT == 3);
// The bandwidth set holds every bandwidth of either band, so every legal
// value is reachable from the panel whichever band the frequency is on.
const fn holds_every(table: &[(u8, u32)]) -> bool {
    let mut i = 0;
    while i < table.len() {
        let mut found = false;
        let mut j = 0;
        while j < BANDWIDTH_STEPS.len() {
            if BANDWIDTH_STEPS[j] == table[i].1 as i64 {
                found = true;
            }
            j += 1;
        }
        if !found {
            return false;
        }
        i += 1;
    }
    true
}
const _: () = assert!(holds_every(&crate::lr1121::lora::BANDWIDTHS));
const _: () = assert!(holds_every(&crate::lr1121::lora::BANDWIDTHS_2G4));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::{CR_MAX, CR_MIN, DEFAULT, RNODE_SF_MAX, RNODE_SF_MIN};
    use crate::lr1121::lora;

    /// A configuration a host might have left: values that are not defaults.
    fn config() -> RadioConfig {
        RadioConfig {
            frequency_hz: 906_875_000,
            bandwidth_hz: 250_000,
            spreading_factor: 11,
            coding_rate: 5,
            tx_power_dbm: 17,
            ..DEFAULT
        }
    }

    /// A board on the other band: the bench configuration, which a host can
    /// ask the product for.
    fn high_frequency() -> RadioConfig {
        crate::lr1121::config::BENCH_2G4
    }

    /// An editor opens on the field's current value and nothing else.
    #[test]
    fn an_editor_opens_on_the_current_value() {
        for field in Field::ALL {
            let e = Editor::open(field, config());
            assert_eq!(e.candidate(), field.value_in(&config()), "{field:?}");
            assert!(!e.changed());
            assert_eq!(e.refused(), None);
            assert_eq!(e.original_config(), &config());
        }
        let f = Editor::open(Field::Frequency, config());
        assert_eq!(f.digits(), &[0, 9, 0, 6, 8, 7, 5], "906.875 MHz");
        assert_eq!(f.cursor(), 0);
        assert!(!f.is_stepper());
        assert!(Editor::open(Field::TxPower, config()).is_stepper());
        let hf = Editor::open(Field::Frequency, high_frequency());
        assert_eq!(hf.digits(), &[2, 4, 7, 8, 0, 0, 0], "2478.000 MHz");
        assert_eq!(hf.candidate(), 2_478_000_000);
        assert!(!hf.changed());
    }

    /// **Cancel leaves the previous value in place.** Stepping and then not
    /// confirming changes nothing the caller holds: the original is untouched
    /// and the editor has handed nothing back.
    #[test]
    fn cancelling_leaves_the_original_untouched() {
        for field in Field::ALL {
            let before = config();
            let mut e = Editor::open(field, before);
            e.up();
            e.right();
            assert!(e.changed(), "{field:?}");
            // No confirm. The configuration the editor was given is what it
            // still is, and the caller's own copy was never handed to it by
            // reference in the first place.
            assert_eq!(e.original_config(), &before, "{field:?}");
            assert_eq!(before, config(), "{field:?}");
        }
    }

    /// Confirming a legal value hands back exactly that setting.
    #[test]
    fn confirming_a_legal_value_applies_it() {
        let mut e = Editor::open(Field::TxPower, config());
        e.up(); // 17 -> 18
        assert_eq!(e.confirm(), Confirm::Apply(Setting::TxPower(18)));
        assert_eq!(e.refused(), None);

        let mut e = Editor::open(Field::Bandwidth, config());
        e.up(); // 250 -> 406.25 kHz, the other band's
        e.up(); // -> 500 kHz
        assert_eq!(e.confirm(), Confirm::Apply(Setting::Bandwidth(500_000)));

        let mut e = Editor::open(Field::SpreadingFactor, config());
        e.down(); // 11 -> 10
        assert_eq!(e.confirm(), Confirm::Apply(Setting::SpreadingFactor(10)));

        let mut e = Editor::open(Field::CodingRate, config());
        e.up();
        e.up();
        e.up();
        assert_eq!(e.confirm(), Confirm::Apply(Setting::CodingRate(8)));
    }

    /// **Refused, not clamped.** A value the radio cannot do stays in the
    /// editor with the reason, and is not rounded to one it can.
    #[test]
    fn a_refused_value_is_kept_and_named() {
        let mut e = Editor::open(Field::TxPower, config());
        for _ in 0..4 {
            e.up(); // 17 -> 21
        }
        assert_eq!(e.candidate(), 21);
        assert_eq!(
            e.confirm(),
            Confirm::Refused(ConfigError::PowerAboveModuleRating)
        );
        assert_eq!(e.refused(), Some(ConfigError::PowerAboveModuleRating));
        assert_eq!(e.candidate(), 21, "not clamped to 20");
        // The reason is the same one the host path would give.
        assert_eq!(
            ValidConfig::new(config().with(Setting::TxPower(21))),
            Err(ConfigError::PowerAboveModuleRating)
        );
        // Stepping again clears the refusal; confirming a legal value works.
        e.down();
        assert_eq!(e.refused(), None);
        assert_eq!(e.confirm(), Confirm::Apply(Setting::TxPower(20)));
    }

    /// Every field can reach a refusal from the panel. A field whose editor
    /// could only ever produce legal values would never have its refusal path
    /// looked at.
    #[test]
    fn every_field_can_be_refused() {
        let mut freq = Editor::open(Field::Frequency, config());
        freq.up(); // 0 -> 1: 1906.875 MHz
        assert_eq!(freq.candidate(), 1_906_875_000);
        assert!(matches!(
            freq.confirm(),
            Confirm::Refused(ConfigError::FrequencyOutOfBand)
        ));

        let mut bw = Editor::open(Field::Bandwidth, config());
        bw.down(); // 250 -> 203.125 kHz: the chip has it, on the other band
        assert_eq!(bw.candidate(), 203_125);
        assert!(matches!(
            bw.confirm(),
            Confirm::Refused(ConfigError::BandwidthNotInBand)
        ));
        for _ in 0..3 {
            bw.down(); // -> 125 -> 62.5 -> 41.7
        }
        assert_eq!(bw.candidate(), 41_700);
        assert!(matches!(
            bw.confirm(),
            Confirm::Refused(ConfigError::UnsupportedBandwidth)
        ));

        let mut power = Editor::open(Field::TxPower, config());
        for _ in 0..10 {
            power.up();
        }
        assert_eq!(power.candidate(), 22, "saturates at the top of the set");
        assert!(matches!(
            power.confirm(),
            Confirm::Refused(ConfigError::PowerAboveModuleRating)
        ));
        for _ in 0..50 {
            power.down();
        }
        assert_eq!(power.candidate(), -18, "saturates at the bottom of the set");
        assert!(
            matches!(
                power.confirm(),
                Confirm::Refused(ConfigError::PowerUnreachable)
            ),
            "no sub-GHz PA goes that low"
        );

        // The other band has its own rating, one decibel above the bench
        // configuration, and its own floor, which is the set's.
        let mut hf = Editor::open(Field::TxPower, high_frequency());
        hf.up(); // 11 -> 12
        assert!(matches!(
            hf.confirm(),
            Confirm::Refused(ConfigError::PowerAbove2G4Rating)
        ));
        for _ in 0..50 {
            hf.down();
        }
        assert_eq!(hf.candidate(), pa::HF_MIN_DBM as i64);
        assert_eq!(
            hf.confirm(),
            Confirm::Apply(Setting::TxPower(pa::HF_MIN_DBM)),
            "the high-frequency PA's floor is reachable"
        );

        // Spreading factor and coding rate walk the RNode range, all of which
        // this chip does; there is no illegal value a host could ask for.
        for sf in SF_STEPS {
            assert!(config()
                .with(Setting::SpreadingFactor(sf as u8))
                .check()
                .is_ok());
        }
        for cr in CR_STEPS {
            assert!(config().with(Setting::CodingRate(cr as u8)).check().is_ok());
        }
    }

    /// A stepper stops at both ends rather than wrapping. Wrapping from
    /// 22 dBm to -17 dBm on one more press is the kind of surprise that ends
    /// a field test.
    #[test]
    fn a_stepper_stops_at_both_ends() {
        let mut e = Editor::open(Field::SpreadingFactor, config());
        for _ in 0..20 {
            e.up();
        }
        assert_eq!(e.candidate(), RNODE_SF_MAX as i64);
        for _ in 0..20 {
            e.down();
        }
        assert_eq!(e.candidate(), RNODE_SF_MIN as i64);
        let mut e = Editor::open(Field::CodingRate, config());
        for _ in 0..20 {
            e.down();
        }
        assert_eq!(e.candidate(), CR_MIN as i64);
        for _ in 0..20 {
            e.up();
        }
        assert_eq!(e.candidate(), CR_MAX as i64);
    }

    /// A value a host set that is not in the set steps onto the set, in the
    /// direction pressed, rather than being stuck or skipping past.
    #[test]
    fn a_value_outside_the_set_steps_onto_it() {
        let odd = RadioConfig {
            bandwidth_hz: 100_000,
            ..config()
        };
        let mut up = Editor::open(Field::Bandwidth, odd);
        up.up();
        assert_eq!(up.candidate(), 125_000);
        let mut down = Editor::open(Field::Bandwidth, odd);
        down.down();
        assert_eq!(down.candidate(), 62_500);
    }

    /// The digit editor: the cursor moves, digits wrap, and the value
    /// follows, keeping the part below a kilohertz that it does not show.
    #[test]
    fn the_digit_editor_edits_one_digit_at_a_time() {
        let mut e = Editor::open(
            Field::Frequency,
            RadioConfig {
                frequency_hz: 915_000_500,
                ..config()
            },
        );
        assert_eq!(e.digits(), &[0, 9, 1, 5, 0, 0, 0]);
        for _ in 0..3 {
            e.right();
        }
        assert_eq!(e.cursor(), 3, "the units of megahertz");
        e.up(); // 915 -> 916
        assert_eq!(e.candidate(), 916_000_500, "the 500 Hz is kept");
        for _ in 0..10 {
            e.right();
        }
        assert_eq!(e.cursor(), FREQ_DIGITS - 1, "stops at the last digit");
        e.down(); // 0 -> 9
        assert_eq!(e.digits(), &[0, 9, 1, 6, 0, 0, 9]);
        assert_eq!(e.candidate(), 916_009_500);
        for _ in 0..10 {
            e.left();
        }
        assert_eq!(e.cursor(), 0, "stops at the first digit");
        e.right();
        e.down(); // 9 -> 8
        assert_eq!(e.candidate(), 816_009_500);
        assert!(matches!(
            e.confirm(),
            Confirm::Refused(ConfigError::FrequencyOutOfBand)
        ));
        e.up(); // back to 916
        assert_eq!(e.confirm(), Confirm::Apply(Setting::Frequency(916_009_500)));
    }

    /// **A 2.4 GHz frequency is edited where it is.** Four megahertz digits
    /// hold 2478, the cursor walks all of them, and a change of one digit
    /// confirms on that band -- nothing drags it down to the top of the
    /// sub-GHz band, and a frequency that leaves the band is refused there
    /// rather than moved.
    #[test]
    fn a_high_frequency_is_edited_in_place() {
        let mut e = Editor::open(Field::Frequency, high_frequency());
        assert_eq!(e.candidate(), 2_478_000_000);
        for _ in 0..3 {
            e.right();
        }
        e.up(); // 2478 -> 2479
        assert_eq!(e.candidate(), 2_479_000_000);
        assert_eq!(
            e.confirm(),
            Confirm::Apply(Setting::Frequency(2_479_000_000))
        );
        // The tens digit up takes it past the band's edge: 2483.5 MHz is
        // in, 2489 is not, and it stays 2489 rather than moving anywhere.
        e.left();
        e.up(); // 2479 -> 2489
        assert_eq!(
            e.confirm(),
            Confirm::Refused(ConfigError::FrequencyOutOfBand)
        );
        assert_eq!(e.candidate(), 2_489_000_000, "kept, not moved");
        e.down(); // back to 2479
        assert_eq!(
            e.confirm(),
            Confirm::Apply(Setting::Frequency(2_479_000_000))
        );

        // And the way down to the other band is through the same editor:
        // 0915.000 from 2478.000 is four digits, then the bandwidth is the
        // other band's until it follows.
        let mut down = Editor::open(Field::Frequency, high_frequency());
        down.down(); // 2 -> 1
        down.down(); // 1 -> 0
        down.right();
        for _ in 0..5 {
            down.up(); // 4 -> 9
        }
        down.right();
        for _ in 0..6 {
            down.down(); // 7 -> 1
        }
        down.right();
        for _ in 0..3 {
            down.down(); // 8 -> 5
        }
        assert_eq!(down.candidate(), 915_000_000);
        assert_eq!(
            down.confirm(),
            Confirm::Refused(ConfigError::BandwidthNotInBand),
            "812.5 kHz at 915 MHz"
        );
    }

    /// **A frequency past the field is refused, not wrapped.** Seven digits
    /// of kilohertz reach past what a `u32` of hertz holds, and a plain cast
    /// would turn 5200 MHz into 905 MHz -- a refused value confirming as a
    /// legal one. The setting saturates instead, to a value in neither band.
    #[test]
    fn a_frequency_past_the_field_is_refused_rather_than_wrapped() {
        let mut e = Editor::open(Field::Frequency, high_frequency());
        for _ in 0..3 {
            e.up(); // 2478 -> 5478 MHz
        }
        assert_eq!(e.candidate(), 5_478_000_000);
        assert!(e.candidate() > u32::MAX as i64);
        assert_eq!(
            e.confirm(),
            Confirm::Refused(ConfigError::FrequencyOutOfBand)
        );
        let hz = e.candidate_config().frequency_hz;
        assert_eq!(hz, u32::MAX);
        assert_eq!(pa::band_of(hz), None);
        // The value that would have wrapped into the sub-GHz band.
        assert_eq!(
            Field::Frequency.setting(5_200_000_000),
            Setting::Frequency(u32::MAX)
        );
        assert_eq!(
            Field::Frequency.setting(2_478_000_000),
            Setting::Frequency(2_478_000_000)
        );
    }

    /// Steppers ignore the cursor keys, so a sideways press while stepping
    /// does nothing at all.
    #[test]
    fn steppers_have_no_cursor() {
        let mut e = Editor::open(Field::TxPower, config());
        e.right();
        e.left();
        assert_eq!(e.cursor(), 0);
        assert!(!e.changed());
    }

    /// The bandwidth set holds every bandwidth the chip has on either band,
    /// so every legal value is reachable whichever band the frequency is on,
    /// and the refused ones on each band are exactly the rest.
    #[test]
    fn the_bandwidth_set_covers_both_bands_and_more() {
        for (_, hz) in lora::BANDWIDTHS.iter().chain(&lora::BANDWIDTHS_2G4) {
            assert!(BANDWIDTH_STEPS.contains(&(*hz as i64)), "{hz}");
        }
        let refused_on = |base: RadioConfig| {
            BANDWIDTH_STEPS
                .iter()
                .filter(|&&hz| base.with(Setting::Bandwidth(hz as u32)).check().is_err())
                .count()
        };
        assert_eq!(
            refused_on(config()),
            BANDWIDTH_STEPS.len() - lora::BANDWIDTHS.len()
        );
        assert_eq!(
            refused_on(high_frequency()),
            BANDWIDTH_STEPS.len() - lora::BANDWIDTHS_2G4.len()
        );
        // Stepping from the sub-GHz default up through the set reaches the
        // 2.4 GHz values, each refused as the other band's, not as unknown.
        let mut e = Editor::open(Field::Bandwidth, config());
        e.up(); // 250 -> 406.25
        assert_eq!(e.candidate(), 406_250);
        assert_eq!(
            e.confirm(),
            Confirm::Refused(ConfigError::BandwidthNotInBand)
        );
        // And on 2.4 GHz every one of its three is legal and every sub-GHz
        // one is the other band's.
        for (_, hz) in lora::BANDWIDTHS_2G4 {
            let mut e = Editor::open(Field::Bandwidth, high_frequency());
            while e.candidate() > hz as i64 {
                e.down();
            }
            while e.candidate() < hz as i64 {
                e.up();
            }
            assert_eq!(e.confirm(), Confirm::Apply(Setting::Bandwidth(hz)));
        }
    }

    /// Every field has a title and a label, and no two share one.
    #[test]
    fn every_field_is_named() {
        for (i, a) in Field::ALL.iter().enumerate() {
            assert!(!a.title().is_empty() && !a.label().is_empty() && !a.name().is_empty());
            for b in &Field::ALL[i + 1..] {
                assert_ne!(a.title(), b.title());
                assert_ne!(a.label(), b.label());
                assert_ne!(a.name(), b.name());
            }
        }
    }

    /// The candidate configuration is the original with one field changed,
    /// which is what confirmation validates.
    #[test]
    fn the_candidate_configuration_differs_in_one_field() {
        let mut e = Editor::open(Field::TxPower, config());
        e.up();
        let c = e.candidate_config();
        assert_eq!(c.tx_power_dbm, 18);
        assert_eq!(
            RadioConfig {
                tx_power_dbm: 17,
                ..c
            },
            config()
        );
    }

    /// Each lock has a headline that fits a content line.
    #[test]
    fn locks_have_short_headlines() {
        for lock in [Lock::Usb, Lock::Bluetooth] {
            assert!(lock.headline().len() <= crate::screens::LINE_CHARS);
        }
        assert_ne!(Lock::Usb.headline(), Lock::Bluetooth.headline());
    }
}
