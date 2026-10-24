//! Who else is around: the `lxmf.delivery` announces the node heard, kept
//! as peers.
//!
//! Every announce the engine verifies comes here as a [`Heard`]. The ones for
//! an LXMF delivery address -- what Sideband and MeshChat announce, and what
//! a message is sent to -- become a [`Peer`] or bring one up to date; the
//! rest (propagation nodes, NomadNet pages, telemetry) are somebody else's
//! business and are passed over.
//!
//! # A bounded table
//!
//! [`Table::CAPACITY`] peers, on the heap, held in RAM. When a stranger is
//! heard and the table is full, the peer heard longest ago goes, unless it
//! has a conversation: a peer somebody has written to or heard from is never
//! pushed out for one nobody has. If every peer has one, the stranger is not
//! kept, and that is counted. Nothing sets a conversation yet; the messages
//! that do are the next pieces of the node.
//!
//! # Names
//!
//! A peer's name is LXMF's display name from its announce's app data, read
//! the way LXMF's own `display_name_from_app_data` reads it ([`display_name`]).
//! It is kept to [`NAME_MAX`] bytes, cut on a character boundary.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::rnode::air::Signal;
use crate::screens::{PeerName, PeerRow, Peers};

use super::Heard;

/// The most bytes of a display name kept: as much as any screen shows, and
/// more than most names are.
pub const NAME_MAX: usize = 32;

/// One peer: what its last `lxmf.delivery` announce said, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// Its LXMF address, what a message to it is sent to.
    pub destination: [u8; 16],
    /// The key its announces are signed with, and messages to it encrypted to.
    pub public_key: [u8; 64],
    /// Its display name, if its announce carried one.
    pub name: Option<String>,
    /// When the board last heard it, in milliseconds since boot.
    pub last_heard_ms: u64,
    /// How many hops its last announce had come, counting the one onto the
    /// board's radio: one is straight off the air.
    pub hops: Option<u8>,
    /// The signal its last announce came with, if it came straight off the
    /// air. A relayed announce's signal is the relay's, and is not the
    /// peer's.
    pub signal: Option<Signal>,
    /// It has a conversation, and is never evicted for a stranger.
    pub conversation: bool,
}

/// What [`Table::heard`] did with an announce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Learned {
    /// Not an `lxmf.delivery` announce, so not a peer.
    NotLxmf,
    /// A peer the table did not have.
    New,
    /// A peer it had, brought up to date.
    Updated,
    /// A new peer, in place of the one at this address, heard longest ago.
    Evicted([u8; 16]),
    /// A new peer, not kept: the table is full of peers with conversations.
    Refused,
}

/// The peers, bounded. See the module docs.
#[derive(Debug)]
pub struct Table {
    peers: Vec<Peer>,
    lxmf: [u8; 10],
    refused: u32,
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
}

impl Table {
    /// How many peers it holds: as many as the Peers screen lists.
    pub const CAPACITY: usize = crate::screens::PEERS;

    pub fn new() -> Self {
        Self {
            peers: Vec::new(),
            lxmf: rns_core::destination::name_hash("lxmf", &["delivery"]),
            refused: 0,
        }
    }

    /// A verified announce, heard at `at_ms`.
    pub fn heard(&mut self, heard: &Heard, at_ms: u64) -> Learned {
        if heard.name_hash != self.lxmf {
            return Learned::NotLxmf;
        }
        let name = display_name(heard.app_data.as_deref());
        if let Some(peer) = self
            .peers
            .iter_mut()
            .find(|p| p.destination == heard.destination)
        {
            peer.public_key = heard.public_key;
            peer.name = name;
            peer.last_heard_ms = at_ms;
            peer.hops = Some(heard.hops);
            peer.signal = heard.signal;
            return Learned::Updated;
        }
        let peer = Peer {
            destination: heard.destination,
            public_key: heard.public_key,
            name,
            last_heard_ms: at_ms,
            hops: Some(heard.hops),
            signal: heard.signal,
            conversation: false,
        };
        if self.peers.len() < Self::CAPACITY {
            self.peers.push(peer);
            return Learned::New;
        }
        let oldest = self
            .peers
            .iter_mut()
            .filter(|p| !p.conversation)
            .min_by_key(|p| p.last_heard_ms);
        match oldest {
            Some(slot) => Learned::Evicted(core::mem::replace(slot, peer).destination),
            None => {
                self.refused += 1;
                Learned::Refused
            }
        }
    }

    /// Mark a peer as having a conversation, or not. Says whether the table
    /// has it.
    pub fn set_conversation(&mut self, destination: &[u8; 16], on: bool) -> bool {
        match self
            .peers
            .iter_mut()
            .find(|p| p.destination == *destination)
        {
            Some(peer) => {
                peer.conversation = on;
                true
            }
            None => false,
        }
    }

    /// The peer at an address.
    pub fn get(&self, destination: &[u8; 16]) -> Option<&Peer> {
        self.peers.iter().find(|p| p.destination == *destination)
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Strangers not kept because every peer had a conversation.
    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// What the Peers screen shows at `now_ms`: every peer, the one heard
    /// most recently first.
    pub fn screen(&self, now_ms: u64) -> Peers {
        let mut order: Vec<&Peer> = self.peers.iter().collect();
        order.sort_by_key(|p| core::cmp::Reverse(p.last_heard_ms));
        let mut out = Peers::default();
        for (slot, peer) in out.rows.iter_mut().zip(order) {
            *slot = Some(PeerRow {
                destination: peer.destination,
                name: peer.name.as_deref().map(PeerName::new),
                age_s: now_ms
                    .checked_sub(peer.last_heard_ms)
                    .map(|ms| (ms / 1_000).min(u64::from(u32::MAX)) as u32),
                hops: peer.hops,
                signal: peer.signal,
            });
        }
        out
    }
}

/// The display name in an `lxmf.delivery` announce's app data, as LXMF's
/// `display_name_from_app_data` reads it, cut to [`NAME_MAX`] bytes.
///
/// Since LXMF 0.5.0 the app data is a msgpack array whose first element is
/// the name as bytes, or nil; before that it was the name itself, as UTF-8.
/// LXMF tells the two apart by the first byte: a msgpack array header, fixed
/// or 16-bit, is the new form. Anything that does not read -- an element of
/// another type, bytes that are not UTF-8, an array cut short -- is no name,
/// where LXMF would log an error and say the same. One difference: a name
/// that is empty once its NULs and surrounding spaces are gone is no name
/// here, where LXMF hands back the empty string, because a blank row is
/// not a name anybody can read.
pub fn display_name(app_data: Option<&[u8]>) -> Option<String> {
    let data = app_data?;
    let first = *data.first()?;
    let raw: &[u8] = if (0x90..=0x9f).contains(&first) || first == 0xdc {
        let (count, rest) = match first {
            0xdc => (
                u16::from_be_bytes(data.get(1..3)?.try_into().ok()?) as usize,
                &data[3..],
            ),
            n => ((n & 0x0f) as usize, &data[1..]),
        };
        if count == 0 {
            return None;
        }
        // The first element: bytes, or nil for no name.
        let (len, body) = match *rest.first()? {
            0xc4 => (usize::from(*rest.get(1)?), rest.get(2..)?),
            0xc5 => (
                u16::from_be_bytes(rest.get(1..3)?.try_into().ok()?) as usize,
                rest.get(3..)?,
            ),
            0xc6 => (
                u32::from_be_bytes(rest.get(1..5)?.try_into().ok()?) as usize,
                rest.get(5..)?,
            ),
            _ => return None,
        };
        body.get(..len)?
    } else {
        data
    };
    let text = core::str::from_utf8(raw).ok()?;
    let clean: String = text.chars().filter(|&c| c != '\0').collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    let mut end = clean.len().min(NAME_MAX);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    Some(String::from(&clean[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::lxmf_app_data;
    use alloc::vec;

    const VECTORS: &str = include_str!("vectors/vectors.txt");

    fn vector(name: &str) -> Vec<u8> {
        let line = VECTORS
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .unwrap();
        (0..line.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect()
    }

    fn lxmf() -> [u8; 10] {
        rns_core::destination::name_hash("lxmf", &["delivery"])
    }

    /// A verified `lxmf.delivery` announce from the peer at `n`.
    fn heard(n: u8, name: Option<&str>) -> Heard {
        Heard {
            destination: [n; 16],
            identity: [n; 16],
            public_key: [n; 64],
            name_hash: lxmf(),
            app_data: name.map(lxmf_app_data),
            hops: 1,
            signal: Some(Signal {
                rssi_dbm: -60 - i16::from(n),
                snr_quarter_db: 20,
            }),
        }
    }

    #[test]
    fn the_name_python_put_in_is_the_name_read_out() {
        // The vectors' app data is LXMF's own, from `tools/node_vectors.py`.
        assert_eq!(
            display_name(Some(&vector("app_data"))).as_deref(),
            Some("Bench peer")
        );
    }

    #[test]
    fn names_are_read_as_lxmf_reads_them() {
        let name = |d: &[u8]| display_name(Some(d));
        assert_eq!(name(&lxmf_app_data("Ana")).as_deref(), Some("Ana"));
        // The original format: the app data is the name.
        assert_eq!(name(b"Old Sideband").as_deref(), Some("Old Sideband"));
        // A 16-bit array header, and a 16-bit bin.
        let mut long = vec![0xdc, 0x00, 0x01, 0xc5, 0x00, 0x03];
        long.extend_from_slice(b"Bea");
        assert_eq!(name(&long).as_deref(), Some("Bea"));
        // NULs gone and the ends trimmed, as LXMF does.
        assert_eq!(
            name(&[0x92, 0xc4, 0x05, b' ', b'C', 0, b'y', b' ', 0xc0]).as_deref(),
            Some("Cy")
        );
        // UTF-8 is a name, whatever the panel makes of it.
        assert_eq!(
            name(&lxmf_app_data("Zoë")).as_deref(),
            Some("Zoë"),
            "two bytes for one character"
        );
    }

    #[test]
    fn what_is_not_a_name_is_none() {
        let name = |d: &[u8]| display_name(Some(d));
        assert_eq!(display_name(None), None);
        assert_eq!(name(&[]), None);
        assert_eq!(name(&[0x90]), None, "an empty array");
        assert_eq!(
            name(&[0x92, 0xc0, 0xc0]),
            None,
            "nil, as LXMF writes no name"
        );
        assert_eq!(
            name(&[0x91, 0xa3, b'A', b'n', b'a']),
            None,
            "a str, not bytes"
        );
        assert_eq!(name(&[0x92, 0xc4, 0x09, b'A']), None, "cut short");
        assert_eq!(name(&[0xdc, 0x00]), None, "a header cut short");
        assert_eq!(name(&[0x91, 0xc4, 0x02, 0xc3, 0x28]), None, "not UTF-8");
        assert_eq!(name(b"\xff\xfe"), None, "nor in the old form");
        assert_eq!(name(&lxmf_app_data(" \0 ")), None, "only blanks");
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        let long = "é".repeat(NAME_MAX);
        let kept = display_name(Some(&lxmf_app_data(&long))).unwrap();
        assert_eq!(kept.len(), NAME_MAX);
        assert_eq!(kept, "é".repeat(NAME_MAX / 2));
        let odd = alloc::format!("a{long}");
        let kept = display_name(Some(&lxmf_app_data(&odd))).unwrap();
        assert_eq!(kept.len(), NAME_MAX - 1, "half an é is not kept");
    }

    #[test]
    fn an_announce_becomes_a_peer_with_everything_it_said() {
        let mut table = Table::new();
        assert_eq!(table.heard(&heard(1, Some("Ana")), 5_000), Learned::New);
        let peer = table.get(&[1; 16]).unwrap();
        assert_eq!(peer.public_key, [1; 64]);
        assert_eq!(peer.name.as_deref(), Some("Ana"));
        assert_eq!(peer.last_heard_ms, 5_000);
        assert_eq!(peer.hops, Some(1));
        assert_eq!(peer.signal, heard(1, None).signal);
        assert!(!peer.conversation);
    }

    #[test]
    fn hearing_a_peer_again_brings_it_up_to_date() {
        let mut table = Table::new();
        table.heard(&heard(1, Some("Ana")), 5_000);
        let mut again = heard(1, Some("Ana B"));
        again.hops = 3;
        again.signal = None;
        assert_eq!(table.heard(&again, 9_000), Learned::Updated);
        assert_eq!(table.len(), 1);
        let peer = table.get(&[1; 16]).unwrap();
        assert_eq!(peer.name.as_deref(), Some("Ana B"));
        assert_eq!(peer.last_heard_ms, 9_000);
        assert_eq!(peer.hops, Some(3));
        assert_eq!(peer.signal, None, "a relayed announce's signal is not its");
    }

    #[test]
    fn only_lxmf_delivery_makes_a_peer() {
        let mut table = Table::new();
        let mut node = heard(1, Some("A NomadNet page"));
        node.name_hash = rns_core::destination::name_hash("nomadnetwork", &["node"]);
        assert_eq!(table.heard(&node, 0), Learned::NotLxmf);
        assert!(table.is_empty());
    }

    /// A table filled with peers 1 to `CAPACITY`, peer n heard at n s.
    fn full() -> Table {
        let mut table = Table::new();
        for n in 1..=Table::CAPACITY as u8 {
            assert_eq!(
                table.heard(&heard(n, None), u64::from(n) * 1_000),
                Learned::New
            );
        }
        table
    }

    #[test]
    fn a_full_table_evicts_the_peer_heard_longest_ago() {
        let mut table = full();
        // Peer 1 is heard again, so 2 is now the oldest.
        table.heard(&heard(1, None), 100_000);
        assert_eq!(
            table.heard(&heard(200, None), 101_000),
            Learned::Evicted([2; 16])
        );
        assert_eq!(table.len(), Table::CAPACITY);
        assert!(table.get(&[2; 16]).is_none());
        assert!(table.get(&[200; 16]).is_some());
    }

    #[test]
    fn a_peer_with_a_conversation_is_never_evicted_for_a_stranger() {
        let mut table = full();
        assert!(table.set_conversation(&[1; 16], true));
        assert!(!table.set_conversation(&[99; 16], true), "not in the table");
        assert_eq!(
            table.heard(&heard(200, None), 101_000),
            Learned::Evicted([2; 16])
        );
        assert!(table.get(&[1; 16]).is_some());
    }

    #[test]
    fn a_table_full_of_conversations_turns_a_stranger_away_and_counts_it() {
        let mut table = full();
        for n in 1..=Table::CAPACITY as u8 {
            table.set_conversation(&[n; 16], true);
        }
        assert_eq!(table.heard(&heard(200, None), 101_000), Learned::Refused);
        assert_eq!(table.refused(), 1);
        assert!(table.get(&[200; 16]).is_none());
        // A peer it has is still brought up to date.
        assert_eq!(table.heard(&heard(3, Some("C")), 102_000), Learned::Updated);
        // And one let go makes room again.
        table.set_conversation(&[5; 16], false);
        assert_eq!(
            table.heard(&heard(200, None), 103_000),
            Learned::Evicted([5; 16])
        );
    }

    #[test]
    fn the_screen_lists_peers_most_recent_first_with_their_ages() {
        let mut table = Table::new();
        table.heard(&heard(1, Some("Ana")), 1_000);
        table.heard(&heard(2, None), 61_000);
        table.heard(&heard(3, Some("Cy")), 31_000);
        let screen = table.screen(91_500);
        let order: Vec<_> = screen
            .rows
            .iter()
            .flatten()
            .map(|r| r.destination[0])
            .collect();
        assert_eq!(order, [2, 3, 1]);
        let ages: Vec<_> = screen.rows.iter().flatten().map(|r| r.age_s).collect();
        assert_eq!(ages, [Some(30), Some(60), Some(90)]);
        let row = screen.rows[2].unwrap();
        assert_eq!(row.name.unwrap().as_str(), "Ana");
        assert_eq!(row.hops, Some(1));
        assert_eq!(row.signal, heard(1, None).signal);
        assert_eq!(screen.rows[0].unwrap().name, None);
        assert!(screen.rows[3..].iter().all(Option::is_none));
    }

    #[test]
    fn a_full_table_fills_the_screen() {
        let screen = full().screen(1_000_000);
        assert!(screen.rows.iter().all(Option::is_some));
    }

    #[test]
    fn a_clock_behind_the_peer_is_an_unknown_age_not_a_huge_one() {
        let mut table = Table::new();
        table.heard(&heard(1, None), 5_000);
        assert_eq!(table.screen(4_000).rows[0].unwrap().age_s, None);
    }
}
