//! TCXO startup delay, and what a temperature reading has to look like to be
//! believed.
//!
//! The Base Duo drives a 3.0 V TCXO from the LR1121's own DIO3. Two
//! consequences run through the rest of phase 3:
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

/// `tune` code for a 3.0 V TCXO supply — what this board fits.
pub const TUNE_3V0: u8 = 0x06;

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
