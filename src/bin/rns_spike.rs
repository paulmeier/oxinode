//! Phase 15 spike: `rns-core` running on the board.
//!
//! This is neither a product image nor a bring-up image. It exists to answer
//! the measured questions in `docs/phase-15-spike.md`: whether a `no_std`
//! Reticulum fits in the flash and RAM the `rnode` image leaves, whether it
//! talks to a real Reticulum, and what the cryptography costs on this MCU.
//! Nothing here is meant to survive the spike; the numbers are.
//!
//! # What it does
//!
//! It generates an identity, times every primitive an announce and an
//! encrypted packet need, and then runs a Reticulum node with two
//! interfaces:
//!
//! * **the air**, through the same `Modem` the RNode image uses, on the
//!   configuration the board was provisioned with (or the default if it was
//!   not). Packets go out as raw bytes, which is what `rnode` puts on the air
//!   too, so another oxinode with `rnsd` behind it sees them;
//! * **the first USB port**, as a Reticulum `SerialInterface`: HDLC framing,
//!   the same as `rnsd` speaks to a serial line. This is the interop path
//!   that needs only the one board on the bench.
//!
//! The second USB port carries the defmt log, as in every other image.
//!
//! On both interfaces it announces two destinations, each every twenty
//! seconds and ten seconds apart: `oxinode.spike`, and `lxmf.delivery` for
//! its identity. A packet arriving
//! at the first is decrypted and echoed back to whoever last announced the
//! same name. A packet arriving at the second is parsed as an opportunistic
//! LXMF message -- the format is forty lines and lives below, because rns-rs
//! has no LXMF -- its signature checked against the sender's announce, and
//! answered with one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::MaybeUninit;

use embassy_executor::Spawner;
use embassy_futures::join::{join, join3};
use embassy_futures::select::{select3, Either3};
use embassy_nrf::rng::Rng as NrfRng;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, spim};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::pipe::{Pipe, Reader, Writer};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use embedded_alloc::LlffHeap as Heap;
use oxinode::board::{self, Led};
use oxinode::modem::{CsmaReport, Modem, RxReport, TxOutcome, TxReport};
use oxinode::store::Storage;
use oxinode::{boot, bringup, radio, usb_log};
use oxinode_core::lr1121::config::{self as radio_config, RadioConfig, ValidConfig, MAX_PAYLOAD};
use oxinode_core::lr1121::csma::Backoff;
use oxinode_core::rnode::air::{self, Reassembler, Sequence, Split};
use rns_core::announce::AnnounceData;
use rns_core::constants;
use rns_core::destination;
use rns_core::msgpack::{self, Value};
use rns_core::packet::{PacketFlags, RawPacket};
use rns_core::transport::types::{
    InterfaceId, InterfaceInfo, PacketHashlistAllocation, TransportAction, TransportConfig,
};
use rns_core::transport::{InboundFrame, RxMetadata, TransportEngine};
use rns_crypto::identity::Identity;
use rns_crypto::Rng;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
});

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0006;

// ---------------------------------------------------------------- the heap

/// `rns-core` is `no_std` + `alloc`: its tables are `BTreeMap`s and its
/// packets are `Vec`s. This is the first allocator in any oxinode image, and
/// its size is one of the numbers the spike is for. Static, so it shows up
/// in `tools/check_layout.py`'s RAM figure rather than hiding in the gap the
/// stack grows into.
const HEAP_SIZE: usize = 64 * 1024;

#[global_allocator]
static HEAP: Heap = Heap::empty();

// ---------------------------------------------------------------- the log

/// `rns-core` logs through `log`. defmt cannot take a `fmt::Arguments`, so
/// the line is formatted on the heap and shipped as one string.
struct DefmtLog;

impl log::Log for DefmtLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = alloc::format!("{}", record.args());
        match record.level() {
            log::Level::Error => defmt::error!("rns: {=str}", line.as_str()),
            log::Level::Warn => defmt::warn!("rns: {=str}", line.as_str()),
            log::Level::Info => defmt::info!("rns: {=str}", line.as_str()),
            _ => defmt::debug!("rns: {=str}", line.as_str()),
        }
    }

    fn flush(&self) {}
}

static LOGGER: DefmtLog = DefmtLog;

// ---------------------------------------------------------------- randomness

/// The nRF52840's RNG peripheral, in the shape `rns-crypto` asks for.
struct HwRng<'d>(NrfRng<'d, embassy_nrf::mode::Blocking>);

impl Rng for HwRng<'_> {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.blocking_fill_bytes(dest);
    }
}

// ---------------------------------------------------------------- time

/// Seconds since boot. The board has no wall clock; `rns-core` only compares
/// its times with each other, and the LXMF timestamps below borrow the
/// host's.
fn now() -> f64 {
    Instant::now().as_micros() as f64 / 1_000_000.0
}

fn micros_since(t: Instant) -> u32 {
    t.elapsed().as_micros() as u32
}

// ---------------------------------------------------------------- HDLC

/// The framing `rnsd`'s `SerialInterface` speaks: `0x7E` around a frame,
/// `0x7D` escaping either byte by XOR `0x20`. Decoder state is a byte at a
/// time so the reader can hand over whatever USB delivered.
const FLAG: u8 = 0x7E;
const ESC: u8 = 0x7D;
const ESC_MASK: u8 = 0x20;

struct Hdlc {
    frame: Vec<u8>,
    in_frame: bool,
    escaped: bool,
}

impl Hdlc {
    fn new() -> Self {
        Self {
            frame: Vec::with_capacity(constants::MTU),
            in_frame: false,
            escaped: false,
        }
    }

    /// Feed one byte; a completed frame comes back whole.
    fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        if byte == FLAG {
            let done = if self.in_frame && !self.frame.is_empty() {
                Some(core::mem::take(&mut self.frame))
            } else {
                None
            };
            self.in_frame = true;
            self.escaped = false;
            self.frame.clear();
            return done;
        }
        if !self.in_frame {
            return None;
        }
        if self.escaped {
            self.frame.push(byte ^ ESC_MASK);
            self.escaped = false;
        } else if byte == ESC {
            self.escaped = true;
        } else {
            self.frame.push(byte);
        }
        if self.frame.len() > constants::MTU + 32 {
            // Framing lost; start over at the next flag.
            self.frame.clear();
            self.in_frame = false;
        }
        None
    }

    fn frame(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 2);
        out.push(FLAG);
        for &b in data {
            if b == FLAG || b == ESC {
                out.push(ESC);
                out.push(b ^ ESC_MASK);
            } else {
                out.push(b);
            }
        }
        out.push(FLAG);
        out
    }
}

// ---------------------------------------------------------------- LXMF

/// The part of LXMF an opportunistic message needs: one packet, no link, no
/// propagation node, no stamp. `rns-rs` does not have it, so it is here.
///
/// A packed message is:
///
/// ```text
/// destination hash (16) | source hash (16) | signature (64) | msgpack payload
/// payload = [timestamp, title, content, fields]
/// ```
///
/// The message hash is SHA-256 over destination, source and payload; the
/// signature is over that same run of bytes followed by the hash, made with
/// the source identity's Ed25519 key. **Opportunistic delivery drops the
/// leading destination hash from the wire** -- the packet is addressed to
/// it already -- and the receiver puts its own back before checking
/// anything. That is the detail the first run of this image got wrong.
struct Lxmf {
    source: [u8; 16],
    timestamp: f64,
    title: Vec<u8>,
    content: Vec<u8>,
}

impl Lxmf {
    /// What precedes the payload on the wire: source hash and signature.
    const WIRE_HEADER: usize = 16 + 64;

    /// Parse a decrypted opportunistic message addressed to `own_dest`, and
    /// check its signature against the identity that announced the source.
    fn unpack(
        wire: &[u8],
        own_dest: &[u8; 16],
        source_identity: Option<&Identity>,
    ) -> Result<(Self, bool), &'static str> {
        if wire.len() < Self::WIRE_HEADER {
            return Err("short");
        }
        let (src, rest) = wire.split_at(16);
        let (sig, packed) = rest.split_at(64);
        let (value, _) = msgpack::unpack(packed).map_err(|_| "msgpack")?;
        let items = value.as_array().ok_or("not a list")?;
        if items.len() < 4 {
            return Err("short list");
        }
        let timestamp = items[0].as_number().ok_or("timestamp")?;
        let title = items[1].as_bin().ok_or("title")?.to_vec();
        let content = items[2].as_bin().ok_or("content")?.to_vec();

        // A message carrying a stamp is signed over the first four fields
        // only, re-packed without it.
        let packed_for_hash = if items.len() > 4 {
            msgpack::pack(&Value::Array(items[..4].to_vec()))
        } else {
            packed.to_vec()
        };
        let mut hashed = Vec::with_capacity(32 + packed_for_hash.len());
        hashed.extend_from_slice(own_dest);
        hashed.extend_from_slice(src);
        hashed.extend_from_slice(&packed_for_hash);
        let hash = rns_core::hash::full_hash(&hashed);
        let mut signed = hashed;
        signed.extend_from_slice(&hash);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        let verified = source_identity
            .map(|id| id.verify(&signature, &signed))
            .unwrap_or(false);

        let mut source = [0u8; 16];
        source.copy_from_slice(src);
        Ok((
            Self {
                source,
                timestamp,
                title,
                content,
            },
            verified,
        ))
    }

    /// Build and sign a message from `source` (this node's delivery
    /// destination) to `dest`, in its opportunistic wire form.
    fn pack(
        identity: &Identity,
        source: &[u8; 16],
        dest: &[u8; 16],
        timestamp: f64,
        title: &[u8],
        content: &[u8],
    ) -> Option<Vec<u8>> {
        let payload = Value::Array(alloc::vec![
            Value::Float(timestamp),
            Value::Bin(title.to_vec()),
            Value::Bin(content.to_vec()),
            Value::Map(Vec::new()),
        ]);
        let packed = msgpack::pack(&payload);
        let mut hashed = Vec::with_capacity(32 + packed.len());
        hashed.extend_from_slice(dest);
        hashed.extend_from_slice(source);
        hashed.extend_from_slice(&packed);
        let hash = rns_core::hash::full_hash(&hashed);
        let mut signed = hashed.clone();
        signed.extend_from_slice(&hash);
        let signature = identity.sign(&signed).ok()?;
        // The wire form: no destination hash.
        let mut out = Vec::with_capacity(Self::WIRE_HEADER + packed.len());
        out.extend_from_slice(source);
        out.extend_from_slice(&signature);
        out.extend_from_slice(&packed);
        Some(out)
    }
}

// ---------------------------------------------------------------- the node

const LORA: InterfaceId = InterfaceId(1);
const SERIAL: InterfaceId = InterfaceId(2);

const APP_NAME: &str = "oxinode";
const ASPECT: &str = "spike";

/// What an announce told us about somebody, kept so we can answer them.
struct Peer {
    identity: Identity,
    ratchet: Option<[u8; 32]>,
}

/// Everything about the Reticulum node that is not hardware.
struct Node<'d> {
    engine: TransportEngine,
    identity: Identity,
    rng: HwRng<'d>,
    /// `oxinode.spike` for this identity.
    spike_dest: [u8; 16],
    /// `lxmf.delivery` for this identity.
    lxmf_dest: [u8; 16],
    spike_name: [u8; 10],
    lxmf_name: [u8; 10],
    /// Announced destinations, by destination hash.
    peers: BTreeMap<[u8; 16], Peer>,
    /// The last `oxinode.spike` and `lxmf.delivery` that announced, which
    /// is who a reply goes to.
    last_spike_peer: Option<[u8; 16]>,
    last_lxmf_peer: Option<[u8; 16]>,
    /// Host wall-clock time minus board uptime, learned from the first LXMF
    /// message to arrive. The board has no clock of its own.
    wall_offset: Option<f64>,
    /// Bytes waiting to go to the host over USB.
    serial_out: Vec<u8>,
    /// Frames waiting to go on the air.
    air_out: Vec<Vec<u8>>,
    announces_seen: u32,
    messages_seen: u32,
}

impl<'d> Node<'d> {
    fn new(identity: Identity, rng: HwRng<'d>) -> Self {
        let identity_hash = *identity.hash();
        let config = TransportConfig {
            // An endpoint, not a transport node: it does not forward.
            transport_enabled: false,
            identity_hash: Some(identity_hash),
            local_hops_delta: 0,
            prefer_shorter_path: false,
            max_paths_per_destination: 1,
            // Sized for a bench, not the internet. Every one of these is a
            // table on the heap.
            packet_hashlist_max_entries: 256,
            packet_hashlist_allocation: PacketHashlistAllocation::Eager,
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
            announce_queue_max_interfaces: 2,
        };
        let mut engine = TransportEngine::new(config);

        let spike_name = destination::name_hash(APP_NAME, &[ASPECT]);
        let lxmf_name = destination::name_hash("lxmf", &["delivery"]);
        let spike_dest = destination::destination_hash(APP_NAME, &[ASPECT], Some(&identity_hash));
        let lxmf_dest = destination::destination_hash("lxmf", &["delivery"], Some(&identity_hash));
        engine.register_destination(spike_dest, constants::DESTINATION_SINGLE);
        engine.register_destination(lxmf_dest, constants::DESTINATION_SINGLE);

        Self {
            engine,
            identity,
            rng,
            spike_dest,
            lxmf_dest,
            spike_name,
            lxmf_name,
            peers: BTreeMap::new(),
            last_spike_peer: None,
            last_lxmf_peer: None,
            wall_offset: None,
            serial_out: Vec::new(),
            air_out: Vec::new(),
            announces_seen: 0,
            messages_seen: 0,
        }
    }

    fn add_interface(&mut self, id: InterfaceId, name: &str, bitrate: Option<u64>) {
        self.engine.register_interface(InterfaceInfo {
            id,
            name: String::from(name),
            mode: constants::MODE_FULL,
            gravity: 0,
            recursive_prs: false,
            announces_from_internal: false,
            announces_to_internal: None,
            out_capable: true,
            in_capable: true,
            bitrate,
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
            started: now(),
        });
    }

    /// A frame arrived on an interface.
    fn inbound(&mut self, iface: InterfaceId, raw: &[u8], rx: RxMetadata) {
        let frame = InboundFrame::new(raw, iface, now()).with_rx(rx);
        let actions = self.engine.handle_inbound(frame, &mut self.rng);
        self.dispatch(actions);
    }

    /// Periodic maintenance.
    fn tick(&mut self) {
        let actions = self.engine.tick(now(), &mut self.rng);
        self.dispatch(actions);
    }

    /// Announce one of the two destinations on every interface.
    ///
    /// One at a time, and the loop spaces them out. The first over-the-air
    /// run sent both back to back: the host answered the first within a
    /// hundred milliseconds -- while this board was still transmitting the
    /// second -- so the host's packet and the second announce were lost to
    /// each other every time. Neither `rnode` nor this image listens before
    /// transmitting, which is a finding of its own.
    fn announce(&mut self, which: usize) {
        let spike = (self.spike_dest, self.spike_name, None);
        // LXMF peers read the announce's app data as a display name.
        let lxmf = (self.lxmf_dest, self.lxmf_name, Some(&b"oxinode spike"[..]));
        let (dest, name, app_data) = if which % 2 == 0 { spike } else { lxmf };
        {
            let started = Instant::now();
            // Five random bytes and five of time, as the reference does. The
            // time is uptime, which is the only time there is.
            let mut random_hash = [0u8; 10];
            self.rng.fill_bytes(&mut random_hash[..5]);
            let secs = Instant::now().as_secs();
            random_hash[5..10].copy_from_slice(&secs.to_be_bytes()[3..8]);

            let data = match AnnounceData::pack(
                &self.identity,
                &dest,
                &name,
                &random_hash,
                None,
                app_data,
            ) {
                Ok((data, _)) => data,
                Err(_) => {
                    defmt::error!("announce: could not pack");
                    return;
                }
            };
            let flags = PacketFlags {
                header_type: constants::HEADER_1,
                context_flag: constants::FLAG_UNSET,
                transport_type: constants::TRANSPORT_BROADCAST,
                destination_type: constants::DESTINATION_SINGLE,
                packet_type: constants::PACKET_TYPE_ANNOUNCE,
            };
            let packet =
                match RawPacket::pack(flags, 0, &dest, None, constants::CONTEXT_NONE, &data) {
                    Ok(p) => p,
                    Err(_) => {
                        defmt::error!("announce: could not pack the packet");
                        return;
                    }
                };
            let actions =
                self.engine
                    .handle_outbound(&packet, constants::DESTINATION_SINGLE, None, now());
            defmt::info!(
                "announce: {=[u8]:x} {=usize} bytes, built in {=u32} us",
                &dest[..4],
                packet.raw.len(),
                micros_since(started)
            );
            self.dispatch(actions);
        }
    }

    fn dispatch(&mut self, actions: Vec<TransportAction>) {
        for action in actions {
            match action {
                TransportAction::SendOnInterface { interface, raw } => self.send(interface, &raw),
                TransportAction::BroadcastOnAllInterfaces { raw, exclude } => {
                    for id in [LORA, SERIAL] {
                        if Some(id) != exclude {
                            self.send(id, &raw);
                        }
                    }
                }
                TransportAction::AnnounceReceived {
                    destination_hash,
                    public_key,
                    name_hash,
                    ratchet,
                    hops,
                    receiving_interface,
                    ..
                } => {
                    self.announces_seen += 1;
                    let which = if name_hash == self.spike_name {
                        self.last_spike_peer = Some(destination_hash);
                        "oxinode.spike"
                    } else if name_hash == self.lxmf_name {
                        self.last_lxmf_peer = Some(destination_hash);
                        "lxmf.delivery"
                    } else {
                        "other"
                    };
                    defmt::info!(
                        "announce heard: {=[u8]:x} {=str} hops={=u8} via {=u32}",
                        &destination_hash[..4],
                        which,
                        hops,
                        receiving_interface.0 as u32
                    );
                    self.peers.insert(
                        destination_hash,
                        Peer {
                            identity: Identity::from_public_key(&public_key),
                            ratchet,
                        },
                    );
                }
                TransportAction::PathUpdated {
                    destination_hash,
                    hops,
                    interface,
                    ..
                } => {
                    defmt::debug!(
                        "path: {=[u8]:x} hops={=u8} via {=u32}",
                        &destination_hash[..4],
                        hops,
                        interface.0 as u32
                    );
                }
                TransportAction::DeliverLocal {
                    destination_hash,
                    raw,
                    ..
                } => self.deliver(destination_hash, &raw),
                _ => {}
            }
        }
    }

    /// A packet for one of our destinations.
    fn deliver(&mut self, dest: [u8; 16], raw: &[u8]) {
        let packet = match RawPacket::unpack(raw) {
            Ok(p) => p,
            Err(_) => {
                defmt::warn!("deliver: unpack failed");
                return;
            }
        };
        if packet.flags.packet_type != constants::PACKET_TYPE_DATA {
            defmt::debug!(
                "deliver: ignoring packet type {=u8}",
                packet.flags.packet_type
            );
            return;
        }
        let started = Instant::now();
        let plain = match self.identity.decrypt(&packet.data) {
            Ok(p) => p,
            Err(_) => {
                defmt::warn!(
                    "deliver: decrypt failed ({=usize} bytes)",
                    packet.data.len()
                );
                return;
            }
        };
        let decrypt_us = micros_since(started);
        self.messages_seen += 1;

        if dest == self.spike_dest {
            let text = core::str::from_utf8(&plain).unwrap_or("<binary>");
            defmt::info!(
                "spike message: {=str} ({=usize} bytes, decrypted in {=u32} us)",
                text,
                plain.len(),
                decrypt_us
            );
            let mut reply = Vec::from(&b"echo: "[..]);
            reply.extend_from_slice(&plain);
            if let Some(peer) = self.last_spike_peer {
                self.send_to(peer, &reply);
            } else {
                defmt::warn!("spike message: nobody has announced oxinode.spike; no reply");
            }
        } else if dest == self.lxmf_dest {
            let src_identity = plain
                .get(..16)
                .and_then(|s| self.peers.get(s))
                .map(|p| &p.identity);
            let own_dest = self.lxmf_dest;
            match Lxmf::unpack(&plain, &own_dest, src_identity) {
                Ok((message, verified)) => {
                    let title = core::str::from_utf8(&message.title).unwrap_or("<binary>");
                    let content = core::str::from_utf8(&message.content).unwrap_or("<binary>");
                    defmt::info!(
                        "lxmf message from {=[u8]:x}: title={=str} content={=str} signature {=str} (decrypted in {=u32} us)",
                        &message.source[..4],
                        title,
                        content,
                        if verified { "ok" } else { "UNVERIFIED" },
                        decrypt_us
                    );
                    if self.wall_offset.is_none() {
                        self.wall_offset = Some(message.timestamp - now());
                    }
                    let mut reply = Vec::from(&b"the board says: "[..]);
                    reply.extend_from_slice(&message.content);
                    self.send_lxmf(message.source, b"", &reply);
                }
                Err(why) => defmt::warn!("lxmf message: malformed ({=str})", why),
            }
        }
    }

    /// Encrypt `plain` for a destination somebody announced, and send it.
    fn send_to(&mut self, dest: [u8; 16], plain: &[u8]) {
        let Some(peer) = self.peers.get(&dest) else {
            defmt::warn!("send: no announce for {=[u8]:x}", &dest[..4]);
            return;
        };
        let started = Instant::now();
        let ciphertext =
            match peer
                .identity
                .encrypt_with_ratchet(plain, peer.ratchet.as_ref(), &mut self.rng)
            {
                Ok(c) => c,
                Err(_) => {
                    defmt::error!("send: encrypt failed");
                    return;
                }
            };
        let encrypt_us = micros_since(started);
        let flags = PacketFlags {
            header_type: constants::HEADER_1,
            context_flag: constants::FLAG_UNSET,
            transport_type: constants::TRANSPORT_BROADCAST,
            destination_type: constants::DESTINATION_SINGLE,
            packet_type: constants::PACKET_TYPE_DATA,
        };
        let packet =
            match RawPacket::pack(flags, 0, &dest, None, constants::CONTEXT_NONE, &ciphertext) {
                Ok(p) => p,
                Err(_) => {
                    defmt::error!("send: packet too large ({=usize} bytes)", ciphertext.len());
                    return;
                }
            };
        defmt::info!(
            "send: {=usize} bytes to {=[u8]:x} (encrypted in {=u32} us)",
            packet.raw.len(),
            &dest[..4],
            encrypt_us
        );
        let actions =
            self.engine
                .handle_outbound(&packet, constants::DESTINATION_SINGLE, None, now());
        self.dispatch(actions);
    }

    fn send_lxmf(&mut self, dest: [u8; 16], title: &[u8], content: &[u8]) {
        let timestamp = self.wall_offset.map(|o| o + now()).unwrap_or_else(now);
        let started = Instant::now();
        let Some(packed) = Lxmf::pack(
            &self.identity,
            &self.lxmf_dest,
            &dest,
            timestamp,
            title,
            content,
        ) else {
            defmt::error!("lxmf: could not sign");
            return;
        };
        defmt::info!(
            "lxmf: {=usize} bytes packed and signed in {=u32} us",
            packed.len(),
            micros_since(started)
        );
        self.send_to(dest, &packed);
    }

    /// Queue a frame for an interface. The hardware is drained by the loop.
    fn send(&mut self, iface: InterfaceId, raw: &[u8]) {
        match iface {
            SERIAL => self.serial_out.extend_from_slice(&Hdlc::frame(raw)),
            LORA => {
                if raw.len() > air::PACKET_MAX {
                    // The air carries a packet as one LoRa frame or two, the
                    // way `rnode` and a stock RNode do; more does not go.
                    defmt::warn!(
                        "air: {=usize} bytes is more than two LoRa frames; dropped",
                        raw.len()
                    );
                } else {
                    self.air_out.push(raw.to_vec());
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- crypto timing

/// Time every primitive an announce and an encrypted packet need.
fn bench(rng: &mut HwRng<'_>) -> Identity {
    let best = |name: &str, f: &mut dyn FnMut()| {
        let mut min = u32::MAX;
        let mut total = 0u32;
        const RUNS: u32 = 5;
        for _ in 0..RUNS {
            let t = Instant::now();
            f();
            let us = micros_since(t);
            min = min.min(us);
            total += us;
        }
        defmt::info!(
            "crypto: {=str}: min {=u32} us, mean {=u32} us over {=u32}",
            name,
            min,
            total / RUNS,
            RUNS
        );
    };

    let t = Instant::now();
    let identity = Identity::new(rng);
    defmt::info!(
        "crypto: identity (X25519 + Ed25519 keygen): {=u32} us",
        micros_since(t)
    );

    let message = [0x5Au8; 200];
    let mut signature = [0u8; 64];
    best("ed25519 sign 200 B", &mut || {
        signature = identity.sign(&message).unwrap_or([0; 64]);
    });
    let public = Identity::from_public_key(&identity.get_public_key().unwrap_or([0; 64]));
    let mut ok = false;
    best("ed25519 verify 200 B", &mut || {
        ok = public.verify(&signature, &message);
    });
    defmt::info!("crypto: verify result {=bool}", ok);

    let plain = [0x33u8; 100];
    let mut token = Vec::new();
    best(
        "encrypt 100 B (X25519 + HKDF + AES-CBC + HMAC)",
        &mut || {
            token = public.encrypt(&plain, rng).unwrap_or_default();
        },
    );
    let mut back = Vec::new();
    best("decrypt 100 B", &mut || {
        back = identity.decrypt(&token).unwrap_or_default();
    });
    defmt::info!("crypto: decrypt round trip {=bool}", back == plain);

    let big = [0x11u8; 500];
    best("sha256 500 B", &mut || {
        let _ = rns_core::hash::full_hash(&big);
    });

    let dest = destination::destination_hash(APP_NAME, &[ASPECT], Some(identity.hash()));
    let name = destination::name_hash(APP_NAME, &[ASPECT]);
    let random = [7u8; 10];
    let mut announce = Vec::new();
    best("announce pack (sign)", &mut || {
        announce = AnnounceData::pack(&identity, &dest, &name, &random, None, None)
            .map(|(d, _)| d)
            .unwrap_or_default();
    });
    best("announce validate (verify)", &mut || {
        let _ = AnnounceData::unpack(&announce, false).and_then(|a| a.validate(&dest));
    });

    identity
}

// ---------------------------------------------------------------- USB plumbing

/// Read the first port forever into a pipe, so that the modem loop's
/// `select` never cancels a `read_packet` mid-transfer. See `rnode.rs`.
async fn read_host<'d, D: UsbDriverTrait<'d>, const N: usize>(
    rx: &mut Receiver<'d, D>,
    pipe: &Writer<'_, NoopRawMutex, N>,
) -> ! {
    let mut buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    loop {
        rx.wait_connection().await;
        while let Ok(n) = rx.read_packet(&mut buf).await {
            let mut rest = &buf[..n];
            while !rest.is_empty() {
                let written = pipe.write(rest).await;
                rest = &rest[written..];
            }
        }
    }
}

/// Write `data` to the host in 64-byte packets, with the zero-length packet
/// a full final one needs. Bounded, because a host that closed the port
/// never asks for the rest.
async fn write_host<'d, D: UsbDriverTrait<'d>>(tx: &mut Sender<'d, D>, data: &[u8]) {
    let max = usb_log::MAX_PACKET_SIZE as usize;
    for chunk in data.chunks(max) {
        if with_timeout(Duration::from_millis(500), tx.write_packet(chunk))
            .await
            .is_err()
        {
            return;
        }
    }
    if !data.is_empty() && data.len() % max == 0 {
        let _ = with_timeout(Duration::from_millis(500), tx.write_packet(&[])).await;
    }
}

// ---------------------------------------------------------------- the loop

/// Between announces. Both destinations go out together, back to back, as
/// the first phase 15 run sent them -- which is exactly what collided with
/// the host's reply before the modem listened first. Phase 16 put them back
/// together on purpose, to reproduce that.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_secs(1);

/// A frame off the air: through the reassembler, and if it completes a
/// packet, to the node with its signal -- the mean of two frames' for a
/// packet that came as two.
fn heard(node: &mut Node<'_>, air_in: &mut Reassembler, report: &RxReport, rx_buf: &[u8]) {
    let signal = air::Signal {
        rssi_dbm: report.rssi_dbm,
        snr_quarter_db: report.snr_quarter_db,
    };
    match air_in.feed(&rx_buf[..report.len], signal, Instant::now().as_micros()) {
        Some(packet) => {
            let rx = RxMetadata {
                rssi: Some(packet.signal.rssi_dbm),
                snr: Some(packet.signal.snr_quarter_db as f32 / 4.0),
            };
            node.inbound(LORA, packet.payload, rx);
        }
        None => defmt::debug!(
            "air: {=usize} bytes, half of a split packet; holding it",
            report.len
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run<'d, D, S, B>(
    mut node: Node<'_>,
    config: ValidConfig,
    dev: &mut lr11xx::Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    tx: &mut Sender<'d, D>,
    host: &mut Reader<'_, NoopRawMutex, 2048>,
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
) -> !
where
    D: UsbDriverTrait<'d>,
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let mut hdlc = Hdlc::new();
    let mut usb_buf = [0u8; 64];
    let mut rx_buf = [0u8; MAX_PAYLOAD as usize];
    // Frames off the air become packets here, and packets become frames on
    // the way out, the same as in `rnode`: one header byte, and a split at
    // 255. See `oxinode_core::rnode::air`.
    let mut air_in = Reassembler::new(air::max_age_us(&config));
    let mut sequence = Sequence::new(Instant::now().as_ticks() as u8);
    let mut last_announce: Option<Instant> = None;
    let mut announced = 0usize;
    let mut ticks = 0u32;
    let mut peak_heap = 0usize;

    let mut receiving = {
        let mut modem = Modem::new(dev, irq);
        match modem.start_rx(&config).await {
            Ok(()) => true,
            Err(e) => {
                defmt::error!("radio: start_rx failed: {}", e);
                false
            }
        }
    };

    loop {
        let event = select3(
            host.read(&mut usb_buf),
            Timer::after(TICK),
            irq.wait_asserted(Duration::from_secs(3600)),
        )
        .await;

        match event {
            Either3::First(n) => {
                for &b in &usb_buf[..n] {
                    if let Some(frame) = hdlc.push(b) {
                        defmt::debug!("serial: frame of {=usize} bytes", frame.len());
                        node.inbound(SERIAL, &frame, RxMetadata::default());
                    }
                }
            }
            Either3::Second(()) => {
                ticks = ticks.wrapping_add(1);
                if usb_log::is_bootloader_touch_tx(tx, control) {
                    boot::reboot_to_bootloader();
                }
                node.tick();
                let due = last_announce.is_none_or(|t| t.elapsed() >= ANNOUNCE_EVERY);
                if due {
                    node.announce(announced);
                    node.announce(announced + 1);
                    announced += 2;
                    last_announce = Some(Instant::now());
                }
                peak_heap = peak_heap.max(HEAP.used());
                if ticks % 10 == 0 {
                    defmt::info!(
                        "heap: {=usize} used now, {=usize} peak, {=usize} free of {=usize}; stack gap {=u32}; paths {=usize}; announces heard {=u32}; messages {=u32}",
                        HEAP.used(),
                        peak_heap,
                        HEAP.free(),
                        HEAP_SIZE,
                        board::free_ram_bytes().unwrap_or(0),
                        node.engine.path_table_count(),
                        node.announces_seen,
                        node.messages_seen
                    );
                }
            }
            Either3::Third(Ok(())) => {
                let mut modem = Modem::new(dev, irq);
                if !receiving {
                    // Level-triggered and nobody listening: clear it or spin.
                    // See the same branch in `rnode.rs`.
                    let _ = modem.clear_interrupts().await;
                } else {
                    match modem.receive(&mut rx_buf, Duration::from_millis(20)).await {
                        Ok(Some(report)) => {
                            led.toggle();
                            defmt::debug!(
                                "air: {=usize} bytes, rssi {=i16} dBm, snr {=i16} dB",
                                report.len,
                                report.rssi_dbm,
                                report.snr_db
                            );
                            heard(&mut node, &mut air_in, &report, &rx_buf);
                        }
                        Ok(None) => {}
                        Err(e) => defmt::error!("air: receive failed: {}", e),
                    }
                }
            }
            Either3::Third(Err(_)) => {}
        }

        // Drain what the node queued: the host first, since it is cheap.
        if !node.serial_out.is_empty() {
            let out = core::mem::take(&mut node.serial_out);
            if control.dtr() {
                write_host(tx, &out).await;
            }
        }
        while let Some(packet) = node.air_out.first().cloned() {
            node.air_out.remove(0);
            // `send` refused anything longer than two frames, so this holds.
            let Some(split) = Split::new(&packet, sequence.take()) else {
                continue;
            };
            let mut frame = [0u8; air::FRAME_MAX];
            let Some(n) = split.frame(0, &mut frame) else {
                continue;
            };
            let mut backoff = Backoff::new(&config, n as u8, Instant::now().as_ticks());
            let sent = loop {
                let mut modem = Modem::new(dev, irq);
                match modem
                    .transmit(&config, &frame[..n], &mut backoff, &mut rx_buf)
                    .await
                {
                    Ok(TxOutcome::Sent(report)) => break Ok(report),
                    // Heard while waiting: the node gets it now, and what it
                    // queues in answer goes out after this frame.
                    Ok(TxOutcome::Heard(report)) => {
                        led.toggle();
                        defmt::debug!(
                            "air: {=usize} bytes while waiting to send, rssi {=i16} dBm, snr {=i16} dB",
                            report.len,
                            report.rssi_dbm,
                            report.snr_db
                        );
                        heard(&mut node, &mut air_in, &report, &rx_buf);
                    }
                    Err(e) => break Err(e),
                }
            };
            // The second frame goes straight after the first, with no wait:
            // the receiver is holding the first half for it.
            let sent = match sent {
                Ok(report) if split.frames() == 2 => {
                    let Some(n) = split.frame(1, &mut frame) else {
                        continue;
                    };
                    let mut modem = Modem::new(dev, irq);
                    match modem
                        .send(&config, &frame[..n], CsmaReport::default())
                        .await
                    {
                        Ok(second) => Ok(TxReport {
                            elapsed_us: report.elapsed_us + second.elapsed_us,
                            airtime_us: report.airtime_us + second.airtime_us,
                            ..report
                        }),
                        Err(e) => Err(e),
                    }
                }
                other => other,
            };
            match sent {
                Ok(report) => defmt::info!(
                    "air: sent {=usize} bytes in {=usize} frames, {=u32} us (airtime {=u32} us) after {=u32} senses ({=u32} busy, {=u32} us waiting{=str})",
                    packet.len(),
                    split.frames(),
                    report.elapsed_us,
                    report.airtime_us,
                    report.csma.senses,
                    report.csma.busy,
                    report.csma.waited_us,
                    if report.csma.forced { ", forced" } else { "" }
                ),
                Err(e) => defmt::error!("air: transmit failed: {}", e),
            }
            let mut modem = Modem::new(dev, irq);
            receiving = match modem.start_rx(&config).await {
                Ok(()) => true,
                Err(e) => {
                    defmt::error!("radio: start_rx failed: {}", e);
                    false
                }
            };
        }
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    {
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        // SAFETY: called once, before anything allocates, on memory nothing
        // else names.
        unsafe { HEAP.init(&raw mut HEAP_MEM as usize, HEAP_SIZE) }
    }
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Debug);

    let spi = radio::new_spi(p.SPI2, Irqs, p.P1_13, p.P1_15, p.P1_14, p.P1_12);
    let reset = radio::RadioReset::new(p.P1_10, p.P1_11);
    let mut irq = radio::RadioIrq::new(p.P1_08);

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode rns spike");
    config.serial_number = Some(serial);
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    static CONFIG_DESC: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static DATA_STATE: StaticCell<State> = StaticCell::new();
    static LOG_STATE: StaticCell<State> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 512]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );

    // The Reticulum serial interface first, so it has DTR and the lower
    // number; the log second, as in `rnode`.
    let data = CdcAcmClass::new(
        &mut builder,
        DATA_STATE.init(State::new()),
        usb_log::MAX_PACKET_SIZE,
    );
    let (mut data_tx, mut data_rx, control) = data.split_with_control();
    let logs = CdcAcmClass::new(
        &mut builder,
        LOG_STATE.init(State::new()),
        usb_log::MAX_PACKET_SIZE,
    );
    let (mut log_tx, _, log_control) = logs.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    static HOST_RX: StaticCell<Pipe<NoopRawMutex, 2048>> = StaticCell::new();
    let (mut host_reader, host_writer) = HOST_RX.init(Pipe::new()).split();

    // The radio configuration the board was provisioned with, if it was.
    let mut storage = Storage::new(p.NVMC);
    let device = storage.load();
    let radio_config = match device.rom.stored_config() {
        Some(stored) => {
            defmt::info!(
                "radio: stored configuration {=u32} Hz, {=u32} Hz bw, sf {=u8}, cr 4/{=u8}, {=i8} dBm",
                stored.frequency_hz,
                stored.bandwidth_hz,
                stored.spreading_factor,
                stored.coding_rate,
                stored.tx_power_dbm
            );
            RadioConfig {
                frequency_hz: stored.frequency_hz,
                bandwidth_hz: stored.bandwidth_hz,
                spreading_factor: stored.spreading_factor,
                coding_rate: stored.coding_rate,
                tx_power_dbm: stored.tx_power_dbm,
                ..radio_config::DEFAULT
            }
        }
        None => {
            defmt::info!("radio: no stored configuration; using the default");
            radio_config::DEFAULT
        }
    };
    let valid = match ValidConfig::new(radio_config) {
        Ok(v) => v,
        Err(e) => {
            defmt::error!(
                "radio: stored configuration refused: {=str}; using the default",
                e.message()
            );
            ValidConfig::new(radio_config::DEFAULT).unwrap_or_else(|_| unreachable!())
        }
    };

    let nrf_rng = NrfRng::new_blocking(p.RNG);
    nrf_rng.set_bias_correction(true);
    let mut rng = HwRng(nrf_rng);

    let run_usb = usb.run();
    let pump = usb_log::pump(&mut log_tx, || log_control.dtr());

    let work = async {
        // The log is the whole output of this image; wait for a reader, as
        // the bring-up images do, but not forever.
        let mut waited = 0u32;
        while !log_control.dtr() && waited < 200 {
            Timer::after(Duration::from_millis(50)).await;
            waited += 1;
        }
        defmt::info!("rns-spike: rns-core on the nRF52840");
        defmt::info!(
            "heap: {=usize} bytes; stack gap at boot {=u32}",
            HEAP_SIZE,
            board::free_ram_bytes().unwrap_or(0)
        );

        let identity = bench(&mut rng);
        defmt::info!("identity: {=[u8]:x}", &identity.hash()[..]);
        defmt::info!("heap: {=usize} used after the crypto timing", HEAP.used());

        let mut node = Node::new(identity, rng);
        node.add_interface(LORA, "LoRa", Some(valid.get().bitrate_bps() as u64));
        node.add_interface(SERIAL, "Serial", None);
        defmt::info!(
            "heap: {=usize} used with the transport engine built; spike {=[u8]:x} lxmf {=[u8]:x}",
            HEAP.used(),
            &node.spike_dest[..],
            &node.lxmf_dest[..]
        );

        let mut dev = match bringup::bring_up(spi, reset, &mut irq).await {
            Ok(dev) => dev,
            Err(e) => {
                defmt::error!("radio: bring-up failed: {}; only the touch is served", e);
                loop {
                    Timer::after(Duration::from_millis(20)).await;
                    if usb_log::is_bootloader_touch_tx(&data_tx, &control) {
                        boot::reboot_to_bootloader();
                    }
                }
            }
        };
        {
            let mut modem = Modem::new(&mut dev, &mut irq);
            match modem.apply(&valid).await {
                Ok(()) => defmt::info!("radio: configured, {=u32} bps", valid.get().bitrate_bps()),
                Err(e) => defmt::error!("radio: configure failed: {}", e),
            }
        }

        join(
            read_host(&mut data_rx, &host_writer),
            run(
                node,
                valid,
                &mut dev,
                &mut irq,
                &mut data_tx,
                &mut host_reader,
                &control,
                &mut led,
            ),
        )
        .await
    };

    join3(run_usb, pump, work).await;
    unreachable!()
}
