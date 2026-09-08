//! Listening before transmitting: when the channel counts as clear, and how
//! long to wait when it is not.
//!
//! Phase 15's spike put two oxinodes on the bench and lost a packet every
//! time one transmitted while the other was still on the air. A LoRa radio
//! is half duplex and the modem transmitted the moment a host handed it a
//! frame; nothing anywhere asked whether somebody else was already talking.
//!
//! This module is the decision. The chip-facing part -- issuing a channel
//! activity detection and reading its answer -- is in `src/modem.rs`; what is
//! here is everything that can be decided from a sequence of answers, so it
//! can be tested without a radio.
//!
//! # The shape of it
//!
//! Time is divided into **slots** derived from the symbol time, so a slot is
//! a fixed fraction of a packet at any data rate rather than a fixed number
//! of milliseconds. Before a transmission the channel must be heard clear for
//! a **DIFS** of two slots and then for a further random number of slots, the
//! **contention window**, drawn afresh each time the channel is heard busy.
//! Two boards that both want to talk the moment the air goes quiet draw
//! different windows and one of them goes first; the other hears it and
//! waits. This is the stock RNode's arrangement, as described by its
//! documentation and behaviour: slots of twelve symbols bounded to a range of
//! milliseconds, a DIFS of two slots, and a contention window of up to
//! fifteen. It is not its code, which is GPL and has not been read.
//!
//! One difference is deliberate. The stock firmware waits forever; this
//! waits for a **budget** measured in airtimes of the packet it is holding
//! and then transmits regardless, because a modem loop that never returns
//! also never services the host, and a channel that reads busy for ten
//! airtimes running is more likely a detector fooled by noise than a
//! neighbour with that much to say. The report says when that happened.
//!
//! # Sensing
//!
//! Each sense is a LoRa channel activity detection: the chip correlates a few
//! symbols against the modulation it is configured for and says whether it
//! found one. That hears a transmission on the same spreading factor and
//! bandwidth, which is the only kind this modem can collide with in a way
//! either party would notice.
//!
//! The slots between senses are spent in receive, and a detection falls into
//! receive too, so a packet that starts during the wait is received rather
//! than merely avoided. That is not this module's concern -- it holds no
//! radio -- but it is why a [`Channel::Busy`] may be followed by the caller
//! handing a packet up before asking again.

use super::config::ValidConfig;

/// A slot is this many symbols.
pub const SLOT_SYMBOLS: u32 = 12;
/// Shortest a slot may be. Below this the sense would take longer than the
/// slot it is timing.
pub const SLOT_MIN_US: u32 = 6_000;
/// Longest a slot may be. At SF12 and 62.5 kHz twelve symbols is nearly
/// 800 ms, and a contention window of fifteen of those is twelve seconds.
pub const SLOT_MAX_US: u32 = 100_000;
/// Slots the channel must be clear before the contention window starts.
pub const DIFS_SLOTS: u32 = 2;
/// The contention window is a draw from `0..=CW_MAX` slots.
pub const CW_MAX: u32 = 15;
/// Airtimes of the held packet to wait before transmitting regardless.
pub const BUDGET_AIRTIMES: u32 = 10;
/// The budget is never shorter than this, so a short packet at a fast rate
/// still waits out a long one at the same rate.
pub const BUDGET_MIN_US: u32 = 2_000_000;
/// And never longer than this, so a full packet at the slowest rate does not
/// hold the modem loop for a minute.
pub const BUDGET_MAX_US: u32 = 20_000_000;

/// Symbols a channel activity detection listens for.
///
/// Four rather than the two of Semtech's reference examples: this is used to
/// hear a packet already in progress, not only its preamble, and the detector
/// has fewer false negatives with more symbols to correlate over.
pub const CAD_SYMBOLS: u8 = 4;
/// Detection peak threshold, from Semtech's reference examples for this chip.
pub const CAD_DET_PEAK: u8 = 50;
/// Detection minimum threshold, from the same.
pub const CAD_DET_MIN: u8 = 10;

/// How long a slot lasts for a configuration.
pub const fn slot_us(config: &ValidConfig) -> u32 {
    let slot = SLOT_SYMBOLS * config.get().symbol_time_us();
    if slot < SLOT_MIN_US {
        SLOT_MIN_US
    } else if slot > SLOT_MAX_US {
        SLOT_MAX_US
    } else {
        slot
    }
}

/// How long a channel activity detection may take before it is called a
/// hang.
///
/// The chip listens for [`CAD_SYMBOLS`] symbols and then spends a moment
/// deciding. Twice the symbols plus a margin for the oscillator startup that
/// may be charged to the first operation after standby.
pub const fn cad_timeout_us(config: &ValidConfig) -> u32 {
    2 * CAD_SYMBOLS as u32 * config.get().symbol_time_us() + 20_000
}

/// How long a positive detection may stay in receive waiting for the packet
/// it detected to sync.
///
/// A detection made on a preamble is followed by the rest of that preamble
/// and a header, so the airtime of an empty packet -- preamble, header and
/// the minimum eight payload symbols -- covers it. A detection made on data
/// symbols in the middle of a packet never syncs and waits this out, which
/// is the price of hearing mid-packet at all.
pub const fn cad_rx_timeout_us(config: &ValidConfig) -> u32 {
    config.get().airtime_us(0)
}

/// Symbols a listening receive may run without a valid header before the
/// chip calls it a timeout.
///
/// The receive timer is stopped on preamble detection so that a packet
/// starting mid-slot is received whole -- and a *false* preamble, or one
/// whose header is corrupted by a collision, would then leave the chip in
/// receive with no timer at all. This is the chip's other timer, in symbols
/// from the start of the receive: enough for a slot, the longest preamble
/// this configuration sends, a header, and a margin. A real packet validates
/// its header inside that; anything else ends the receive.
///
/// Saturated at the field's width. A configuration with a very long preamble
/// gets the ceiling, which still ends the receive -- just possibly before a
/// packet that started at the very end of the slot has validated.
pub const fn sync_timeout_symbols(config: &ValidConfig) -> u8 {
    let slot = slot_us(config).div_ceil(config.get().symbol_time_us());
    let preamble = config.get().preamble_symbols as u32 + 5;
    let header = 8;
    let margin = 8;
    let symbols = slot + preamble + header + margin;
    if symbols > u8::MAX as u32 {
        u8::MAX
    } else {
        symbols as u8
    }
}

/// How long to keep trying before transmitting into whatever is there.
pub const fn budget_us(config: &ValidConfig, payload_len: u8) -> u32 {
    let budget = BUDGET_AIRTIMES as u64 * config.get().airtime_us(payload_len) as u64;
    if budget < BUDGET_MIN_US as u64 {
        BUDGET_MIN_US
    } else if budget > BUDGET_MAX_US as u64 {
        BUDGET_MAX_US
    } else {
        budget as u32
    }
}

/// A draw from `0..=CW_MAX`, from a seed.
///
/// SplitMix64's output function: every bit of the seed reaches every bit of
/// the result, so seeds that differ by one -- consecutive timer ticks, say --
/// give unrelated draws. Not a random number generator, and not trying to
/// be: the property that matters is that two boards seeded from their own
/// uptimes do not draw the same window every time.
pub const fn draw(seed: u64) -> u32 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z % (CW_MAX as u64 + 1)) as u32
}

/// What a sense heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Clear,
    Busy,
}

/// What to do after a sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Wait this many microseconds, then sense again.
    Wait(u32),
    /// The channel has been clear for long enough. Transmit.
    Transmit,
    /// The budget is spent. Transmit anyway, and say so.
    Force,
}

/// How the wait went, for the log and the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Report {
    /// Senses taken, including the one that cleared the way.
    pub senses: u32,
    /// Senses that heard something.
    pub busy: u32,
    /// Microseconds spent waiting between senses.
    pub waited_us: u32,
    /// Whether the budget ran out and the packet went regardless.
    pub forced: bool,
}

/// The wait before one transmission.
///
/// Fed one sense at a time through [`Backoff::next`], and answers with what
/// to do next. Holds no time of its own: the caller measures the slots, and
/// this counts them.
#[derive(Debug, Clone)]
pub struct Backoff {
    slot_us: u32,
    budget_us: u32,
    seed: u64,
    /// Clear senses needed in a row: the DIFS and the current window.
    need: u32,
    /// Clear senses seen in a row.
    run: u32,
    report: Report,
}

impl Backoff {
    /// A wait for `payload_len` bytes under `config`, with `seed` deciding
    /// the contention windows.
    pub const fn new(config: &ValidConfig, payload_len: u8, seed: u64) -> Self {
        Self {
            slot_us: slot_us(config),
            budget_us: budget_us(config, payload_len),
            seed,
            need: DIFS_SLOTS + draw(seed),
            run: 0,
            report: Report {
                senses: 0,
                busy: 0,
                waited_us: 0,
                forced: false,
            },
        }
    }

    /// The slot this wait is counting in.
    pub const fn slot_us(&self) -> u32 {
        self.slot_us
    }

    /// The budget this wait is bounded by.
    pub const fn budget_us(&self) -> u32 {
        self.budget_us
    }

    /// Clear senses still needed, at this moment.
    pub const fn remaining(&self) -> u32 {
        self.need - self.run
    }

    /// How it has gone so far.
    pub const fn report(&self) -> Report {
        self.report
    }

    /// Record a sense and say what to do next.
    pub fn next(&mut self, heard: Channel) -> Step {
        self.report.senses += 1;
        match heard {
            Channel::Clear => {
                self.run += 1;
                if self.run >= self.need {
                    return Step::Transmit;
                }
            }
            Channel::Busy => {
                self.report.busy += 1;
                self.run = 0;
                // A fresh window, from a fresh seed: the same draw every time
                // would make two boards that collided once collide again.
                self.seed = self.seed.wrapping_add(1);
                self.need = DIFS_SLOTS + draw(self.seed);
            }
        }
        if self.report.waited_us >= self.budget_us {
            self.report.forced = true;
            return Step::Force;
        }
        self.report.waited_us = self.report.waited_us.saturating_add(self.slot_us);
        Step::Wait(self.slot_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::{RadioConfig, DEFAULT};

    fn valid(sf: u8, bandwidth_hz: u32) -> ValidConfig {
        let config = RadioConfig {
            spreading_factor: sf,
            bandwidth_hz,
            ..DEFAULT
        };
        ValidConfig::new(config).expect("a valid configuration")
    }

    /// The spike's configuration: SF8 at 125 kHz.
    fn spike() -> ValidConfig {
        valid(8, 125_000)
    }

    /// Drive a wait to its end with the channel behaving as `heard` says,
    /// slot by slot, and return the last step and the slots waited.
    fn run(backoff: &mut Backoff, mut heard: impl FnMut(u32) -> Channel) -> (Step, u32) {
        let mut slots = 0;
        loop {
            match backoff.next(heard(slots)) {
                Step::Wait(_) => slots += 1,
                done => return (done, slots),
            }
        }
    }

    #[test]
    fn a_slot_is_twelve_symbols_within_its_bounds() {
        // SF8 at 125 kHz: 2.048 ms a symbol, 24.576 ms a slot.
        assert_eq!(slot_us(&spike()), 24_576);
        // SF7 at 500 kHz: 256 µs a symbol, 3 ms of symbols, clamped up.
        assert_eq!(slot_us(&valid(7, 500_000)), SLOT_MIN_US);
        // SF12 at 62.5 kHz: 65.5 ms a symbol, clamped down.
        assert_eq!(slot_us(&valid(12, 62_500)), SLOT_MAX_US);
        // SF12 at 125 kHz: 32.768 ms a symbol, 393 ms of symbols, clamped.
        assert_eq!(slot_us(&valid(12, 125_000)), SLOT_MAX_US);
    }

    #[test]
    fn the_budget_is_airtimes_within_its_bounds() {
        // 180 bytes at SF8/125k is about half a second; ten of those.
        let config = spike();
        let airtime = config.airtime_us(180);
        assert!((400_000..600_000).contains(&airtime), "{airtime}");
        assert_eq!(budget_us(&config, 180), BUDGET_AIRTIMES * airtime);
        // A tiny packet at a fast rate still gets the floor.
        assert_eq!(budget_us(&valid(7, 500_000), 1), BUDGET_MIN_US);
        // A full packet at the slowest rate gets the ceiling.
        assert_eq!(budget_us(&valid(12, 62_500), 255), BUDGET_MAX_US);
    }

    #[test]
    fn a_detection_waits_in_receive_for_an_empty_packets_worth() {
        // SF8 at 125 kHz, eight symbols of preamble: 12.25 symbols of it,
        // then 13 of header, CRC and the minimum block at 4/5.
        assert_eq!(cad_rx_timeout_us(&spike()), 51_712);
        // It is a wait that ends: well under a slot's budget share.
        assert!(cad_rx_timeout_us(&spike()) < budget_us(&spike(), 1) / 10);
    }

    #[test]
    fn the_symbol_timeout_covers_a_slot_a_preamble_and_a_header() {
        // SF8 at 125 kHz: 12 slot symbols, 8 + 5 of preamble, 8, 8.
        assert_eq!(sync_timeout_symbols(&spike()), 41);
        // A fast link's slot is clamped up to 6 ms, which is many symbols.
        let fast = valid(7, 500_000);
        assert_eq!(sync_timeout_symbols(&fast), 24 + 13 + 16);
        // A very long preamble saturates rather than wrapping.
        let long = ValidConfig::new(RadioConfig {
            preamble_symbols: 1000,
            ..DEFAULT
        })
        .expect("valid");
        assert_eq!(sync_timeout_symbols(&long), u8::MAX);
    }

    #[test]
    fn the_cad_timeout_covers_the_symbols_and_the_oscillator() {
        let t = cad_timeout_us(&spike());
        assert!(t > CAD_SYMBOLS as u32 * 2048);
        assert!(t >= 20_000);
    }

    #[test]
    fn the_draw_covers_the_window_and_only_the_window() {
        let mut seen = [0u32; CW_MAX as usize + 1];
        for seed in 0..4096u64 {
            let d = draw(seed);
            assert!(d <= CW_MAX, "seed {seed} drew {d}");
            seen[d as usize] += 1;
        }
        for (slot, n) in seen.iter().enumerate() {
            assert!(*n > 150, "slot {slot} drawn only {n} times in 4096");
        }
    }

    #[test]
    fn consecutive_seeds_give_unrelated_draws() {
        // Uptime ticks are the seed on the board, and two boards that boot
        // together tick together. Neighbouring seeds must not draw the same
        // window more often than chance.
        let same = (0..1000u64).filter(|s| draw(*s) == draw(s + 1)).count();
        assert!(same < 120, "{same} of 1000 neighbouring seeds agreed");
    }

    #[test]
    fn a_clear_channel_is_transmitted_on_after_the_difs_and_the_window() {
        for seed in 0..64 {
            let mut b = Backoff::new(&spike(), 180, seed);
            let window = draw(seed);
            let (step, slots) = run(&mut b, |_| Channel::Clear);
            assert_eq!(step, Step::Transmit);
            // The first sense is immediate; each further one follows a slot.
            assert_eq!(slots, DIFS_SLOTS + window - 1, "seed {seed}");
            let r = b.report();
            assert_eq!(r.senses, DIFS_SLOTS + window);
            assert_eq!(r.busy, 0);
            assert!(!r.forced);
            assert_eq!(r.waited_us, slots * b.slot_us());
        }
    }

    #[test]
    fn a_zero_window_still_waits_the_difs() {
        let seed = (0..u64::MAX).find(|s| draw(*s) == 0).unwrap();
        let mut b = Backoff::new(&spike(), 180, seed);
        assert_eq!(b.next(Channel::Clear), Step::Wait(b.slot_us()));
        assert_eq!(b.next(Channel::Clear), Step::Transmit);
    }

    #[test]
    fn a_busy_sense_starts_the_count_over_with_a_new_window() {
        let seed = (0..u64::MAX).find(|s| draw(*s) == 3).unwrap();
        let mut b = Backoff::new(&spike(), 180, seed);
        assert_eq!(b.remaining(), DIFS_SLOTS + 3);
        b.next(Channel::Clear);
        b.next(Channel::Clear);
        assert_eq!(b.remaining(), 3);
        b.next(Channel::Busy);
        assert_eq!(b.remaining(), DIFS_SLOTS + draw(seed + 1));
        assert_eq!(b.report().busy, 1);
    }

    #[test]
    fn a_packet_in_progress_is_waited_out_and_then_the_way_is_clear() {
        // The phase 15 collision: the other board is mid-way through half a
        // second of announce when the host hands this one a reply. Busy for
        // twenty slots, then quiet.
        let mut b = Backoff::new(&spike(), 131, 7);
        let (step, slots) = run(&mut b, |slot| {
            if slot < 20 {
                Channel::Busy
            } else {
                Channel::Clear
            }
        });
        assert_eq!(step, Step::Transmit);
        assert!(slots >= 20 + DIFS_SLOTS - 1, "{slots}");
        assert!(!b.report().forced);
        assert_eq!(b.report().busy, 20);
    }

    #[test]
    fn a_channel_that_never_clears_is_forced_within_the_budget() {
        let config = spike();
        let mut b = Backoff::new(&config, 180, 1);
        let (step, slots) = run(&mut b, |_| Channel::Busy);
        assert_eq!(step, Step::Force);
        let r = b.report();
        assert!(r.forced);
        assert!(r.waited_us >= b.budget_us());
        assert!(r.waited_us < b.budget_us() + b.slot_us());
        assert_eq!(r.busy, r.senses);
        // Ten airtimes of half a second at 24.6 ms a slot: about 200 slots.
        assert!((150..250).contains(&slots), "{slots}");
    }

    #[test]
    fn the_budget_is_not_exceeded_by_much_at_the_slowest_rate() {
        let config = valid(12, 62_500);
        let mut b = Backoff::new(&config, 255, 1);
        let (step, _) = run(&mut b, |_| Channel::Busy);
        assert_eq!(step, Step::Force);
        assert!(b.report().waited_us <= BUDGET_MAX_US + SLOT_MAX_US);
    }

    #[test]
    fn two_boards_that_hear_the_air_clear_together_rarely_draw_the_same_window() {
        // Both boards have a packet when the announce ends; each is seeded by
        // its own uptime tick. They collide only if they draw the same window.
        let collisions = (0..1000u64)
            .filter(|a| {
                let b = a.wrapping_mul(7919).wrapping_add(31);
                draw(*a) == draw(b)
            })
            .count();
        // One in sixteen by chance; allow for variance.
        assert!(collisions < 110, "{collisions} of 1000");
    }
}
