//! When the board announces itself, and how much of the channel that may
//! take.
//!
//! Announcing is what makes the board exist to anyone else, and it is also
//! the largest steady cost the node puts on a channel it shares. So the
//! budget comes first and the reasons to announce second: whatever asks for
//! an announce, one goes only when the one before it has had its quiet time.
//!
//! # The budget
//!
//! Reticulum caps announce traffic on an interface at a fraction of its
//! bitrate, 2% by default ([`CAP_PERCENT`]). The board holds its own
//! announces to the same cap, measured the way the cap means it: in time on
//! the air. An announce that took *t* to send is followed by at least
//! 100/2 = 50 *t* of silence ([`gap_ms`]) before the next. At SF9 and
//! 125 kHz an announce is under a second on the air, so the gap is under a
//! minute; at SF12 and 125 kHz it is over four minutes; at the slowest
//! configuration the radio accepts, SF12 at 62.5 kHz and 4/8, it is about
//! seventeen.
//!
//! # The reasons
//!
//! * **Starting.** When the board becomes a node -- at boot, or when the
//!   last host has gone ([`crate::mode`]) -- because nobody on the air has
//!   heard from it while it was somebody's RNode.
//! * **The interval**, [`INTERVAL_MS`], so peers that came up later learn
//!   it without asking.
//! * **Asked.** *Announce Now* on the panel.
//! * **A path request** for the board's address, answered with an announce
//!   marked as a path response. A peer that knows only the hash finds the
//!   board this way. An ordinary announce answers it as well, so when both
//!   are waiting one ordinary announce goes.
//!
//! None of them bypasses the budget. A reason that arrives during the gap
//! waits for the end of it; several arriving during one gap are one
//! announce.

use crate::lr1121::config::ValidConfig;
use crate::rnode::air::Split;

/// Reticulum's default announce cap, as a percentage of an interface's time.
pub const CAP_PERCENT: u64 = 2;

/// How often the board announces with nothing else asking it to.
///
/// Half an hour: often enough that a peer that came up later finds the
/// board within one, rarely enough that a channel full of boards spends
/// almost nothing on it.
pub const INTERVAL_MS: u64 = 30 * 60 * 1_000;

/// The quiet the cap asks for after something that took `airtime_us` on
/// the air, in milliseconds.
pub const fn gap_ms(airtime_us: u32) -> u64 {
    airtime_us as u64 * (100 / CAP_PERCENT) / 1_000
}

/// How long a packet of `len` bytes takes on the air with `config`, as the
/// modem sends it: a header byte in front, and two frames for a packet that
/// does not fit in one.
pub fn airtime_us(config: &ValidConfig, len: usize) -> Option<u32> {
    let packet = [0u8; 2 * crate::rnode::air::FRAME_MAX];
    let split = Split::new(packet.get(..len)?, 0)?;
    let mut frame = [0u8; crate::rnode::air::FRAME_MAX];
    let mut total = 0u32;
    for i in 0..split.frames() {
        let n = split.frame(i, &mut frame)?;
        total += config.airtime_us(n as u8);
    }
    Some(total)
}

/// Why an announce is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// An answer to a path request for the board's address.
    PathResponse,
    /// An ordinary announce, for anyone listening.
    Announce,
}

/// When the next announce may go, and what is waiting to.
#[derive(Debug, Clone, Default)]
pub struct Schedule {
    /// When the last one went, and the quiet it bought.
    last: Option<(u64, u64)>,
    /// What is waiting for the budget, if anything.
    pending: Option<Kind>,
    /// When the interval next asks.
    next_interval_ms: Option<u64>,
}

impl Schedule {
    pub const fn new() -> Self {
        Self {
            last: None,
            pending: None,
            next_interval_ms: None,
        }
    }

    /// Something asks for an announce. An ordinary one covers a path
    /// response, so the stronger of the two waiting is kept.
    pub fn want(&mut self, kind: Kind) {
        self.pending = Some(self.pending.map_or(kind, |k| k.max(kind)));
    }

    /// The board has become a node: announce, and start the interval.
    pub fn started(&mut self, now_ms: u64) {
        self.want(Kind::Announce);
        self.next_interval_ms = Some(now_ms + INTERVAL_MS);
    }

    /// A host has the radio: nothing waits to be sent under it. What the
    /// last announce bought stays, since the channel remembers it too.
    pub fn withhold(&mut self) {
        self.pending = None;
        self.next_interval_ms = None;
    }

    /// The earliest the budget lets the next announce go.
    pub fn allowed_from(&self) -> u64 {
        self.last.map_or(0, |(at, gap)| at + gap)
    }

    /// What should go now, if anything. Taking it is a promise to report
    /// [`Schedule::sent`] when it has, or [`Schedule::want`] it again if it
    /// did not.
    pub fn due(&mut self, now_ms: u64) -> Option<Kind> {
        if self.next_interval_ms.is_some_and(|t| now_ms >= t) {
            self.want(Kind::Announce);
            self.next_interval_ms = Some(now_ms + INTERVAL_MS);
        }
        if now_ms < self.allowed_from() {
            return None;
        }
        self.pending.take()
    }

    /// An announce went at `now_ms`, taking `airtime_us`.
    pub fn sent(&mut self, now_ms: u64, airtime_us: u32) {
        self.last = Some((now_ms, gap_ms(airtime_us)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::{RadioConfig, DEFAULT};

    fn config(sf: u8, bw: u32) -> ValidConfig {
        ValidConfig::new(RadioConfig {
            spreading_factor: sf,
            bandwidth_hz: bw,
            ..DEFAULT
        })
        .expect("a configuration the radio can do")
    }

    /// A board's announce: 167 bytes of keys, hashes and signature, and the
    /// display name `oxinode 7F23` in LXMF's app data.
    const ANNOUNCE_LEN: usize = 167 + 16;

    #[test]
    fn the_gap_is_fifty_airtimes() {
        assert_eq!(gap_ms(1_000_000), 50_000);
        assert_eq!(gap_ms(0), 0);
    }

    #[test]
    fn airtime_counts_the_header_byte_and_a_second_frame() {
        let c = config(9, 125_000);
        assert_eq!(airtime_us(&c, 100), Some(c.airtime_us(101)));
        // 300 bytes are two frames; together longer than one frame of 255.
        let two = airtime_us(&c, 300).unwrap();
        assert!(two > c.airtime_us(255), "{two}");
        assert_eq!(airtime_us(&c, 1_000), None, "more than two frames");
    }

    #[test]
    fn at_sf12_and_125_khz_the_budget_holds() {
        let c = config(12, 125_000);
        let air = airtime_us(&c, ANNOUNCE_LEN).unwrap();
        // Over five seconds on the air, so over four minutes between.
        assert!(air > 5_000_000, "{air}");
        let mut s = Schedule::new();
        s.started(0);
        assert_eq!(s.due(0), Some(Kind::Announce));
        s.sent(0, air);
        s.want(Kind::Announce);
        let gap = gap_ms(air);
        assert!(gap > 4 * 60 * 1_000, "{gap}");
        assert_eq!(s.due(gap - 1), None);
        assert_eq!(s.due(gap), Some(Kind::Announce));
    }

    #[test]
    fn at_the_fastest_setting_the_budget_still_holds() {
        let c = config(5, 500_000);
        let air = airtime_us(&c, ANNOUNCE_LEN).unwrap();
        let mut s = Schedule::new();
        s.sent(0, air);
        s.want(Kind::Announce);
        let gap = gap_ms(air);
        assert!(gap > 0);
        assert_eq!(s.due(gap - 1), None);
        assert_eq!(s.due(gap), Some(Kind::Announce));
    }

    #[test]
    fn at_the_slowest_setting_the_gap_is_long_and_under_ten_hours() {
        // SF12, 62.5 kHz and 4/8: the slowest configuration the radio
        // accepts. The System screen writes the gap with one digit of hours.
        let c = ValidConfig::new(RadioConfig {
            spreading_factor: 12,
            bandwidth_hz: 62_500,
            coding_rate: 8,
            ..DEFAULT
        })
        .expect("the slowest configuration is still one the radio can do");
        let gap = gap_ms(airtime_us(&c, ANNOUNCE_LEN).unwrap());
        // About seventeen minutes, measured.
        assert!(gap > 10 * 60 * 1_000, "{gap}");
        assert!(gap < 10 * 3_600 * 1_000, "{gap}");
    }

    #[test]
    fn whatever_asks_no_announce_comes_inside_the_gap() {
        // A path request every second, Announce Now pressed every ten, for
        // ten minutes at SF10: never more often than the cap.
        let c = config(10, 125_000);
        let air = airtime_us(&c, ANNOUNCE_LEN).unwrap();
        let mut s = Schedule::new();
        s.started(0);
        let mut sent = Vec::new();
        for t in (0..600_000).step_by(1_000) {
            s.want(Kind::PathResponse);
            if t % 10_000 == 0 {
                s.want(Kind::Announce);
            }
            if s.due(t).is_some() {
                s.sent(t, air);
                sent.push(t);
            }
        }
        for pair in sent.windows(2) {
            assert!(pair[1] - pair[0] >= gap_ms(air), "{sent:?}");
        }
        // And it does use the budget rather than starve.
        assert!(sent.len() >= (600_000 / gap_ms(air)) as usize, "{sent:?}");
    }

    #[test]
    fn several_reasons_in_one_gap_are_one_announce_and_the_ordinary_kind_wins() {
        let mut s = Schedule::new();
        s.sent(0, 1_000_000); // 50 s of gap
        s.want(Kind::PathResponse);
        s.want(Kind::Announce);
        s.want(Kind::PathResponse);
        assert_eq!(s.due(49_999), None);
        assert_eq!(s.due(50_000), Some(Kind::Announce));
        assert_eq!(s.due(50_001), None, "one announce, not three");
    }

    #[test]
    fn a_path_request_alone_is_answered_as_one() {
        let mut s = Schedule::new();
        s.want(Kind::PathResponse);
        assert_eq!(s.due(0), Some(Kind::PathResponse));
    }

    #[test]
    fn the_interval_asks_every_half_hour() {
        let mut s = Schedule::new();
        s.started(0);
        assert_eq!(s.due(0), Some(Kind::Announce));
        s.sent(0, 500_000);
        assert_eq!(s.due(INTERVAL_MS - 1), None);
        assert_eq!(s.due(INTERVAL_MS), Some(Kind::Announce));
        s.sent(INTERVAL_MS, 500_000);
        assert_eq!(s.due(2 * INTERVAL_MS - 1), None);
        assert_eq!(s.due(2 * INTERVAL_MS), Some(Kind::Announce));
    }

    #[test]
    fn under_a_host_nothing_waits_and_the_gap_is_kept() {
        let mut s = Schedule::new();
        s.started(0);
        assert_eq!(s.due(0), Some(Kind::Announce));
        s.sent(0, 1_000_000);
        s.want(Kind::PathResponse);
        s.withhold();
        assert_eq!(
            s.due(INTERVAL_MS * 3),
            None,
            "nothing waited and no interval ran"
        );
        // The host leaves 10 s after the announce: the new start waits out
        // the gap the last announce bought.
        s.started(10_000);
        assert_eq!(s.due(10_000), None);
        assert_eq!(s.due(50_000), Some(Kind::Announce));
    }
}
