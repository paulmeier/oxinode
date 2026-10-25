//! The board as a Reticulum node of its own, for when no host is attached.
//!
//! Everything else in this crate is an RNode: the host is the node, and the
//! board carries its packets. This module is the other mode. It wraps
//! `rns-core`'s `TransportEngine` (rns-rs, under the Reticulum License) in
//! the shape the rest of the firmware already has: it owns no peripheral, it
//! is told what came off the air and what time it is, and it answers with
//! what should go on the air and what it learned. So it is driven from host
//! tests exactly as the RNode protocol is, with real packets from Python
//! Reticulum as the input (see `tools/node_vectors.py`).
//!
//! It has an identity of its own ([`key`]), the one the board keeps for its
//! life, and an `lxmf.delivery` destination derived from it. It hears
//! announces and keeps the `lxmf.delivery` ones as [`peers`]; it announces
//! itself within a budget ([`announce`]) and answers path requests for its
//! address. It receives LXMF messages that come in one packet ([`lxmf`]),
//! proves each one so its sender shows it delivered, and keeps them
//! ([`messages`]); what it does with each is [`Station::take`]'s. It sends
//! messages of one packet too, and follows each until its proof comes back
//! or it fails ([`outbox`]); what is due is [`Station::step`]'s.
//!
//! # One interface, and no transport
//!
//! The engine is told of exactly one interface, the air, and runs as an
//! endpoint rather than a transport node: it never forwards. A board that
//! relayed other people's traffic would spend the channel it has to share,
//! and is a decision for later, not a default.
//!
//! # Time and floating point
//!
//! The rest of the firmware keeps time in integer milliseconds and signal in
//! integer quarter-dB, and nothing here changes that. The engine's API takes
//! seconds and an SNR as floats, so the conversion happens at this boundary
//! and nowhere else, and no float is ever stored on this side of it.
//!
//! # The engine's clock starts far in the future
//!
//! The board's clock is milliseconds since boot, and the engine is told
//! that plus [`CLOCK_OFFSET_S`]: 2^40 seconds, beyond any emission time an
//! announce can carry in its five bytes. The engine only ever compares its
//! clock with itself, except in one place: `rns-core`, departing from
//! Python's `Transport`, treats an announce stamped more than a day after
//! its own clock as unstamped. Told seconds since boot, it took every real
//! Unix time -- the GPS's, Sideband's, MeshChat's -- for none, and so
//! dropped every re-announce, since a later one must be strictly newer.
//! With the offset, every stamp counts, and announces are compared with
//! each other exactly as Python compares them. The clock never jumps, so
//! path expiry and the other timers are as they were.
//!
//! # The tables are bounded
//!
//! Every limit in [`config`] is a table on the heap. They are sized for the
//! handful of peers a LoRa channel at a few kilobits a second can carry, not
//! for the internet, and they are what keeps the heap the firmware gives
//! this module from growing without bound.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use rns_core::constants;
use rns_core::packet::RawPacket;
use rns_core::transport::types::{
    InterfaceId, InterfaceInfo, PacketHashlistAllocation, TransportAction, TransportConfig,
};
use rns_core::transport::{InboundFrame, RxMetadata, TransportEngine};

use crate::rnode::air::Signal;

pub use rns_crypto::Rng;

pub mod announce;
pub mod key;
pub mod lxmf;
pub mod messages;
pub mod outbox;
pub mod peers;
use key::{Hashes, NodeKey};
use messages::{Arrival, Messages, Settled};
use outbox::{Due, Outbox};
use peers::{Learned, Table};

use crate::screens::{
    ConversationRow, Conversations, Entry, Failure, Read, Thread, Wait, CONVERSATIONS,
};

/// Where the engine's randomness comes from: ChaCha20, seeded once.
///
/// Not the chip's RNG itself, for two reasons. The Bluetooth controller owns
/// that peripheral from boot onwards and takes its bytes when it likes, so
/// the node cannot share it without a lock the controller does not know
/// about. And the hardware RNG makes one byte at a time from thermal noise,
/// more slowly still with bias correction on, which is the setting the
/// firmware seeds with: too slow to be what every packet draws on. So the
/// firmware takes 32 bytes from the hardware before the controller starts
/// and seeds this with them, and the stream from there is ChaCha20's.
pub struct Entropy(rand_chacha::ChaCha20Rng);

impl Entropy {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        use rand_chacha::rand_core::SeedableRng;
        Self(rand_chacha::ChaCha20Rng::from_seed(seed))
    }
}

impl Rng for Entropy {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand_chacha::rand_core::Rng::fill_bytes(&mut self.0, dest);
    }
}

/// The one interface the engine knows: the radio.
pub const AIR: InterfaceId = InterfaceId(1);

/// What the engine is told about its tables, for the board's identity. See
/// the module docs.
pub fn config(identity: [u8; 16]) -> TransportConfig {
    TransportConfig {
        // An endpoint, not a transport node: it does not forward.
        transport_enabled: false,
        identity_hash: Some(identity),
        local_hops_delta: 0,
        prefer_shorter_path: false,
        max_paths_per_destination: 1,
        packet_hashlist_max_entries: 256,
        // Allocated as it fills rather than all at once, so the heap the
        // node starts with is the heap it needs, not the heap it might.
        packet_hashlist_allocation: PacketHashlistAllocation::Lazy,
        max_discovery_pr_tags: 64,
        max_path_destinations: 64,
        max_tunnel_destinations_total: 0,
        destination_timeout_secs: constants::DESTINATION_TIMEOUT,
        announce_table_ttl_secs: constants::ANNOUNCE_TABLE_TTL,
        announce_table_max_bytes: 8 * 1024,
        announce_sig_cache_enabled: true,
        announce_sig_cache_max_entries: 32,
        announce_sig_cache_ttl_secs: constants::ANNOUNCE_SIG_CACHE_TTL,
        announce_queue_max_entries: 16,
        announce_queue_max_interfaces: 1,
    }
}

/// An announce the engine heard and whose signature it checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub destination: [u8; 16],
    pub identity: [u8; 16],
    pub public_key: [u8; 64],
    /// Which kind of destination: `lxmf.delivery`, for instance.
    pub name_hash: [u8; 10],
    pub app_data: Option<Vec<u8>>,
    pub hops: u8,
    /// The signal it came with, if it came straight off the air: one hop.
    /// A relayed announce's signal is the relay's.
    pub signal: Option<Signal>,
}

/// An LXMF message delivered to the board, and the packet it came in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub message: lxmf::Message,
    /// The hash of the packet, which is what a delivery proof signs.
    pub packet_hash: [u8; 32],
}

/// A delivery proof heard on the air. Proofs are addressed to the first
/// sixteen bytes of the hash of the packet they prove, so anyone can send
/// one for anything; it means something only once the signature in it is
/// checked against the key of the peer the packet went to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    /// The truncated hash of the packet it says was delivered.
    pub packet: [u8; 16],
    /// The signature, with the full packet hash in front if it is explicit.
    pub data: Vec<u8>,
}

/// A message's packet, ready for the air, and the hash its proof will sign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub packet: Vec<u8>,
    pub packet_hash: [u8; 32],
}

/// A packet for the board's address that is not a message it can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// Not a single opportunistic packet: a link request, or a packet with
    /// a context the board does not read. Links are later work (#67).
    NotOpportunistic { packet_type: u8, context: u8 },
    /// It did not decrypt with the board's key.
    Undecryptable,
    /// It decrypted, and is not an LXMF message.
    Malformed(lxmf::Malformed),
}

/// What one call into the node produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Packets to put on the air, in order.
    pub air: Vec<Vec<u8>>,
    /// Announces heard and verified.
    pub heard: Vec<Heard>,
    /// Our own destinations someone asked for a path to. Answering one is an
    /// announce, which needs the keys, so it is the caller's to send.
    pub path_requests: Vec<[u8; 16]>,
    /// Messages delivered to the board, not yet proved: whether to prove one
    /// depends on what the board knows of its sender ([`Station::take`]).
    pub messages: Vec<Delivery>,
    /// Packets for the board's address that it could not read.
    pub unreadable: Vec<Unreadable>,
    /// Delivery proofs, any of which may be for a message the board sent:
    /// which one, and whether it holds, is [`Station::take`]'s to say.
    pub proofs: Vec<Proof>,
    /// Actions the node had no use for, counted so a log can say so.
    pub ignored: u32,
}

/// The engine and the one interface it has. See the module docs.
pub struct Node {
    engine: TransportEngine,
    /// `rnstransport.path.request`, where every path request is addressed.
    path_request: [u8; 16],
    hashes: Hashes,
    /// The board's identity, to sign its announces with.
    identity: rns_crypto::identity::Identity,
}

impl Node {
    /// The board's node, as `key`, listening on the air at `bitrate_bps`
    /// from `now_ms`.
    ///
    /// The bitrate is what the engine budgets announces against. A radio
    /// with no configuration yet has none, and is passed as zero.
    pub fn new(bitrate_bps: u32, now_ms: u64, key: &NodeKey) -> Self {
        let hashes = key.hashes();
        let mut engine = TransportEngine::new(config(hashes.identity));
        engine.register_interface(air(bitrate_bps, now_ms));
        // Path requests come addressed to a plain destination every node
        // listens on, as Python's `Transport` registers it at start. The
        // engine delivers them there and leaves the answering to its driver,
        // which is this.
        let path_request =
            rns_core::destination::destination_hash("rnstransport", &["path", "request"], None);
        engine.register_destination(path_request, constants::DESTINATION_PLAIN);
        // The address peers send to, so a path request for it is reported.
        engine.register_destination(hashes.lxmf, constants::DESTINATION_SINGLE);
        Self {
            engine,
            path_request,
            hashes,
            identity: key.identity(),
        }
    }

    /// What the board is called on the network.
    pub fn hashes(&self) -> Hashes {
        self.hashes
    }

    /// The board's `lxmf.delivery` announce, ready for the air.
    ///
    /// `app_data` is what peers show as the board's name ([`lxmf_app_data`]).
    /// `emitted_s` is the emission time a receiver compares paths by: Unix
    /// seconds when the GPS has said what time it is, and zero, which
    /// Reticulum reads as not known, when it has not. The board has no other
    /// clock. A path response is the same announce marked as answering a
    /// path request.
    pub fn announce(
        &self,
        kind: announce::Kind,
        app_data: &[u8],
        emitted_s: u64,
        rng: &mut dyn Rng,
    ) -> Option<Vec<u8>> {
        let mut random_hash = [0u8; 10];
        rng.fill_bytes(&mut random_hash[..5]);
        random_hash[5..].copy_from_slice(&emitted_s.to_be_bytes()[3..]);
        let name_hash = rns_core::destination::name_hash("lxmf", &["delivery"]);
        let (data, _) = rns_core::announce::AnnounceData::pack(
            &self.identity,
            &self.hashes.lxmf,
            &name_hash,
            &random_hash,
            None,
            Some(app_data),
        )
        .ok()?;
        let flags = rns_core::packet::PacketFlags {
            header_type: constants::HEADER_1,
            context_flag: constants::FLAG_UNSET,
            transport_type: constants::TRANSPORT_BROADCAST,
            destination_type: constants::DESTINATION_SINGLE,
            packet_type: constants::PACKET_TYPE_ANNOUNCE,
        };
        let context = match kind {
            announce::Kind::Announce => constants::CONTEXT_NONE,
            announce::Kind::PathResponse => constants::CONTEXT_PATH_RESPONSE,
        };
        RawPacket::pack(flags, 0, &self.hashes.lxmf, None, context, &data)
            .ok()
            .map(|p| p.raw)
    }

    /// A packet came off the air, already reassembled and with its header
    /// byte gone: exactly what the RNode would have given a host.
    pub fn heard(
        &mut self,
        packet: &[u8],
        signal: Option<Signal>,
        now_ms: u64,
        rng: &mut dyn Rng,
    ) -> Outcome {
        let rx = signal.map_or_else(RxMetadata::default, |s| RxMetadata {
            rssi: Some(s.rssi_dbm),
            snr: Some(f32::from(s.snr_quarter_db) / 4.0),
        });
        let now = seconds(now_ms);
        let frame = InboundFrame::new(packet, AIR, now).with_rx(rx);
        let mut actions = self.engine.handle_inbound(frame, rng);
        // A path request is delivered to us like any packet for a local
        // destination, and goes back into the engine as a path request. A
        // packet for our own address is a message, or something that is not.
        let mut answers = Vec::new();
        let mut messages = Vec::new();
        let mut unreadable = Vec::new();
        let mut proofs = Vec::new();
        actions.retain(|action| match action {
            // A proof is delivered locally whoever it is for: the engine
            // keeps no receipts, so the board matches it to what it sent.
            TransportAction::DeliverLocal {
                destination_hash,
                raw,
                ..
            } if raw.get(..1).is_some_and(|f| {
                rns_core::packet::PacketFlags::unpack(f[0]).packet_type
                    == constants::PACKET_TYPE_PROOF
            }) =>
            {
                if let Ok(proof) = RawPacket::unpack(raw) {
                    if proof.context == constants::CONTEXT_NONE {
                        proofs.push(Proof {
                            packet: *destination_hash,
                            data: proof.data().to_vec(),
                        });
                    }
                }
                false
            }
            TransportAction::DeliverLocal {
                destination_hash,
                raw,
                ..
            } if *destination_hash == self.path_request => {
                if let Ok(request) = RawPacket::unpack(raw) {
                    let data = &request.raw[RawPacket::data_offset(request.flags.header_type)..];
                    answers.append(&mut self.engine.handle_path_request(data, AIR, now));
                }
                false
            }
            TransportAction::DeliverLocal {
                destination_hash,
                raw,
                packet_hash,
                ..
            } if *destination_hash == self.hashes.lxmf => {
                match self.open(raw, packet_hash) {
                    Ok(delivery) => messages.push(delivery),
                    Err(why) => unreadable.push(why),
                }
                false
            }
            _ => true,
        });
        actions.append(&mut answers);
        let mut out = route(actions);
        out.messages = messages;
        out.unreadable = unreadable;
        out.proofs = proofs;
        // The board's own announce, relayed back by somebody, is not a peer;
        // Reticulum ignores announces for its own destinations as well.
        let ours = self.hashes.lxmf;
        out.heard.retain(|h| h.destination != ours);
        for heard in &mut out.heard {
            heard.signal = signal.filter(|_| heard.hops == 1);
        }
        out
    }

    /// A packet for the board's address, as a message: one opportunistic
    /// packet, decrypted with the board's key and read as LXMF. Reticulum
    /// proves a packet as soon as it decrypts; the board waits to know what
    /// it believes of the sender, so the proof is [`Node::prove`]'s.
    fn open(&self, raw: &[u8], packet_hash: &[u8; 32]) -> Result<Delivery, Unreadable> {
        let packet = RawPacket::unpack(raw).map_err(|_| Unreadable::Undecryptable)?;
        if packet.flags.packet_type != constants::PACKET_TYPE_DATA
            || packet.flags.destination_type != constants::DESTINATION_SINGLE
            || packet.context != constants::CONTEXT_NONE
        {
            return Err(Unreadable::NotOpportunistic {
                packet_type: packet.flags.packet_type,
                context: packet.context,
            });
        }
        let plaintext = self
            .identity
            .decrypt(packet.data())
            .map_err(|_| Unreadable::Undecryptable)?;
        let message =
            lxmf::Message::unpack(&self.hashes.lxmf, &plaintext).map_err(Unreadable::Malformed)?;
        Ok(Delivery {
            message,
            packet_hash: *packet_hash,
        })
    }

    /// The delivery proof for the packet with this hash, as Reticulum's
    /// `Identity.prove` makes it: the board's signature over the hash and
    /// nothing else (an implicit proof), addressed to the packet's own
    /// truncated hash, which is where the sender waits for it.
    pub fn prove(&self, packet_hash: &[u8; 32]) -> Option<Vec<u8>> {
        let signature = self.identity.sign(packet_hash).ok()?;
        let flags = rns_core::packet::PacketFlags {
            header_type: constants::HEADER_1,
            context_flag: constants::FLAG_UNSET,
            transport_type: constants::TRANSPORT_BROADCAST,
            destination_type: constants::DESTINATION_SINGLE,
            packet_type: constants::PACKET_TYPE_PROOF,
        };
        let mut to = [0u8; 16];
        to.copy_from_slice(&packet_hash[..16]);
        RawPacket::pack(flags, 0, &to, None, constants::CONTEXT_NONE, &signature)
            .ok()
            .map(|p| p.raw)
    }

    /// A message from the board to `destination`, packed and signed: see
    /// [`lxmf::pack`].
    pub fn pack(
        &self,
        destination: &[u8; 16],
        timestamp_s: u64,
        content: &str,
    ) -> Result<lxmf::Outbound, lxmf::TooLong> {
        lxmf::pack(
            &self.identity,
            &self.hashes.lxmf,
            destination,
            timestamp_s,
            content,
        )
    }

    /// One attempt at sending `message` to the peer whose key is
    /// `public_key`: encrypted afresh, as LXMF encrypts every retry, and
    /// routed by the engine, which sends it through a transport node when
    /// that is how the peer was heard. Each attempt is another packet, with
    /// a hash of its own for its proof.
    pub fn attempt(
        &mut self,
        message: &lxmf::Outbound,
        public_key: &[u8; 64],
        now_ms: u64,
        rng: &mut dyn Rng,
    ) -> Option<Attempt> {
        let recipient = rns_crypto::identity::Identity::from_public_key(public_key).ok()?;
        let token = recipient.encrypt(message.plaintext(), rng).ok()?;
        let flags = rns_core::packet::PacketFlags {
            header_type: constants::HEADER_1,
            context_flag: constants::FLAG_UNSET,
            transport_type: constants::TRANSPORT_BROADCAST,
            destination_type: constants::DESTINATION_SINGLE,
            packet_type: constants::PACKET_TYPE_DATA,
        };
        let packet = RawPacket::pack(
            flags,
            0,
            &message.destination,
            None,
            constants::CONTEXT_NONE,
            &token,
        )
        .ok()?;
        let actions = self.engine.handle_outbound(
            &packet,
            constants::DESTINATION_SINGLE,
            None,
            seconds(now_ms),
        );
        let raw = route(actions).air.into_iter().next()?;
        (raw.len() <= constants::MTU).then_some(Attempt {
            packet: raw,
            packet_hash: packet.packet_hash,
        })
    }

    /// A path request for `destination`, as an endpoint makes one: its
    /// address and a random tag of sixteen bytes (`Identity.get_random_hash`
    /// in Python's `Transport.request_path`), broadcast to
    /// `rnstransport.path.request`. The answer is the destination's
    /// announce, which carries its key.
    pub fn path_request(&self, destination: &[u8; 16], rng: &mut dyn Rng) -> Option<Vec<u8>> {
        let mut data = [0u8; 16 + 16];
        data[..16].copy_from_slice(destination);
        rng.fill_bytes(&mut data[16..]);
        let flags = rns_core::packet::PacketFlags {
            header_type: constants::HEADER_1,
            context_flag: constants::FLAG_UNSET,
            transport_type: constants::TRANSPORT_BROADCAST,
            destination_type: constants::DESTINATION_PLAIN,
            packet_type: constants::PACKET_TYPE_DATA,
        };
        RawPacket::pack(
            flags,
            0,
            &self.path_request,
            None,
            constants::CONTEXT_NONE,
            &data,
        )
        .ok()
        .map(|p| p.raw)
    }

    /// Housekeeping: timeouts, queued announces, table culls. The engine
    /// asks for this every quarter of a second or so.
    pub fn tick(&mut self, now_ms: u64, rng: &mut dyn Rng) -> Outcome {
        route(self.engine.tick(seconds(now_ms), rng))
    }

    /// How many destinations the engine holds a path to.
    pub fn paths(&self) -> usize {
        self.engine.path_table_count()
    }
}

/// What the board knows of the others: the peers it has heard, the
/// messages it has had from them, and the ones it has sent them.
#[derive(Debug, Default)]
pub struct Station {
    pub peers: Table,
    pub messages: Messages,
    pub outbox: Outbox,
    /// The newest time a message was stamped with, and when it came: the
    /// board's clock when the GPS has none. See [`Station::clock_s`].
    told: Option<(u64, u64)>,
}

/// What [`Station::take`] and [`Station::step`] did, one thing at a time,
/// for a log to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An announce, and what the peer table made of it.
    Peer {
        destination: [u8; 16],
        learned: Learned,
    },
    /// A sender's key arrived, and settled messages that were waiting for it.
    Settled { source: [u8; 16], settled: Settled },
    /// A message, and what the store made of it.
    Message {
        hash: [u8; 32],
        source: [u8; 16],
        arrival: Arrival,
    },
    /// A packet for the board's address it could not read.
    Unreadable(Unreadable),
    /// A message the board sent was proved by its recipient.
    Delivered { hash: [u8; 32], to: [u8; 16] },
    /// A proof for none of the board's packets, or one that does not hold.
    Unclaimed,
    /// The board asked for a path, to send a message.
    Asked { to: [u8; 16] },
    /// A message the board sent has failed.
    Failed {
        hash: [u8; 32],
        to: [u8; 16],
        why: Failure,
    },
}

/// Why [`Station::compose`] did not queue a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsent {
    /// It does not fit in one packet, by this many bytes.
    TooLong(usize),
    /// Every message kept is still on its way.
    Full,
}

/// What [`Station::step`] has for the air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A packet the board needs, a path request: like the engine's own.
    Packet(Vec<u8>),
    /// An attempt at the message with this hash. What became of it goes to
    /// [`Outbox::went`].
    Attempt { hash: [u8; 32], attempt: Attempt },
}

impl Station {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take what the engine produced at `at_ms`: peers into the table, keys
    /// to the messages waiting for them, messages into the store, and
    /// proofs to the messages the board sent. What that calls for -- a
    /// proof, or a path request to learn a sender's key -- is added to
    /// `out.air`, after the engine's own packets.
    pub fn take(
        &mut self,
        node: &Node,
        out: &mut Outcome,
        at_ms: u64,
        rng: &mut dyn Rng,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        for heard in &out.heard {
            let learned = self.peers.heard(heard, at_ms);
            events.push(Event::Peer {
                destination: heard.destination,
                learned,
            });
            let settled = self.messages.heard(heard, &mut self.peers);
            if settled != Settled::default() {
                events.push(Event::Settled {
                    source: heard.destination,
                    settled,
                });
            }
        }
        for delivery in core::mem::take(&mut out.messages) {
            let (hash, source) = (delivery.message.hash, delivery.message.source);
            let sent_s = delivery.message.sent_s;
            let arrival = self
                .messages
                .receive(delivery.message, &mut self.peers, at_ms);
            if arrival.prove() {
                out.air.extend(node.prove(&delivery.packet_hash));
            }
            if arrival.ask_for_key() {
                out.air.extend(node.path_request(&source, rng));
            }
            if let (Arrival::New(_), Some(sent_s)) = (arrival, sent_s) {
                if self.told.is_none_or(|(s, _)| sent_s > s) {
                    self.told = Some((sent_s, at_ms));
                }
            }
            events.push(Event::Message {
                hash,
                source,
                arrival,
            });
        }
        events.extend(out.unreadable.drain(..).map(Event::Unreadable));
        for proof in core::mem::take(&mut out.proofs) {
            events.push(match self.outbox.proved(&proof, &self.peers) {
                Some((hash, to)) => Event::Delivered { hash, to },
                None => Event::Unclaimed,
            });
        }
        events
    }

    /// What time it is, in Unix seconds, for a message's timestamp: the
    /// GPS's, when it has said (`gps_s`); with no fix, the newest stamp a
    /// message has arrived with, run on by the board's own clock since; and
    /// with neither, zero, which is what the board then sends. The stamp is
    /// the sender's word, which is why the GPS comes first.
    pub fn clock_s(&self, gps_s: Option<u64>, now_ms: u64) -> u64 {
        gps_s
            .or_else(|| {
                self.told
                    .map(|(s, at)| s + now_ms.saturating_sub(at) / 1_000)
            })
            .unwrap_or(0)
    }

    /// Write `content` to `to` at `now_ms`: packed, signed and queued; held,
    /// and why, if the node is not running. The peer has a conversation from
    /// now on. Says the message's hash.
    pub fn compose(
        &mut self,
        node: &Node,
        to: &[u8; 16],
        content: &str,
        timestamp_s: u64,
        now_ms: u64,
        held: Option<Wait>,
    ) -> Result<[u8; 32], Unsent> {
        let message = node
            .pack(to, timestamp_s, content)
            .map_err(|e| Unsent::TooLong(e.over))?;
        let hash = message.hash;
        self.outbox
            .queue(message, String::from(content), now_ms, held)
            .map_err(|_| Unsent::Full)?;
        self.peers.set_conversation(to, true);
        Ok(hash)
    }

    /// What the board's messages need on the air at `now_ms`, if anything:
    /// a path request, or an attempt at one, built here -- encrypting is the
    /// slow part, about a tenth of a second. A message that fails says so in
    /// `events`.
    pub fn step(
        &mut self,
        node: &mut Node,
        now_ms: u64,
        rng: &mut dyn Rng,
        events: &mut Vec<Event>,
    ) -> Option<Step> {
        loop {
            match self.outbox.due(now_ms, &self.peers)? {
                Due::Ask(to) => {
                    events.push(Event::Asked { to });
                    return node.path_request(&to, rng).map(Step::Packet);
                }
                Due::Failed { hash, to, why } => events.push(Event::Failed { hash, to, why }),
                Due::Send { hash, to } => {
                    let key = self.peers.get(&to).map(|p| p.public_key);
                    let message = self.outbox.get(&hash).map(|s| s.message.clone());
                    let attempt = key
                        .zip(message)
                        .and_then(|(key, m)| node.attempt(&m, &key, now_ms, rng));
                    match attempt {
                        Some(attempt) => return Some(Step::Attempt { hash, attempt }),
                        None => {
                            self.outbox.unbuildable(&hash);
                            events.push(Event::Failed {
                                hash,
                                to,
                                why: Failure::NoPath,
                            });
                        }
                    }
                }
            }
        }
    }

    /// The conversations, newest first, as the Messages screen lists them
    /// at `now_ms`: one for each peer with a message kept, either way.
    pub fn conversations(&self, now_ms: u64) -> Conversations {
        // (peer, newest, unread), newest first.
        let mut all: Vec<([u8; 16], u64, bool)> = Vec::new();
        let mut note = |peer: [u8; 16], at_ms: u64, unread: bool| match all
            .iter_mut()
            .find(|(p, _, _)| *p == peer)
        {
            Some(row) => {
                row.1 = row.1.max(at_ms);
                row.2 |= unread;
            }
            None => all.push((peer, at_ms, unread)),
        };
        for r in self.messages.iter() {
            note(r.message.source, r.at_ms, r.unread);
        }
        for s in self.outbox.iter() {
            note(s.message.destination, s.at_ms, false);
        }
        // Stable: of two as new as each other, the one received first.
        all.sort_by_key(|row| core::cmp::Reverse(row.1));
        let mut out = Conversations::default();
        for (slot, (peer, at_ms, unread)) in out.rows.iter_mut().zip(all.iter().take(CONVERSATIONS))
        {
            *slot = Some(ConversationRow {
                peer: *peer,
                name: messages::name_of(&self.peers, peer),
                unread: *unread,
                age_s: messages::age_s(*at_ms, now_ms),
            });
        }
        out
    }

    /// The conversation with `peer`, oldest first, both ways, as the thread
    /// draws it at `now_ms`. [`Station::read`] counts in the same order.
    pub fn entries(&self, peer: &[u8; 16], now_ms: u64) -> Vec<Entry<'_>> {
        let mut all: Vec<(u64, Entry<'_>)> = self
            .messages
            .entries(peer, now_ms)
            .chain(self.outbox.entries(peer, now_ms))
            .collect();
        // Stable: a reply written in the same millisecond comes after.
        all.sort_by_key(|(at, _)| *at);
        all.into_iter().map(|(_, e)| e).collect()
    }

    /// The thread with `peer`, over `entries` from [`Station::entries`].
    pub fn thread<'a>(&self, peer: &[u8; 16], entries: &'a [Entry<'a>]) -> Thread<'a> {
        self.messages.thread(peer, &self.peers, entries)
    }

    /// The panel has shown a conversation this far, counting both ways:
    /// the messages received among its first `through` are read. Says how
    /// many that made read.
    pub fn read(&mut self, read: Read, now_ms: u64) -> usize {
        let received = self
            .entries(&read.peer, now_ms)
            .iter()
            .take(read.through)
            .filter(|e| matches!(e.direction, crate::screens::Direction::Received(_)))
            .count();
        self.messages.read(Read {
            peer: read.peer,
            through: received,
        })
    }
}

/// LXMF's announce app data for a display name: a two-element msgpack array
/// of the name as bytes and the stamp cost, which the board does not set.
/// What Sideband and MeshChat put there, and what they read a name out of.
pub fn lxmf_app_data(name: &str) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(u8::MAX as usize)];
    let mut out = Vec::with_capacity(name.len() + 4);
    out.extend_from_slice(&[0x92, 0xc4, name.len() as u8]);
    out.extend_from_slice(name);
    out.push(0xc0);
    out
}

/// How long the board's announce is with `app_data_len` bytes of app data:
/// the header and the address, the public key, the name and random hashes,
/// the signature, then the app data.
pub const fn announce_len(app_data_len: usize) -> usize {
    2 + 16 + 1 + 64 + 10 + 10 + 64 + app_data_len
}

/// A packet off the air, waiting to be handed to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub packet: Vec<u8>,
    pub signal: Option<Signal>,
    /// When it was heard, in milliseconds since boot.
    pub at_ms: u64,
}

/// Packets heard and not yet looked at, waiting for the radio to be quiet.
///
/// Verifying an announce takes the engine about 45 ms, and the radio comes
/// first: the modem loop takes a packet off the chip as soon as it is there,
/// queues it here, and hands the engine one only on a pass where the radio
/// had nothing. The channel cannot deliver announces faster than the board
/// verifies them for long, so this bounds memory rather than speed:
/// [`Inbox::CAPACITY`] packets, and when it is full the oldest goes, counted,
/// as the pad's channel counts what it loses.
#[derive(Debug, Default)]
pub struct Inbox {
    packets: alloc::collections::VecDeque<Inbound>,
    dropped: u32,
}

impl Inbox {
    pub const CAPACITY: usize = 8;

    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a packet; says whether one was dropped to make room.
    pub fn push(&mut self, packet: &[u8], signal: Option<Signal>, at_ms: u64) -> bool {
        let full = self.packets.len() == Self::CAPACITY;
        if full {
            self.packets.pop_front();
            self.dropped += 1;
        }
        self.packets.push_back(Inbound {
            packet: packet.to_vec(),
            signal,
            at_ms,
        });
        full
    }

    /// The oldest packet waiting.
    pub fn pop(&mut self) -> Option<Inbound> {
        self.packets.pop_front()
    }

    /// Everything waiting, gone, as when a host arrives; says how many.
    pub fn clear(&mut self) -> usize {
        let n = self.packets.len();
        self.packets.clear();
        n
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// Packets dropped because newer ones arrived while it was full.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

/// Packets the node has to send, waiting for the air.
///
/// Bounded: [`Queue::CAPACITY`] packets, and when it is full the oldest goes,
/// counted, as the pad's channel drops and counts. When a host arrives the
/// whole queue goes the same way: nothing the node wanted to say is sent
/// under somebody else's RNode.
#[derive(Debug, Default)]
pub struct Queue {
    packets: alloc::collections::VecDeque<(Vec<u8>, Option<announce::Kind>)>,
    dropped: u32,
}

impl Queue {
    pub const CAPACITY: usize = 4;

    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a packet, with the kind of announce it is if it is one.
    pub fn push(&mut self, packet: Vec<u8>, announce: Option<announce::Kind>) {
        if self.packets.len() == Self::CAPACITY {
            self.packets.pop_front();
            self.dropped += 1;
        }
        self.packets.push_back((packet, announce));
    }

    /// The next packet to send.
    pub fn pop(&mut self) -> Option<(Vec<u8>, Option<announce::Kind>)> {
        self.packets.pop_front()
    }

    /// Everything waiting, gone; says how many.
    pub fn clear(&mut self) -> usize {
        let n = self.packets.len();
        self.dropped += n as u32;
        self.packets.clear();
        n
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// Packets that never went: pushed out by newer ones, or cleared.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

/// What the node has to send and when it may: the announce [`Schedule`] and
/// the [`Queue`], and the rule that ties them to the mode.
///
/// The rule: the node sends only while it is running (`Mode::Node`, see
/// [`crate::mode`]), and only while nobody is on the line who may be about
/// to speak -- no port open, no phone linked -- because a host that opens the
/// port sends its detect two seconds later and gives the board a fifth of a
/// second to answer. When the node stops, whatever was waiting is dropped and
/// counted, and the schedule stops asking; when it starts, an announce is
/// due, inside the budget the last one bought.
///
/// [`Schedule`]: announce::Schedule
#[derive(Debug, Default)]
pub struct Sending {
    schedule: announce::Schedule,
    queue: Queue,
    running: bool,
}

/// What [`Sending::follow`] saw happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Followed {
    /// The node started: an announce is due.
    Started,
    /// The node stopped, and this many packets it had waiting went with it.
    Stopped(usize),
}

impl Sending {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the node is running now; says when that changed.
    pub fn follow(&mut self, running: bool, now_ms: u64) -> Option<Followed> {
        let was = core::mem::replace(&mut self.running, running);
        match (was, running) {
            (false, true) => {
                self.schedule.started(now_ms);
                Some(Followed::Started)
            }
            (true, false) => {
                self.schedule.withhold();
                Some(Followed::Stopped(self.queue.clear()))
            }
            _ => None,
        }
    }

    /// Something asks for an announce. Nothing is asked for while stopped.
    pub fn want(&mut self, kind: announce::Kind) {
        if self.running {
            self.schedule.want(kind);
        }
    }

    /// The announce due now, if any, to be built and handed to [`push`].
    ///
    /// [`push`]: Sending::push
    pub fn due(&mut self, now_ms: u64) -> Option<announce::Kind> {
        if self.running {
            self.schedule.due(now_ms)
        } else {
            None
        }
    }

    /// A packet for the air: an announce of `kind`, or `None` for one of the
    /// engine's own.
    pub fn push(&mut self, packet: Vec<u8>, kind: Option<announce::Kind>) {
        if self.running {
            self.queue.push(packet, kind);
        }
    }

    /// The next packet to send, if the node is running and nobody is on the
    /// line.
    pub fn next(&mut self, nobody_on_the_line: bool) -> Option<(Vec<u8>, Option<announce::Kind>)> {
        if self.running && nobody_on_the_line {
            self.queue.pop()
        } else {
            None
        }
    }

    /// What became of a packet [`Sending::next`] gave out: `Some(airtime)`
    /// if it went. An announce that went spends the budget; one that did not
    /// -- refused by the radio, or given up because a host may be about to
    /// speak -- is asked for again, and waits like any other.
    pub fn sent(&mut self, kind: Option<announce::Kind>, went: Option<u32>, now_ms: u64) {
        match (kind, went) {
            (Some(_), Some(airtime_us)) => self.schedule.sent(now_ms, airtime_us),
            (Some(kind), None) => self.want(kind),
            (None, _) => {}
        }
    }

    /// The earliest the budget lets the next announce go.
    pub fn allowed_from(&self) -> u64 {
        self.schedule.allowed_from()
    }

    /// Packets that never went, in all.
    pub fn dropped(&self) -> u32 {
        self.queue.dropped()
    }
}

/// How often [`Node::tick`] should be called, in milliseconds.
pub const TICK_MS: u64 = 250;

/// What the engine's clock reads at boot. See the module docs.
pub const CLOCK_OFFSET_S: u64 = 1 << 40;

/// The latest emission time an announce's five bytes can carry.
pub const MAX_EMITTED_S: u64 = (1 << 40) - 1;

// Every stamp is behind the engine's clock from boot onwards.
const _: () = assert!(MAX_EMITTED_S < CLOCK_OFFSET_S);

/// The engine's time for `ms` since boot. At 2^40 an `f64` still resolves
/// a ten-thousandth of a second.
fn seconds(ms: u64) -> f64 {
    (CLOCK_OFFSET_S as f64) + ms as f64 / 1000.0
}

fn air(bitrate_bps: u32, now_ms: u64) -> InterfaceInfo {
    InterfaceInfo {
        id: AIR,
        name: String::from("air"),
        mode: constants::MODE_FULL,
        gravity: 0,
        recursive_prs: false,
        announces_from_internal: false,
        announces_to_internal: None,
        out_capable: true,
        in_capable: true,
        bitrate: (bitrate_bps > 0).then_some(u64::from(bitrate_bps)),
        airtime_profile: None,
        announce_rate_target: None,
        announce_rate_grace: 0,
        announce_rate_penalty: 0.0,
        announce_cap: constants::ANNOUNCE_CAP,
        is_local_client: false,
        wants_tunnel: false,
        tunnel_id: None,
        mtu: constants::MTU as u32,
        ingress_control: Default::default(),
        ia_freq: 0.0,
        ip_freq: 0.0,
        op_freq: 0.0,
        op_samples: 0,
        started: seconds(now_ms),
    }
}

/// What the engine asked for, sorted into what the board can do with it.
///
/// The engine knows only one interface, so anything addressed to another
/// one is a bug in the engine or here, and is ignored rather than sent.
fn route(actions: Vec<TransportAction>) -> Outcome {
    let mut out = Outcome::default();
    for action in actions {
        match action {
            TransportAction::SendOnInterface { interface, raw } if interface == AIR => {
                out.air.push(raw.to_vec());
            }
            TransportAction::BroadcastOnAllInterfaces { raw, exclude } if exclude != Some(AIR) => {
                out.air.push(raw.to_vec());
            }
            TransportAction::AnnounceReceived(announce) => out.heard.push(Heard {
                destination: announce.destination_hash,
                identity: announce.identity_hash,
                public_key: announce.public_key,
                name_hash: announce.name_hash,
                app_data: announce.app_data,
                hops: announce.hops,
                signal: None,
            }),
            TransportAction::AnnounceLocalPathResponse {
                destination_hash, ..
            } => out.path_requests.push(destination_hash),
            _ => out.ignored += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use rns_crypto::FixedRng;

    const ANNOUNCE: &[u8] = include_bytes!("node/vectors/announce.bin");
    const PATH_REQUEST: &[u8] = include_bytes!("node/vectors/path_request.bin");
    const REANNOUNCE: &[u8] = include_bytes!("node/vectors/reannounce.bin");
    const VECTORS: &str = include_str!("node/vectors/vectors.txt");

    /// A field of `vectors.txt`, as bytes.
    fn vector(name: &str) -> Vec<u8> {
        let line = VECTORS
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("{name} is not in vectors.txt"));
        (0..line.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect()
    }

    fn rng() -> FixedRng {
        FixedRng::new(&[0x5a; 64])
    }

    /// A board's identity: not the one in the vectors, which is the peer's.
    fn board() -> NodeKey {
        NodeKey::generate(&mut Entropy::from_seed([9; 32]))
    }

    /// The peer's identity, the one `tools/node_vectors.py` announced.
    fn peer() -> NodeKey {
        NodeKey::from_private(core::array::from_fn(|i| i as u8))
    }

    /// The SF9 / 125 kHz / 4:5 default, near enough.
    const BITRATE: u32 = 1757;

    const SIGNAL: Signal = Signal {
        rssi_dbm: -61,
        snr_quarter_db: 38,
    };

    #[test]
    fn a_real_announce_is_heard_with_everything_python_put_in_it() {
        let mut node = Node::new(BITRATE, 0, &board());
        let out = node.heard(ANNOUNCE, Some(SIGNAL), 1_000, &mut rng());
        assert_eq!(
            out.air,
            Vec::<Vec<u8>>::new(),
            "an endpoint forwards nothing"
        );
        let [heard] = out.heard.as_slice() else {
            panic!("expected one announce, got {:?}", out.heard);
        };
        assert_eq!(heard.destination.as_slice(), vector("destination"));
        assert_eq!(heard.identity.as_slice(), vector("identity"));
        assert_eq!(heard.public_key.as_slice(), vector("public_key"));
        assert_eq!(heard.name_hash.as_slice(), vector("name_hash"));
        assert_eq!(
            heard.app_data.as_deref(),
            Some(vector("app_data").as_slice())
        );
        assert_eq!(heard.hops, 1, "one hop: it came off the air");
        assert_eq!(heard.signal, Some(SIGNAL), "and so its signal is its own");
        assert_eq!(node.paths(), 1);
    }

    #[test]
    fn a_heard_announce_is_never_passed_on() {
        // A transport node rebroadcasts an announce a moment later, from its
        // housekeeping rather than on receipt, so a minute of ticks after
        // hearing one is what shows the board is an endpoint.
        //
        // The node has an identity, which is what the engine needs to
        // rebroadcast at all, so this is the test that turning forwarding on
        // fails: within 250 ms.
        let mut node = Node::new(BITRATE, 0, &board());
        let mut rng = rng();
        assert_eq!(
            node.heard(ANNOUNCE, Some(SIGNAL), 1_000, &mut rng)
                .heard
                .len(),
            1
        );
        for t in 1..=240 {
            let now = 1_000 + t * TICK_MS;
            assert!(
                node.tick(now, &mut rng).air.is_empty(),
                "sent something at {now} ms"
            );
        }
    }

    #[test]
    fn python_announcing_again_a_second_later_is_heard_again() {
        // Two announces stock Reticulum made, a second apart, stamped with
        // Unix time. Before `CLOCK_OFFSET_S` the engine dropped the second.
        let mut node = Node::new(BITRATE, 0, &board());
        let first = node.heard(ANNOUNCE, Some(SIGNAL), 1_000, &mut rng());
        assert_eq!(first.heard.len(), 1);
        let again = node.heard(REANNOUNCE, Some(SIGNAL), 120_000, &mut rng());
        let [heard] = again.heard.as_slice() else {
            panic!("the second announce was not reported: {again:?}");
        };
        assert_eq!(heard.destination.as_slice(), vector("destination"));
        assert_eq!(node.paths(), 1);

        // And the other way round the older one is the replay, and is not.
        let mut node = Node::new(BITRATE, 0, &board());
        assert_eq!(
            node.heard(REANNOUNCE, None, 1_000, &mut rng()).heard.len(),
            1
        );
        assert!(node
            .heard(ANNOUNCE, None, 2_000, &mut rng())
            .heard
            .is_empty());
    }

    #[test]
    fn a_peer_heard_again_is_brought_up_to_date_in_the_table() {
        let mut node = Node::new(BITRATE, 0, &board());
        let mut table = peers::Table::new();
        for (packet, at_ms) in [(ANNOUNCE, 1_000), (REANNOUNCE, 120_000)] {
            for heard in node.heard(packet, Some(SIGNAL), at_ms, &mut rng()).heard {
                table.heard(&heard, at_ms);
            }
        }
        let destination: [u8; 16] = vector("destination").try_into().unwrap();
        assert_eq!(table.get(&destination).unwrap().last_heard_ms, 120_000);
        assert_eq!(table.screen(125_000).rows[0].unwrap().age_s, Some(5));
    }

    #[test]
    fn the_same_announce_twice_is_heard_once() {
        let mut node = Node::new(BITRATE, 0, &board());
        assert_eq!(
            node.heard(ANNOUNCE, Some(SIGNAL), 1_000, &mut rng())
                .heard
                .len(),
            1
        );
        assert_eq!(
            node.heard(ANNOUNCE, Some(SIGNAL), 2_000, &mut rng())
                .heard
                .len(),
            0
        );
    }

    #[test]
    fn a_damaged_announce_is_not_heard() {
        // Every byte flipped in turn: the flags, the hashes, the key, the
        // signature, the app data. Whatever it hits, the signature no longer
        // covers what arrived, or the packet no longer parses.
        //
        // Except the two header bytes Reticulum does not sign. The hop count
        // is incremented by every transport node on the way, so no signature
        // could cover it. The context byte says whether the announce answers
        // a path request, which changes how it is passed on and not what it
        // claims; Python's `validate_announce` does not read it either.
        for i in (0..ANNOUNCE.len()).filter(|&i| i != HOPS && i != CONTEXT) {
            let mut damaged = ANNOUNCE.to_vec();
            damaged[i] ^= 0x01;
            let out = Node::new(BITRATE, 0, &board()).heard(&damaged, None, 1_000, &mut rng());
            let heard_ours = out
                .heard
                .iter()
                .any(|h| h.destination.as_slice() == vector("destination"));
            assert!(
                !heard_ours,
                "byte {i} flipped and the announce was still believed"
            );
        }
    }

    /// Header bytes of a packet with one address: flags, hops, the
    /// destination, then the context.
    const HOPS: usize = 1;
    const CONTEXT: usize = 18;

    #[test]
    fn the_hop_count_is_not_signed_and_is_read_as_sent() {
        let mut relayed = ANNOUNCE.to_vec();
        relayed[HOPS] = 2;
        let out = Node::new(BITRATE, 0, &board()).heard(&relayed, None, 1_000, &mut rng());
        // Two hops before it reached us, and the one onto our radio.
        assert_eq!(out.heard.iter().map(|h| h.hops).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn a_relayed_announce_does_not_carry_the_relays_signal() {
        let mut relayed = ANNOUNCE.to_vec();
        relayed[HOPS] = 1;
        let out = Node::new(BITRATE, 0, &board()).heard(&relayed, Some(SIGNAL), 1_000, &mut rng());
        assert_eq!(out.heard.len(), 1);
        assert_eq!(out.heard[0].hops, 2);
        assert_eq!(out.heard[0].signal, None);
    }

    #[test]
    fn the_inbox_is_bounded_drops_the_oldest_and_counts_it() {
        let mut inbox = Inbox::new();
        for i in 0..Inbox::CAPACITY as u8 {
            assert!(!inbox.push(&[i], Some(SIGNAL), u64::from(i)));
        }
        assert!(inbox.push(&[100], None, 100), "full: one went");
        assert!(inbox.push(&[101], None, 101));
        assert_eq!(inbox.len(), Inbox::CAPACITY);
        assert_eq!(inbox.dropped(), 2);
        let first = inbox.pop().unwrap();
        assert_eq!(
            first,
            Inbound {
                packet: alloc::vec![2],
                signal: Some(SIGNAL),
                at_ms: 2
            },
            "the two oldest went, and the rest kept what they came with"
        );
        assert_eq!(inbox.clear(), Inbox::CAPACITY - 1);
        assert!(inbox.is_empty() && inbox.pop().is_none());
        assert_eq!(inbox.dropped(), 2, "a clear is not a loss to the flood");
    }

    #[test]
    fn a_short_packet_is_nothing_and_not_a_panic() {
        for len in 0..ANNOUNCE.len() {
            let out =
                Node::new(BITRATE, 0, &board()).heard(&ANNOUNCE[..len], None, 1_000, &mut rng());
            assert!(out.heard.is_empty() && out.air.is_empty(), "{len} bytes");
        }
    }

    #[test]
    fn a_path_request_for_our_destination_is_reported() {
        // The vectors' path request asks for the peer's address, so here the
        // board is the peer, and its LXMF address is registered by being its.
        let mut node = Node::new(BITRATE, 0, &peer());
        let ours: [u8; 16] = vector("destination").try_into().unwrap();
        assert_eq!(node.hashes().lxmf, ours);
        let out = node.heard(PATH_REQUEST, Some(SIGNAL), 1_000, &mut rng());
        assert_eq!(out.path_requests, [ours]);
        assert!(
            out.air.is_empty(),
            "answering it needs the keys, so it is the caller's"
        );
    }

    #[test]
    fn a_path_request_for_someone_else_is_ignored() {
        let mut node = Node::new(BITRATE, 0, &board());
        let out = node.heard(PATH_REQUEST, Some(SIGNAL), 1_000, &mut rng());
        assert_eq!(out, Outcome::default());
    }

    #[test]
    fn an_idle_node_sends_nothing() {
        let mut node = Node::new(BITRATE, 0, &board());
        let mut rng = rng();
        for t in 0..400 {
            let out = node.tick(t * TICK_MS, &mut rng);
            assert!(out.air.is_empty(), "sent something at {} ms", t * TICK_MS);
        }
    }

    #[test]
    fn a_radio_with_no_configuration_still_listens() {
        let out = Node::new(0, 0, &board()).heard(ANNOUNCE, None, 1_000, &mut rng());
        assert_eq!(out.heard.len(), 1);
    }

    fn drawn(entropy: &mut Entropy) -> [u8; 32] {
        let mut out = [0u8; 32];
        entropy.fill_bytes(&mut out);
        out
    }

    #[test]
    fn entropy_is_chacha20_from_the_seed() {
        // RFC 8439, appendix A.1, test vector 1: the keystream for an
        // all-zero key and nonce from block 0. If this is not that, the
        // generator is not the one the docs say it is.
        let mut zero = Entropy::from_seed([0; 32]);
        let first = drawn(&mut zero);
        assert_eq!(
            first[..16],
            [
                0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90, 0x40, 0x5d, 0x6a, 0xe5, 0x53, 0x86,
                0xbd, 0x28
            ]
        );
        assert_ne!(drawn(&mut zero), first, "the stream moves on");
        assert_ne!(
            drawn(&mut Entropy::from_seed([1; 32])),
            first,
            "the seed matters"
        );
    }

    #[test]
    fn the_node_runs_on_entropy() {
        let mut entropy = Entropy::from_seed([7; 32]);
        let out = Node::new(BITRATE, 0, &board()).heard(ANNOUNCE, None, 1_000, &mut entropy);
        assert_eq!(out.heard.len(), 1);
    }

    fn bytes(b: &[u8]) -> Arc<[u8]> {
        Arc::from(b)
    }

    #[test]
    fn what_is_addressed_to_the_air_goes_on_the_air_and_nothing_else_does() {
        let other = InterfaceId(2);
        let out = route(alloc::vec![
            TransportAction::SendOnInterface {
                interface: AIR,
                raw: bytes(b"one"),
            },
            TransportAction::SendOnInterface {
                interface: other,
                raw: bytes(b"not ours"),
            },
            TransportAction::BroadcastOnAllInterfaces {
                raw: bytes(b"two"),
                exclude: None,
            },
            TransportAction::BroadcastOnAllInterfaces {
                raw: bytes(b"three"),
                exclude: Some(other),
            },
            TransportAction::BroadcastOnAllInterfaces {
                raw: bytes(b"not on the air"),
                exclude: Some(AIR),
            },
        ]);
        assert_eq!(
            out.air,
            [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );
        assert_eq!(out.ignored, 2);
    }
}

#[cfg(test)]
mod announcing {
    use super::announce::Kind;
    use super::*;
    use rns_crypto::FixedRng;

    const VECTORS: &str = include_str!("node/vectors/vectors.txt");
    const PATH_REQUEST: &[u8] = include_bytes!("node/vectors/path_request.bin");
    const BOARD_ANNOUNCE: &[u8] = include_bytes!("node/vectors/board_announce.bin");
    const BOARD_PATH_RESPONSE: &[u8] = include_bytes!("node/vectors/board_path_response.bin");

    /// The board in the vectors: a key from a fixed seed, its name, and an
    /// emission time of 2026-10-24 00:00:00 UTC.
    fn board() -> NodeKey {
        NodeKey::generate(&mut Entropy::from_seed([0x0b; 32]))
    }
    const NAME: &str = "oxinode 7F23";
    const EMITTED: u64 = 1_792_800_000;

    fn peer() -> NodeKey {
        NodeKey::generate(&mut Entropy::from_seed([0x0c; 32]))
    }

    fn rng() -> FixedRng {
        FixedRng::new(&[0x5a; 64])
    }

    fn make(kind: Kind) -> Vec<u8> {
        Node::new(1757, 0, &board())
            .announce(kind, &lxmf_app_data(NAME), EMITTED, &mut rng())
            .unwrap()
    }

    /// The announce a peer hears: verified by the same engine, in a node
    /// with an identity of its own.
    fn heard_by_a_peer(packet: &[u8]) -> Outcome {
        Node::new(1757, 0, &peer()).heard(packet, None, 1_000, &mut rng())
    }

    #[test]
    fn the_app_data_is_lxmfs_to_the_byte() {
        let line = VECTORS
            .lines()
            .find_map(|l| l.strip_prefix("app_data "))
            .unwrap();
        let python: Vec<u8> = (0..line.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(lxmf_app_data("Bench peer"), python);
    }

    #[test]
    fn a_peer_hears_the_board_by_its_address_and_name() {
        let packet = make(Kind::Announce);
        assert_eq!(packet.len(), announce_len(lxmf_app_data(NAME).len()));
        let out = heard_by_a_peer(&packet);
        let [heard] = out.heard.as_slice() else {
            panic!("{out:?}");
        };
        let hashes = board().hashes();
        assert_eq!(heard.destination, hashes.lxmf);
        assert_eq!(heard.identity, hashes.identity);
        assert_eq!(
            heard.name_hash,
            rns_core::destination::name_hash("lxmf", &["delivery"])
        );
        assert_eq!(heard.app_data.as_deref(), Some(&lxmf_app_data(NAME)[..]));
    }

    #[test]
    fn a_path_response_is_the_same_announce_marked_as_one() {
        let ordinary = make(Kind::Announce);
        let response = make(Kind::PathResponse);
        // Byte 18, the context, says which; nothing signed differs.
        assert_eq!(ordinary[18], constants::CONTEXT_NONE);
        assert_eq!(response[18], constants::CONTEXT_PATH_RESPONSE);
        assert_eq!(ordinary[..18], response[..18]);
        assert_eq!(ordinary[19..], response[19..]);
        assert_eq!(heard_by_a_peer(&response).heard.len(), 1);
    }

    #[test]
    fn the_random_hash_ends_in_the_emission_time_and_starts_random() {
        let packet = make(Kind::Announce);
        let at = 19 + 64 + 10;
        assert_eq!(packet[at + 5..at + 10], EMITTED.to_be_bytes()[3..]);
        let mut entropy = Entropy::from_seed([1; 32]);
        let node = Node::new(1757, 0, &board());
        let a = node
            .announce(Kind::Announce, &[], EMITTED, &mut entropy)
            .unwrap();
        let b = node
            .announce(Kind::Announce, &[], EMITTED, &mut entropy)
            .unwrap();
        assert_ne!(a[at..at + 5], b[at..at + 5]);
        // And with no clock, zero, which Reticulum reads as not known.
        let unknown = node.announce(Kind::Announce, &[], 0, &mut entropy).unwrap();
        assert_eq!(unknown[at + 5..at + 10], [0; 5]);
    }

    #[test]
    fn a_path_request_for_the_board_is_answered_with_one_a_peer_believes() {
        // The vectors' path request asks for the Python peer's address, so
        // the board here is that identity.
        let python = key::NodeKey::from_private(core::array::from_fn(|i| i as u8));
        let mut node = Node::new(1757, 0, &python);
        let out = node.heard(PATH_REQUEST, None, 1_000, &mut rng());
        assert_eq!(out.path_requests, [python.hashes().lxmf]);
        let answer = node
            .announce(Kind::PathResponse, &lxmf_app_data(NAME), 0, &mut rng())
            .unwrap();
        let heard = heard_by_a_peer(&answer);
        assert_eq!(heard.heard.len(), 1);
        assert_eq!(heard.heard[0].destination, python.hashes().lxmf);
    }

    /// The peer's announce emitted at `emitted_s`, as the board makes them:
    /// the same signing, the same framing, a new random half each time.
    fn peer_announce(emitted_s: u64, entropy: &mut Entropy) -> Vec<u8> {
        Node::new(1757, 0, &peer())
            .announce(Kind::Announce, &lxmf_app_data("Peer"), emitted_s, entropy)
            .unwrap()
    }

    #[test]
    fn a_peer_announcing_again_with_the_time_is_heard_again() {
        // What every Sideband, MeshChat and GPS-clocked oxinode sends: Unix
        // seconds. Told seconds since boot, the engine took these for
        // unstamped and dropped the second; see `CLOCK_OFFSET_S`.
        let mut entropy = Entropy::from_seed([4; 32]);
        let mut node = Node::new(1757, 0, &board());
        for (n, (emitted, at_ms)) in [
            (EMITTED, 10_000),
            (EMITTED + 100, 140_000),
            (EMITTED + 1_800, 1_900_000),
        ]
        .into_iter()
        .enumerate()
        {
            let out = node.heard(
                &peer_announce(emitted, &mut entropy),
                None,
                at_ms,
                &mut rng(),
            );
            assert_eq!(out.heard.len(), 1, "announce {n}, emitted {emitted}");
        }
    }

    #[test]
    fn an_older_or_unstamped_announce_after_a_newer_one_is_not() {
        // Python's rule, and the engine's: a later announce over the same
        // path must be strictly newer. A replay of an older one is not, and
        // neither is a board with no clock announcing again.
        let mut entropy = Entropy::from_seed([5; 32]);
        let mut node = Node::new(1757, 0, &board());
        let first = node.heard(
            &peer_announce(EMITTED, &mut entropy),
            None,
            10_000,
            &mut rng(),
        );
        assert_eq!(first.heard.len(), 1);
        for emitted in [EMITTED - 1, EMITTED, 0] {
            let out = node.heard(
                &peer_announce(emitted, &mut entropy),
                None,
                20_000,
                &mut rng(),
            );
            assert!(out.heard.is_empty(), "emitted {emitted}");
        }
        let mut node = Node::new(1757, 0, &board());
        node.heard(&peer_announce(0, &mut entropy), None, 10_000, &mut rng());
        let again = node.heard(&peer_announce(0, &mut entropy), None, 20_000, &mut rng());
        assert!(again.heard.is_empty(), "unstamped twice is not newer");
    }

    #[test]
    fn a_stamp_from_the_far_future_still_counts() {
        // The largest five bytes can say: still behind the engine's clock.
        const MAX: u64 = MAX_EMITTED_S;
        let mut entropy = Entropy::from_seed([6; 32]);
        let mut node = Node::new(1757, 0, &board());
        node.heard(
            &peer_announce(MAX - 1, &mut entropy),
            None,
            10_000,
            &mut rng(),
        );
        let out = node.heard(&peer_announce(MAX, &mut entropy), None, 20_000, &mut rng());
        assert_eq!(out.heard.len(), 1);
    }

    #[test]
    fn the_board_does_not_take_its_own_announce_for_a_peer() {
        // Relayed back by a transport node, one hop on.
        let mut echoed = make(Kind::Announce);
        echoed[1] = 1;
        let mut node = Node::new(1757, 0, &board());
        let out = node.heard(&echoed, None, 1_000, &mut rng());
        assert!(
            !out.heard
                .iter()
                .any(|h| h.destination == board().hashes().lxmf),
            "{out:?}"
        );
    }

    #[test]
    fn the_vectors_are_what_the_board_makes() {
        // Ed25519 is deterministic and the rest is fixed, so the board's
        // announce is the same bytes every time. They are committed for
        // tools/test_peer.py, which holds them to the peer tool's reading
        // and to Reticulum's. OXINODE_WRITE_VECTORS=1 rewrites them.
        let announce = make(Kind::Announce);
        let response = make(Kind::PathResponse);
        if std::env::var_os("OXINODE_WRITE_VECTORS").is_some() {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/node/vectors");
            std::fs::write(format!("{dir}/board_announce.bin"), &announce).unwrap();
            std::fs::write(format!("{dir}/board_path_response.bin"), &response).unwrap();
            return;
        }
        assert_eq!(announce, BOARD_ANNOUNCE);
        assert_eq!(response, BOARD_PATH_RESPONSE);
    }

    use announce::{gap_ms, INTERVAL_MS};

    /// A node that started at `t` and sent its first announce at once.
    fn sending_after_first_announce(t: u64, airtime_us: u32) -> Sending {
        let mut s = Sending::new();
        assert_eq!(s.follow(true, t), Some(Followed::Started));
        assert_eq!(s.due(t), Some(Kind::Announce));
        s.push(alloc::vec![1], Some(Kind::Announce));
        let (_, kind) = s.next(true).unwrap();
        s.sent(kind, Some(airtime_us), t);
        s
    }

    #[test]
    fn a_node_that_starts_announces_at_once() {
        let mut s = Sending::new();
        assert_eq!(s.due(0), None, "not before it is running");
        assert_eq!(s.follow(true, 0), Some(Followed::Started));
        assert_eq!(s.due(0), Some(Kind::Announce));
        assert_eq!(
            s.follow(true, 1),
            None,
            "running twice is not starting twice"
        );
    }

    #[test]
    fn nothing_goes_while_somebody_is_on_the_line() {
        let mut s = Sending::new();
        s.follow(true, 0);
        s.push(alloc::vec![1], None);
        assert_eq!(s.next(false), None, "a port open or a phone linked");
        assert!(s.next(true).is_some(), "and it is still there afterwards");
    }

    #[test]
    fn a_host_arriving_drops_what_was_waiting_and_stops_the_asking() {
        let mut s = sending_after_first_announce(0, 1_000_000);
        s.want(Kind::PathResponse);
        s.push(alloc::vec![2], None);
        s.push(alloc::vec![3], Some(Kind::Announce));
        assert_eq!(s.follow(false, 5_000), Some(Followed::Stopped(2)));
        assert_eq!(s.dropped(), 2);
        assert_eq!(s.next(true), None);
        assert_eq!(
            s.due(INTERVAL_MS * 4),
            None,
            "no interval runs under a host"
        );
        s.want(Kind::Announce);
        s.push(alloc::vec![4], None);
        assert_eq!(s.next(true), None, "and nothing new is taken while stopped");
    }

    #[test]
    fn a_host_arriving_while_a_packet_is_on_its_way_is_counted_as_not_sent() {
        // The packet had been handed out when the port opened; the wait was
        // given up, and the announce is wanted again -- for after the host.
        let mut s = Sending::new();
        s.follow(true, 0);
        assert_eq!(s.due(0), Some(Kind::Announce));
        s.push(alloc::vec![1], Some(Kind::Announce));
        let (_, kind) = s.next(true).unwrap();
        s.sent(kind, None, 100);
        assert_eq!(s.follow(false, 100), Some(Followed::Stopped(0)));
        assert_eq!(s.due(1_000), None, "not under the host");
        assert_eq!(s.follow(true, 10_000), Some(Followed::Started));
        assert_eq!(s.due(10_000), Some(Kind::Announce), "but once it has gone");
    }

    #[test]
    fn resuming_after_a_host_waits_out_the_last_announces_gap() {
        let air = 2_000_000; // 100 s of gap
        let mut s = sending_after_first_announce(0, air);
        s.follow(false, 10_000);
        assert_eq!(s.follow(true, 20_000), Some(Followed::Started));
        assert_eq!(s.due(20_000), None);
        assert_eq!(s.due(gap_ms(air) - 1), None);
        assert_eq!(s.due(gap_ms(air)), Some(Kind::Announce));
    }

    #[test]
    fn a_failed_announce_is_tried_again_inside_the_budget() {
        let mut s = Sending::new();
        s.follow(true, 0);
        assert_eq!(s.due(0), Some(Kind::Announce));
        s.sent(Some(Kind::Announce), None, 0);
        assert_eq!(s.due(1), Some(Kind::Announce));
    }

    #[test]
    fn the_engines_own_packets_do_not_touch_the_budget() {
        let mut s = sending_after_first_announce(0, 1_000_000);
        s.sent(None, Some(5_000_000), 1_000);
        assert_eq!(s.allowed_from(), gap_ms(1_000_000));
    }

    #[test]
    fn the_queue_is_bounded_and_counts_what_it_loses() {
        let mut q = Queue::new();
        for i in 0..Queue::CAPACITY as u8 + 2 {
            q.push(alloc::vec![i], None);
        }
        assert_eq!(q.len(), Queue::CAPACITY);
        assert_eq!(q.dropped(), 2);
        assert_eq!(q.pop().unwrap().0, [2], "the oldest went first");
        assert_eq!(q.clear(), Queue::CAPACITY - 1);
        assert!(q.is_empty());
        assert_eq!(q.dropped(), 2 + Queue::CAPACITY as u32 - 1);
    }
}

#[cfg(test)]
mod receiving {
    use super::lxmf::tests::{board, vector};
    use super::messages::Trust;
    use super::*;
    use rns_core::receipt::{validate_proof, ProofResult};
    use rns_crypto::FixedRng;

    const ANNOUNCE: &[u8] = include_bytes!("node/vectors/announce.bin");
    const MESSAGE: &[u8] = include_bytes!("node/vectors/message.bin");
    const RETRY: &[u8] = include_bytes!("node/vectors/message_retry.bin");
    const STRANGER: &[u8] = include_bytes!("node/vectors/stranger_message.bin");
    const STRANGER_ANNOUNCE: &[u8] = include_bytes!("node/vectors/stranger_announce.bin");
    const PROOF: &[u8] = include_bytes!("node/vectors/proof.bin");
    const PATH_REQUEST: &[u8] = include_bytes!("node/vectors/path_request.bin");

    fn rng() -> FixedRng {
        FixedRng::new(&[0x5a; 64])
    }

    /// The board, and what it knows, having heard nobody.
    fn fresh() -> (Node, Station) {
        (Node::new(1757, 0, &board()), Station::new())
    }

    /// The board, having heard the bench peer announce.
    fn knowing_the_peer() -> (Node, Station) {
        let (mut node, mut station) = fresh();
        hear(&mut node, &mut station, ANNOUNCE, 1_000);
        (node, station)
    }

    /// A packet off the air, all the way through: what went on the air in
    /// answer, and what the station made of it.
    fn hear(
        node: &mut Node,
        station: &mut Station,
        packet: &[u8],
        at_ms: u64,
    ) -> (Vec<Vec<u8>>, Vec<Event>) {
        let mut out = node.heard(packet, None, at_ms, &mut rng());
        let events = station.take(node, &mut out, at_ms, &mut rng());
        (out.air, events)
    }

    fn board_identity() -> rns_crypto::identity::Identity {
        let public = board().identity().get_public_key().unwrap();
        rns_crypto::identity::Identity::from_public_key(&public).unwrap()
    }

    #[test]
    fn a_message_from_a_peer_is_proved_with_the_proof_python_makes() {
        let (mut node, mut station) = knowing_the_peer();
        let (air, events) = hear(&mut node, &mut station, MESSAGE, 5_000);
        let hash: [u8; 32] = vector("message_hash").try_into().unwrap();
        assert_eq!(
            events,
            [Event::Message {
                hash,
                source: vector("sender").try_into().unwrap(),
                arrival: Arrival::New(Trust::Verified),
            }]
        );
        assert_eq!(air, [PROOF.to_vec()], "Reticulum's own proof, to the byte");
        let packet_hash: [u8; 32] = vector("packet_hash").try_into().unwrap();
        assert_eq!(
            validate_proof(&air[0][19..], &packet_hash, &board_identity()),
            ProofResult::Valid
        );
        assert_eq!(station.messages.unread(), 1);
        assert_eq!(
            station
                .messages
                .get(&hash)
                .unwrap()
                .message
                .content
                .as_bytes(),
            vector("content")
        );
    }

    #[test]
    fn a_retry_gets_a_proof_of_its_own_and_is_not_a_second_message() {
        let (mut node, mut station) = knowing_the_peer();
        hear(&mut node, &mut station, MESSAGE, 5_000);
        let (air, events) = hear(&mut node, &mut station, RETRY, 9_000);
        assert!(matches!(
            events[..],
            [Event::Message {
                arrival: Arrival::Duplicate(Some(Trust::Verified)),
                ..
            }]
        ));
        let [proof] = air.as_slice() else {
            panic!("{air:?}")
        };
        assert_ne!(proof[..], PROOF[..], "another packet, another proof");
        let retry = RawPacket::unpack(RETRY).unwrap();
        assert_eq!(proof[2..18], retry.packet_hash[..16]);
        assert_eq!(
            validate_proof(&proof[19..], &retry.packet_hash, &board_identity()),
            ProofResult::Valid
        );
        assert_eq!(station.messages.len(), 1);
    }

    #[test]
    fn the_same_packet_twice_is_the_engines_duplicate_and_is_not_proved_again() {
        // Heard twice, relayed by two neighbours: Reticulum's packet filter
        // drops the second, and so the board does as Python does.
        let (mut node, mut station) = knowing_the_peer();
        assert_eq!(hear(&mut node, &mut station, MESSAGE, 5_000).0.len(), 1);
        let (air, events) = hear(&mut node, &mut station, MESSAGE, 6_000);
        assert!(air.is_empty() && events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_stranger_is_kept_proved_and_asked_for_and_its_announce_settles_it() {
        let (mut node, mut station) = fresh();
        let (air, events) = hear(&mut node, &mut station, STRANGER, 5_000);
        let stranger: [u8; 16] = vector("stranger").try_into().unwrap();
        assert!(matches!(
            events[..],
            [Event::Message {
                arrival: Arrival::New(Trust::Unverified),
                ..
            }]
        ));
        let [proof, request] = air.as_slice() else {
            panic!("{air:?}")
        };
        let packet = RawPacket::unpack(STRANGER).unwrap();
        assert_eq!(
            validate_proof(&proof[19..], &packet.packet_hash, &board_identity()),
            ProofResult::Valid
        );
        // The request is Python's in all but its random tag.
        assert_eq!(request.len(), PATH_REQUEST.len());
        assert_eq!(request[..19], PATH_REQUEST[..19]);
        assert_eq!(request[19..35], stranger);
        // And the stranger's node hears it as a request for its address.
        let stranger_key = key::NodeKey::from_private(core::array::from_fn(|i| 128 + i as u8));
        assert_eq!(stranger_key.hashes().lxmf, stranger);
        let asked = Node::new(1757, 0, &stranger_key).heard(request, None, 1_000, &mut rng());
        assert_eq!(asked.path_requests, [stranger]);

        // Its answer settles the message.
        let (air, events) = hear(&mut node, &mut station, STRANGER_ANNOUNCE, 8_000);
        assert!(air.is_empty());
        assert_eq!(
            events,
            [
                Event::Peer {
                    destination: stranger,
                    learned: Learned::New
                },
                Event::Settled {
                    source: stranger,
                    settled: Settled {
                        verified: 1,
                        failed: 0
                    }
                }
            ]
        );
        assert!(station.peers.get(&stranger).unwrap().conversation);
    }

    #[test]
    fn what_the_board_cannot_read_is_said_and_neither_kept_nor_proved() {
        let (mut node, mut station) = knowing_the_peer();
        // Its last byte changed: the token's HMAC no longer holds.
        let mut damaged = MESSAGE.to_vec();
        *damaged.last_mut().unwrap() ^= 1;
        let (air, events) = hear(&mut node, &mut station, &damaged, 5_000);
        assert!(air.is_empty());
        assert_eq!(events, [Event::Unreadable(Unreadable::Undecryptable)]);
        // A context the board does not read.
        let mut other = MESSAGE.to_vec();
        other[18] = 0x09;
        let (air, events) = hear(&mut node, &mut station, &other, 6_000);
        assert!(air.is_empty());
        assert_eq!(
            events,
            [Event::Unreadable(Unreadable::NotOpportunistic {
                packet_type: constants::PACKET_TYPE_DATA,
                context: 0x09
            })]
        );
        assert!(station.messages.is_empty());
    }

    #[test]
    fn a_short_message_packet_is_nothing_and_not_a_panic() {
        for len in 0..MESSAGE.len() {
            let (mut node, mut station) = knowing_the_peer();
            let (air, _) = hear(&mut node, &mut station, &MESSAGE[..len], 5_000);
            assert!(air.is_empty(), "{len} bytes");
            assert!(station.messages.is_empty(), "{len} bytes");
        }
    }
}

#[cfg(test)]
mod sending {
    use super::announce::Kind;
    use super::lxmf::tests::board;
    use super::outbox::wait_ms;
    use super::*;
    use crate::screens::{Delivery, Direction, Wait};
    use rns_crypto::FixedRng;

    const ANNOUNCE: &[u8] = include_bytes!("node/vectors/announce.bin");
    const BOARD_MESSAGE: &[u8] = include_bytes!("node/vectors/board_message.bin");

    fn rng() -> FixedRng {
        FixedRng::new(&[0x5a; 64])
    }

    /// The bench peer's key: the one whose announce is in the vectors.
    fn peer_key() -> NodeKey {
        NodeKey::from_private(core::array::from_fn(|i| i as u8))
    }

    /// The board and the bench peer, each having heard the other announce.
    struct Bench {
        board: Node,
        station: Station,
        peer: Node,
        peers_station: Station,
        /// One stream for the whole bench, as the board has one: so two
        /// attempts are encrypted with different keys, and the run is the
        /// same every time.
        rng: Entropy,
    }

    fn bench() -> Bench {
        let mut b = Bench {
            board: Node::new(1757, 0, &board()),
            station: Station::new(),
            peer: Node::new(1757, 0, &peer_key()),
            peers_station: Station::new(),
            rng: Entropy::from_seed([0x0d; 32]),
        };
        let mut out = b.board.heard(ANNOUNCE, None, 1_000, &mut rng());
        b.station.take(&b.board, &mut out, 1_000, &mut rng());
        let ours = b
            .board
            .announce(
                Kind::Announce,
                &lxmf_app_data("oxinode 7F23"),
                0,
                &mut rng(),
            )
            .unwrap();
        let mut out = b.peer.heard(&ours, None, 1_000, &mut rng());
        b.peers_station.take(&b.peer, &mut out, 1_000, &mut rng());
        b
    }

    fn to() -> [u8; 16] {
        peer_key().hashes().lxmf
    }

    fn attempt(b: &mut Bench, now: u64) -> ([u8; 32], Attempt) {
        let mut events = Vec::new();
        match b.station.step(&mut b.board, now, &mut b.rng, &mut events) {
            Some(Step::Attempt { hash, attempt }) => (hash, attempt),
            other => panic!("expected an attempt at {now} ms, got {other:?} and {events:?}"),
        }
    }

    /// The peer hears a packet: what it sends back, and what it made of it.
    fn peer_hears(b: &mut Bench, packet: &[u8], now: u64) -> (Vec<Vec<u8>>, Vec<Event>) {
        let mut out = b.peer.heard(packet, None, now, &mut rng());
        let events = b.peers_station.take(&b.peer, &mut out, now, &mut rng());
        (out.air, events)
    }

    fn board_hears(b: &mut Bench, packet: &[u8], now: u64) -> Vec<Event> {
        let mut out = b.board.heard(packet, None, now, &mut rng());
        b.station.take(&b.board, &mut out, now, &mut rng())
    }

    fn delivery(b: &Bench) -> Delivery {
        b.station.outbox.iter().next().unwrap().delivery
    }

    #[test]
    fn a_message_goes_out_is_read_and_verified_and_its_proof_delivers_it() {
        let mut b = bench();
        let hash = b
            .station
            .compose(&b.board, &to(), "Yes", 1_792_800_000, 5_000, None)
            .unwrap();
        assert!(b.station.peers.get(&to()).unwrap().conversation);
        let (h, attempt) = attempt(&mut b, 5_000);
        assert_eq!(h, hash);
        b.station
            .outbox
            .went(&hash, &attempt.packet_hash, Some(400_000), 5_500);
        assert_eq!(delivery(&b), Delivery::Sent(1));

        let (air, events) = peer_hears(&mut b, &attempt.packet, 6_000);
        let source = board().hashes().lxmf;
        assert_eq!(
            events,
            [Event::Message {
                hash,
                source,
                arrival: Arrival::New(messages::Trust::Verified)
            }],
            "the peer reads it, signed by the key the board announced"
        );
        let got = b.peers_station.messages.get(&hash).unwrap();
        assert_eq!(got.message.content, "Yes");
        assert_eq!(got.message.sent_s, Some(1_792_800_000));
        let [proof] = air.as_slice() else {
            panic!("{air:?}")
        };
        assert_eq!(
            board_hears(&mut b, proof, 7_000),
            [Event::Delivered { hash, to: to() }]
        );
        assert_eq!(delivery(&b), Delivery::Delivered);
        assert_eq!(b.station.outbox.delivered(), 1);
        // Heard twice, relayed, it is the engine's duplicate and nothing.
        assert!(board_hears(&mut b, proof, 8_000).is_empty());
        let mut events = Vec::new();
        assert_eq!(
            b.station
                .step(&mut b.board, 1 << 40, &mut rng(), &mut events),
            None
        );
    }

    #[test]
    fn a_retry_is_a_new_packet_with_the_same_message_and_either_proof_counts() {
        let mut b = bench();
        let hash = b
            .station
            .compose(&b.board, &to(), "Yes", 0, 0, None)
            .unwrap();
        let (_, first) = attempt(&mut b, 0);
        b.station
            .outbox
            .went(&hash, &first.packet_hash, Some(400_000), 0);
        let (_, second) = attempt(&mut b, wait_ms(1, 400_000));
        assert_ne!(first.packet, second.packet, "encrypted afresh");
        assert_ne!(first.packet_hash, second.packet_hash);
        b.station
            .outbox
            .went(&hash, &second.packet_hash, Some(400_000), 20_000);
        assert_eq!(delivery(&b), Delivery::Sent(2));

        let (_, events) = peer_hears(&mut b, &second.packet, 21_000);
        assert!(matches!(
            events[..],
            [Event::Message {
                arrival: Arrival::New(_),
                ..
            }]
        ));
        // The first went astray and arrives late: the same message, proved.
        let (air, events) = peer_hears(&mut b, &first.packet, 22_000);
        assert!(matches!(
            events[..],
            [Event::Message {
                arrival: Arrival::Duplicate(_),
                ..
            }]
        ));
        assert_eq!(
            board_hears(&mut b, &air[0], 23_000),
            [Event::Delivered { hash, to: to() }],
            "a proof of the first attempt"
        );
    }

    #[test]
    fn a_proof_somebody_else_signed_delivers_nothing() {
        let mut b = bench();
        let hash = b
            .station
            .compose(&b.board, &to(), "Yes", 0, 0, None)
            .unwrap();
        let (_, attempt) = attempt(&mut b, 0);
        b.station
            .outbox
            .went(&hash, &attempt.packet_hash, Some(400_000), 0);
        // A node that is not the recipient proves the board's packet.
        let other = Node::new(1757, 0, &NodeKey::from_private([7; 64]));
        let forged = other.prove(&attempt.packet_hash).unwrap();
        assert_eq!(board_hears(&mut b, &forged, 1_000), [Event::Unclaimed]);
        assert_eq!(delivery(&b), Delivery::Sent(1));
        // And a proof of a packet the board never sent is nobody's.
        let stray = b.peer.prove(&[3; 32]).unwrap();
        assert_eq!(board_hears(&mut b, &stray, 2_000), [Event::Unclaimed]);
    }

    #[test]
    fn to_a_peer_never_heard_it_asks_for_the_path_and_goes_when_the_key_comes() {
        let mut b = bench();
        b.board = Node::new(1757, 0, &board());
        b.station = Station::new();
        b.station
            .compose(&b.board, &to(), "Yes", 0, 0, None)
            .unwrap();
        let mut events = Vec::new();
        let step = b.station.step(&mut b.board, 0, &mut rng(), &mut events);
        assert_eq!(events, [Event::Asked { to: to() }]);
        let Some(Step::Packet(request)) = step else {
            panic!("{step:?}")
        };
        // The peer hears the request as one for its own address.
        assert_eq!(
            b.peer.heard(&request, None, 100, &mut rng()).path_requests,
            [to()]
        );
        assert_eq!(delivery(&b), Delivery::Queued(Wait::Path));
        // Its answer is its announce; the next step is the message.
        board_hears(&mut b, ANNOUNCE, 3_000);
        attempt(&mut b, 3_000);
    }

    #[test]
    fn written_under_a_host_it_is_held_and_says_so() {
        let mut b = bench();
        b.station
            .compose(&b.board, &to(), "Yes", 0, 0, Some(Wait::Host))
            .unwrap();
        let mut events = Vec::new();
        assert_eq!(
            b.station.step(&mut b.board, 0, &mut rng(), &mut events),
            None
        );
        let entries = b.station.entries(&to(), 1_000);
        assert_eq!(
            entries[0].direction,
            Direction::Sent(Delivery::Queued(Wait::Host))
        );
        b.station.outbox.resume(5_000);
        attempt(&mut b, 5_000);
    }

    #[test]
    fn a_message_that_does_not_fit_is_refused_not_cut() {
        let b = bench();
        let mut station = Station::new();
        let long = "x".repeat(lxmf::MAX_CONTENT + 1);
        assert_eq!(
            station.compose(&b.board, &to(), &long, 0, 0, None),
            Err(Unsent::TooLong(1))
        );
        assert!(station.outbox.is_empty());
        assert!(station.peers.get(&to()).is_none());
    }

    #[test]
    fn the_longest_message_fits_even_through_a_transport_node() {
        let mut b = bench();
        b.board = Node::new(1757, 0, &board());
        b.station = Station::new();
        // The peer heard through one relay: two addresses in the header,
        // the relay's first, and marked as in transport.
        let mut relayed = ANNOUNCE.to_vec();
        relayed[1] = 1;
        relayed[0] |= 0x50;
        let mut header2 = alloc::vec![relayed[0], relayed[1]];
        header2.extend_from_slice(&[0xab; 16]);
        header2.extend_from_slice(&relayed[2..]);
        board_hears(&mut b, &header2, 1_000);
        assert!(
            b.station.peers.get(&to()).is_some(),
            "heard through the relay"
        );
        let longest = "x".repeat(lxmf::MAX_CONTENT);
        b.station
            .compose(&b.board, &to(), &longest, 0, 2_000, None)
            .unwrap();
        let (_, attempt) = attempt(&mut b, 2_000);
        let packet = RawPacket::unpack(&attempt.packet).unwrap();
        assert_eq!(packet.flags.header_type, constants::HEADER_2);
        assert_eq!(packet.transport_id, Some([0xab; 16]));
        assert!(attempt.packet.len() <= constants::MTU);
        assert_eq!(attempt.packet.len(), constants::MTU - 1);
        assert_eq!(
            packet.packet_hash, attempt.packet_hash,
            "the hash is the same either way"
        );
    }

    #[test]
    fn a_thread_is_both_ways_in_the_order_written_and_reading_counts_both() {
        let mut b = bench();
        // The peer writes; the board answers.
        let incoming =
            b.peers_station
                .compose(&b.peer, &board().hashes().lxmf, "Where?", 0, 2_000, None);
        assert!(incoming.is_ok());
        let mut events = Vec::new();
        let Some(Step::Attempt { attempt, .. }) =
            b.peers_station
                .step(&mut b.peer, 2_000, &mut rng(), &mut events)
        else {
            panic!("{events:?}")
        };
        board_hears(&mut b, &attempt.packet, 3_000);
        b.station
            .compose(&b.board, &to(), "Here", 0, 4_000, None)
            .unwrap();

        let entries = b.station.entries(&to(), 10_000);
        let texts: Vec<&str> = entries.iter().map(|e| e.content).collect();
        assert_eq!(texts, ["Where?", "Here"]);
        assert!(matches!(entries[0].direction, Direction::Received(_)));
        assert!(matches!(entries[1].direction, Direction::Sent(_)));
        let list = b.station.conversations(10_000);
        assert_eq!(list.rows[0].unwrap().age_s, Some(6), "aged by the reply");
        assert!(list.rows[0].unwrap().unread);
        assert!(list.rows[1].is_none(), "one conversation, both ways");
        assert_eq!(
            b.station.read(
                crate::screens::Read {
                    peer: to(),
                    through: 2
                },
                10_000
            ),
            1
        );
        assert!(!b.station.conversations(10_000).rows[0].unwrap().unread);
    }

    #[test]
    fn the_clock_is_the_gps_then_the_newest_stamp_heard_then_zero() {
        let mut b = bench();
        assert_eq!(b.station.clock_s(None, 5_000), 0);
        assert_eq!(b.station.clock_s(Some(1_800_000_000), 5_000), 1_800_000_000);
        b.peers_station
            .compose(
                &b.peer,
                &board().hashes().lxmf,
                "hi",
                1_792_800_000,
                0,
                None,
            )
            .unwrap();
        let mut events = Vec::new();
        let Some(Step::Attempt { attempt, .. }) =
            b.peers_station
                .step(&mut b.peer, 0, &mut rng(), &mut events)
        else {
            panic!()
        };
        board_hears(&mut b, &attempt.packet, 10_000);
        assert_eq!(b.station.clock_s(None, 10_000), 1_792_800_000);
        assert_eq!(
            b.station.clock_s(None, 75_500),
            1_792_800_065,
            "run on since"
        );
        assert_eq!(b.station.clock_s(Some(5), 75_500), 5, "the GPS first");
    }

    #[test]
    fn a_failure_is_an_event() {
        let mut b = bench();
        b.station = Station::new();
        b.station
            .compose(&b.board, &to(), "Yes", 0, 0, None)
            .unwrap();
        let mut events = Vec::new();
        b.station.step(&mut b.board, 0, &mut rng(), &mut events);
        events.clear();
        let mut now = 0;
        while events.is_empty() {
            now += 10_000;
            b.station.step(&mut b.board, now, &mut rng(), &mut events);
            events.retain(|e| !matches!(e, Event::Asked { .. }));
        }
        assert_eq!(now, outbox::PATH_TIMEOUT_MS);
        assert!(matches!(
            events[..],
            [Event::Failed {
                why: Failure::NoPath,
                ..
            }]
        ));
        assert!(b.station.outbox.due(now, &b.station.peers).is_none());
    }

    /// From the gestures to the packet: open the conversation, open its
    /// menu, go down to *Yes*, press OK -- what the modem loop does with the
    /// action -- and the peer reads "Yes" from the board.
    #[test]
    fn a_reply_chosen_on_the_pad_is_the_message_the_peer_reads() {
        use crate::ui::{self, Input, NavExt};
        let mut b = bench();
        let mut nav = ui::nav();
        nav.open_conversation(to());
        nav.handle(Input::Select);
        nav.handle(Input::Down); // Type a Message
        nav.handle(Input::Down); // OK
        nav.handle(Input::Down); // Yes
        let action = nav.handle(Input::Select).unwrap();
        assert_eq!(action, ui::Action::Reply(1));
        let peer = nav.conversation().unwrap().peer();
        let text = action.reply().unwrap();
        let hash = b
            .station
            .compose(&b.board, &peer, text, 0, 0, None)
            .unwrap();
        let (_, attempt) = attempt(&mut b, 0);
        let (air, _) = peer_hears(&mut b, &attempt.packet, 500);
        let got = b.peers_station.messages.get(&hash).unwrap();
        assert_eq!(got.message.content, "Yes");
        assert_eq!(got.message.source, board().hashes().lxmf);
        assert_eq!(air.len(), 1, "and proves it");
    }

    /// The same, typed: open the keyboard from the menu, type "Hi 2" --
    /// *Shift*, a capital, a space, a digit -- press *Send*, and the peer
    /// reads exactly that.
    #[test]
    fn a_message_typed_on_the_pad_is_the_message_the_peer_reads() {
        use crate::ui::{self, Input, NavExt};
        use Input::{Down, Left, Right, Select, Up};
        let mut b = bench();
        let mut nav = ui::nav();
        nav.open_conversation(to());
        nav.handle(Select);
        nav.handle(Down);
        assert_eq!(nav.handle(Select), Some(ui::Action::Compose));
        nav.compose();
        // From h: down to the bottom row, left round to Shift; up to h's
        // row and round to h again; i; Space; then 2, and Send.
        let presses = [
            Down, Left, Left, Left, Left, Left, Left, Select, // Shift
            Up, Right, Right, Right, Right, Right, Right, Select, // H
            Up, Right, Right, Select, // i
            Down, Right, Right, Right, Select, // Space
            Up, Up, Right, Right, Select, // 2
            Down, Left, Left, Select, // Send
        ];
        let mut chosen = Vec::new();
        for input in presses {
            chosen.extend(nav.handle(input));
        }
        assert_eq!(chosen, [ui::Action::SendTyped]);
        let peer = nav.conversation().unwrap().peer();
        let text = nav.typing().unwrap().text();
        assert_eq!(text, "Hi 2");
        let hash = b
            .station
            .compose(&b.board, &peer, text, 0, 0, None)
            .unwrap();
        nav.written();
        assert!(nav.typing().is_none());
        let (_, attempt) = attempt(&mut b, 0);
        let (_, _) = peer_hears(&mut b, &attempt.packet, 500);
        let got = b.peers_station.messages.get(&hash).unwrap();
        assert_eq!(got.message.content, "Hi 2");
    }

    /// Written first, from the Peers screen: OK on the peer as the screen
    /// lists it opens a thread with nothing in it, the keyboard types "Hi",
    /// and the peer reads exactly that -- and from then on the peer has a
    /// conversation, so it is on the Messages screen and kept in the table.
    #[test]
    fn a_peer_written_to_first_from_the_peers_screen_reads_it() {
        use crate::screens::{self, State};
        use crate::sh1107::Frame;
        use crate::ui::{self, Input, NavExt, Screen};
        use Input::{Down, Right, Select, Up};
        let mut b = bench();
        assert!(!b.station.peers.get(&to()).unwrap().conversation);
        assert_eq!(b.station.conversations(1_000).rows[0], None);
        let state = State {
            peers: b.station.peers.screen(1_000),
            ..State::default()
        };
        let mut nav = ui::nav();
        while nav.current() != Screen::Peers {
            nav.handle(Right);
        }
        screens::render(&mut Frame::new(), &mut nav, &state);
        assert_eq!(nav.handle(Select), Some(ui::Action::Open));
        // The modem loop's answer: the peer on the line as last drawn.
        let peer = nav.selected().and_then(|n| state.peers.nth(n)).unwrap();
        assert_eq!(peer.destination, to());
        nav.open_conversation(peer.destination);
        let entries = b.station.entries(&to(), 1_000);
        assert!(entries.is_empty(), "nothing said yet");
        nav.handle(Select);
        nav.handle(Down);
        assert_eq!(nav.handle(Select), Some(ui::Action::Compose));
        nav.compose();
        // From h, Shift on the bottom row; up and round to h; i; Send.
        let presses = [
            Down,
            Input::Left,
            Input::Left,
            Input::Left,
            Input::Left,
            Input::Left,
            Input::Left,
            Select, // Shift
            Up,
            Right,
            Right,
            Right,
            Right,
            Right,
            Right,
            Select, // H
            Up,
            Right,
            Right,
            Select, // i
            Right,
            Right,
            Right,
            Select, // Send
        ];
        let mut chosen = Vec::new();
        for input in presses {
            chosen.extend(nav.handle(input));
        }
        assert_eq!(chosen, [ui::Action::SendTyped]);
        let peer = nav.conversation().unwrap().peer();
        let text = nav.typing().unwrap().text();
        assert_eq!(text, "Hi");
        let hash = b
            .station
            .compose(&b.board, &peer, text, 0, 1_000, None)
            .unwrap();
        nav.written();
        assert!(b.station.peers.get(&to()).unwrap().conversation);
        assert_eq!(
            b.station.conversations(1_000).rows[0].map(|r| r.peer),
            Some(to())
        );
        let (_, attempt) = attempt(&mut b, 1_000);
        let (_, _) = peer_hears(&mut b, &attempt.packet, 1_500);
        let got = b.peers_station.messages.get(&hash).unwrap();
        assert_eq!(got.message.content, "Hi");
    }

    /// The keyboard's limit is exactly what fits: a full keyboard's text
    /// is written, and nothing typed can be refused as too long.
    #[test]
    fn a_full_keyboard_fits_one_packet() {
        use crate::ui::{self, Input, Typing};
        let mut typing = Typing::new();
        for _ in 0..ui::MESSAGE_MAX + 5 {
            typing.handle(Input::Select);
        }
        assert_eq!(typing.len(), lxmf::MAX_CONTENT);
        let mut b = bench();
        assert!(b
            .station
            .compose(&b.board, &to(), typing.text(), 0, 0, None)
            .is_ok());
    }

    #[test]
    fn the_vector_is_what_the_board_sends() {
        // The board's message to the bench peer, for tools/test_peer.py to
        // read with the peer's code and with LXMF. Deterministic: the
        // signature always is, and the encryption is with a fixed RNG.
        let mut b = bench();
        b.station
            .compose(
                &b.board,
                &to(),
                "Yes \u{2013} from the board",
                1_792_800_000,
                0,
                None,
            )
            .unwrap();
        let (_, attempt) = attempt(&mut b, 0);
        if std::env::var_os("OXINODE_WRITE_VECTORS").is_some() {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/node/vectors");
            std::fs::write(format!("{dir}/board_message.bin"), &attempt.packet).unwrap();
            return;
        }
        assert_eq!(attempt.packet, BOARD_MESSAGE);
    }
}
