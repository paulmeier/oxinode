//! The messages the board has received, and what it believes about each.
//!
//! A message comes here once the engine has delivered it and [`lxmf`] has
//! read it. What happens to it depends on whether the board knows the
//! sender's key, which it has if it has heard the sender announce:
//!
//! * **Known, and the signature holds:** kept, verified.
//! * **Known, and the signature fails:** not kept, and no proof is sent. The
//!   key is the one the sender's own announce carried, and the packet's
//!   encryption already rules out damage on the way, so this is somebody
//!   writing as somebody else.
//! * **Not known:** kept, marked unverified, and the board asks for the
//!   sender's path, whose answer is its announce and so its key. When that
//!   comes the message is checked, and is then verified or marked as failed
//!   for good. One that never hears an answer stays unverified.
//!
//! Every message kept is proved, so the sender shows it as delivered -- the
//! unverified ones too, because they were delivered.
//!
//! # Duplicates
//!
//! A sender that hears no proof sends the message again, as a new packet
//! with the same message hash. So messages are known by their hash: one seen
//! before is proved again, since the last proof was evidently lost, and is
//! not kept a second time. The hashes of the last [`SEEN`] messages are
//! remembered, including those no longer kept.
//!
//! # A bounded store, in RAM
//!
//! [`Messages::CAPACITY`] messages. When a new one arrives and it is full,
//! the oldest message already read goes, or the oldest of all if none has
//! been, counted. A message is read once the conversation screen has shown
//! it ([`Messages::read`]). Kept across a power cycle is #65.
//!
//! # Conversations
//!
//! The messages from one sender are a conversation with it: listed for the
//! Messages screen by [`Messages::conversations`], newest first, and drawn
//! one at a time by [`Messages::thread`].
//!
//! [`lxmf`]: super::lxmf

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::screens::{
    ConversationRow, Conversations, Direction, Entry, PeerName, Read, Thread, CONVERSATIONS,
};

use super::lxmf::Message;
use super::peers::Table;
use super::Heard;

/// How many message hashes are remembered, to know a retry when it comes.
pub const SEEN: usize = 64;

/// What the board believes about who sent a message: the screens' word for
/// it, since the thread draws it.
pub use crate::screens::Trust;

/// What [`Messages::receive`] did with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// A message the board did not have, kept, with what it believes of it.
    New(Trust),
    /// One it had: a retry, sent because a proof went missing. Kept once;
    /// with what the board believes of the one it has, or `None` if that
    /// one has since been let go.
    Duplicate(Option<Trust>),
    /// From a sender whose key the board knows, and not signed by it. Not
    /// kept and not proved.
    Forged,
}

impl Arrival {
    /// Whether to send the sender a delivery proof.
    pub fn prove(self) -> bool {
        !matches!(self, Arrival::Forged)
    }

    /// Whether to ask for the sender's path, and so its key. A duplicate of
    /// a message still unverified asks again, in case the first went
    /// unheard.
    pub fn ask_for_key(self) -> bool {
        matches!(
            self,
            Arrival::New(Trust::Unverified) | Arrival::Duplicate(Some(Trust::Unverified))
        )
    }
}

/// One message, as the board holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub message: Message,
    /// When it arrived, in milliseconds since boot.
    pub at_ms: u64,
    pub trust: Trust,
    pub unread: bool,
}

/// What hearing a sender's key settled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settled {
    pub verified: usize,
    pub failed: usize,
}

/// The messages, bounded. See the module docs.
#[derive(Debug, Default)]
pub struct Messages {
    kept: VecDeque<Received>,
    seen: VecDeque<[u8; 32]>,
    arrived: u32,
    dropped: u32,
    forged: u32,
}

impl Messages {
    /// How many messages it keeps.
    pub const CAPACITY: usize = 16;

    pub fn new() -> Self {
        Self::default()
    }

    /// A message that arrived at `at_ms`. `peers` has the keys of the
    /// senders the board has heard, and a sender it keeps a message from has
    /// a conversation there from now on.
    pub fn receive(&mut self, mut message: Message, peers: &mut Table, at_ms: u64) -> Arrival {
        if self.seen.contains(&message.hash) {
            let trust = self
                .kept
                .iter()
                .find(|r| r.message.hash == message.hash)
                .map(|r| r.trust);
            return Arrival::Duplicate(trust);
        }
        let trust = match peers.get(&message.source) {
            Some(peer) if message.verify(&peer.public_key) => Trust::Verified,
            Some(_) => {
                self.forged += 1;
                return Arrival::Forged;
            }
            None => Trust::Unverified,
        };
        if trust == Trust::Verified {
            message.settle();
        }
        peers.set_conversation(&message.source, true);
        if self.seen.len() == SEEN {
            self.seen.pop_front();
        }
        self.seen.push_back(message.hash);
        if self.kept.len() == Self::CAPACITY {
            let oldest = self.kept.iter().position(|r| !r.unread).unwrap_or(0);
            self.kept.remove(oldest);
            self.dropped += 1;
        }
        self.kept.push_back(Received {
            message,
            at_ms,
            trust,
            unread: true,
        });
        self.arrived = self.arrived.wrapping_add(1);
        Arrival::New(trust)
    }

    /// A verified announce: if it is the key of a sender whose messages are
    /// waiting for it, check them.
    pub fn heard(&mut self, heard: &Heard, peers: &mut Table) -> Settled {
        let mut settled = Settled::default();
        for received in &mut self.kept {
            if received.trust != Trust::Unverified || received.message.source != heard.destination {
                continue;
            }
            if received.message.verify(&heard.public_key) {
                received.trust = Trust::Verified;
                settled.verified += 1;
            } else {
                received.trust = Trust::Failed;
                settled.failed += 1;
            }
            received.message.settle();
        }
        if settled != Settled::default() {
            peers.set_conversation(&heard.destination, true);
        }
        settled
    }

    /// The messages kept, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Received> {
        self.kept.iter()
    }

    /// The message with this hash, if it is kept.
    pub fn get(&self, hash: &[u8; 32]) -> Option<&Received> {
        self.kept.iter().find(|r| r.message.hash == *hash)
    }

    pub fn len(&self) -> usize {
        self.kept.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// How many kept messages have not been read.
    pub fn unread(&self) -> usize {
        self.kept.iter().filter(|r| r.unread).count()
    }

    /// How many new messages have arrived since boot, wrapping: a number
    /// that moves exactly when one does, for the panel to wake on.
    pub fn arrived(&self) -> u32 {
        self.arrived
    }

    /// Messages let go to make room for newer ones.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Messages refused because a known sender did not sign them.
    pub fn forged(&self) -> u32 {
        self.forged
    }

    /// What the Messages screen shows at `now_ms`: a line for each sender
    /// with a message kept, the newest first, named as the peer table names
    /// it.
    pub fn conversations(&self, peers: &Table, now_ms: u64) -> Conversations {
        // Newest first: the kept messages are oldest first, so from the end,
        // each sender where its newest message is.
        let mut order: Vec<[u8; 16]> = Vec::new();
        for received in self.kept.iter().rev() {
            if !order.contains(&received.message.source) {
                order.push(received.message.source);
            }
        }
        let mut out = Conversations::default();
        for (slot, peer) in out.rows.iter_mut().zip(order.iter().take(CONVERSATIONS)) {
            let mine = || self.kept.iter().filter(|r| r.message.source == *peer);
            *slot = Some(ConversationRow {
                peer: *peer,
                name: name_of(peers, peer),
                unread: mine().any(|r| r.unread),
                age_s: mine()
                    .map(|r| r.at_ms)
                    .max()
                    .and_then(|at| age_s(at, now_ms)),
            });
        }
        out
    }

    /// The messages from `peer`, oldest first, as the thread draws them at
    /// `now_ms`. [`Messages::read`] counts in the same order.
    pub fn entries(&self, peer: &[u8; 16], now_ms: u64) -> Vec<Entry<'_>> {
        self.kept
            .iter()
            .filter(|r| r.message.source == *peer)
            .map(|r| Entry {
                direction: Direction::Received(r.trust),
                age_s: age_s(r.at_ms, now_ms),
                title: &r.message.title,
                content: &r.message.content,
                attachment: r.message.attachment,
                unread: r.unread,
            })
            .collect()
    }

    /// The thread with `peer`, over `entries` from [`Messages::entries`].
    pub fn thread<'a>(
        &self,
        peer: &[u8; 16],
        peers: &Table,
        entries: &'a [Entry<'a>],
    ) -> Thread<'a> {
        Thread {
            peer: *peer,
            name: name_of(peers, peer),
            entries,
        }
    }

    /// The panel has shown a conversation this far: its first `through`
    /// messages are read. Says how many that made read.
    pub fn read(&mut self, read: Read) -> usize {
        let mut marked = 0;
        for received in self
            .kept
            .iter_mut()
            .filter(|r| r.message.source == read.peer)
            .take(read.through)
        {
            if received.unread {
                received.unread = false;
                marked += 1;
            }
        }
        marked
    }
}

/// What the peer table calls a sender, if it has heard one.
fn name_of(peers: &Table, peer: &[u8; 16]) -> Option<PeerName> {
    peers
        .get(peer)
        .and_then(|p| p.name.as_deref())
        .map(PeerName::new)
}

/// Seconds from `at_ms` to `now_ms`, as the screens take an age.
fn age_s(at_ms: u64, now_ms: u64) -> Option<u32> {
    now_ms
        .checked_sub(at_ms)
        .map(|ms| (ms / 1_000).min(u64::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::lxmf::tests::{board, opened};
    use crate::node::Node;
    use crate::screens::{Direction, PeerName, Read};
    use rns_crypto::FixedRng;

    const ANNOUNCE: &[u8] = include_bytes!("vectors/announce.bin");
    const MESSAGE: &[u8] = include_bytes!("vectors/message.bin");
    const RETRY: &[u8] = include_bytes!("vectors/message_retry.bin");
    const FIELDS: &[u8] = include_bytes!("vectors/message_fields.bin");
    const STRANGER: &[u8] = include_bytes!("vectors/stranger_message.bin");
    const STRANGER_ANNOUNCE: &[u8] = include_bytes!("vectors/stranger_announce.bin");

    fn rng() -> FixedRng {
        FixedRng::new(&[0x5a; 64])
    }

    fn read(packet: &[u8]) -> Message {
        Message::unpack(&board().hashes().lxmf, &opened(packet)).unwrap()
    }

    /// What a node hears in an announce from the vectors.
    fn announce(packet: &[u8]) -> Heard {
        let mut out = Node::new(1757, 0, &board()).heard(packet, None, 1_000, &mut rng());
        out.heard.pop().unwrap()
    }

    /// A table that has heard the bench peer, the vectors' sender.
    fn knowing_the_sender() -> Table {
        let mut peers = Table::new();
        peers.heard(&announce(ANNOUNCE), 1_000);
        peers
    }

    #[test]
    fn a_message_from_a_known_peer_is_verified_kept_and_unread() {
        let mut peers = knowing_the_sender();
        let mut messages = Messages::new();
        let message = read(MESSAGE);
        let source = message.source;
        assert_eq!(
            messages.receive(message, &mut peers, 5_000),
            Arrival::New(Trust::Verified)
        );
        let kept = messages.iter().next().unwrap();
        assert_eq!(kept.at_ms, 5_000);
        assert!(kept.unread);
        assert!(kept.message.settled(), "verified: nothing more to check");
        assert_eq!(messages.unread(), 1);
        assert_eq!(messages.arrived(), 1);
        assert!(peers.get(&source).unwrap().conversation);
    }

    #[test]
    fn a_retry_is_proved_again_and_kept_once() {
        let mut peers = knowing_the_sender();
        let mut messages = Messages::new();
        messages.receive(read(MESSAGE), &mut peers, 5_000);
        let again = messages.receive(read(RETRY), &mut peers, 9_000);
        assert_eq!(again, Arrival::Duplicate(Some(Trust::Verified)));
        assert!(again.prove() && !again.ask_for_key());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages.arrived(), 1, "nothing new arrived");
        assert_eq!(messages.iter().next().unwrap().at_ms, 5_000);
    }

    #[test]
    fn a_stranger_is_kept_unverified_and_asked_for_until_its_key_comes() {
        let mut peers = Table::new();
        let mut messages = Messages::new();
        let arrival = messages.receive(read(STRANGER), &mut peers, 5_000);
        assert_eq!(arrival, Arrival::New(Trust::Unverified));
        assert!(arrival.prove(), "it was delivered");
        assert!(arrival.ask_for_key());
        assert!(!messages.iter().next().unwrap().message.settled());
        // A retry while still unverified asks again.
        let retry = messages.receive(read(STRANGER), &mut peers, 6_000);
        assert_eq!(retry, Arrival::Duplicate(Some(Trust::Unverified)));
        assert!(retry.ask_for_key());

        // Another peer's announce settles nothing.
        assert_eq!(
            messages.heard(&announce(ANNOUNCE), &mut peers),
            Settled::default()
        );
        // The stranger's own does.
        let key = announce(STRANGER_ANNOUNCE);
        peers.heard(&key, 7_000);
        assert_eq!(
            messages.heard(&key, &mut peers),
            Settled {
                verified: 1,
                failed: 0
            }
        );
        let kept = messages.iter().next().unwrap();
        assert_eq!(kept.trust, Trust::Verified);
        assert!(kept.message.settled());
        assert!(peers.get(&key.destination).unwrap().conversation);
        assert_eq!(messages.heard(&key, &mut peers), Settled::default(), "once");
    }

    #[test]
    fn a_message_whose_key_comes_and_does_not_sign_it_is_failed_for_good() {
        // The stranger's message, claiming the bench peer's address: the
        // board has not heard the peer, so it is kept and waits.
        let mut message = read(STRANGER);
        message.source = announce(ANNOUNCE).destination;
        let mut peers = Table::new();
        let mut messages = Messages::new();
        assert_eq!(
            messages.receive(message, &mut peers, 5_000),
            Arrival::New(Trust::Unverified)
        );
        let key = announce(ANNOUNCE);
        assert_eq!(
            messages.heard(&key, &mut peers),
            Settled {
                verified: 0,
                failed: 1
            }
        );
        assert_eq!(messages.iter().next().unwrap().trust, Trust::Failed);
        // Hearing the key again changes nothing: failed is for good.
        assert_eq!(messages.heard(&key, &mut peers), Settled::default());
    }

    #[test]
    fn a_known_sender_not_signing_is_forged_refused_and_not_proved() {
        let mut message = read(STRANGER);
        let mut peers = knowing_the_sender();
        message.source = announce(ANNOUNCE).destination;
        let mut messages = Messages::new();
        let arrival = messages.receive(message, &mut peers, 5_000);
        assert_eq!(arrival, Arrival::Forged);
        assert!(!arrival.prove() && !arrival.ask_for_key());
        assert!(messages.is_empty());
        assert_eq!(messages.forged(), 1);
        assert_eq!(messages.arrived(), 0);
        assert!(
            !peers
                .get(&announce(ANNOUNCE).destination)
                .unwrap()
                .conversation
        );
    }

    /// `n` distinct messages: the vectors' one, its hash changed.
    fn numbered(n: u8) -> Message {
        let mut message = read(FIELDS);
        message.hash[0] = n;
        message.hash[1] = 0xee;
        message
    }

    #[test]
    fn a_full_store_lets_the_oldest_read_message_go_first() {
        let mut peers = Table::new();
        let mut messages = Messages::new();
        for n in 0..Messages::CAPACITY as u8 {
            messages.receive(numbered(n), &mut peers, u64::from(n));
        }
        // Nothing is read: the oldest goes.
        messages.receive(numbered(100), &mut peers, 100);
        assert_eq!(messages.len(), Messages::CAPACITY);
        assert_eq!(messages.dropped(), 1);
        assert!(messages.get(&numbered(0).hash).is_none());
        // One is read: it goes before an older unread one.
        messages.kept[5].unread = false;
        let read_one = messages.kept[5].message.hash;
        messages.receive(numbered(101), &mut peers, 101);
        assert!(messages.get(&read_one).is_none());
        assert!(messages.get(&numbered(1).hash).is_some());
        assert_eq!(messages.unread(), Messages::CAPACITY);
    }

    /// Each sender is a conversation, the newest first, named as the table
    /// names it, marked while anything in it is unread, aged by its newest.
    #[test]
    fn senders_are_conversations_newest_first() {
        let mut peers = knowing_the_sender();
        let mut messages = Messages::new();
        let bench = read(MESSAGE);
        let source = bench.source;
        messages.receive(bench, &mut peers, 5_000);
        let stranger = read(STRANGER);
        let other = stranger.source;
        messages.receive(stranger, &mut peers, 9_000);
        let list = messages.conversations(&peers, 20_000);
        let rows: Vec<_> = list.rows.iter().flatten().collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].peer, other, "the newest first");
        assert_eq!(rows[0].name, None, "a stranger is not named");
        assert_eq!(rows[0].age_s, Some(11));
        assert_eq!(rows[1].peer, source);
        assert_eq!(rows[1].name, Some(PeerName::new("Bench peer")));
        assert!(rows[0].unread && rows[1].unread);

        // A newer message from the bench puts it first again.
        let mut again = numbered(1);
        again.source = source;
        messages.receive(again, &mut peers, 12_000);
        let list = messages.conversations(&peers, 20_000);
        assert_eq!(list.rows[0].unwrap().peer, source);
        assert_eq!(list.rows[0].unwrap().age_s, Some(8));
        assert!(list.rows[2].is_none());
    }

    /// A thread is the sender's messages, oldest first, as the store holds
    /// them; reading it marks them read in that order, and only theirs.
    #[test]
    fn a_thread_is_read_as_far_as_it_was_shown() {
        let mut peers = knowing_the_sender();
        let mut messages = Messages::new();
        let first = read(MESSAGE);
        let source = first.source;
        messages.receive(first, &mut peers, 5_000);
        let stranger = read(STRANGER);
        let other = stranger.source;
        messages.receive(stranger, &mut peers, 6_000);
        let mut second = numbered(2);
        second.source = source;
        second.content = "Second".into();
        messages.receive(second, &mut peers, 7_000);

        let entries = messages.entries(&source, 10_000);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].age_s, Some(5));
        assert_eq!(entries[1].content, "Second");
        assert_eq!(entries[0].direction, Direction::Received(Trust::Verified));
        assert!(entries.iter().all(|e| e.unread));
        let thread = messages.thread(&source, &peers, &entries);
        assert_eq!(thread.name, Some(PeerName::new("Bench peer")));
        assert_eq!(thread.entries.len(), 2);

        let read_one = Read {
            peer: source,
            through: 1,
        };
        assert_eq!(messages.read(read_one), 1);
        assert_eq!(messages.read(read_one), 0, "once");
        assert_eq!(messages.unread(), 2, "the second, and the stranger's");
        assert_eq!(
            messages.read(Read {
                peer: source,
                through: 5
            }),
            1
        );
        assert_eq!(messages.unread(), 1);
        assert!(messages.entries(&other, 10_000)[0].unread);
        let list = messages.conversations(&peers, 10_000);
        let bench = list.rows.iter().flatten().find(|r| r.peer == source);
        assert!(!bench.unwrap().unread);
    }

    #[test]
    fn a_retry_is_known_after_its_message_has_gone() {
        let mut peers = Table::new();
        let mut messages = Messages::new();
        messages.receive(numbered(0), &mut peers, 0);
        for n in 1..=Messages::CAPACITY as u8 {
            messages.receive(numbered(n), &mut peers, u64::from(n));
        }
        assert!(messages.get(&numbered(0).hash).is_none(), "let go");
        let gone = messages.receive(numbered(0), &mut peers, 50);
        assert_eq!(gone, Arrival::Duplicate(None));
        assert!(gone.prove());
        // Past what is remembered, it is new again.
        for n in 1..=SEEN as u8 {
            messages.receive(numbered(n + 100), &mut peers, 100);
        }
        assert_eq!(
            messages.receive(numbered(0), &mut peers, 200),
            Arrival::New(Trust::Unverified)
        );
    }
}
