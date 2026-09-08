//! Editing one radio parameter from the panel: the value model.
//!
//! Phase 12. The shell stopped at two levels on purpose and left room for one
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
//! legal value. That is phase 4's rule, and the panel is not an exception to
//! it: a person who asked for 21 dBm and was given 20 without being told has
//! no way to find out.
//!
//! The sets a stepper walks are therefore deliberately wider than what the
//! radio accepts. Power runs to 22 dBm because that is what an RNode host can
//! ask for and the refusal has to be reachable to be honest; bandwidth lists
//! the ten LoRa bandwidths a host offers, six of which this chip does not have.
//! A stepper that only ever produced legal values would never show the refusal
//! path, and the first time it mattered would be a value nobody had tested.
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
    pub const fn value_in(self, config: &RadioConfig) -> i32 {
        match self {
            Field::Frequency => config.frequency_hz as i32,
            Field::Bandwidth => config.bandwidth_hz as i32,
            Field::SpreadingFactor => config.spreading_factor as i32,
            Field::CodingRate => config.coding_rate as i32,
            Field::TxPower => config.tx_power_dbm as i32,
        }
    }

    /// A value of this field as a [`Setting`].
    ///
    /// The narrowing casts are safe by construction: a stepper only produces
    /// members of its set, and the digit editor only produces nine digits of
    /// hertz, which is under 2^30.
    pub const fn setting(self, value: i32) -> Setting {
        match self {
            Field::Frequency => Setting::Frequency(value as u32),
            Field::Bandwidth => Setting::Bandwidth(value as u32),
            Field::SpreadingFactor => Setting::SpreadingFactor(value as u8),
            Field::CodingRate => Setting::CodingRate(value as u8),
            Field::TxPower => Setting::TxPower(value as i8),
        }
    }

    /// The fixed set a stepper walks, ascending; empty for the digit editor.
    pub const fn steps(self) -> &'static [i32] {
        match self {
            Field::Frequency => &[],
            Field::Bandwidth => &BANDWIDTH_STEPS,
            Field::SpreadingFactor => &SF_STEPS,
            Field::CodingRate => &CR_STEPS,
            Field::TxPower => &POWER_STEPS,
        }
    }
}

/// The bandwidths a LoRa host offers, in hertz. Six of them this chip cannot
/// do, and they are here so the refusal is reachable -- see the module docs.
pub const BANDWIDTH_STEPS: [i32; 10] = [
    7_800, 10_400, 15_600, 20_800, 31_250, 41_700, 62_500, 125_000, 250_000, 500_000,
];
/// The spreading factors an RNode host can ask for.
pub const SF_STEPS: [i32; 6] = [7, 8, 9, 10, 11, 12];
/// The coding rates, as the denominator of 4/n.
pub const CR_STEPS: [i32; 4] = [5, 6, 7, 8];
/// Powers from the bottom of the low-power PA to the top of what a host can
/// ask for, one decibel apart. The last two are above the module's rating and
/// are refused when confirmed.
pub const POWER_STEPS: [i32; 40] = {
    let mut steps = [0i32; 40];
    let mut i = 0;
    while i < steps.len() {
        steps[i] = pa::LP_MIN_DBM as i32 + i as i32;
        i += 1;
    }
    steps
};

/// How many digits the frequency editor shows: `MMM.kkk`, megahertz to the
/// kilohertz. Anything under a kilohertz is kept from the original value and
/// not shown, because no channel plan is drawn in hertz.
pub const FREQ_DIGITS: usize = 6;

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
    candidate: i32,
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
            // Kilohertz, six digits, most significant first.
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
    pub const fn candidate(&self) -> i32 {
        self.candidate
    }

    /// The value the field had when the editor opened.
    pub const fn original(&self) -> i32 {
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

    /// The frequency editor's digits, `MMM.kkk` without the point.
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
    fn frequency_from_digits(&self) -> i32 {
        let mut khz: u32 = 0;
        for &d in &self.digits {
            khz = khz * 10 + d as u32;
        }
        (khz * 1_000 + self.original.frequency_hz % 1_000) as i32
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
const fn ascending(steps: &[i32]) -> bool {
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
// The power set has to reach past the module's rating, or the refusal path is
// unreachable from the panel; and it has to start where the measured PA does.
const _: () = assert!(POWER_STEPS[0] == pa::LP_MIN_DBM as i32);
const _: () = assert!(POWER_STEPS[POWER_STEPS.len() - 1] == pa::HP_MAX_DBM as i32);
const _: () = assert!(POWER_STEPS[POWER_STEPS.len() - 1] > pa::MODULE_MAX_SUB_GHZ_DBM as i32);
// Six digits of kilohertz reach 999.999 MHz, which covers the band.
const _: () = assert!(999_999_000 > pa::US915_MAX_HZ);

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
        assert_eq!(f.digits(), &[9, 0, 6, 8, 7, 5], "906.875 MHz");
        assert_eq!(f.cursor(), 0);
        assert!(!f.is_stepper());
        assert!(Editor::open(Field::TxPower, config()).is_stepper());
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
        e.up(); // 250 -> 500 kHz
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
        freq.up(); // 9 -> 0: 006.875 MHz
        assert!(matches!(
            freq.confirm(),
            Confirm::Refused(ConfigError::FrequencyOutOfBand)
        ));

        let mut bw = Editor::open(Field::Bandwidth, config());
        for _ in 0..3 {
            bw.down(); // 250 -> 125 -> 62.5 -> 41.7
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
        assert_eq!(e.candidate(), RNODE_SF_MAX as i32);
        for _ in 0..20 {
            e.down();
        }
        assert_eq!(e.candidate(), RNODE_SF_MIN as i32);
        let mut e = Editor::open(Field::CodingRate, config());
        for _ in 0..20 {
            e.down();
        }
        assert_eq!(e.candidate(), CR_MIN as i32);
        for _ in 0..20 {
            e.up();
        }
        assert_eq!(e.candidate(), CR_MAX as i32);
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
        assert_eq!(e.digits(), &[9, 1, 5, 0, 0, 0]);
        e.right();
        e.right();
        assert_eq!(e.cursor(), 2);
        e.up(); // 915 -> 916
        assert_eq!(e.candidate(), 916_000_500, "the 500 Hz is kept");
        for _ in 0..10 {
            e.right();
        }
        assert_eq!(e.cursor(), FREQ_DIGITS - 1, "stops at the last digit");
        e.down(); // 0 -> 9
        assert_eq!(e.digits(), &[9, 1, 6, 0, 0, 9]);
        assert_eq!(e.candidate(), 916_009_500);
        for _ in 0..10 {
            e.left();
        }
        assert_eq!(e.cursor(), 0, "stops at the first digit");
        e.down(); // 9 -> 8
        assert_eq!(e.candidate(), 816_009_500);
        assert!(matches!(
            e.confirm(),
            Confirm::Refused(ConfigError::FrequencyOutOfBand)
        ));
        e.up(); // back to 916
        assert_eq!(e.confirm(), Confirm::Apply(Setting::Frequency(916_009_500)));
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

    /// The bandwidth set holds every bandwidth the chip has, so every legal
    /// value is reachable, and the refused ones are the ones the chip lacks.
    #[test]
    fn the_bandwidth_set_covers_the_chip_and_more() {
        for (_, hz) in lora::BANDWIDTHS {
            assert!(BANDWIDTH_STEPS.contains(&(hz as i32)), "{hz}");
        }
        let refused = BANDWIDTH_STEPS
            .iter()
            .filter(|&&hz| {
                config()
                    .with(Setting::Bandwidth(hz as u32))
                    .check()
                    .is_err()
            })
            .count();
        assert_eq!(refused, BANDWIDTH_STEPS.len() - lora::BANDWIDTHS.len());
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
