//! Power amplifier configuration, output power limits, and the rules for
//! keying a carrier without damaging anything.
//!
//! Everything in this module is a constraint rather than a feature. The LR1121
//! will happily be told to transmit at a power its PA cannot deliver, into a
//! supply that cannot support it, on a frequency outside the band the antenna
//! is cut for, for as long as anybody likes. None of those produce an error;
//! they produce heat, spurious emissions, or a dead part.
//!
//! So the limits live here, where they are tested, and the firmware asks rather
//! than assumes.

/// `SetPaConfig`'s four bytes, in the order they go on the wire.
///
/// Built here rather than through `lr11xx`'s `PaConfig` because that type has a
/// bug: it declares **both** `vbat` and `hp` at bit 24. Two fields cannot share
/// a bit, and the consequence is not cosmetic — asking for the VBAT supply with
/// `with_vbat(true)` sets the same bit as `with_hp(true)`, so a request to
/// change the *power source* silently selects the *high-power PA* instead.
///
/// The layout below puts the PA select in bits 24..=31, the supply select in
/// bits 16..=23, the duty cycle in 8..=15 and the HP size in 0..=7, which is
/// the big-endian byte order the command is sent in. Two of those four agree
/// with `lr11xx`, and the low-power default this produces — `0x0000_0400` — is
/// byte-for-byte the crate's own default, which is the corroboration available
/// without the datasheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaConfigWord {
    /// 0 selects the low-power PA, 1 the high-power PA.
    pub pa_sel: u8,
    /// 0 powers the PA from the internal regulator, 1 from VBAT.
    pub reg_pa_supply: u8,
    /// PA duty cycle.
    pub duty_cycle: u8,
    /// High-power PA size. Meaningless when `pa_sel` is 0.
    pub hp_sel: u8,
}

impl PaConfigWord {
    /// Pack into the 32-bit argument `SetPaConfig` takes.
    pub const fn to_raw(&self) -> u32 {
        ((self.pa_sel as u32) << 24)
            | ((self.reg_pa_supply as u32) << 16)
            | ((self.duty_cycle as u32) << 8)
            | (self.hp_sel as u32)
    }
}

/// The low-power PA on the internal regulator — the safe default, and what a
/// first carrier should use.
pub const LOW_POWER: PaConfigWord = PaConfigWord {
    pa_sel: 0,
    reg_pa_supply: 0,
    duty_cycle: 0x04,
    hp_sel: 0x00,
};

/// The high-power PA on the internal regulator, at the duty cycle and PA size
/// reference implementations use for sub-GHz.
///
/// Kept here as a *diagnostic* alternative rather than as a configuration
/// choice: the module's sub-GHz output may simply not be fed from the low-power
/// PA, and the only way to find out is to try the other one.
pub const HIGH_POWER: PaConfigWord = PaConfigWord {
    pa_sel: 1,
    reg_pa_supply: 0,
    duty_cycle: 0x04,
    hp_sel: 0x07,
};

/// Lowest output power the high-power PA accepts, in dBm.
///
/// The two PAs overlap over most of their range, which is why
/// [`pa_config_for`] has to prefer one rather than compute one.
pub const HP_MIN_DBM: i8 = -9;
/// Highest output power the high-power PA accepts, in dBm — the *die's*
/// number. [`MODULE_MAX_SUB_GHZ_DBM`] is lower and is the one that binds.
pub const HP_MAX_DBM: i8 = 22;

/// Lowest output power the low-power PA accepts, in dBm.
pub const LP_MIN_DBM: i8 = -17;
/// Highest output power the low-power PA accepts, in dBm.
pub const LP_MAX_DBM: i8 = 14;

/// Above this output power the PA must be run from VBAT rather than the
/// internal regulator.
pub const VBAT_REQUIRED_ABOVE_DBM: i8 = 14;

/// What the *module* is rated for on the sub-GHz path, in dBm.
///
/// Deliberately the module's number and not the LR1121's headline 22 dBm. The
/// Elecrow nRFLR1121 datasheet rates the sub-GHz output at 20 dBm; the die can
/// be told to do more, and the part between the die and the connector cannot.
pub const MODULE_MAX_SUB_GHZ_DBM: i8 = 20;

/// What the module is rated for at 2.4 GHz, in dBm.
///
/// 11.5 dBm rounded down, against the LR1121's headline 13 dBm.
pub const MODULE_MAX_2G4_DBM: i8 = 11;

/// Whether the low-power PA can produce this output power.
pub const fn low_power_pa_accepts(dbm: i8) -> bool {
    dbm >= LP_MIN_DBM && dbm <= LP_MAX_DBM
}

/// Whether this output power requires switching the PA supply to VBAT.
pub const fn requires_vbat_supply(dbm: i8) -> bool {
    dbm > VBAT_REQUIRED_ABOVE_DBM
}

/// Whether the high-power PA can produce this output power.
pub const fn high_power_pa_accepts(dbm: i8) -> bool {
    dbm >= HP_MIN_DBM && dbm <= HP_MAX_DBM
}

/// The high-power PA, with the supply that power actually needs.
///
/// [`HIGH_POWER`] hard-codes the internal regulator because it was written as a
/// one-off diagnostic. Driving the high-power PA above 14 dBm from the internal
/// regulator is a brown-out rather than a refusal, so a configuration layer that
/// takes an arbitrary power cannot use that constant.
pub const fn high_power(dbm: i8) -> PaConfigWord {
    PaConfigWord {
        pa_sel: 1,
        reg_pa_supply: if requires_vbat_supply(dbm) { 1 } else { 0 },
        duty_cycle: 0x04,
        hp_sel: 0x07,
    }
}

/// Which PA to use for an output power, or `None` if neither reaches it.
///
/// Prefers the low-power PA wherever it reaches, which is everything from
/// −17 dBm to 14 dBm. That is a deliberate preference and not an arithmetic
/// consequence: the two PAs overlap from −9 to 14 dBm, the low-power one draws
/// less current, and — the reason that decides it — the low-power one is the
/// only one this project has ever measured. Bring-up keyed carriers at −17,
/// 0 and +14 dBm and saw them on a receiver. Nothing has ever come out of the
/// high-power PA on this board.
pub const fn pa_config_for(dbm: i8) -> Option<PaConfigWord> {
    if low_power_pa_accepts(dbm) {
        Some(LOW_POWER)
    } else if high_power_pa_accepts(dbm) {
        Some(high_power(dbm))
    } else {
        None
    }
}

/// The US915 ISM band, in hertz.
pub const US915_MIN_HZ: u32 = 902_000_000;
/// See [`US915_MIN_HZ`].
pub const US915_MAX_HZ: u32 = 928_000_000;

/// Frequency for the bench carrier: the middle of US915.
///
/// Middle of the band on purpose — far from both edges, so a tuning error in
/// either direction is visible as an offset rather than as silence, and easy to
/// find on a receiver.
pub const CW_TEST_HZ: u32 = 915_000_000;

/// Two carriers for the frequency-error experiment, as far apart as one
/// RTL-SDR capture window allows.
///
/// The question they answer: the measured carrier sits 66 kHz below where it
/// was commanded, and that is either the transmitter's clock or the receiver's.
/// The two are not distinguishable from a single measurement, and — less
/// obviously — they are not distinguishable by sweeping the transmitter *with*
/// the receiver either. Writing `C` for the receiver's nominal centre, `F` for
/// the true carrier, `e` for the receiver's clock error and `p` for the
/// transmitter's:
///
/// ```text
/// reported = F - C·e = F_commanded·(1 + p) - C·e
/// ```
///
/// so the apparent error is `F_commanded·p - C·e`. Move `C` along with
/// `F_commanded` and both terms scale together — the experiment is degenerate.
/// **`C` has to be held fixed while `F_commanded` moves**, which confines the
/// sweep to one capture window, and makes the whole signal
/// `(F₂ - F₁)·p` — about 115 Hz at −72 ppm over this 1.6 MHz span.
///
/// Small, but the term it has to be separated from cancels exactly: `C·e` does
/// not depend on `F_commanded` at all.
pub const CW_SWEEP_HZ: [u32; 2] = [913_700_000, 915_300_000];

/// Receiver centre frequency the sweep assumes, in hertz.
///
/// Not used by the firmware — recorded here so the constant the host tunes to
/// and the constants the firmware transmits on cannot drift apart. It sits
/// between the two carriers so neither lands on the receiver's DC spike.
pub const CW_SWEEP_CENTER_HZ: u32 = 914_500_000;

/// How far the bench board's transmitter sits from where it is told to be, in
/// parts per million. **Measured, and it is the transmitter, not the receiver.**
///
/// A commanded 915.000 MHz carrier lands at 914.934 MHz — 66 kHz low. That is
/// either the LR1121's clock or the receiver's, and a single measurement cannot
/// say which, because both produce exactly the same number.
///
/// The experiment that separates them is in [`CW_SWEEP_HZ`]: hold the receiver's
/// tuning fixed, move the transmitter, and the receiver's contribution cancels
/// in the difference. Chopping between 913.7 and 915.3 MHz 22 times inside one
/// capture, with every measurement bracketed by its neighbours so thermal drift
/// cancels, gave a difference of **−117.3 ± 0.8 Hz** across the 1.6 MHz span,
/// against −116 Hz predicted if the transmitter is at fault and 0 Hz if the
/// receiver is.
///
/// So: transmitter −73.3 ± 0.5 ppm, receiver −1.4 ppm. The RTL-SDR is fine.
///
/// **This is a problem, not a curiosity.** 73 ppm is 67 kHz at 915 MHz — over
/// half of a 125 kHz LoRa channel — and it is far outside what a TCXO should
/// do, which suggests the TCXO is not being driven as it expects rather than
/// that it is simply a bad part. The protocol layer cannot ignore it.
pub const MEASURED_TX_ERROR_PPM: f32 = -73.3;

/// Whether a frequency is inside the US915 ISM band.
pub const fn is_in_us915(hz: u32) -> bool {
    hz >= US915_MIN_HZ && hz <= US915_MAX_HZ
}

/// How long a continuous-wave burst may last, in milliseconds.
///
/// A bare carrier is a bench diagnostic, not a mode that satisfies FCC Part
/// 15.247 — which expects digital modulation or frequency hopping in
/// 902–928 MHz. The cap exists so that a host that crashes, a serial cable that
/// falls out, or an operator who walks away cannot leave an unmodulated carrier
/// sitting in the band. The firmware enforces it itself rather than trusting
/// anything off-board to send a stop.
pub const CW_MAX_BURST_MS: u32 = 10_000;

// These are relationships between the constants above, so they are checked when
// the crate is compiled rather than when its tests are run. Every one of them
// is a rule about what may be transmitted; a build that breaks one should not
// exist, let alone reach a board with an antenna on it.
const _: () = assert!(
    low_power_pa_accepts(LP_MIN_DBM) && low_power_pa_accepts(LP_MAX_DBM),
    "the range must include its own endpoints"
);
const _: () = assert!(
    !low_power_pa_accepts(LP_MIN_DBM - 1) && !low_power_pa_accepts(LP_MAX_DBM + 1),
    "the range must exclude everything outside its endpoints"
);
// 14 dBm is the last power the internal regulator can supply and 15 the first
// that cannot. An off-by-one here is a PA browning out under load.
const _: () = assert!(!requires_vbat_supply(14) && requires_vbat_supply(15));
// The module is the limit, not the die. Stated explicitly because the LR1121's
// headline numbers -- 22 dBm and 13 dBm -- are the ones everyone quotes,
// including Meshtastic, which clamps to them.
const _: () = assert!(MODULE_MAX_SUB_GHZ_DBM < 22 && MODULE_MAX_2G4_DBM < 13);
// The band edges are in the band; a hertz outside either is not. 868 MHz is
// Europe's band and must not pass on a US antenna.
const _: () = assert!(is_in_us915(US915_MIN_HZ) && is_in_us915(US915_MAX_HZ));
const _: () = assert!(!is_in_us915(US915_MIN_HZ - 1) && !is_in_us915(US915_MAX_HZ + 1));
const _: () = assert!(!is_in_us915(868_000_000));
const _: () = assert!(
    CW_TEST_HZ == (US915_MIN_HZ + US915_MAX_HZ) / 2,
    "the bench carrier sits in the middle of the band so a tuning error reads \
     as an offset rather than as silence"
);
// Long enough to integrate on a receiver, short enough that nobody is running
// an unmodulated carrier as a lifestyle.
const _: () = assert!(CW_MAX_BURST_MS >= 1_000 && CW_MAX_BURST_MS <= 30_000);
const _: () = assert!(
    low_power_pa_accepts(0),
    "0 dBm must be reachable on the low-power PA; it is the default bench level"
);
const _: () = assert!(
    !requires_vbat_supply(LP_MAX_DBM),
    "nothing the low-power PA can produce should need the VBAT supply, or the \
     safe default is not safe"
);
const _: () = assert!(is_in_us915(CW_TEST_HZ));
const _: () = assert!(is_in_us915(CW_SWEEP_HZ[0]) && is_in_us915(CW_SWEEP_HZ[1]));
// The span is the entire signal in the frequency-error experiment: the
// receiver's contribution cancels, so what is left is (F2 - F1) x p. Too narrow
// a span and there is nothing left to measure.
const _: () = assert!(
    CW_SWEEP_HZ[1] - CW_SWEEP_HZ[0] >= 1_500_000,
    "the sweep span is too narrow to separate a transmitter error from a \
     receiver one"
);
// And the measured error is not a rounding detail: at 915 MHz it is over a
// third of a 125 kHz LoRa channel. Pinned so that "only a few tens of ppm"
// cannot creep into anyone's reasoning later.
const _: () = assert!(
    (CW_TEST_HZ as f32) * MEASURED_TX_ERROR_PPM * 1e-6 < -60_000.0,
    "the transmitter's measured frequency error is tens of kilohertz; anything \
     that makes this assertion fail has lost that fact"
);
// Both carriers must fit inside one 2.048 MS/s capture window centred on
// CW_SWEEP_CENTER_HZ, or the receiver cannot see them without retuning -- and
// retuning is precisely what the experiment may not do.
const _: () = assert!(
    CW_SWEEP_HZ[0] > CW_SWEEP_CENTER_HZ - 1_000_000
        && CW_SWEEP_HZ[1] < CW_SWEEP_CENTER_HZ + 1_000_000,
    "a sweep carrier falls outside the receiver's capture window"
);
// Neither may land on the receiver's own DC spike.
const _: () = assert!(
    CW_SWEEP_HZ[0] + 100_000 < CW_SWEEP_CENTER_HZ && CW_SWEEP_HZ[1] > CW_SWEEP_CENTER_HZ + 100_000,
    "a sweep carrier sits on the receiver's DC spike, where it cannot be measured"
);
const _: () = assert!(LP_MAX_DBM < MODULE_MAX_SUB_GHZ_DBM);
// The low-power PA cannot reach the module's rating, so a configuration layer
// that only ever used it could not offer the top 6 dB the hardware is rated
// for. That is the whole reason `pa_config_for` has to know about both.
const _: () = assert!(high_power_pa_accepts(MODULE_MAX_SUB_GHZ_DBM));
// ...and the module's rating is inside the die's range, or the clamp is
// clamping to something the PA cannot produce.
const _: () = assert!(MODULE_MAX_SUB_GHZ_DBM <= HP_MAX_DBM);
// The overlap is real and the preference resolves it. If these two ever stop
// overlapping, `pa_config_for` has a gap in the middle of its range.
const _: () = assert!(HP_MIN_DBM < LP_MAX_DBM);

#[cfg(test)]
mod tests {
    use super::*;

    /// `lr11xx`'s own `PaConfig` defaults to this exact word for the low-power
    /// PA. Reproducing it byte-for-byte from named fields is the only check
    /// available on the layout without the datasheet — so if this ever fails,
    /// the field order here is wrong, not the test.
    #[test]
    fn the_low_power_default_matches_the_crates_own() {
        assert_eq!(LOW_POWER.to_raw(), 0x0000_0400);
    }

    #[test]
    fn each_field_occupies_its_own_byte() {
        let w = PaConfigWord {
            pa_sel: 0x01,
            reg_pa_supply: 0x02,
            duty_cycle: 0x03,
            hp_sel: 0x04,
        };
        assert_eq!(w.to_raw(), 0x0102_0304);
    }

    /// The low-power PA is preferred throughout its range, including the part
    /// it shares with the high-power one. It is the only PA that has ever been
    /// measured on this board, so anything that quietly moved the boundary
    /// would be transmitting through an unproven path.
    #[test]
    fn the_low_power_pa_is_preferred_wherever_it_reaches() {
        for dbm in LP_MIN_DBM..=LP_MAX_DBM {
            assert_eq!(pa_config_for(dbm), Some(LOW_POWER), "{dbm} dBm");
        }
        for dbm in (LP_MAX_DBM + 1)..=MODULE_MAX_SUB_GHZ_DBM {
            let cfg = pa_config_for(dbm).expect("the module's rating must be reachable");
            assert_eq!(cfg.pa_sel, 1, "{dbm} dBm should use the high-power PA");
        }
    }

    /// Above 14 dBm the internal regulator cannot supply the PA. Getting this
    /// wrong is not an error the chip reports — it is a brown-out.
    #[test]
    fn the_high_power_pa_switches_to_vbat_exactly_where_it_must() {
        assert_eq!(high_power(VBAT_REQUIRED_ABOVE_DBM).reg_pa_supply, 0);
        assert_eq!(high_power(VBAT_REQUIRED_ABOVE_DBM + 1).reg_pa_supply, 1);
        assert_eq!(high_power(MODULE_MAX_SUB_GHZ_DBM).reg_pa_supply, 1);
    }

    /// Neither PA reaches outside its own range, and nothing between them is
    /// silently rounded into something the hardware would accept.
    #[test]
    fn a_power_neither_pa_can_produce_is_refused_rather_than_clamped() {
        assert_eq!(pa_config_for(LP_MIN_DBM - 1), None);
        assert_eq!(pa_config_for(HP_MAX_DBM + 1), None);
        assert_eq!(pa_config_for(-100), None);
        assert_eq!(pa_config_for(127), None);
    }

    /// `HIGH_POWER` stays what it was — a fixed diagnostic word — and is not
    /// quietly the same thing as the supply-aware builder.
    #[test]
    fn the_diagnostic_constant_is_not_the_configurable_one() {
        assert_eq!(high_power(0), HIGH_POWER);
        assert_ne!(high_power(MODULE_MAX_SUB_GHZ_DBM), HIGH_POWER);
    }

    /// The bug this module exists to route around: in `lr11xx`, selecting the
    /// VBAT supply and selecting the high-power PA set the same bit. Here they
    /// are a byte apart, so one cannot be mistaken for the other.
    #[test]
    fn the_supply_select_and_the_pa_select_are_different_bits() {
        let vbat = PaConfigWord {
            reg_pa_supply: 1,
            ..LOW_POWER
        };
        let hp = PaConfigWord {
            pa_sel: 1,
            ..LOW_POWER
        };
        assert_ne!(vbat.to_raw(), hp.to_raw());
        assert_eq!(vbat.to_raw() & hp.to_raw(), LOW_POWER.to_raw());
    }
}
