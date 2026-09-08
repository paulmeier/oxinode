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
}
