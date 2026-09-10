//! The battery, from a number the ADC produced.
//!
//! The Base Duo brings its cell to P0.31 through an 806 kΩ / 1.5 MΩ divider,
//! so the pin sees 0.65048 of the battery -- 4.2 V arrives as 2.73 V, inside
//! the 3.6 V the ADC can measure at its lowest gain. This module turns the
//! ADC's count back into the cell's voltage, and the voltage into a percentage
//! a person can act on. Nothing here touches the ADC; the firmware reads it and
//! hands the count over.
//!
//! # What a percentage means
//!
//! A lithium cell's voltage is not linear in its charge, so a percentage that
//! is `(v - 3.0) / 1.2` is wrong by twenty points through the middle of the
//! curve. [`OCV_MV`] is the open-circuit table the board's Meshtastic variant
//! carries for this exact cell chemistry, eleven points from full to empty, and
//! the percentage is a straight line between the two points a reading falls
//! between. It is still only an estimate -- a cell under load reads lower than
//! its state of charge, and one on the charger reads higher -- which is why the
//! screen shows the voltage beside it rather than the percentage alone.
//!
//! # When there is no battery
//!
//! With no cell fitted the divider is pulled to nothing and the pin reads near
//! zero. That is not a flat battery, and [`reading`] says so by returning
//! `None` rather than `0%`: the screen then shows a dash, which is the truth.
//!
//! # Why the screen does not follow every sample
//!
//! One 12-bit count is 1.35 mV at the cell, and the count wanders by a few
//! either way between one conversion and the next. Near the top of the table
//! a percentage point is four millivolts wide, so a reading shown straight
//! from the ADC flickers between two numbers twice a second for as long as
//! anyone watches it. [`Smoothed`] is the cure: an average that follows the
//! cell over a few seconds, and a dead band so the number on the screen moves
//! only when the cell has.
//!
//! # The charger
//!
//! The BQ25185 says what it is doing on two open-drain status pins, and it
//! takes both to tell charging from a fault: one pin low is "charging", the
//! other low is "fault", both low is a fault the chip has latched off on,
//! both high is done or disabled. That table is defined with an input
//! present. With nothing on the connector the chip is in battery-only mode,
//! which the table does not cover, and on this board it holds the "charging"
//! pin low there -- so read on their own the pins say a board on its cell is
//! charging. The USB power sense settles it: no input, no charger, whatever
//! the pins say. [`Charger::decode`] is that table, in one place, on the
//! host.

/// The divider's upper resistor, battery side, in ohms.
pub const DIVIDER_TOP_OHMS: u32 = 806_000;
/// The divider's lower resistor, ground side, in ohms.
pub const DIVIDER_BOTTOM_OHMS: u32 = 1_500_000;

/// What the ADC reads at full scale, in millivolts.
///
/// Gain 1/6 against the 0.6 V internal reference: the peripheral's own
/// defaults for a single-ended channel, and the only setting that reaches
/// 3.6 V. The firmware configures exactly this, and the number is pinned here
/// so a change to either side is a change to both.
pub const FULL_SCALE_MV: u32 = 3_600;
/// Counts at full scale: 12-bit resolution.
pub const FULL_SCALE_COUNTS: u32 = 4_096;

/// Below this the pin is not reading a cell. A lithium cell that low is one a
/// protection circuit has already disconnected, and the divider with nothing
/// behind it reads near zero.
pub const NO_CELL_BELOW_MV: u32 = 2_500;

/// Open-circuit voltage from full to empty, in millivolts, ten percent apart.
///
/// `OCV_MV[0]` is 100 %, `OCV_MV[10]` is 0 %. From the board's Meshtastic
/// variant (`OCV_ARRAY`), which is the closest thing to a datasheet the cell
/// has.
pub const OCV_MV: [u16; 11] = [
    4050, 4010, 3990, 3930, 3870, 3820, 3740, 3630, 3550, 3450, 3100,
];

/// One measurement of the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The cell's voltage.
    pub millivolts: u32,
    /// The estimate, 0 to 100.
    pub percent: u8,
}

/// The voltage at the pin, from the ADC's count.
///
/// A negative count -- which a single-ended channel can produce from noise
/// around zero -- is zero volts, not a wrap.
pub const fn pin_millivolts(count: i16) -> u32 {
    let count = if count < 0 { 0 } else { count as u32 };
    count * FULL_SCALE_MV / FULL_SCALE_COUNTS
}

/// The cell's voltage, from the ADC's count: the pin reading with the divider
/// undone.
pub const fn cell_millivolts(count: i16) -> u32 {
    // Multiply before dividing, to keep the millivolt.
    let pin = pin_millivolts(count) as u64;
    (pin * (DIVIDER_TOP_OHMS + DIVIDER_BOTTOM_OHMS) as u64 / DIVIDER_BOTTOM_OHMS as u64) as u32
}

/// The state of charge, from the cell's voltage, by the table.
///
/// Clamped to 0 and 100 outside it: a cell above 4.05 V is full, and one below
/// 3.1 V is empty, whatever else it is.
pub fn percent(millivolts: u32) -> u8 {
    let mv = millivolts as i64;
    if mv >= OCV_MV[0] as i64 {
        return 100;
    }
    // Walk the table from full to empty; each step is ten percent.
    for (i, pair) in OCV_MV.windows(2).enumerate() {
        let (high, low) = (pair[0] as i64, pair[1] as i64);
        if mv >= low {
            let above = 100 - 10 * i as i64;
            // Straight line between the two points, rounded to nearest.
            let span = high - low;
            let fraction = (mv - low) * 10 + span / 2;
            return (above - 10 + fraction / span) as u8;
        }
    }
    0
}

/// The cell, from the ADC's count -- or nothing, if no cell is there.
pub fn reading(count: i16) -> Option<Reading> {
    let millivolts = cell_millivolts(count);
    if millivolts < NO_CELL_BELOW_MV {
        return None;
    }
    Some(Reading {
        millivolts,
        percent: percent(millivolts),
    })
}

/// A reading the screen can be shown: averaged, and held until the cell has
/// really moved.
///
/// Feed it every sample; read back what to display. The average is a
/// first-order filter with a quarter of the gap closed per sample, which at
/// two samples a second follows a change in about three seconds -- quick
/// enough that plugging the charger in shows within a breath, slow enough that
/// the conversion noise is gone. The displayed value then moves only when the
/// average is [`Smoothed::DEAD_BAND_MV`] away from it, and the percentage is
/// taken from the displayed voltage, so the two never disagree and neither
/// flickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Smoothed {
    /// The running average, in sixteenths of a millivolt so the filter
    /// cannot get stuck short of its target by integer truncation.
    average: Option<i32>,
    /// What the screen was last told.
    shown: Option<Reading>,
}

impl Smoothed {
    /// How far the average has to drift from the shown value before the
    /// shown value follows it. Ten millivolts is one step of the hundredths
    /// digit on the screen, so a value that is shown is the value that is
    /// there, to the digit.
    pub const DEAD_BAND_MV: u32 = 10;
    const SCALE: i32 = 16;

    pub const fn new() -> Self {
        Self {
            average: None,
            shown: None,
        }
    }

    /// Take one sample and return what to display.
    ///
    /// No cell resets the filter: the next cell to appear is shown at once,
    /// not averaged in from zero.
    pub fn update(&mut self, sample: Option<Reading>) -> Option<Reading> {
        let Some(fresh) = sample else {
            *self = Self::new();
            return None;
        };
        let target = fresh.millivolts as i32 * Self::SCALE;
        let average = match self.average {
            None => target,
            Some(avg) => avg + (target - avg) / 4,
        };
        self.average = Some(average);
        let millivolts = (average / Self::SCALE) as u32;
        let moved = self
            .shown
            .is_none_or(|shown| shown.millivolts.abs_diff(millivolts) >= Self::DEAD_BAND_MV);
        if moved {
            self.shown = Some(Reading {
                millivolts,
                percent: percent(millivolts),
            });
        }
        self.shown
    }

    /// The last value shown, without taking a sample.
    pub const fn shown(&self) -> Option<Reading> {
        self.shown
    }
}

/// What the charger is doing, from its two status pins and the USB power
/// sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Charger {
    /// No input power: the board is on its cell, and the status pins mean
    /// nothing.
    #[default]
    Unplugged,
    /// Charging, including the automatic top-up after a full cell has
    /// drooped.
    Charging,
    /// Input power present and the charger idle: the cell is full, or the
    /// charger has stopped for a reason it does not report as a fault --
    /// an input too weak to charge from, or charging disabled.
    Full,
    /// A fault the chip will recover from on its own: input over-voltage,
    /// the cell too hot or too cold, the chip too hot, a system short.
    Fault,
    /// A fault the chip has latched off on until the input is cycled: the
    /// safety timer, a cell over-current, a shorted current-set pin.
    LatchedOff,
}

impl Charger {
    /// The BQ25185's status table, for the two open-drain pins read with
    /// pull-ups: `stat1_low` and `stat2_low` are the pins pulled down by
    /// the chip, `input` is whether there is power on USB.
    ///
    /// The table applies only with an input. Without one the chip is in
    /// battery-only mode, the table says nothing, and on the bench the
    /// board holds `STAT2` low there -- the "charging" row. So no input is
    /// [`Unplugged`](Self::Unplugged) before the pins are looked at.
    ///
    /// | STAT1 | STAT2 | meaning, with input |
    /// |---|---|---|
    /// | high | high | done, sleeping or disabled: [`Full`](Self::Full) |
    /// | high | low | [`Charging`](Self::Charging) |
    /// | low | high | [`Fault`](Self::Fault), recoverable |
    /// | low | low | [`LatchedOff`](Self::LatchedOff) |
    pub const fn decode(stat1_low: bool, stat2_low: bool, input: bool) -> Self {
        if !input {
            return Charger::Unplugged;
        }
        match (stat1_low, stat2_low) {
            (false, false) => Charger::Full,
            (false, true) => Charger::Charging,
            (true, false) => Charger::Fault,
            (true, true) => Charger::LatchedOff,
        }
    }

    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Charger::Unplugged => "unplugged",
            Charger::Charging => "charging",
            Charger::Full => "full",
            Charger::Fault => "fault",
            Charger::LatchedOff => "latched off",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The divider is what the README says it is.
    #[test]
    fn the_divider_ratio_is_the_documented_one() {
        // 0.65048 in the README; 1.537 as Meshtastic's ADC_MULTIPLIER.
        let ratio = DIVIDER_BOTTOM_OHMS as f64 / (DIVIDER_TOP_OHMS + DIVIDER_BOTTOM_OHMS) as f64;
        assert!((ratio - 0.65048).abs() < 1e-5, "{ratio}");
        assert!((1.0 / ratio - 1.537).abs() < 1e-3);
    }

    /// Full scale is full scale, and zero is zero.
    #[test]
    fn the_pin_voltage_spans_the_adc() {
        assert_eq!(pin_millivolts(0), 0);
        assert_eq!(pin_millivolts(-5), 0, "noise below zero is zero");
        assert_eq!(pin_millivolts(4095), 3599);
        assert_eq!(pin_millivolts(2048), 1800);
    }

    /// A full cell at the pin comes back as a full cell.
    #[test]
    fn the_divider_is_undone() {
        // 4.20 V × 0.65048 = 2.732 V at the pin = 3108 counts.
        let count = (2_732 * FULL_SCALE_COUNTS / FULL_SCALE_MV) as i16;
        let mv = cell_millivolts(count);
        assert!((4_190..=4_210).contains(&mv), "{mv}");
    }

    /// The table's own points come back as their own percentages.
    #[test]
    fn the_table_points_are_exact() {
        for (i, &mv) in OCV_MV.iter().enumerate() {
            assert_eq!(percent(mv as u32), 100 - 10 * i as u8, "{mv} mV");
        }
    }

    /// Between points the line is straight, and it never goes backwards.
    #[test]
    fn the_percentage_is_monotonic_and_interpolated() {
        let mut last = 0;
        for mv in 3_000..4_300 {
            let p = percent(mv);
            assert!(p >= last, "{mv} mV: {p}% after {last}%");
            last = p;
        }
        // Halfway between 3870 (60 %) and 3820 (50 %).
        assert_eq!(percent(3_845), 55);
        // Above the top and below the bottom are clamped, not extrapolated.
        assert_eq!(percent(4_300), 100);
        assert_eq!(percent(2_000), 0);
    }

    /// No cell is not an empty cell.
    #[test]
    fn no_cell_is_reported_as_none_rather_than_as_empty() {
        assert_eq!(reading(0), None);
        assert_eq!(reading(100), None, "a divider with nothing behind it");
        let full = reading(3_100).expect("a cell");
        assert!(full.percent >= 95, "{full:?}");
        let low = reading(2_300).expect("a low cell is still a cell");
        assert!(low.millivolts >= NO_CELL_BELOW_MV);
        assert!(low.percent < 20, "{low:?}");
    }

    /// A reading that wanders by a count or two is shown as one number.
    #[test]
    fn a_noisy_cell_is_shown_as_one_steady_number() {
        let mut smooth = Smoothed::new();
        let mut seen = std::collections::BTreeSet::new();
        // 3.85 V at the pin, with the conversion wandering by two counts
        // either way: 2 counts is about 2.7 mV at the cell.
        for i in 0..200 {
            let count = 2_849 + [0, 2, -1, 1, -2][i % 5];
            let shown = smooth.update(reading(count)).expect("a cell");
            seen.insert((shown.millivolts, shown.percent));
        }
        // The first sample is shown as it is; after that the average sits in
        // the noise and the dead band holds the display still.
        assert!(seen.len() <= 2, "the display wandered: {seen:?}");
    }

    /// A cell that really moves is followed, and the percentage with it.
    #[test]
    fn a_real_change_is_followed_within_a_few_samples() {
        let mut smooth = Smoothed::new();
        let full = (4_020.0 * 0.65048f64 * FULL_SCALE_COUNTS as f64 / FULL_SCALE_MV as f64) as i16;
        let low = (3_700.0 * 0.65048f64 * FULL_SCALE_COUNTS as f64 / FULL_SCALE_MV as f64) as i16;
        let first = smooth.update(reading(full)).unwrap();
        assert!(first.percent >= 90, "{first:?}");
        let mut shown = first;
        // A quarter of the gap per sample: twenty samples, ten seconds on
        // the board, closes a 320 mV step to a millivolt.
        for _ in 0..20 {
            shown = smooth.update(reading(low)).unwrap();
        }
        assert!(shown.millivolts.abs_diff(3_700) < 15, "{shown:?}");
        assert!(shown.percent < 50, "{shown:?}");
        assert_eq!(shown.percent, percent(shown.millivolts), "shown as a pair");
    }

    /// No cell clears the filter, and a cell that appears is shown at once.
    #[test]
    fn no_cell_resets_the_filter() {
        let mut smooth = Smoothed::new();
        smooth.update(reading(3_100));
        assert_eq!(smooth.update(None), None);
        assert_eq!(smooth.shown(), None);
        let back = smooth.update(reading(2_300)).expect("a cell again");
        assert!(
            back.percent < 20,
            "averaged in from the old full cell: {back:?}"
        );
    }

    /// The charger's table, all eight rows. The four without an input are
    /// one answer: a board on its cell showed the "charging" row on the
    /// bench, which is what the table means by not covering that mode.
    #[test]
    fn the_charger_table_is_decoded() {
        use Charger::*;
        assert_eq!(decode(false, false, true), Full);
        assert_eq!(decode(false, true, true), Charging);
        assert_eq!(decode(true, false, true), Fault);
        assert_eq!(decode(true, true, true), LatchedOff);
        for stat1_low in [false, true] {
            for stat2_low in [false, true] {
                assert_eq!(decode(stat1_low, stat2_low, false), Unplugged);
            }
        }
        fn decode(a: bool, b: bool, c: bool) -> Charger {
            Charger::decode(a, b, c)
        }
    }
}
