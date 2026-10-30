//! The old 73 ppm "reference correction", kept as a bench offset and off by
//! default.
//!
//! # What was believed
//!
//! A commanded 915.000 MHz carrier was measured on an RTL-SDR at 914.934 MHz,
//! −73.3 ppm, and a chopping experiment (see `pa::MEASURED_TX_ERROR_PPM`) was
//! taken to show that the error was the transmitter's rather than the
//! receiver's. Both Base Duos agreed with each other, so the error was put down
//! to the nRFLR1121 module and cancelled here in arithmetic, on by default.
//!
//! # What was measured instead
//!
//! Against a SenseCAP P1 (SX1262 with a TCXO) the corrected oxinodes landed
//! **+67.03 kHz and +67.14 kHz** above 915.000 MHz, which is the correction and
//! nothing else: with it removed, the two modules sit at −0.04 and +0.08 ppm.
//! The P1 was itself checked against four independent Meshtastic nodes on the
//! public US mesh, all within about 0.5 ppm of it. The offset was read from
//! the SX1262's frequency-error estimate on clean packets (SNR +12 dB) at five
//! receiver settings 5 kHz apart, so its scale cancels and only the zero
//! crossing is used.
//!
//! # Why the SDR measurement could not tell
//!
//! An RTL-SDR derives both its tuner and its ADC sample clock from one crystal.
//! Holding the tuner fixed cancels the tuner's error, but the two carriers'
//! baseband frequencies are still read against the sample clock, and a sample
//! clock running fast by *p* shrinks their 1.6 MHz spacing by the same *p*. An
//! SDR 73 ppm fast therefore produces both the −117 Hz chop difference and the
//! −66 kHz absolute reading, exactly as a transmitter 73 ppm slow would. The
//! "receiver −1.4 ppm" figure is what is left of the absolute reading after
//! the chop result has been attributed to the transmitter, so it rests on the
//! assumption being tested. Agreement between the two boards only ever showed
//! that they matched each other.
//!
//! # What is left here
//!
//! The arithmetic, unchanged, so `RadioConfig::correct_reference` and the
//! `radio` image's `R` key can still reproduce the old behaviour for
//! comparison. Nothing turns it on by default, and it should not be turned on
//! to talk to any correctly tuned radio: it moves the carrier 67 kHz high.
//!
//! # Why parts per ten million
//!
//! The measurement has a tenth of a ppm of resolution and the correction is
//! not an integer number of ppm. Carrying it in tenths keeps the arithmetic in
//! integers: a float here would be a different number on the host than on the
//! target for no benefit, and this value is multiplied by a frequency near
//! 2³⁰, where the difference is tens of hertz.

/// What the RTL-SDR reported as this module's error, in tenths of a ppm.
///
/// **Not the module's error**: that is within ±0.5 ppm of zero (see the module
/// documentation). This is the number the old correction was built from, kept
/// so the bench offset reproduces exactly what the firmware used to command.
pub const MEASURED_ERROR_TENTH_PPM: i32 = -733;

/// How much to add to a commanded frequency, in tenths of a ppm.
///
/// The negation of the error, to first order. The exact correction is
/// `1/(1 − p) − 1` rather than `p`, and at 73 ppm those differ by 5 parts per
/// billion — 5 Hz at 915 MHz, against a 0.5 ppm measurement uncertainty of
/// 460 Hz. Using the first-order form is not an approximation that matters; it
/// is an approximation two orders of magnitude below the noise.
pub const CORRECTION_TENTH_PPM: i32 = -MEASURED_ERROR_TENTH_PPM;

/// Temperature coefficient of the reference, in hundredths of a ppm per °C.
///
/// Measured on the same RTL-SDR as the 73 ppm, so it carries the same doubt:
/// an SDR warming up drifts too. Recorded here because it bounds how good a
/// *static* correction can be. A TCXO is 0.5–2 ppm over its whole range; this
/// reads as that much every two or three degrees, which is either the module's
/// reference or the SDR's, and has not been separated.
pub const TEMPCO_HUNDREDTH_PPM_PER_C: i32 = 65;

/// Shift a frequency by a signed number of tenths of a ppm.
///
/// The rounding is toward zero, which for a 73 ppm correction at 915 MHz
/// discards half a hertz. Stated rather than hidden because the *interesting*
/// error here is 67 kHz and anything that rounds by more than a few hertz has
/// a different bug.
///
/// Saturating rather than wrapping at the ends. A frequency that would go
/// negative or exceed `u32` is not a frequency anything should be commanded to,
/// but silently wrapping 4 GHz to 0 Hz would be a radio quietly transmitting
/// somewhere else, and this arithmetic sits directly in front of the register
/// write.
pub const fn shift_tenth_ppm(hz: u32, tenth_ppm: i32) -> u32 {
    let delta = (hz as i64) * (tenth_ppm as i64) / 10_000_000;
    let shifted = hz as i64 + delta;
    if shifted < 0 {
        0
    } else if shifted > u32::MAX as i64 {
        u32::MAX
    } else {
        shifted as u32
    }
}

/// What to command in order to land on `hz`.
///
/// This is the function that goes in front of `SetRfFrequency`. Everything
/// above it — the config, the console, the RNode protocol — deals in the
/// frequency that is wanted; only this converts to the one the chip has to be
/// told.
pub const fn command_for(hz: u32) -> u32 {
    shift_tenth_ppm(hz, CORRECTION_TENTH_PPM)
}

/// Where a commanded frequency actually lands, uncorrected.
///
/// The inverse of the problem rather than of [`command_for`]: this is what the
/// hardware does, and it is here so that a prediction about the bench can be
/// written down and checked instead of estimated.
pub const fn lands_at(commanded_hz: u32) -> u32 {
    shift_tenth_ppm(commanded_hz, MEASURED_ERROR_TENTH_PPM)
}

/// How far the correction moves a frequency, in hertz. Signed, and it is the
/// number the sweep in step 5 has to see move.
pub const fn correction_hz(hz: u32) -> i32 {
    (command_for(hz) as i64 - hz as i64) as i32
}

// The correction and the error must be opposites; a sign error here would
// double the problem rather than remove it, and would do so silently.
const _: () = assert!(CORRECTION_TENTH_PPM == -MEASURED_ERROR_TENTH_PPM);
const _: () = assert!(
    CORRECTION_TENTH_PPM > 0,
    "the SDR read the transmitter low, so the old correction commands higher"
);
// Round-tripping is the property that matters: commanding `command_for(f)` on a
// chip that is `MEASURED_ERROR_TENTH_PPM` off must land back on `f`. Checked at
// compile time at the band edges and the middle, where the arithmetic is
// largest and the rounding worst.
const _: () = assert!(lands_at(command_for(902_000_000)).abs_diff(902_000_000) <= 100);
const _: () = assert!(lands_at(command_for(915_000_000)).abs_diff(915_000_000) <= 100);
const _: () = assert!(lands_at(command_for(928_000_000)).abs_diff(928_000_000) <= 100);
// And the correction is not a rounding detail. At 915 MHz it is tens of
// kilohertz -- over half a 125 kHz LoRa channel -- which is why turning it on
// by mistake takes a board off the air for every correctly tuned peer.
const _: () = assert!(correction_hz(915_000_000) > 60_000);

#[cfg(test)]
mod tests {
    use super::*;

    /// The number the SDR measured, in the units it was measured in. Pinned so
    /// that a change to the constant has to be a decision rather than a typo.
    #[test]
    fn the_correction_is_the_measured_error_negated() {
        assert_eq!(MEASURED_ERROR_TENTH_PPM, -733);
        assert_eq!(CORRECTION_TENTH_PPM, 733);
    }

    /// 915 MHz × 73.3 ppm = 67.07 kHz. Worked by hand so the implementation has
    /// something to disagree with.
    #[test]
    fn the_correction_at_the_band_centre_is_sixty_seven_kilohertz() {
        assert_eq!(command_for(915_000_000), 915_067_069);
        assert_eq!(correction_hz(915_000_000), 67_069);
    }

    /// A ppm correction is proportional, so it must be bigger at 928 MHz than
    /// at 902 MHz. If it were ever implemented as a fixed offset this is the
    /// test that would notice.
    #[test]
    fn the_correction_scales_with_frequency() {
        let low = correction_hz(902_000_000);
        let high = correction_hz(928_000_000);
        assert!(high > low, "{high} should exceed {low}");
        // 26 MHz of span at 73.3 ppm is about 1.9 kHz of difference.
        assert!((high - low).abs_diff(1_906) < 20, "{}", high - low);
    }

    /// The property the whole module is for: correct the command, and the
    /// hardware's own error puts it back where it was asked to be.
    #[test]
    fn correcting_a_frequency_lands_it_where_it_was_asked_for() {
        for hz in (902_000_000u32..=928_000_000).step_by(1_000_000) {
            let landed = lands_at(command_for(hz));
            assert!(
                landed.abs_diff(hz) <= 100,
                "{hz} -> commanded {} -> landed {landed}",
                command_for(hz)
            );
        }
    }

    /// Without the correction the same frequency is 67 kHz low — which is the
    /// negative control, and the reason the test above is not vacuous.
    #[test]
    fn an_uncorrected_frequency_is_off_by_the_measured_error() {
        let landed = lands_at(915_000_000);
        assert_eq!(landed, 914_932_931);
        assert!(915_000_000 - landed > 60_000);
    }

    /// Zero is the identity, and it is what the firmware uses when the
    /// correction is switched off. It must not shift anything at all.
    #[test]
    fn a_zero_shift_changes_nothing() {
        for hz in [0u32, 1, 902_000_000, 915_000_000, u32::MAX] {
            assert_eq!(shift_tenth_ppm(hz, 0), hz);
        }
    }

    /// Saturating rather than wrapping. Neither of these is a frequency
    /// anything would command, but the arithmetic sits in front of a register
    /// write and a wrap would put a carrier somewhere nobody chose.
    #[test]
    fn the_ends_saturate_rather_than_wrapping() {
        assert_eq!(shift_tenth_ppm(u32::MAX, 10_000_000), u32::MAX);
        assert_eq!(shift_tenth_ppm(1_000, -100_000_000), 0);
        // A shift big enough to invert a real frequency clamps at zero rather
        // than becoming an enormous one.
        assert_eq!(shift_tenth_ppm(915_000_000, -20_000_000), 0);
    }

    /// The drift bounds what a static correction can achieve, so the units it
    /// is recorded in have to survive. 0.65 ppm/°C over the 20 °C between a
    /// bench and a rooftop is 13 ppm — a fifth of the error being corrected,
    /// and not nothing.
    #[test]
    fn the_temperature_coefficient_is_recorded_in_hundredths() {
        assert_eq!(TEMPCO_HUNDREDTH_PPM_PER_C, 65);
        let drift_over_20c = TEMPCO_HUNDREDTH_PPM_PER_C * 20 / 100;
        assert_eq!(drift_over_20c, 13);
        assert!(drift_over_20c * 10 < CORRECTION_TENTH_PPM);
    }
}
