//! The Reticulum node, on the chip: its heap, its log, its seed, and the
//! [`Listener`] the modem loop hands every heard packet to.
//!
//! The node itself is [`oxinode_core::node`], where it is tested. This is the
//! part that has to own something: the allocator `rns-core` needs, the
//! `log` sink it writes to, and the 32 bytes of hardware entropy it is
//! seeded with.
//!
//! Without the `node` feature, [`Listener`] is still here and does nothing,
//! so the modem loop is the same code in both builds and the RNode image
//! links no stack. What the loop saw comes as a closure rather than a value
//! for the same reason: reading it is loads the compiler may not drop, and
//! the RNode should not pay for a question nobody asks.
//!
//! # When it runs
//!
//! Only in [`Mode::Node`]: no host has spoken, or the last one has been gone
//! for a few seconds, and a configuration is stored. The rule, and why it
//! is that rule, is [`oxinode_core::mode`]. With a host, the host is the node
//! and the board is its RNode, and nothing here runs, so a host sees the
//! RNode it always saw, timing included. A packet still goes to the host
//! exactly as before; the listener is handed a copy only when the board is a
//! node.
//!
//! The listener says when the mode changes, and the modem loop acts on it:
//! when the last host has gone and a configuration is stored, the radio goes
//! back to that configuration, as it does at boot, because the host may have
//! left it on its own or switched it off on the way out.
//!
//! An announce takes the engine about 45 ms to verify, and the radio comes
//! first: a heard packet is queued ([`oxinode_core::node::Inbox`]) and the
//! engine is handed one on a pass of the modem loop on which the radio had
//! nothing. The modem loop waits for that, which with no host delays nothing
//! but the panel. What the engine verifies as an `lxmf.delivery` announce
//! becomes a peer ([`oxinode_core::node::peers`]), and the Peers screen
//! lists them. A message delivered to the board is kept, and proved so its
//! sender shows it delivered; one from a sender not yet heard is kept
//! unverified while the board asks for the sender's key. What is done with
//! each is [`oxinode_core::node::Station::take`]'s, and is tested there; this
//! logs it. A new message wakes the panel.

use embassy_nrf::peripherals::RNG;
use embassy_nrf::Peri;
use oxinode_core::mode::{Change, Mode, Seen};
use oxinode_core::rnode::air::Signal;
use oxinode_core::screens::{NodeHashes, Peers};

use crate::store::{RecordFlash, Storage};

#[cfg(feature = "node")]
pub use imp::*;

#[cfg(feature = "node")]
mod imp {
    extern crate alloc;

    use core::fmt::Write;
    use core::mem::MaybeUninit;

    use alloc::string::String;
    use alloc::vec::Vec;
    use embassy_nrf::rng::Rng;
    use embassy_time::Instant;
    use embedded_alloc::LlffHeap as Heap;

    use oxinode_core::lr1121::config::ValidConfig;
    use oxinode_core::mode::Presence;
    use oxinode_core::node::announce::{self, Kind};
    use oxinode_core::node::key::{NodeKey, Stored};
    use oxinode_core::node::messages::{Arrival, Trust};
    use oxinode_core::node::peers::{Learned, Table};
    use oxinode_core::node::{
        announce_len, lxmf_app_data, Entropy, Event, Followed, Inbox, Node, Outcome, Sending,
        Station, Unreadable, TICK_MS,
    };

    use super::*;

    /// The heap `rns-core` allocates from.
    ///
    /// Its tables are `BTreeMap`s and `HashMap`s and its packets are `Vec`s.
    /// Measured on the board in the spike, an engine with one interface and
    /// the tables in `oxinode_core::node::config` used under 40 KB of heap at
    /// its peak under traffic, so 64 KB is that with half again for the
    /// identity, the messages and the screens that come after it. The figure
    /// the board reaches is in the log ("node: heap peak") and is the number
    /// to revise this from. An allocation that fails panics, and a panic
    /// reboots into the bootloader, so running out is a board that went
    /// quiet in DFU, not one that corrupts itself.
    pub const HEAP_SIZE: usize = 64 * 1024;

    #[global_allocator]
    static HEAP: Heap = Heap::empty();

    /// How much more heap than the last report it takes to report again.
    const PEAK_STEP: usize = 1024;

    /// The node, once it has its identity; whether it may run; and when it
    /// last ran its housekeeping.
    pub struct Listener {
        node: Option<Node>,
        presence: Presence,
        /// Whether a configuration was stored at the last look.
        stored_config: bool,
        /// What waits for the air, and when announces may go; the rule is
        /// `oxinode_core::node::Sending`.
        sending: Sending,
        /// The board's name in its announces, as LXMF's app data.
        app_data: Vec<u8>,
        /// Packets heard and waiting for the engine.
        inbox: Inbox,
        /// Who else is around, and what they have sent.
        station: Station,
        entropy: Entropy,
        last_tick_ms: u64,
        reported_peak: usize,
    }

    impl Listener {
        /// The heap, the log and the seed. The engine comes with the identity,
        /// in [`Listener::adopt`], once the flash it is kept in can be read.
        ///
        /// Takes the RNG before the Bluetooth controller does, which is why it
        /// is built before Bluetooth is brought up, and why it is built at most
        /// once: there is one `RNG`, so there can be one `Listener`, and the
        /// heap is initialised exactly once.
        pub fn new(rng: Peri<'_, RNG>) -> Self {
            {
                static mut MEMORY: [MaybeUninit<u8>; HEAP_SIZE] =
                    [MaybeUninit::uninit(); HEAP_SIZE];
                // SAFETY: `MEMORY` is touched nowhere else, and this runs once:
                // building a `Listener` takes the chip's only `RNG`.
                unsafe { HEAP.init(&raw mut MEMORY as usize, HEAP_SIZE) }
            }
            if log::set_logger(&LOG).is_ok() {
                log::set_max_level(log::STATIC_MAX_LEVEL);
            }

            let seed = {
                let mut hw = Rng::new_blocking(rng);
                hw.set_bias_correction(true);
                let mut seed = [0u8; 32];
                hw.blocking_fill_bytes(&mut seed);
                // Back as the controller has always found it.
                hw.set_bias_correction(false);
                seed
            };

            Self {
                node: None,
                presence: Presence::new(),
                stored_config: false,
                sending: Sending::new(),
                app_data: Vec::new(),
                inbox: Inbox::new(),
                station: Station::new(),
                entropy: Entropy::from_seed(seed),
                last_tick_ms: now_ms(),
                reported_peak: 0,
            }
        }

        /// The board's identity, from its page, or a new one written there if
        /// it has none; then the engine, listening as that identity.
        ///
        /// A new identity is made only when the page holds no key: never
        /// written, which is a board's first boot with a node, or written and
        /// damaged, which the log says apart. One that cannot be stored is
        /// still used, and the log says that the next boot will make another.
        ///
        /// `tag` is the four hex digits the board is known by everywhere --
        /// the end of its Bluetooth name, top right on the panel -- and its
        /// announces call it `oxinode` and those.
        pub async fn adopt<F: RecordFlash>(&mut self, storage: &mut Storage<F>, tag: &[u8]) {
            let mut name = String::from("oxinode ");
            name.push_str(core::str::from_utf8(tag).unwrap_or("?"));
            self.app_data = lxmf_app_data(&name);
            let key = match storage.load_key() {
                Stored::Key(key) => {
                    let hashes = key.hashes();
                    defmt::info!(
                        "node: identity {=[u8]:02x}, LXMF address {=[u8]:02x}",
                        &hashes.identity[..],
                        &hashes.lxmf[..]
                    );
                    key
                }
                found => {
                    if found == Stored::Damaged {
                        defmt::warn!("node: the identity page does not hold a key it can trust");
                    }
                    let key = NodeKey::generate(&mut self.entropy);
                    let hashes = key.hashes();
                    match storage.save_key(&key).await {
                        Ok(()) => defmt::info!(
                            "node: a new identity {=[u8]:02x}, LXMF address {=[u8]:02x}, stored",
                            &hashes.identity[..],
                            &hashes.lxmf[..]
                        ),
                        Err(e) => defmt::error!(
                            "node: a new identity {=[u8]:02x} could not be stored ({}); the next boot makes another",
                            &hashes.identity[..],
                            e
                        ),
                    }
                    key
                }
            };
            let now = now_ms();
            // No bitrate yet: it is what the engine budgets our own announces
            // against, and the board does not announce.
            self.node = Some(Node::new(0, now, &key));
            self.last_tick_ms = now;
            defmt::info!(
                "node: listening while no host is attached; heap {=usize} of {=usize} bytes",
                HEAP.used(),
                HEAP_SIZE
            );
        }

        /// What the System screen shows of the node.
        pub fn hashes(&self) -> Option<NodeHashes> {
            self.node.as_ref().map(Node::hashes)
        }

        /// What the modem loop saw on this pass, and whether a configuration
        /// is stored. Says when the board changes from one mode to the other.
        pub fn observe(
            &mut self,
            seen: impl FnOnce() -> Seen,
            stored_config: bool,
        ) -> Option<Change> {
            self.stored_config = stored_config;
            let change = self.presence.update(now_ms(), seen())?;
            match change {
                Change::HostArrived(transport) => defmt::info!(
                    "node: a host spoke over {=str}; the board is its RNode",
                    transport_name(transport)
                ),
                Change::HostLeft if stored_config => defmt::info!(
                    "node: no host for {=u64} ms; the board is a node, on its stored configuration",
                    oxinode_core::mode::HOLD_MS
                ),
                Change::HostLeft => defmt::info!(
                    "node: no host for {=u64} ms, and no stored configuration to be a node on",
                    oxinode_core::mode::HOLD_MS
                ),
            }
            // The engine's housekeeping has been on hold; it starts again
            // from now rather than catching up.
            self.last_tick_ms = now_ms();
            Some(change)
        }

        /// Notice the node starting or stopping, which a host coming or
        /// going does, and so does a configuration being stored or reset.
        /// Starting announces; stopping drops whatever was waiting to go.
        fn follow_mode(&mut self) {
            let running = self.running() && self.node.is_some();
            match self.sending.follow(running, now_ms()) {
                Some(Followed::Started) => defmt::info!("node: running; an announce is due"),
                Some(Followed::Stopped(dropped)) => defmt::info!(
                    "node: stopped; {=usize} packets it had waiting were dropped, {=u32} in all; {=usize} heard and not yet looked at",
                    dropped,
                    self.sending.dropped(),
                    self.inbox.clear()
                ),
                None => {}
            }
        }

        /// *Announce Now*, from the panel: as soon as the budget allows.
        pub fn announce_now(&mut self) {
            if self.running() {
                defmt::info!(
                    "node: an announce asked for; the budget allows one from {=u64} ms",
                    self.sending.allowed_from()
                );
                self.sending.want(Kind::Announce);
            }
        }

        /// The next packet for the air, if the board is a node and nobody is
        /// on the line who may be about to speak: a port open or a phone
        /// linked. Listening goes on regardless; only sending waits.
        pub fn next_packet(
            &mut self,
            nobody_on_the_line: impl FnOnce() -> bool,
        ) -> Option<Outgoing> {
            self.follow_mode();
            if !self.running() {
                return None;
            }
            self.sending
                .next(nobody_on_the_line())
                .map(|(packet, kind)| Outgoing { packet, kind })
        }

        /// What became of a packet [`Listener::next_packet`] gave out:
        /// `Some(airtime)` if it went. An announce that went spends the
        /// budget; one that did not is asked for again.
        pub fn sent(&mut self, out: Outgoing, went: Option<u32>) {
            self.sending.sent(out.kind, went, now_ms());
            if let (Some(kind), Some(airtime_us)) = (out.kind, went) {
                defmt::info!(
                    "node: {=str} sent, {=u32} us on the air; the next may go in {=u64} ms",
                    match kind {
                        Kind::Announce => "announce",
                        Kind::PathResponse => "path response",
                    },
                    airtime_us,
                    announce::gap_ms(airtime_us)
                );
            }
        }

        /// The least time between two announces at `config`, by the budget,
        /// for the System screen.
        pub fn announce_gap_s(&self, config: Option<ValidConfig>) -> Option<u32> {
            let airtime = announce::airtime_us(&config?, announce_len(self.app_data.len()))?;
            Some((announce::gap_ms(airtime) / 1_000) as u32)
        }

        /// What the GPS says the time is, for an announce's emission time:
        /// Unix seconds, or zero, which Reticulum reads as not known.
        fn emitted_s() -> u64 {
            let position = crate::gps::position();
            match (position.date, position.time) {
                (Some(date), Some(time)) => date.unix_seconds(time).unwrap_or(0),
                _ => 0,
            }
        }

        /// Which of the two the board is, for the Home screen.
        pub fn mode(&self) -> Option<Mode> {
            Some(self.presence.mode(self.stored_config))
        }

        /// Whether the node may run now.
        fn running(&self) -> bool {
            self.presence.mode(self.stored_config).is_node()
        }

        /// What the Peers screen shows.
        pub fn peers(&self) -> Peers {
            self.station.peers.screen(now_ms())
        }

        /// Messages not yet read, for the Home screen.
        pub fn unread(&self) -> Option<u16> {
            Some(self.station.messages.unread().min(usize::from(u16::MAX)) as u16)
        }

        /// A number that moves when a new message arrives, for the panel to
        /// wake on.
        pub fn arrived(&self) -> u32 {
            self.station.messages.arrived()
        }

        /// A packet came off the air: queued for the engine, which sees it
        /// on a pass when the radio is quiet.
        pub fn heard(&mut self, packet: &[u8], signal: Signal) {
            if !self.running() || self.node.is_none() {
                return;
            }
            if self.inbox.push(packet, Some(signal), now_ms()) {
                defmt::warn!(
                    "node: {=usize} packets waiting; the oldest was dropped, {=u32} in all",
                    Inbox::CAPACITY,
                    self.inbox.dropped()
                );
            }
        }

        /// Called on every pass of the modem loop; runs the engine's
        /// housekeeping as often as it asks for it, and, when the radio had
        /// nothing on this pass, hands the engine one packet that waited.
        pub fn tick(&mut self, radio_quiet: bool) {
            self.follow_mode();
            if !self.running() {
                return;
            }
            if radio_quiet {
                if let (Some(node), Some(inbound)) = (self.node.as_mut(), self.inbox.pop()) {
                    let mut out =
                        node.heard(&inbound.packet, inbound.signal, now_ms(), &mut self.entropy);
                    let events =
                        self.station
                            .take(node, &mut out, inbound.at_ms, &mut self.entropy);
                    self.learn(&events);
                    self.report(out);
                }
            }
            let now = now_ms();
            if now.saturating_sub(self.last_tick_ms) < TICK_MS {
                return;
            }
            let Some(node) = self.node.as_mut() else {
                return;
            };
            let out = node.tick(now, &mut self.entropy);
            self.last_tick_ms = now;
            self.report(out);
            // An announce, if one is due and the budget allows it.
            if let Some(kind) = self.sending.due(now) {
                let emitted = Self::emitted_s();
                let built = self.node.as_ref().and_then(|node| {
                    node.announce(kind, &self.app_data, emitted, &mut self.entropy)
                });
                match built {
                    Some(packet) => self.sending.push(packet, Some(kind)),
                    None => defmt::error!("node: the announce could not be built"),
                }
            }
        }

        /// What the station made of a packet, in the log.
        fn learn(&self, events: &[Event]) {
            let peers = &self.station.peers;
            let name = |destination: &[u8; 16]| {
                peers
                    .get(destination)
                    .and_then(|p| p.name.clone())
                    .unwrap_or_default()
            };
            for event in events {
                match event {
                    Event::Peer { destination, learned } => match learned {
                        Learned::NotLxmf => {}
                        Learned::New => defmt::info!(
                            "node: a new peer {=[u8]:02x} \"{=str}\", {=usize} of {=usize}",
                            &destination[..],
                            name(destination).as_str(),
                            peers.len(),
                            Table::CAPACITY
                        ),
                        Learned::Updated => defmt::info!(
                            "node: peer {=[u8]:02x} \"{=str}\" heard again",
                            &destination[..],
                            name(destination).as_str()
                        ),
                        Learned::Evicted(gone) => defmt::info!(
                            "node: a new peer {=[u8]:02x} \"{=str}\" in place of {=[u8]:02x}, heard longest ago",
                            &destination[..],
                            name(destination).as_str(),
                            &gone[..]
                        ),
                        Learned::Refused => defmt::warn!(
                            "node: a new peer {=[u8]:02x} not kept: every peer has a conversation ({=u32} turned away)",
                            &destination[..],
                            peers.refused()
                        ),
                    },
                    Event::Settled { source, settled } => defmt::info!(
                        "node: the key of {=[u8]:02x} arrived: {=usize} messages verified, {=usize} failed",
                        &source[..],
                        settled.verified,
                        settled.failed
                    ),
                    Event::Message {
                        hash,
                        source,
                        arrival,
                    } => self.log_message(hash, source, *arrival),
                    Event::Unreadable(why) => match why {
                        Unreadable::NotOpportunistic {
                            packet_type,
                            context,
                        } => defmt::info!(
                            "node: a packet for the board of type {=u8}, context {=u8}, which it does not read",
                            packet_type,
                            context
                        ),
                        Unreadable::Undecryptable => {
                            defmt::warn!("node: a packet for the board that does not decrypt")
                        }
                        Unreadable::Malformed(m) => defmt::warn!(
                            "node: a packet for the board that is not an LXMF message ({=str})",
                            malformed_name(*m)
                        ),
                    },
                }
            }
        }

        fn log_message(&self, hash: &[u8; 32], source: &[u8; 16], arrival: Arrival) {
            let messages = &self.station.messages;
            match arrival {
                Arrival::New(trust) => {
                    let (len, attachment) = messages.get(hash).map_or((0, false), |r| {
                        (r.message.content.len(), r.message.attachment)
                    });
                    defmt::info!(
                        "node: a message {=[u8]:02x} from {=[u8]:02x}, {=str}, {=usize} bytes{=str}; proved; {=usize} unread, {=usize} kept",
                        &hash[..8],
                        &source[..],
                        trust_name(trust),
                        len,
                        if attachment { ", with an attachment" } else { "" },
                        messages.unread(),
                        messages.len()
                    );
                    if trust == Trust::Unverified {
                        defmt::info!(
                            "node: {=[u8]:02x} has not been heard; its path, and so its key, asked for",
                            &source[..]
                        );
                    }
                }
                Arrival::Duplicate(trust) => defmt::info!(
                    "node: message {=[u8]:02x} again, from {=[u8]:02x} ({=str}): proved again, not kept twice",
                    &hash[..8],
                    &source[..],
                    trust.map_or("let go since", trust_name)
                ),
                Arrival::Forged => defmt::warn!(
                    "node: a message {=[u8]:02x} claiming {=[u8]:02x} is not signed by its key: refused, not proved ({=u32} in all)",
                    &hash[..8],
                    &source[..],
                    messages.forged()
                ),
            }
            if messages.dropped() > 0 && matches!(arrival, Arrival::New(_)) {
                defmt::info!(
                    "node: {=u32} messages let go to make room, in all",
                    messages.dropped()
                );
            }
        }

        fn report(&mut self, out: Outcome) {
            for heard in &out.heard {
                defmt::info!(
                    "node: announce from {=[u8]:02x}, {=u8} hops, {=usize} bytes of app data",
                    &heard.destination[..],
                    heard.hops,
                    heard.app_data.as_ref().map_or(0, |d| d.len())
                );
            }
            for destination in &out.path_requests {
                defmt::info!(
                    "node: a path request for {=[u8]:02x}; a path response is due",
                    &destination[..]
                );
                self.sending.want(Kind::PathResponse);
            }
            for packet in out.air {
                self.sending.push(packet, None);
            }
            let used = HEAP.used();
            if used >= self.reported_peak + PEAK_STEP {
                self.reported_peak = used;
                defmt::info!(
                    "node: heap peak {=usize} of {=usize} bytes, {=usize} paths",
                    used,
                    HEAP_SIZE,
                    self.node.as_ref().map_or(0, Node::paths)
                );
            }
        }
    }

    /// A packet of the node's on its way to the air.
    pub struct Outgoing {
        packet: Vec<u8>,
        kind: Option<Kind>,
    }

    impl Outgoing {
        pub fn bytes(&self) -> &[u8] {
            &self.packet
        }
    }

    fn now_ms() -> u64 {
        Instant::now().as_millis()
    }

    fn trust_name(trust: Trust) -> &'static str {
        match trust {
            Trust::Verified => "verified",
            Trust::Unverified => "unverified",
            Trust::Failed => "failed verification",
        }
    }

    fn malformed_name(m: oxinode_core::node::lxmf::Malformed) -> &'static str {
        use oxinode_core::node::lxmf::Malformed;
        match m {
            Malformed::Short => "short",
            Malformed::Payload => "payload",
            Malformed::Timestamp => "timestamp",
            Malformed::Text => "text",
            Malformed::Fields => "fields",
            Malformed::Length => "length",
        }
    }

    fn transport_name(transport: oxinode_core::rnode::hosts::Transport) -> &'static str {
        match transport {
            oxinode_core::rnode::hosts::Transport::Usb => "USB",
            oxinode_core::rnode::hosts::Transport::Bluetooth => "Bluetooth",
        }
    }

    /// `rns-core`'s log lines, in the defmt log.
    ///
    /// `log` formats at run time, so each line is formatted into a buffer
    /// here and handed to defmt as a string. A line longer than the buffer
    /// is cut, and says so.
    struct DefmtLog;

    static LOG: DefmtLog = DefmtLog;

    const LINE: usize = 160;

    struct Line {
        buf: [u8; LINE],
        len: usize,
        cut: bool,
    }

    impl Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for (i, c) in s.char_indices() {
                let end = i + c.len_utf8();
                if self.len + c.len_utf8() > LINE {
                    self.cut = true;
                    return Ok(());
                }
                self.buf[self.len..self.len + c.len_utf8()].copy_from_slice(&s.as_bytes()[i..end]);
                self.len += c.len_utf8();
            }
            Ok(())
        }
    }

    impl log::Log for DefmtLog {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::STATIC_MAX_LEVEL
        }

        fn log(&self, record: &log::Record) {
            let mut line = Line {
                buf: [0; LINE],
                len: 0,
                cut: false,
            };
            let _ = write!(line, "{}", record.args());
            // Whole characters only were copied, so this is valid UTF-8.
            let text = core::str::from_utf8(&line.buf[..line.len]).unwrap_or("?");
            let cut = if line.cut { " [cut]" } else { "" };
            match record.level() {
                log::Level::Error => defmt::error!("rns: {=str}{=str}", text, cut),
                log::Level::Warn => defmt::warn!("rns: {=str}{=str}", text, cut),
                _ => defmt::info!("rns: {=str}{=str}", text, cut),
            }
        }

        fn flush(&self) {}
    }
}

/// Without the `node` feature: the same calls, and nothing behind them.
#[cfg(not(feature = "node"))]
pub struct Listener;

/// Without the node there is nothing to send, and this says so in the type:
/// no value of it exists, so the modem loop's sending code is dead.
#[cfg(not(feature = "node"))]
pub enum Outgoing {}

#[cfg(not(feature = "node"))]
impl Outgoing {
    pub fn bytes(&self) -> &[u8] {
        match *self {}
    }
}

#[cfg(not(feature = "node"))]
impl Listener {
    pub fn new(_rng: Peri<'_, RNG>) -> Self {
        Listener
    }

    #[inline(always)]
    pub async fn adopt<F: RecordFlash>(&mut self, _storage: &mut Storage<F>, _tag: &[u8]) {}

    #[inline(always)]
    pub fn announce_now(&mut self) {}

    #[inline(always)]
    pub fn next_packet(&mut self, _nobody_on_the_line: impl FnOnce() -> bool) -> Option<Outgoing> {
        None
    }

    #[inline(always)]
    pub fn sent(&mut self, out: Outgoing, _went: Option<u32>) {
        match out {}
    }

    #[inline(always)]
    pub fn announce_gap_s(
        &self,
        _config: Option<oxinode_core::lr1121::config::ValidConfig>,
    ) -> Option<u32> {
        None
    }

    #[inline(always)]
    pub fn hashes(&self) -> Option<NodeHashes> {
        None
    }

    #[inline(always)]
    pub fn observe(
        &mut self,
        _seen: impl FnOnce() -> Seen,
        _stored_config: bool,
    ) -> Option<Change> {
        None
    }

    #[inline(always)]
    pub fn mode(&self) -> Option<Mode> {
        None
    }

    #[inline(always)]
    pub fn heard(&mut self, _packet: &[u8], _signal: Signal) {}

    #[inline(always)]
    pub fn tick(&mut self, _radio_quiet: bool) {}

    #[inline(always)]
    pub fn peers(&self) -> Peers {
        Peers::default()
    }

    #[inline(always)]
    pub fn unread(&self) -> Option<u16> {
        None
    }

    #[inline(always)]
    pub fn arrived(&self) -> u32 {
        0
    }
}
