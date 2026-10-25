//! Who the board is right now: a host's RNode, or a node of its own.
//!
//! One image, two modes. With a host on the line -- `rnsd` or MeshChat over
//! USB, Sideband over Bluetooth -- the host is the node and the board is its
//! RNode, exactly as an image without the node is. With nobody there, the
//! board is a Reticulum node of its own. This is the rule that decides which,
//! and it lives here, where a cable that is plugged in and out four times in
//! a second is a test rather than a bench session.
//!
//! # A host is a host when it has spoken
//!
//! Only a KISS frame makes a host present. Enumerating on USB to charge from
//! a laptop does not; neither does a terminal that opened the port and said
//! nothing, nor a bonded phone in range that has not connected. Every real
//! host speaks at once -- `RNodeInterface` sends a detect the moment it opens
//! the port, Sideband the moment the link is up -- so waiting for a frame
//! costs nothing and rules out every one of those.
//!
//! A host arrives at its first frame, immediately: the RNode answers its
//! host with no delay, and the node gives way at once.
//!
//! # A host is gone when it has been gone for a while
//!
//! On USB, a host has gone when the bus goes away (the cable, or the
//! computer asleep) or when DTR falls after having been up, which is what
//! closing the port does. Over Bluetooth, when the link drops. Neither ends
//! the RNode at once: the host has to stay gone for [`HOLD_MS`], so a cable
//! knocked out and pushed back, or `rnsd` restarted, finds the board still
//! its RNode and the node never noticed. After that the host is forgotten,
//! and coming back means speaking again.
//!
//! # What the node does with the radio
//!
//! Nothing of its own. Standalone, the node uses the configuration TNC mode
//! keeps -- the one `rnodeconf --tnc` or *Save Config* stored, which the
//! board already brings its radio up on at boot. A board with no stored
//! configuration has no configuration that is anybody's choice for it to be
//! a node on, so it is [`Mode::Idle`]: the node does not run, and the radio
//! is left as the host left it.

use crate::rnode::hosts::Transport;

/// How long a host has to stay gone before the board is a node again.
pub const HOLD_MS: u64 = 3_000;

/// Which of the two the board is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A host is present, on this transport (USB if both are): the board is
    /// its RNode, and the node neither listens nor sends.
    Rnode(Transport),
    /// No host, and a stored configuration: the board is a node of its own.
    Node,
    /// No host, and nothing stored to be a node on.
    Idle,
}

impl Mode {
    /// The word on the Home screen.
    pub const fn word(self) -> &'static str {
        match self {
            Mode::Rnode(_) => "RNode",
            Mode::Node => "node",
            Mode::Idle => "no radio config",
        }
    }

    /// Whether the node may run: listen, and later send.
    pub const fn is_node(self) -> bool {
        matches!(self, Mode::Node)
    }

    /// Whether a host is on the line to read what the RNode would tell it.
    pub const fn has_host(self) -> bool {
        matches!(self, Mode::Rnode(_))
    }
}

/// What the modem loop saw since it last asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Seen {
    /// The USB bus is configured and not suspended.
    pub usb_bus: bool,
    /// The KISS port's DTR line.
    pub usb_dtr: bool,
    /// A whole KISS frame arrived over USB.
    pub usb_frame: bool,
    /// A phone holds a link.
    pub ble_link: bool,
    /// A whole KISS frame arrived over the link.
    pub ble_frame: bool,
}

/// What changed, for the modem loop to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A host has spoken: the node stops, at once.
    HostArrived(Transport),
    /// The last host has been gone for [`HOLD_MS`]. With a stored
    /// configuration, the radio goes back to it and the node starts.
    HostLeft,
}

/// One transport's host, as far as the rule is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Side {
    /// It has spoken and has not been gone for long enough to be forgotten.
    present: bool,
    /// When it was last seen to go, while it is still counted present.
    gone_since: Option<u64>,
}

impl Side {
    /// `spoke`: a frame arrived. `attached`: whatever this transport says
    /// about a host still being there.
    fn update(&mut self, now_ms: u64, spoke: bool, attached: bool) {
        if spoke {
            self.present = true;
        }
        if !self.present {
            return;
        }
        if attached || spoke {
            self.gone_since = None;
            return;
        }
        let since = *self.gone_since.get_or_insert(now_ms);
        if now_ms.saturating_sub(since) >= HOLD_MS {
            *self = Side::default();
        }
    }
}

/// The rule, with the little it has to remember.
#[derive(Debug, Clone, Default)]
pub struct Presence {
    usb: Side,
    ble: Side,
    /// DTR has been up since the USB host last spoke. A host that never
    /// raises it -- pyserial can be told not to -- is then not ended by it
    /// being down, only by the bus going.
    dtr_seen: bool,
    /// Whether the board was the RNode at the last update.
    was_rnode: bool,
}

impl Presence {
    pub const fn new() -> Self {
        Self {
            usb: Side {
                present: false,
                gone_since: None,
            },
            ble: Side {
                present: false,
                gone_since: None,
            },
            dtr_seen: false,
            was_rnode: false,
        }
    }

    /// Take in what the loop saw at `now_ms`; says what changed, if anything.
    pub fn update(&mut self, now_ms: u64, seen: Seen) -> Option<Change> {
        if seen.usb_frame && seen.usb_dtr {
            self.dtr_seen = true;
        }
        if seen.usb_dtr && self.usb.present {
            self.dtr_seen = true;
        }
        let usb_attached = seen.usb_bus && (seen.usb_dtr || !self.dtr_seen);
        self.usb
            .update(now_ms, seen.usb_frame && seen.usb_bus, usb_attached);
        if !self.usb.present {
            self.dtr_seen = false;
        }
        self.ble
            .update(now_ms, seen.ble_frame && seen.ble_link, seen.ble_link);

        let host = self.host();
        let change = match (self.was_rnode, host) {
            (false, Some(transport)) => Some(Change::HostArrived(transport)),
            (true, None) => Some(Change::HostLeft),
            _ => None,
        };
        self.was_rnode = host.is_some();
        change
    }

    /// The host the board is the RNode of, if any. USB first when both are
    /// present, as the display lock already decides.
    pub fn host(&self) -> Option<Transport> {
        if self.usb.present {
            Some(Transport::Usb)
        } else if self.ble.present {
            Some(Transport::Bluetooth)
        } else {
            None
        }
    }

    /// The mode, given whether a configuration is stored.
    pub fn mode(&self, stored_config: bool) -> Mode {
        match self.host() {
            Some(transport) => Mode::Rnode(transport),
            None if stored_config => Mode::Node,
            None => Mode::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A computer with nothing open on the port.
    const PLUGGED: Seen = Seen {
        usb_bus: true,
        usb_dtr: false,
        usb_frame: false,
        ble_link: false,
        ble_frame: false,
    };
    /// `rnsd` with the port open, between frames.
    const OPEN: Seen = Seen {
        usb_dtr: true,
        ..PLUGGED
    };
    /// `rnsd` sending a frame.
    const SPEAKING: Seen = Seen {
        usb_frame: true,
        ..OPEN
    };
    const NOTHING: Seen = Seen {
        usb_bus: false,
        ..PLUGGED
    };
    const LINKED: Seen = Seen {
        ble_link: true,
        ..NOTHING
    };
    const PHONE_SPEAKING: Seen = Seen {
        ble_frame: true,
        ..LINKED
    };

    /// Run `steps` of (milliseconds later, what was seen), collecting every
    /// change with the time it happened.
    fn run(p: &mut Presence, start: u64, steps: &[(u64, Seen)]) -> Vec<(u64, Change)> {
        let mut t = start;
        let mut changes = Vec::new();
        for &(dt, seen) in steps {
            t += dt;
            if let Some(c) = p.update(t, seen) {
                changes.push((t, c));
            }
        }
        changes
    }

    /// `rnsd` opened the port and spoke at `t`.
    fn with_usb_host() -> Presence {
        let mut p = Presence::new();
        assert_eq!(
            p.update(1_000, SPEAKING),
            Some(Change::HostArrived(Transport::Usb))
        );
        p
    }

    #[test]
    fn nobody_there_is_a_node_if_there_is_a_configuration() {
        let p = Presence::new();
        assert_eq!(p.mode(true), Mode::Node);
        assert_eq!(p.mode(false), Mode::Idle);
    }

    #[test]
    fn charging_from_a_computer_is_not_a_host() {
        let mut p = Presence::new();
        assert_eq!(run(&mut p, 0, &[(10, PLUGGED); 1000]), []);
        assert_eq!(p.mode(true), Mode::Node);
    }

    #[test]
    fn a_port_opened_and_never_spoken_on_is_not_a_host() {
        let mut p = Presence::new();
        assert_eq!(run(&mut p, 0, &[(10, OPEN); 1000]), []);
        assert_eq!(p.mode(true), Mode::Node);
    }

    #[test]
    fn a_host_arrives_with_its_first_frame() {
        let mut p = Presence::new();
        assert_eq!(run(&mut p, 0, &[(10, OPEN), (10, OPEN)]), []);
        assert_eq!(
            p.update(30, SPEAKING),
            Some(Change::HostArrived(Transport::Usb))
        );
        assert_eq!(p.mode(true), Mode::Rnode(Transport::Usb));
        assert_eq!(p.mode(false), Mode::Rnode(Transport::Usb));
    }

    #[test]
    fn a_quiet_host_with_the_port_open_stays_a_host() {
        let mut p = with_usb_host();
        // An hour of nothing: rnsd with nothing to say.
        assert_eq!(run(&mut p, 1_000, &[(1_000, OPEN); 3600]), []);
        assert_eq!(p.host(), Some(Transport::Usb));
    }

    #[test]
    fn closing_the_port_ends_it_after_the_hold() {
        let mut p = with_usb_host();
        let changes = run(&mut p, 1_000, &[(50, PLUGGED); 100]);
        let [(at, Change::HostLeft)] = changes[..] else {
            panic!("{changes:?}");
        };
        // DTR fell at 1050; the hold runs from there.
        assert_eq!(at, 1_050 + HOLD_MS);
        assert_eq!(p.mode(true), Mode::Node);
    }

    #[test]
    fn pulling_the_cable_ends_it_after_the_hold() {
        let mut p = with_usb_host();
        let changes = run(&mut p, 1_000, &[(100, NOTHING); 40]);
        assert_eq!(changes, [(1_100 + HOLD_MS, Change::HostLeft)]);
    }

    #[test]
    fn a_cable_in_and_out_quickly_never_lets_the_node_in() {
        let mut p = with_usb_host();
        // Four times a second for ten seconds, never out for a whole hold.
        let flapping: Vec<(u64, Seen)> = (0..40)
            .map(|i| (250, if i % 2 == 0 { NOTHING } else { OPEN }))
            .collect();
        assert_eq!(run(&mut p, 1_000, &flapping), []);
        assert_eq!(p.host(), Some(Transport::Usb));
    }

    #[test]
    fn a_cable_flapping_with_no_host_never_makes_one() {
        let mut p = Presence::new();
        let flapping: Vec<(u64, Seen)> = (0..40)
            .map(|i| (250, if i % 2 == 0 { NOTHING } else { OPEN }))
            .collect();
        assert_eq!(run(&mut p, 0, &flapping), []);
        assert_eq!(p.mode(true), Mode::Node);
    }

    #[test]
    fn a_host_without_dtr_after_one_with_it_is_judged_on_its_own() {
        // rnsd raised DTR and left; then a host that never raises it speaks
        // once and goes quiet with the cable in. The first host's DTR must
        // not make the second one's low line read as it leaving.
        let mut p = with_usb_host();
        let changes = run(&mut p, 1_000, &[(100, PLUGGED); 40]);
        assert_eq!(changes.len(), 1, "the first host left: {changes:?}");
        let silent_dtr = Seen {
            usb_frame: true,
            ..PLUGGED
        };
        assert_eq!(
            p.update(10_000, silent_dtr),
            Some(Change::HostArrived(Transport::Usb))
        );
        assert_eq!(run(&mut p, 10_000, &[(1_000, PLUGGED); 10]), []);
        assert_eq!(p.host(), Some(Transport::Usb));
    }

    #[test]
    fn rnsd_restarting_inside_the_hold_is_the_same_host() {
        let mut p = with_usb_host();
        let mut steps = vec![(100, PLUGGED); 20]; // two seconds closed
        steps.extend([(100, OPEN), (100, SPEAKING)]);
        steps.extend([(1_000, OPEN); 10]);
        assert_eq!(run(&mut p, 1_000, &steps), []);
    }

    #[test]
    fn a_host_that_comes_back_after_the_hold_has_to_speak_again() {
        let mut p = with_usb_host();
        let mut steps = vec![(100, PLUGGED); 40]; // four seconds closed
        steps.extend([(100, OPEN); 10]); // reopened, silent
        let changes = run(&mut p, 1_000, &steps);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].1, Change::HostLeft);
        assert_eq!(p.host(), None, "an open port is not a host until it speaks");
        assert_eq!(
            p.update(10_000, SPEAKING),
            Some(Change::HostArrived(Transport::Usb))
        );
    }

    #[test]
    fn a_host_that_never_raises_dtr_is_ended_by_the_bus_alone() {
        let mut p = Presence::new();
        let silent_dtr = Seen {
            usb_frame: true,
            ..PLUGGED
        };
        assert_eq!(
            p.update(0, silent_dtr),
            Some(Change::HostArrived(Transport::Usb))
        );
        assert_eq!(run(&mut p, 0, &[(1_000, PLUGGED); 10]), []);
        assert_eq!(
            run(&mut p, 10_000, &[(100, NOTHING); 40]),
            [(10_100 + HOLD_MS, Change::HostLeft)]
        );
    }

    #[test]
    fn a_frame_without_a_bus_is_not_a_host() {
        // Bytes still draining from a buffer after the cable went.
        let mut p = Presence::new();
        let stale = Seen {
            usb_frame: true,
            ..NOTHING
        };
        assert_eq!(p.update(0, stale), None);
    }

    #[test]
    fn a_bonded_phone_in_range_is_not_a_host_until_it_speaks() {
        let mut p = Presence::new();
        assert_eq!(run(&mut p, 0, &[(100, LINKED); 50]), []);
        assert_eq!(p.mode(true), Mode::Node);
        assert_eq!(
            p.update(6_000, PHONE_SPEAKING),
            Some(Change::HostArrived(Transport::Bluetooth))
        );
        assert_eq!(p.mode(true), Mode::Rnode(Transport::Bluetooth));
    }

    #[test]
    fn a_phone_connecting_while_the_node_runs_stops_it_at_once() {
        // The node is mid-send when the phone's first frame comes: that pass
        // is the one that says so, not one a hold later.
        let mut p = Presence::new();
        assert!(p.mode(true).is_node());
        assert_eq!(
            p.update(500, PHONE_SPEAKING),
            Some(Change::HostArrived(Transport::Bluetooth))
        );
        assert!(!p.mode(true).is_node());
    }

    #[test]
    fn a_phone_walking_away_ends_it_after_the_hold() {
        let mut p = Presence::new();
        p.update(0, PHONE_SPEAKING);
        assert_eq!(
            run(&mut p, 0, &[(100, NOTHING); 40]),
            [(100 + HOLD_MS, Change::HostLeft)]
        );
    }

    #[test]
    fn with_both_the_board_is_usbs_and_stays_an_rnode_until_both_go() {
        let mut p = with_usb_host();
        let both = Seen {
            ble_link: true,
            ble_frame: true,
            ..OPEN
        };
        assert_eq!(p.update(1_100, both), None, "already an RNode");
        assert_eq!(p.host(), Some(Transport::Usb));
        // USB goes; the phone is still there.
        let phone_only = Seen {
            ble_link: true,
            ..NOTHING
        };
        assert_eq!(run(&mut p, 1_100, &[(500, phone_only); 20]), []);
        assert_eq!(p.host(), Some(Transport::Bluetooth));
        // Then the phone.
        let changes = run(&mut p, 20_000, &[(500, NOTHING); 20]);
        assert_eq!(changes, [(20_500 + HOLD_MS, Change::HostLeft)]);
    }

    #[test]
    fn the_words_fit_the_home_screen() {
        for mode in [Mode::Rnode(Transport::Usb), Mode::Node, Mode::Idle] {
            // A label of four and a space leave sixteen of twenty-one.
            assert!(mode.word().len() <= 16, "{mode:?}");
        }
    }
}
