//! The messages the board sends, and where each has got to.
//!
//! A message is packed and signed once, when it is written
//! ([`super::lxmf::pack`]), and queued here. From then on it is in one of
//! four states, which the thread shows ([`Delivery`]):
//!
//! * **Queued**, waiting for its turn on the channel; for the recipient's
//!   key, if the board has not heard it announce; or for a host to leave.
//! * **Sent**, on the air, waiting for the recipient's proof.
//! * **Delivered**: a proof came back, signed by the recipient's key, for
//!   one of the packets the message went in.
//! * **Failed**: no path, or no proof after every attempt. A failed message
//!   is kept, and *Retry Failed* queues it again.
//!
//! # No path
//!
//! The board encrypts to the recipient's key, which it has once it has
//! heard the recipient announce. Without it the message asks for the
//! recipient's path, whose answer is its announce, every [`PATH_ASK_MS`],
//! and fails after [`PATH_TIMEOUT_MS`] if no answer has come. A message is
//! never held forever.
//!
//! # Retries
//!
//! A message that went and heard no proof goes again, as a new packet
//! encrypted afresh and with the same message hash, which is how LXMF
//! retries and how the recipient knows a retry. It goes at most
//! [`MAX_ATTEMPTS`] times. After each it waits [`wait_ms`]: twice as long
//! each time, starting from [`RETRY_MS`] or from the announce budget's
//! quiet for a packet that long, whichever is the longer, so the retries of
//! one message spend no more of the channel than announcing does (see
//! [`announce`]). When the wait after the last attempt is over the message
//! has failed; [`give_up_ms`] is how long that takes. Every attempt waits
//! for a clear channel like any packet the board sends.
//!
//! A proof for any attempt counts, a late one included: a message that was
//! failed and then proved is delivered, because it was.
//!
//! # Under a host
//!
//! A host owns the radio, and the board sends nothing of its own under one.
//! When one arrives, every message not yet delivered or failed is held,
//! and says so; an attempt that had been handed to the radio and had not
//! gone is not counted. When the host has gone, the held messages are
//! queued again. A board with no host and no configuration stored is not a
//! node either ([`crate::mode`]), and holds its messages the same way.
//!
//! # Bounded
//!
//! [`Outbox::CAPACITY`] messages. A new one makes room by letting the oldest
//! delivered one go, then the oldest failed one; when every one is still on
//! its way, a new one is refused rather than one in flight dropped.
//!
//! [`announce`]: super::announce

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use rns_core::receipt::{validate_proof, ProofResult};

use crate::screens::{Delivery, Direction, Entry, Failure, Wait};

use super::announce::gap_ms;
use super::lxmf::Outbound;
use super::peers::Table;
use super::Proof;

/// How many times a message goes before it fails for want of a proof.
pub const MAX_ATTEMPTS: u8 = 3;

/// The least wait for a proof after the first attempt, for a short packet
/// on a fast channel. A proof comes back in a few seconds, through a phone
/// and its RNode as well as from a board; this is room for that several
/// times over. At the default settings the airtime rule asks for more.
pub const RETRY_MS: u64 = 20_000;

/// How often a message with no path asks for one.
pub const PATH_ASK_MS: u64 = 20_000;

/// How long a message with no path waits for one before it fails.
pub const PATH_TIMEOUT_MS: u64 = 60_000;

/// How soon an attempt that did not go -- refused by the radio, or given
/// up for a host about to speak -- is tried again.
pub const AGAIN_MS: u64 = 1_000;

/// The wait after attempt `attempt` (from one) that took `airtime_us` on
/// the air, before it is counted as unproved. See the module docs.
pub fn wait_ms(attempt: u8, airtime_us: u32) -> u64 {
    let base = RETRY_MS.max(gap_ms(airtime_us));
    base << attempt.saturating_sub(1).min(8)
}

/// How long after its first attempt a message whose proof never comes is
/// failed, if every attempt goes at once and takes `airtime_us`.
pub fn give_up_ms(airtime_us: u32) -> u64 {
    (1..=MAX_ATTEMPTS).map(|n| wait_ms(n, airtime_us)).sum()
}

/// One message the board sent, or is sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sending {
    pub message: Outbound,
    /// What it says, for the thread.
    pub content: String,
    /// When it was written, in milliseconds since boot.
    pub at_ms: u64,
    pub delivery: Delivery,
    /// Attempts that went.
    attempts: u8,
    /// When it next needs looking at; `u64::MAX` while an attempt is with
    /// the radio, or while it waits for nothing the clock brings.
    next_ms: u64,
    /// Since when it has waited for a path.
    asked_since_ms: u64,
    /// The hash of every packet it went in, for a proof of any of them.
    packets: Vec<[u8; 32]>,
}

impl Sending {
    /// How many times it has gone.
    pub fn attempts(&self) -> u8 {
        self.attempts
    }
}

/// What [`Outbox::due`] says is to be done now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Ask for the path to this address: a message to it has no key.
    Ask([u8; 16]),
    /// Send the message with this hash, to this address, whose key the
    /// table has. Answer with [`Outbox::went`].
    Send { hash: [u8; 32], to: [u8; 16] },
    /// The message with this hash, to this address, has failed.
    Failed {
        hash: [u8; 32],
        to: [u8; 16],
        why: Failure,
    },
}

/// Why a message was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Every message kept is still on its way.
    Full,
}

/// The messages, bounded. See the module docs.
#[derive(Debug, Default)]
pub struct Outbox {
    kept: Vec<Sending>,
    delivered: u32,
    failed: u32,
}

impl Outbox {
    /// How many messages it keeps.
    pub const CAPACITY: usize = 8;

    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `message`, which says `content`, written at `now_ms`; held
    /// from the start, and why, if the node is not running.
    pub fn queue(
        &mut self,
        message: Outbound,
        content: String,
        now_ms: u64,
        held: Option<Wait>,
    ) -> Result<(), Refused> {
        if self.kept.len() == Self::CAPACITY {
            let done = |d: &Delivery| matches!(d, Delivery::Delivered);
            let failed = |d: &Delivery| matches!(d, Delivery::Failed(_));
            let oldest = self
                .kept
                .iter()
                .position(|s| done(&s.delivery))
                .or_else(|| self.kept.iter().position(|s| failed(&s.delivery)))
                .ok_or(Refused::Full)?;
            self.kept.remove(oldest);
        }
        self.kept.push(Sending {
            message,
            content,
            at_ms: now_ms,
            delivery: Delivery::Queued(held.unwrap_or(Wait::Turn)),
            attempts: 0,
            next_ms: if held.is_some() { u64::MAX } else { now_ms },
            asked_since_ms: now_ms,
            packets: Vec::new(),
        });
        Ok(())
    }

    /// The next thing to do at `now_ms`, oldest message first, with the
    /// keys `peers` has. A message handed out to send is not handed out
    /// again until [`Outbox::went`] says what became of it.
    pub fn due(&mut self, now_ms: u64, peers: &Table) -> Option<Due> {
        for s in &mut self.kept {
            let to = s.message.destination;
            let known = peers.get(&to).is_some();
            match s.delivery {
                Delivery::Queued(Wait::Turn) if now_ms >= s.next_ms => {
                    if known {
                        s.next_ms = u64::MAX;
                        return Some(Due::Send {
                            hash: s.message.hash,
                            to,
                        });
                    }
                    s.delivery = Delivery::Queued(Wait::Path);
                    s.asked_since_ms = now_ms;
                    s.next_ms = now_ms + PATH_ASK_MS;
                    return Some(Due::Ask(to));
                }
                Delivery::Queued(Wait::Path) => {
                    if known {
                        s.delivery = Delivery::Queued(Wait::Turn);
                        s.next_ms = u64::MAX;
                        return Some(Due::Send {
                            hash: s.message.hash,
                            to,
                        });
                    }
                    if now_ms.saturating_sub(s.asked_since_ms) >= PATH_TIMEOUT_MS {
                        s.delivery = Delivery::Failed(Failure::NoPath);
                        self.failed += 1;
                        return Some(Due::Failed {
                            hash: s.message.hash,
                            to,
                            why: Failure::NoPath,
                        });
                    }
                    if now_ms >= s.next_ms {
                        s.next_ms = now_ms + PATH_ASK_MS;
                        return Some(Due::Ask(to));
                    }
                }
                Delivery::Sent(n) if now_ms >= s.next_ms => {
                    if n >= MAX_ATTEMPTS {
                        s.delivery = Delivery::Failed(Failure::NoProof);
                        s.next_ms = u64::MAX;
                        self.failed += 1;
                        return Some(Due::Failed {
                            hash: s.message.hash,
                            to,
                            why: Failure::NoProof,
                        });
                    }
                    if known {
                        s.next_ms = u64::MAX;
                        return Some(Due::Send {
                            hash: s.message.hash,
                            to,
                        });
                    }
                    // The key has gone from the table since the last attempt:
                    // ask again, as for a message that never had it.
                    s.delivery = Delivery::Queued(Wait::Path);
                    s.asked_since_ms = now_ms;
                    s.next_ms = now_ms + PATH_ASK_MS;
                    return Some(Due::Ask(to));
                }
                _ => {}
            }
        }
        None
    }

    /// The message with this hash, as packed, to build an attempt from.
    pub fn get(&self, hash: &[u8; 32]) -> Option<&Sending> {
        self.kept.iter().find(|s| s.message.hash == *hash)
    }

    /// What became of an attempt [`Outbox::due`] handed out at `now_ms`:
    /// the packet it went in and `Some(airtime)` if it went, `None` if it
    /// did not -- then it waits [`AGAIN_MS`] and is tried again, uncounted.
    pub fn went(
        &mut self,
        hash: &[u8; 32],
        packet_hash: &[u8; 32],
        airtime_us: Option<u32>,
        now_ms: u64,
    ) {
        let Some(s) = self.kept.iter_mut().find(|s| s.message.hash == *hash) else {
            return;
        };
        // Held, or proved, while the radio had it: what it is now stands.
        if !matches!(s.delivery, Delivery::Queued(Wait::Turn) | Delivery::Sent(_)) {
            if airtime_us.is_some() {
                s.packets.push(*packet_hash);
            }
            return;
        }
        match airtime_us {
            Some(airtime) => {
                s.attempts = s.attempts.saturating_add(1);
                s.packets.push(*packet_hash);
                s.delivery = Delivery::Sent(s.attempts);
                s.next_ms = now_ms + wait_ms(s.attempts, airtime);
            }
            None => s.next_ms = now_ms + AGAIN_MS,
        }
    }

    /// A message that could not be made into a packet at all: a key the
    /// table holds that is not a key. It fails, as for no path.
    pub fn unbuildable(&mut self, hash: &[u8; 32]) {
        if let Some(s) = self.kept.iter_mut().find(|s| s.message.hash == *hash) {
            s.delivery = Delivery::Failed(Failure::NoPath);
            s.next_ms = u64::MAX;
            self.failed += 1;
        }
    }

    /// A proof heard on the air: if it is for a packet a message went in,
    /// and the recipient's key in `peers` signed it, that message is
    /// delivered. Says which, the first time.
    pub fn proved(&mut self, proof: &Proof, peers: &Table) -> Option<([u8; 32], [u8; 16])> {
        let (s, packet) = self.kept.iter_mut().find_map(|s| {
            let packet = *s.packets.iter().find(|p| p[..16] == proof.packet)?;
            (s.delivery != Delivery::Delivered).then_some((s, packet))
        })?;
        let key = peers.get(&s.message.destination)?.public_key;
        let identity = rns_crypto::identity::Identity::from_public_key(&key).ok()?;
        if validate_proof(&proof.data, &packet, &identity) != ProofResult::Valid {
            return None;
        }
        s.delivery = Delivery::Delivered;
        s.next_ms = u64::MAX;
        self.delivered += 1;
        Some((s.message.hash, s.message.destination))
    }

    /// The node has stopped, for `why` -- a host, or no configuration:
    /// hold everything still on its way. Says how many.
    pub fn hold(&mut self, why: Wait) -> usize {
        let mut held = 0;
        for s in &mut self.kept {
            if matches!(s.delivery, Delivery::Queued(_) | Delivery::Sent(_)) {
                s.delivery = Delivery::Queued(why);
                s.next_ms = u64::MAX;
                held += 1;
            }
        }
        held
    }

    /// The node runs again: queue again what was held. Says how many.
    pub fn resume(&mut self, now_ms: u64) -> usize {
        let mut resumed = 0;
        for s in &mut self.kept {
            if matches!(s.delivery, Delivery::Queued(Wait::Host | Wait::Idle)) {
                s.delivery = Delivery::Queued(Wait::Turn);
                s.next_ms = now_ms;
                resumed += 1;
            }
        }
        resumed
    }

    /// *Retry Failed* in the conversation with `peer`: its failed messages
    /// are queued again, with every attempt still to come. Says how many.
    pub fn retry(&mut self, peer: &[u8; 16], now_ms: u64, held: Option<Wait>) -> usize {
        let mut again = 0;
        for s in &mut self.kept {
            if s.message.destination == *peer && matches!(s.delivery, Delivery::Failed(_)) {
                s.delivery = Delivery::Queued(held.unwrap_or(Wait::Turn));
                s.attempts = 0;
                s.next_ms = if held.is_some() { u64::MAX } else { now_ms };
                s.asked_since_ms = now_ms;
                again += 1;
            }
        }
        again
    }

    /// The messages kept, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Sending> {
        self.kept.iter()
    }

    pub fn len(&self) -> usize {
        self.kept.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// Messages delivered since boot.
    pub fn delivered(&self) -> u32 {
        self.delivered
    }

    /// Messages failed since boot, counted each time one fails.
    pub fn failed(&self) -> u32 {
        self.failed
    }

    /// Messages to `peer`, oldest first, as the thread draws them at
    /// `now_ms`, each with when it was written.
    pub fn entries(&self, peer: &[u8; 16], now_ms: u64) -> impl Iterator<Item = (u64, Entry<'_>)> {
        let peer = *peer;
        self.kept
            .iter()
            .filter(move |s| s.message.destination == peer)
            .map(move |s| {
                (
                    s.at_ms,
                    Entry {
                        direction: Direction::Sent(s.delivery),
                        age_s: super::messages::age_s(s.at_ms, now_ms),
                        title: "",
                        content: &s.content,
                        attachment: false,
                        unread: false,
                    },
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::lxmf::tests::board;
    use crate::node::{Heard, Node};
    use rns_crypto::FixedRng;

    const ANNOUNCE: &[u8] = include_bytes!("vectors/announce.bin");

    fn rng() -> FixedRng {
        FixedRng::new(&[0x5a; 64])
    }

    /// The bench peer, as the board hears it announce.
    fn bench() -> Heard {
        let mut out = Node::new(1757, 0, &board()).heard(ANNOUNCE, None, 1_000, &mut rng());
        out.heard.pop().unwrap()
    }

    fn knowing_the_bench() -> Table {
        let mut peers = Table::new();
        peers.heard(&bench(), 1_000);
        peers
    }

    /// A message from the board to `to`, saying `content`.
    fn message(to: [u8; 16], content: &str) -> Outbound {
        let key = board();
        super::super::lxmf::pack(&key.identity(), &key.hashes().lxmf, &to, 0, content).unwrap()
    }

    /// An airtime with a short gap: under the least wait.
    const AIR: u32 = 100_000;

    fn send(outbox: &mut Outbox, peers: &Table, now: u64) -> [u8; 32] {
        match outbox.due(now, peers) {
            Some(Due::Send { hash, .. }) => hash,
            other => panic!("expected a send at {now} ms, got {other:?}"),
        }
    }

    fn packet(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn state(outbox: &Outbox) -> Delivery {
        outbox.iter().next().unwrap().delivery
    }

    #[test]
    fn the_wait_doubles_and_is_never_under_the_announce_budget() {
        assert_eq!(wait_ms(1, AIR), RETRY_MS);
        assert_eq!(wait_ms(2, AIR), 2 * RETRY_MS);
        assert_eq!(wait_ms(3, AIR), 4 * RETRY_MS);
        assert_eq!(
            give_up_ms(AIR),
            7 * RETRY_MS,
            "140 s at the default settings"
        );
        // A packet whose quiet is longer than the least wait waits that.
        let slow = 2_000_000; // 2 s on the air: 100 s of quiet
        assert_eq!(wait_ms(1, slow), gap_ms(slow));
        assert_eq!(wait_ms(3, slow), 4 * gap_ms(slow));
        assert_eq!(wait_ms(255, AIR), RETRY_MS << 8, "bounded, not an overflow");
    }

    /// The figures the docs give: at the default settings a canned reply,
    /// 211 bytes on the air, waits 30, 59 and 119 s, and fails 208 s after
    /// it first went; the longest message, one packet of 499 bytes, 484 s.
    /// The airtime rule, not the least wait, sets them.
    #[test]
    fn at_the_default_settings_the_airtime_sets_the_waits() {
        use crate::lr1121::config::{ValidConfig, DEFAULT};
        use crate::node::announce::airtime_us;
        let config = ValidConfig::new(DEFAULT).unwrap();
        let reply = airtime_us(&config, 211).unwrap();
        assert_eq!(
            (1..=MAX_ATTEMPTS)
                .map(|n| wait_ms(n, reply) / 1_000)
                .collect::<Vec<_>>(),
            [29, 59, 118]
        );
        assert_eq!(give_up_ms(reply) / 1_000, 208);
        let longest = airtime_us(&config, 499).unwrap();
        assert_eq!(give_up_ms(longest) / 1_000, 484);
        assert!(wait_ms(1, reply) > RETRY_MS);
    }

    #[test]
    fn a_message_goes_at_once_and_waits_for_its_proof() {
        let peers = knowing_the_bench();
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "hi"), "hi".into(), 5_000, None)
            .unwrap();
        assert_eq!(state(&outbox), Delivery::Queued(Wait::Turn));
        let hash = send(&mut outbox, &peers, 5_000);
        assert_eq!(outbox.due(5_000, &peers), None, "handed out once");
        outbox.went(&hash, &packet(1), Some(AIR), 6_000);
        assert_eq!(state(&outbox), Delivery::Sent(1));
        assert_eq!(outbox.due(6_000 + RETRY_MS - 1, &peers), None);
        assert_eq!(send(&mut outbox, &peers, 6_000 + RETRY_MS), hash, "again");
    }

    #[test]
    fn an_attempt_that_did_not_go_is_tried_again_and_not_counted() {
        let peers = knowing_the_bench();
        let mut outbox = Outbox::new();
        outbox
            .queue(message(bench().destination, "hi"), "hi".into(), 0, None)
            .unwrap();
        let hash = send(&mut outbox, &peers, 0);
        outbox.went(&hash, &packet(1), None, 10);
        assert_eq!(state(&outbox), Delivery::Queued(Wait::Turn));
        assert_eq!(outbox.due(10 + AGAIN_MS - 1, &peers), None);
        send(&mut outbox, &peers, 10 + AGAIN_MS);
        outbox.went(&hash, &packet(2), Some(AIR), 2_000);
        assert_eq!(outbox.iter().next().unwrap().attempts(), 1);
    }

    #[test]
    fn with_no_proof_it_fails_after_every_attempt_and_its_waits() {
        let peers = knowing_the_bench();
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "hi"), "hi".into(), 0, None)
            .unwrap();
        let mut now = 0;
        for n in 1..=MAX_ATTEMPTS {
            let hash = send(&mut outbox, &peers, now);
            outbox.went(&hash, &packet(n), Some(AIR), now);
            assert_eq!(state(&outbox), Delivery::Sent(n));
            now += wait_ms(n, AIR);
        }
        assert_eq!(now, give_up_ms(AIR));
        assert_eq!(outbox.due(now - 1, &peers), None);
        let hash = outbox.iter().next().unwrap().message.hash;
        assert_eq!(
            outbox.due(now, &peers),
            Some(Due::Failed {
                hash,
                to,
                why: Failure::NoProof
            })
        );
        assert_eq!(state(&outbox), Delivery::Failed(Failure::NoProof));
        assert_eq!(
            outbox.due(now + 1_000_000, &peers),
            None,
            "failed stays failed"
        );
        assert_eq!(outbox.failed(), 1);
    }

    #[test]
    fn with_no_key_it_asks_for_the_path_and_fails_when_none_comes() {
        let none = Table::new();
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "hi"), "hi".into(), 0, None)
            .unwrap();
        assert_eq!(outbox.due(0, &none), Some(Due::Ask(to)));
        assert_eq!(state(&outbox), Delivery::Queued(Wait::Path));
        assert_eq!(outbox.due(PATH_ASK_MS - 1, &none), None);
        assert_eq!(
            outbox.due(PATH_ASK_MS, &none),
            Some(Due::Ask(to)),
            "asked again"
        );
        assert_eq!(outbox.due(2 * PATH_ASK_MS, &none), Some(Due::Ask(to)));
        assert!(matches!(
            outbox.due(PATH_TIMEOUT_MS, &none),
            Some(Due::Failed {
                why: Failure::NoPath,
                ..
            })
        ));
        assert_eq!(state(&outbox), Delivery::Failed(Failure::NoPath));
        assert_eq!(outbox.due(PATH_TIMEOUT_MS + 1, &none), None);
    }

    #[test]
    fn the_key_arriving_sends_it_at_once() {
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "hi"), "hi".into(), 0, None)
            .unwrap();
        assert_eq!(outbox.due(0, &Table::new()), Some(Due::Ask(to)));
        // The answer comes before the next ask is due.
        send(&mut outbox, &knowing_the_bench(), 3_000);
    }

    #[test]
    fn a_host_holds_everything_on_its_way_and_its_going_queues_it_again() {
        let peers = knowing_the_bench();
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "one"), "one".into(), 0, None)
            .unwrap();
        outbox
            .queue(message(to, "two"), "two".into(), 0, None)
            .unwrap();
        let one = send(&mut outbox, &peers, 0);
        outbox.went(&one, &packet(1), Some(AIR), 100);
        // The second is with the radio when the host's first frame comes.
        let two = send(&mut outbox, &peers, 200);
        assert_eq!(outbox.hold(Wait::Host), 2);
        outbox.went(&two, &packet(2), None, 300);
        assert!(outbox
            .iter()
            .all(|s| s.delivery == Delivery::Queued(Wait::Host)));
        assert_eq!(outbox.due(10_000_000, &peers), None, "nothing under a host");
        // Written under the host: held from the start.
        outbox
            .queue(message(to, "three"), "three".into(), 400, Some(Wait::Host))
            .unwrap();
        assert_eq!(
            outbox.iter().last().unwrap().delivery,
            Delivery::Queued(Wait::Host)
        );

        assert_eq!(outbox.resume(20_000), 3);
        assert_eq!(send(&mut outbox, &peers, 20_000), one);
        assert_eq!(send(&mut outbox, &peers, 20_000), two);
        let two_state = outbox.get(&two).unwrap();
        assert_eq!(
            two_state.attempts(),
            0,
            "the attempt that did not go is not counted"
        );
        assert_eq!(
            outbox.get(&one).unwrap().attempts(),
            1,
            "the one that went is"
        );
    }

    #[test]
    fn retry_failed_queues_a_conversations_failed_messages_with_every_attempt() {
        let peers = knowing_the_bench();
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "hi"), "hi".into(), 0, None)
            .unwrap();
        outbox
            .queue(message([9; 16], "other"), "other".into(), 0, None)
            .unwrap();
        // The other has no key and fails for it; the bench one for no proof.
        let mut now = 0;
        while outbox
            .iter()
            .any(|s| !matches!(s.delivery, Delivery::Failed(_)))
        {
            if let Some(Due::Send { hash, .. }) = outbox.due(now, &peers) {
                outbox.went(&hash, &packet(now as u8), Some(AIR), now);
            }
            now += 1_000;
        }
        assert_eq!(outbox.retry(&to, now, None), 1);
        assert_eq!(state(&outbox), Delivery::Queued(Wait::Turn));
        assert_eq!(outbox.iter().next().unwrap().attempts(), 0);
        assert_eq!(
            outbox.iter().nth(1).unwrap().delivery,
            Delivery::Failed(Failure::NoPath),
            "another conversation's is left"
        );
        send(&mut outbox, &peers, now);
        assert_eq!(outbox.retry(&to, now, None), 0, "nothing failed now");
        // Under a host, a retry is held.
        let mut held = Outbox::new();
        held.queue(message(to, "x"), "x".into(), 0, None).unwrap();
        let x = held.iter().next().unwrap().message.hash;
        held.unbuildable(&x);
        assert_eq!(held.retry(&to, 0, Some(Wait::Host)), 1);
        assert_eq!(state(&held), Delivery::Queued(Wait::Host));
    }

    #[test]
    fn a_full_outbox_lets_the_oldest_done_go_and_refuses_when_all_are_on_their_way() {
        let to = bench().destination;
        let mut outbox = Outbox::new();
        for n in 0..Outbox::CAPACITY {
            let text = format!("m{n}");
            outbox.queue(message(to, &text), text, 0, None).unwrap();
        }
        assert_eq!(
            outbox.queue(message(to, "more"), "more".into(), 0, None),
            Err(Refused::Full)
        );
        // The third fails, the fifth is delivered: the delivered goes first.
        let third = outbox.iter().nth(2).unwrap().message.hash;
        let fifth = outbox.iter().nth(4).unwrap().message.hash;
        outbox.unbuildable(&third);
        outbox.kept[4].delivery = Delivery::Delivered;
        outbox
            .queue(message(to, "more"), "more".into(), 1, None)
            .unwrap();
        assert!(outbox.get(&fifth).is_none() && outbox.get(&third).is_some());
        outbox
            .queue(message(to, "again"), "again".into(), 2, None)
            .unwrap();
        assert!(outbox.get(&third).is_none());
        assert_eq!(outbox.len(), Outbox::CAPACITY);
    }

    #[test]
    fn its_entries_are_the_threads_sent_messages() {
        let to = bench().destination;
        let mut outbox = Outbox::new();
        outbox
            .queue(message(to, "hi"), "hi".into(), 5_000, None)
            .unwrap();
        outbox
            .queue(message([9; 16], "x"), "x".into(), 5_000, None)
            .unwrap();
        let entries: Vec<_> = outbox.entries(&to, 65_000).collect();
        assert_eq!(entries.len(), 1);
        let (at, e) = entries[0];
        assert_eq!(at, 5_000);
        assert_eq!(e.direction, Direction::Sent(Delivery::Queued(Wait::Turn)));
        assert_eq!((e.content, e.age_s, e.unread), ("hi", Some(60), false));
    }
}
