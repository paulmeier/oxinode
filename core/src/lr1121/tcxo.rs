//! TCXO startup delay, and what a temperature reading has to look like to be
//! believed.
//!
//! The Base Duo drives a 3.0 V TCXO from the LR1121's own DIO3. Two
//! consequences run through the rest of the radio bring-up:
//!
//! * **DIO3 is not available as an interrupt line**, which is why the board
//!   jumpers DIO9 out to the MCU on P1.08 instead.
//! * **Nothing that needs the 32 MHz oscillator works until `SetTcxoMode` has
//!   run.** That includes `GetTemp`, which is otherwise the obvious smoke test
//!   and is in fact a test of this step.
//!
//! The delay field is the part worth getting right, and the part worth testing:
//! it is expressed in 30.52 µs steps, so every value in the datasheet, in this
//! repository and in a log is in different units from at least one of the
//! others.

/// One unit of the `SetTcxoMode` delay field, in nanoseconds.
///
/// 30.52 µs. Nanoseconds rather than microseconds because 30.52 is not an
/// integer number of microseconds, and rounding the *step* before rounding the
/// *count* loses more than it looks: at 164 steps the difference is over
/// 80 µs.
pub const STEP_NS: u32 = 30_520;

/// `tune` code for a 3.0 V TCXO supply.
///
/// **Chosen as a nominal value, not from the board.** Nobody has established
/// what the Base Duo's TCXO actually wants, and the transmitter is 73 ppm low —
/// far outside what a TCXO should manage, which points at it being driven wrong
/// rather than at a bad part. See [`TUNE_CODES`].
pub const TUNE_3V0: u8 = 0x06;

/// Every supply voltage `SetTcxoMode` can select, in the order the chip codes
/// them, with the voltage in millivolts.
///
/// Kept because the sweep is worth repeating on another board, and because the
/// result was not the expected one.
///
/// **Swept, and the voltage makes no difference.** The oscillator starts at
/// every code from 1.6 V to 3.3 V, and the transmit frequency across all eight
/// varied by 902 Hz — every hertz of which was warm-up drift, not voltage: a
/// control run holding 3.0 V for all eight measurements produced an 811 Hz
/// spread with the same shape, correlating with the sweep at +0.992. The 1.6 V
/// point, measured last, sits at the *top* of the trend rather than back down
/// beside 1.7 V.
///
/// An oscillator indifferent to its own supply is not being fed by it, so DIO3
/// is not powering this one. `SetTcxoMode` is still required — without it the
/// oscillator does not start at all and `hf_xosc_start` latches — so what the
/// command achieves here is telling the chip to expect an external clock rather
/// than drive a crystal.
pub const TUNE_CODES: [(u8, u16); 8] = [
    (0x00, 1600),
    (0x01, 1700),
    (0x02, 1800),
    (0x03, 2200),
    (0x04, 2400),
    (0x05, 2700),
    (0x06, 3000),
    (0x07, 3300),
];

/// The supply voltage a `tune` code selects, in millivolts.
pub const fn tune_millivolts(code: u8) -> Option<u16> {
    let mut i = 0;
    while i < TUNE_CODES.len() {
        if TUNE_CODES[i].0 == code {
            return Some(TUNE_CODES[i].1);
        }
        i += 1;
    }
    None
}

/// How long the LR1121 is given for its 32 MHz oscillator to start.
///
/// 5 ms, which is what `lr11xx` itself uses in `init_lora` and what the
/// reference implementations settle on.
///
/// **Deliberately not more, and this was measured rather than assumed.** The
/// datasheet wording — "maximum duration for the 32 MHz oscillator to start and
/// stabilize" — reads like a timeout that ends early once the oscillator is
/// detected. It does not behave like one. Raising this constant to 20 ms on the
/// bench board moved the time taken by `Calibrate` from 43548 µs to 60058 µs:
/// +16.5 ms of work for +15.0 ms of programmed delay, near enough 1:1.
/// `SetTcxoMode` itself took ~220 µs either way, so the wait is not paid by
/// that command — it is paid by the first operation that actually needs the
/// oscillator.
///
/// So this is a real cost on XOSC startup, not a safety margin that is free
/// when unused. The margin belongs on the *evidence* instead: `hf_xosc_start`
/// either clears or it does not, and on this board at 5 ms it clears.
pub const STARTUP_US: u32 = 5_000;

/// [`STARTUP_US`] in the units the chip actually wants.
pub const STARTUP_STEPS: u32 = steps_for_us(STARTUP_US);

/// Convert microseconds to `SetTcxoMode` delay steps, **rounding up**.
///
/// Up, always. A delay one step short of what the oscillator needs produces
/// `HF_XOSC_START_ERR` and a radio that does not work; a delay one step long
/// costs 30 µs. There is no version of this where rounding down is the safer
/// mistake.
pub const fn steps_for_us(us: u32) -> u32 {
    ((us as u64 * 1_000).div_ceil(STEP_NS as u64)) as u32
}

/// Convert `SetTcxoMode` delay steps back to microseconds, for a log.
///
/// Truncates, so the answer is never larger than the delay actually programmed.
pub const fn us_for_steps(steps: u32) -> u32 {
    ((steps as u64 * STEP_NS as u64) / 1_000) as u32
}

/// The delay field is 24 bits wide.
pub const MAX_STEPS: u32 = (1 << 24) - 1;

const _: () = assert!(
    STARTUP_STEPS <= MAX_STEPS,
    "the SetTcxoMode delay field is 24 bits; this value would be truncated"
);
const _: () = assert!(
    STARTUP_STEPS > 0,
    "a zero delay disables TCXO mode rather than configuring it"
);

/// How far the reference drifts with die temperature, in ppm per °C.
///
/// **Measured, roughly, and the roughness is the point.** Eight identical
/// carrier bursts at a fixed TCXO supply, over 25 s of warm-up: the die went
/// from 18.85 °C to 20.01 °C and the carrier moved +787 Hz at 913.7 MHz. That
/// is ≈0.65 ppm/°C, from 1.2 °C of range read by a sensor quantised at
/// 0.39 °C — an order of magnitude, not a specification.
///
/// The order of magnitude is enough. A TCXO holds ±0.5 to ±2 ppm across its
/// *entire* rated temperature range, call it 0.03 ppm/°C. This part is roughly
/// twenty times worse, which is uncompensated-crystal behaviour.
///
/// The practical consequence: the 73 ppm the transmitter is out by cannot be
/// dismissed as a one-off trim. A static correction would fix the bulk of it,
/// but the residual moves with temperature, so anything relying on it needs to
/// know that.
pub const MEASURED_TEMPCO_PPM_PER_C: f32 = 0.65;

// The whole reason this constant is recorded: it is far outside TCXO territory.
// If it ever drops to something a TCXO could manage, the measurement behind it
// has been replaced by an assumption.
const _: () = assert!(
    MEASURED_TEMPCO_PPM_PER_C > 0.1,
    "a reference this stable would be a TCXO, and the bench board's is not"
);

/// Coldest die temperature worth believing from a board on a bench.
pub const PLAUSIBLE_MIN_C: f32 = -20.0;
/// Warmest die temperature worth believing from a board on a bench.
///
/// Generous against room temperature on purpose: the sensor reads the die, not
/// the air, and a chip that has been transmitting runs well above ambient.
pub const PLAUSIBLE_MAX_C: f32 = 70.0;

const _: () = assert!(
    PLAUSIBLE_MIN_C < PLAUSIBLE_MAX_C,
    "an inverted range accepts nothing, so every reading would be reported as \
     implausible and step 4 could never pass"
);

/// Whether a `GetTemp` reading is worth believing.
///
/// This is the actual acceptance test for step 4 and it is not a formality.
/// `GetTemp` runs off the 32 MHz oscillator, so before `SetTcxoMode` it does
/// not fail — it returns a *number*, computed from whatever the ADC made of a
/// clock that is not running. Wildly out-of-range readings are the signature of
/// that, which makes "is this a plausible temperature" a direct test of whether
/// the TCXO came up.
///
/// NaN is not plausible, and is what an out-of-range conversion can produce.
/// `contains` is the right shape for that: it is the ordered comparison, so it
/// rejects NaN, where the negated form (`!(c < min || c > max)`) accepts it —
/// every comparison against NaN being false.
pub fn temperature_is_plausible(celsius: f32) -> bool {
    (PLAUSIBLE_MIN_C..=PLAUSIBLE_MAX_C).contains(&celsius)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number the crate and the reference implementations use for 5 ms, so
    /// this conversion agreeing with them is worth pinning.
    /// The codes are the chip's, so their order and values are not ours to
    /// choose. Pinned because a sweep that mislabels which voltage it applied
    /// produces a confident and completely wrong conclusion.
    #[test]
    fn the_tune_codes_are_dense_ordered_and_correctly_labelled() {
        for (i, (code, mv)) in TUNE_CODES.iter().enumerate() {
            assert_eq!(*code as usize, i, "codes must be 0..8 in order");
            assert_eq!(tune_millivolts(*code), Some(*mv));
        }
        let volts: Vec<u16> = TUNE_CODES.iter().map(|(_, mv)| *mv).collect();
        let mut sorted = volts.clone();
        sorted.sort_unstable();
        assert_eq!(volts, sorted, "voltages must rise with the code");
        assert_eq!(tune_millivolts(0x08), None);
    }

    #[test]
    fn the_default_tune_is_three_volts_and_is_in_the_table() {
        assert_eq!(tune_millivolts(TUNE_3V0), Some(3000));
    }

    #[test]
    fn five_milliseconds_is_the_familiar_164_steps() {
        assert_eq!(steps_for_us(5_000), 164);
        assert_eq!(STARTUP_STEPS, 164);
    }

    /// The single most important property here. Rounding down produces a delay
    /// shorter than asked for, which is exactly the condition that raises
    /// HF_XOSC_START_ERR.
    #[test]
    fn conversion_never_rounds_down() {
        for us in [1u32, 29, 30, 31, 61, 100, 999, 1_000, 4_999, 5_000, 123_456] {
            let steps = steps_for_us(us);
            assert!(
                us_for_steps(steps) + 1 >= us,
                "{us} us became {steps} steps = {} us",
                us_for_steps(steps)
            );
            // ...and never more than one step of slack, or it is not rounding.
            assert!(us_for_steps(steps.saturating_sub(1)) < us, "{us} us");
        }
    }

    #[test]
    fn one_step_is_thirty_point_five_two_microseconds() {
        assert_eq!(STEP_NS, 30_520);
        assert_eq!(steps_for_us(30), 1);
        assert_eq!(steps_for_us(31), 2);
        assert_eq!(us_for_steps(1), 30);
        assert_eq!(us_for_steps(2), 61);
    }

    /// Rounding the step to 30 µs before counting would give 167 steps for
    /// 5 ms rather than 164 — a 90 µs error that no one would ever notice and
    /// that would quietly disagree with every reference implementation.
    #[test]
    fn the_fractional_step_is_not_rounded_away() {
        let naive = 5_000u32.div_ceil(30);
        assert_ne!(naive, steps_for_us(5_000));
        assert_eq!(naive, 167);
    }

    #[test]
    fn zero_is_zero_because_zero_means_disabled() {
        assert_eq!(steps_for_us(0), 0);
        assert_eq!(us_for_steps(0), 0);
    }

    #[test]
    fn the_conversion_does_not_overflow_on_large_delays() {
        // A delay this long is absurd, but it must not wrap into a short one.
        let steps = steps_for_us(u32::MAX);
        assert!(steps > MAX_STEPS);
        assert!(us_for_steps(MAX_STEPS) > 500_000_000);
    }

    #[test]
    fn a_room_is_plausible_and_the_surface_of_the_sun_is_not() {
        for c in [-19.0, 0.0, 21.5, 25.0, 40.0, 69.0] {
            assert!(temperature_is_plausible(c), "{c}");
        }
        for c in [
            -273.0,
            -20.1,
            70.1,
            1000.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert!(!temperature_is_plausible(c), "{c}");
        }
    }

    /// A conversion applied to a clock that is not running can land on NaN.
    /// Every comparison against NaN is false, so the negated form of this range
    /// check — `!(c < min || c > max)` — would accept it. The implementation
    /// uses the ordered form for exactly this reason.
    #[test]
    fn nan_is_not_a_temperature() {
        assert!(!temperature_is_plausible(f32::NAN));
    }
}
