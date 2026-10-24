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
//! An announce takes the engine about 43 ms to verify, and the modem loop
//! waits for it. With no host that delays nothing but the panel.

use embassy_nrf::peripherals::RNG;
use embassy_nrf::Peri;
use oxinode_core::mode::{Change, Mode, Seen};
use oxinode_core::rnode::air::Signal;
use oxinode_core::screens::NodeHashes;

use crate::store::{RecordFlash, Storage};

#[cfg(feature = "node")]
pub use imp::*;

#[cfg(feature = "node")]
mod imp {
    use core::fmt::Write;
    use core::mem::MaybeUninit;

    use embassy_nrf::rng::Rng;
    use embassy_time::Instant;
    use embedded_alloc::LlffHeap as Heap;
    use oxinode_core::mode::Presence;
    use oxinode_core::node::key::{NodeKey, Stored};
    use oxinode_core::node::{Entropy, Node, Outcome, TICK_MS};

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
        pub async fn adopt<F: RecordFlash>(&mut self, storage: &mut Storage<F>) {
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

        /// Which of the two the board is, for the Home screen.
        pub fn mode(&self) -> Option<Mode> {
            Some(self.presence.mode(self.stored_config))
        }

        /// Whether the node may run now.
        fn running(&self) -> bool {
            self.presence.mode(self.stored_config).is_node()
        }

        /// A packet came off the air and went wherever it goes.
        pub fn heard(&mut self, packet: &[u8], signal: Signal) {
            if !self.running() {
                return;
            }
            let Some(node) = self.node.as_mut() else {
                return;
            };
            let out = node.heard(packet, Some(signal), now_ms(), &mut self.entropy);
            self.report(out);
        }

        /// Called on every pass of the modem loop; runs the engine's
        /// housekeeping as often as it asks for it.
        pub fn tick(&mut self) {
            if !self.running() {
                return;
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
                defmt::warn!(
                    "node: a path request for {=[u8]:02x}, which nothing answers yet",
                    &destination[..]
                );
            }
            if !out.air.is_empty() {
                defmt::warn!(
                    "node: {=usize} packets to send, and nothing sends yet",
                    out.air.len()
                );
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

    fn now_ms() -> u64 {
        Instant::now().as_millis()
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

#[cfg(not(feature = "node"))]
impl Listener {
    pub fn new(_rng: Peri<'_, RNG>) -> Self {
        Listener
    }

    #[inline(always)]
    pub async fn adopt<F: RecordFlash>(&mut self, _storage: &mut Storage<F>) {}

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
    pub fn tick(&mut self) {}
}
