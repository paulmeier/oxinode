//! When the board sounds its buzzer for a message, and what it sounds.
//!
//! A board in a pocket that receives a message says so only on a panel
//! nobody is looking at. So a message that arrives sounds a short pattern --
//! once, however many arrive together -- while the board is a node of its
//! own and the setting is on. Under a host it never sounds: the host's own
//! app tells its user, and the board is then an RNode, which has nothing of
//! its own to say.
//!
//! The decisions are here, where they can be tested; the firmware only
//! drives the pin through [`PATTERN`] when [`Alert::arrived`] says to.
//!
//! # Together
//!
//! Messages come off the air one packet at a time, and the node takes one
//! packet a quiet pass of the modem loop, so three messages sent at once
//! arrive as three calls a few seconds apart, not as one. They are one
//! alert if each came within [`MERGE_MS`] of the one before it, timed by
//! when the packet was heard rather than when it was read. A steady
//! conversation, a message every few seconds, sounds once at its start:
//! whoever is answering is already holding the board.

use crate::mode::Mode;

/// How close two arrivals have to be to share an alert. Ten seconds is
/// several packets' airtime at the slowest settings a board is likely to
/// run, and short enough that a message a minute later is news.
pub const MERGE_MS: u64 = 10_000;

/// The pattern: the buzzer on, then off, in milliseconds, alternately,
/// starting with on. Two 80 ms beeps 80 ms apart.
pub const PATTERN: [u16; 3] = [80, 80, 80];

/// The pitch of a beep. The buzzer is a passive one: the firmware makes the
/// tone, as a square wave on the load switch's enable. Heard on the bench
/// at 1, 2 and 3 kHz, and not at 4, which the switch does not follow; 2 kHz
/// was the loudest of the three.
pub const TONE_HZ: u32 = 2_000;

/// How long the pattern lasts, end to end.
pub const PATTERN_MS: u32 = {
    let mut total = 0u32;
    let mut i = 0;
    while i < PATTERN.len() {
        total += PATTERN[i] as u32;
        i += 1;
    }
    total
};

// It never sounds for long: a buzzer that is held on is a fault, not an
// alert, and the pattern ends on a beep so that the pin is left off.
const _: () = assert!(PATTERN_MS <= 500);
const _: () = assert!(PATTERN.len() % 2 == 1);
// Inside what the switch was heard to follow.
const _: () = assert!(TONE_HZ >= 1_000 && TONE_HZ <= 3_000);

/// The setting and what arrived last: what decides whether the next
/// message sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    on: bool,
    /// When the last message was heard, in milliseconds since boot.
    last_ms: Option<u64>,
    sounded: u32,
}

impl Alert {
    pub const fn new(on: bool) -> Self {
        Alert {
            on,
            last_ms: None,
            sounded: 0,
        }
    }

    /// The setting: *Alert On/Off* on the System screen.
    pub const fn is_on(&self) -> bool {
        self.on
    }

    pub fn set(&mut self, on: bool) {
        self.on = on;
    }

    /// How many alerts have sounded since boot, for a log.
    pub const fn sounded(&self) -> u32 {
        self.sounded
    }

    /// `new` messages arrived in a packet heard at `at_ms`, with the board
    /// in `mode`: whether to sound the pattern now.
    ///
    /// An arrival is remembered whether or not it sounds, so a message that
    /// comes with the setting off, or under a host, still counts as one that
    /// came: the next within [`MERGE_MS`] of it is part of the same lot.
    pub fn arrived(&mut self, new: usize, at_ms: u64, mode: Mode) -> bool {
        if new == 0 {
            return false;
        }
        let together = self
            .last_ms
            .is_some_and(|last| at_ms.saturating_sub(last) < MERGE_MS);
        // A packet that waited in the inbox can be older than the one before
        // it was read; the newest is what the next is measured from.
        self.last_ms = Some(self.last_ms.map_or(at_ms, |last| last.max(at_ms)));
        let sound = self.on && mode.is_node() && !together;
        if sound {
            self.sounded += 1;
        }
        sound
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rnode::hosts::Transport;

    #[test]
    fn a_message_sounds_once() {
        let mut alert = Alert::new(true);
        assert!(alert.arrived(1, 5_000, Mode::Node));
        assert_eq!(alert.sounded(), 1);
    }

    #[test]
    fn three_together_sound_once() {
        let mut alert = Alert::new(true);
        let sounded: Vec<bool> = [1_000, 3_500, 6_000]
            .iter()
            .map(|&at| alert.arrived(1, at, Mode::Node))
            .collect();
        assert_eq!(sounded, [true, false, false]);
        // And three in one packet's worth, the same.
        let mut alert = Alert::new(true);
        assert!(alert.arrived(3, 1_000, Mode::Node));
        assert_eq!(alert.sounded(), 1);
    }

    #[test]
    fn a_message_after_a_quiet_spell_sounds_again() {
        let mut alert = Alert::new(true);
        assert!(alert.arrived(1, 1_000, Mode::Node));
        assert!(!alert.arrived(1, 1_000 + MERGE_MS - 1, Mode::Node));
        // Measured from the last arrival, not from the alert: a run of
        // messages each within the window of the one before is one lot.
        assert!(!alert.arrived(1, 1_000 + 2 * MERGE_MS - 2, Mode::Node));
        assert!(alert.arrived(1, 1_000 + 3 * MERGE_MS - 2, Mode::Node));
        assert_eq!(alert.sounded(), 2);
    }

    #[test]
    fn a_packet_read_late_does_not_reopen_the_window() {
        let mut alert = Alert::new(true);
        assert!(alert.arrived(1, 20_000, Mode::Node));
        // Heard before the last, read after it.
        assert!(!alert.arrived(1, 4_000, Mode::Node));
        assert!(!alert.arrived(1, 20_000 + MERGE_MS - 1, Mode::Node));
    }

    #[test]
    fn nothing_new_sounds_nothing() {
        // A duplicate, a forgery, an announce, a proof: none is a message
        // the board did not have, and none counts as an arrival.
        let mut alert = Alert::new(true);
        assert!(!alert.arrived(0, 1_000, Mode::Node));
        assert!(alert.arrived(1, 1_500, Mode::Node));
    }

    #[test]
    fn off_sounds_nothing() {
        let mut alert = Alert::new(false);
        for at in [0, 20_000, 40_000] {
            assert!(!alert.arrived(1, at, Mode::Node));
        }
        assert_eq!(alert.sounded(), 0);
        alert.set(true);
        assert!(alert.is_on());
        assert!(alert.arrived(1, 60_000, Mode::Node));
    }

    #[test]
    fn arrivals_with_it_off_still_hold_the_window() {
        let mut alert = Alert::new(false);
        assert!(!alert.arrived(1, 1_000, Mode::Node));
        alert.set(true);
        assert!(!alert.arrived(1, 2_000, Mode::Node));
    }

    #[test]
    fn nothing_sounds_under_a_host_or_with_no_config() {
        for mode in [
            Mode::Rnode(Transport::Usb),
            Mode::Rnode(Transport::Bluetooth),
            Mode::Idle,
        ] {
            let mut alert = Alert::new(true);
            assert!(!alert.arrived(1, 1_000, mode), "{mode:?}");
        }
    }

    #[test]
    fn the_pattern_is_two_short_beeps() {
        assert_eq!(PATTERN_MS, 240);
        assert_eq!(PATTERN.iter().step_by(2).count(), 2);
    }
}
