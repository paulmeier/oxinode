//! LR1121 reset timing, and how to read what BUSY did about it.
//!
//! `lr11xx::Lr11xx::new` takes an SPI device and a BUSY pin and no reset pin at
//! all, so bringing the chip out of reset is the firmware's job. On this board
//! that job is unusually blind: NRESET and BUSY are both routed *inside* the
//! nRFLR1121 module, so neither can be probed, and the only evidence a reset
//! produced anything is what BUSY did while we were watching.
//!
//! The timings and the reading of that evidence live here, where they can be
//! tested on a host, rather than in the firmware where they cannot be tested at
//! all.

/// The shortest NRESET pulse the LR1121 is documented to accept.
pub const RESET_PULSE_MIN_US: u32 = 100;

/// How long oxinode actually holds NRESET low.
///
/// Ten times the documented minimum. The margin costs a millisecond once per
/// boot and buys immunity to the one failure mode that would be genuinely
/// horrible here: a reset pulse that is *usually* long enough. With no probe,
/// an intermittent reset would look like an intermittently dead radio.
pub const RESET_PULSE_US: u32 = RESET_PULSE_MIN_US * 10;

/// How long to watch for BUSY to *rise* after NRESET is released.
///
/// This is not a correctness deadline — it is how long we are willing to wait
/// before concluding that the rising edge is not coming, so that a chip which
/// was never busy in the first place does not cost a full timeout.
pub const STARTUP_WINDOW_US: u32 = 5_000;

/// How long the LR1121 on this board actually takes from NRESET release to
/// BUSY low.
///
/// **Measured, not assumed.** The SX126x family trains you to expect
/// "milliseconds", and this part takes 191 ms; a 100 ms timeout built on that
/// assumption failed on hardware. Eight
/// consecutive resets came back at 191101, 191162, 191131, 191162, 191101,
/// 191131, 191162 and 191131 µs — a spread of 61 µs, which is two ticks of the
/// 32.768 kHz clock doing the measuring. So this is deterministic to the limit
/// of what oxinode can observe, and it is nearly two hundred times longer than
/// expected.
///
/// Why it takes that long is not established here. The LR11x0 family carries
/// its own on-chip transceiver firmware, so a boot that verifies or loads an
/// image is the obvious guess — but it is a guess, and the datasheet was not on
/// hand to check it. What is not a guess is the number.
pub const STARTUP_MEASURED_US: u32 = 191_200;

/// How long to wait for BUSY to *fall* before giving up.
///
/// Five times the measured startup. Being generous is free; hanging forever is
/// not, and hanging forever is what the obvious code does.
pub const BUSY_TIMEOUT_US: u32 = 1_000_000;

// These are invariants between the constants above, so they are checked when
// the crate is compiled rather than when its tests are run -- a build that
// violates one should not exist, let alone reach a board.
const _: () = assert!(
    RESET_PULSE_US >= RESET_PULSE_MIN_US * 2,
    "a reset pulse this close to the documented minimum is an intermittent waiting to happen"
);
const _: () = assert!(
    STARTUP_WINDOW_US < BUSY_TIMEOUT_US / 4,
    "watching for the rise must give up well before watching for the fall does, \
     or a chip that is never busy costs the whole timeout on every boot"
);
// The measurement is the part that came from hardware rather than from an
// expectation, so it gets a guard of its own: a 100 ms timeout looked entirely
// reasonable right up until the board disagreed, and the way this regresses is
// someone "correcting" the constant back towards the original assumption of
// "milliseconds".
const _: () = assert!(
    STARTUP_MEASURED_US > 100_000,
    "this board's LR1121 takes ~191 ms to release BUSY; a value in the \
     milliseconds means this was reverted to the assumption it replaced"
);
const _: () = assert!(
    BUSY_TIMEOUT_US >= STARTUP_MEASURED_US * 4,
    "the timeout has to clear the startup this board was measured at, with room \
     for a colder part than the one on the bench"
);

/// How long to wait for BUSY to fall after an ordinary command.
///
/// Command processing is microseconds, not the fifth of a second a reset costs,
/// so this is short on purpose: a command that leaves BUSY high is a fault, and
/// waiting a second to say so helps nobody.
pub const COMMAND_TIMEOUT_US: u32 = 10_000;

const _: () = assert!(
    COMMAND_TIMEOUT_US < STARTUP_MEASURED_US,
    "a command wait long enough to cover a reset cannot distinguish the two"
);

/// What BUSY did across one reset cycle.
///
/// Every field is an observation, not a conclusion. [`ResetVerdict::of`] turns
/// them into a judgement, and the firmware logs both — because on a board where
/// nothing can be measured externally, the raw observations are often more use
/// six months later than the judgement was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyTrace {
    /// BUSY as it read before NRESET was touched at all.
    pub before_reset: bool,
    /// BUSY as it read while NRESET was held low.
    ///
    /// Recorded and logged, but deliberately *not* used to judge anything: the
    /// datasheet on hand does not say what BUSY is required to do while the
    /// chip is held in reset, and inventing a requirement would turn a guess
    /// into a failing check.
    pub during_reset: bool,
    /// Whether BUSY was ever observed high after NRESET was released.
    pub rose: bool,
    /// How long after releasing NRESET the chip reported itself ready, or
    /// `None` if it never did.
    pub fell_after_us: Option<u32>,
}

/// What [`BusyTrace`] amounts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetVerdict {
    /// BUSY rose and then fell: the documented startup sequence, start to end.
    Ready,
    /// BUSY fell — or was already low — but was never seen high.
    Inconclusive,
    /// BUSY never fell.
    Failed,
}

impl ResetVerdict {
    /// Read a trace.
    pub const fn of(trace: &BusyTrace) -> Self {
        match (trace.rose, trace.fell_after_us) {
            (_, None) => Self::Failed,
            (true, Some(_)) => Self::Ready,
            (false, Some(_)) => Self::Inconclusive,
        }
    }

    /// Whether it is worth carrying on to the next step.
    ///
    /// [`Self::Inconclusive`] counts as yes: the next thing that happens is a
    /// `GetVersion`, and its answer settles the question far better than
    /// anything more we could do here.
    pub const fn can_proceed(self) -> bool {
        !matches!(self, Self::Failed)
    }

    /// One line for a log.
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Ready => "BUSY rose and fell as documented",
            Self::Inconclusive => "BUSY never rose, but is low",
            Self::Failed => "BUSY never fell",
        }
    }

    /// What this verdict *cannot* tell apart.
    ///
    /// The point of this string is that it is the honest part. A board with a
    /// debug probe would narrow most of these down in a minute; this one cannot
    /// narrow them down at all, and saying so in the log is better than a
    /// confident error message that sends someone looking in the wrong place.
    pub const fn ambiguity(self) -> &'static str {
        match self {
            Self::Ready => {
                "nothing further -- though this says only that the BUSY pin \
                 behaved, not that the chip on the other end is an LR1121"
            }
            Self::Inconclusive => {
                "a startup that finished before the first sample, from a pin \
                 that does not follow BUSY at all. P1.11 is module-internal, so \
                 the second case means the wrong GPIO was chosen, not a missing \
                 wire. GetVersion settles it"
            }
            Self::Failed => {
                "a dead part, a reset that never released, a BUSY line that is \
                 not the pin we think it is, and a chip sitting in its own \
                 bootloader. All four are inside the module: no probe point, no \
                 continuity check, nothing to measure. Try GetVersion anyway -- \
                 all-0x00 or all-0xFF there means SPI is dead too, which at \
                 least narrows it to the part"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(rose: bool, fell_after_us: Option<u32>) -> BusyTrace {
        BusyTrace {
            before_reset: false,
            during_reset: true,
            rose,
            fell_after_us,
        }
    }

    #[test]
    fn the_documented_sequence_is_the_only_ready() {
        assert_eq!(
            ResetVerdict::of(&trace(true, Some(2_500))),
            ResetVerdict::Ready
        );
    }

    #[test]
    fn a_busy_that_never_falls_is_a_failure_however_it_started() {
        assert_eq!(ResetVerdict::of(&trace(true, None)), ResetVerdict::Failed);
        assert_eq!(ResetVerdict::of(&trace(false, None)), ResetVerdict::Failed);
    }

    /// The case that would be easiest to paper over: BUSY reads low throughout,
    /// so `wait_for_low` returns instantly and everything downstream looks
    /// fine. A pin stuck low is indistinguishable from a chip that is ready,
    /// and calling that success would hide exactly the wrong bug.
    #[test]
    fn low_the_whole_time_is_not_success() {
        let verdict = ResetVerdict::of(&trace(false, Some(0)));
        assert_eq!(verdict, ResetVerdict::Inconclusive);
        assert_ne!(verdict, ResetVerdict::Ready);
    }

    #[test]
    fn only_a_failure_stops_the_bring_up() {
        assert!(ResetVerdict::Ready.can_proceed());
        assert!(ResetVerdict::Inconclusive.can_proceed());
        assert!(!ResetVerdict::Failed.can_proceed());
    }

    #[test]
    fn the_level_during_reset_is_recorded_but_never_judged() {
        for during_reset in [false, true] {
            let t = BusyTrace {
                before_reset: false,
                during_reset,
                rose: true,
                fell_after_us: Some(1),
            };
            assert_eq!(ResetVerdict::of(&t), ResetVerdict::Ready);
        }
    }

    #[test]
    fn every_verdict_says_something_different() {
        let all = [
            ResetVerdict::Ready,
            ResetVerdict::Inconclusive,
            ResetVerdict::Failed,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(!a.summary().is_empty());
            assert!(!a.ambiguity().is_empty());
            for b in &all[i + 1..] {
                assert_ne!(a.summary(), b.summary());
                assert_ne!(a.ambiguity(), b.ambiguity());
            }
        }
    }
}
