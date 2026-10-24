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
//! What it does today is listen: it hears announces and path requests and
//! reports them. It has an identity of its own ([`key`]), the one the board
//! keeps for its life, and an `lxmf.delivery` destination derived from it, so
//! a path request for the board is recognised as one. It announces nothing
//! and answers nothing yet; that arrives with the announcing that builds on
//! this.
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

pub mod key;
use key::{Hashes, NodeKey};

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
    /// Actions the node had no use for, counted so a log can say so.
    pub ignored: u32,
}

/// The engine and the one interface it has. See the module docs.
pub struct Node {
    engine: TransportEngine,
    /// `rnstransport.path.request`, where every path request is addressed.
    path_request: [u8; 16],
    hashes: Hashes,
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
        }
    }

    /// What the board is called on the network.
    pub fn hashes(&self) -> Hashes {
        self.hashes
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
        // destination, and goes back into the engine as a path request.
        let mut answers = Vec::new();
        actions.retain(|action| match action {
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
            _ => true,
        });
        actions.append(&mut answers);
        route(actions)
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

/// How often [`Node::tick`] should be called, in milliseconds.
pub const TICK_MS: u64 = 250;

fn seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
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
