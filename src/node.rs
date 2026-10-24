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
//! links no stack. Who the host is comes as a closure rather than a value
//! for the same reason: reading it is two loads the compiler may not drop,
//! and the RNode should not pay for a question nobody asks.
//!
//! # When it listens
//!
//! Only while no host is attached: nobody has the KISS port open and no
//! phone is connected. With a host, the host is the node and the board is
//! its RNode, and nothing here runs, so a host sees the RNode it always saw,
//! timing included. A packet still goes to the host exactly as before; the
//! listener is handed a copy only when there is no host to give it to.
//! Exactly what "attached" means, and the handover in both directions, are
//! settled with the standalone mode itself; this uses the panel's existing
//! notion of a host until then.
//!
//! An announce takes the engine about 43 ms to verify, and the modem loop
//! waits for it. With no host that delays nothing but the panel.

use embassy_nrf::peripherals::RNG;
use embassy_nrf::Peri;
use oxinode_core::rnode::air::Signal;
use oxinode_core::screens::Host;

#[cfg(feature = "node")]
pub use imp::*;

#[cfg(feature = "node")]
mod imp {
    use core::fmt::Write;
    use core::mem::MaybeUninit;

    use embassy_nrf::rng::Rng;
    use embassy_time::Instant;
    use embedded_alloc::LlffHeap as Heap;
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

    /// The node, and when it last ran its housekeeping.
    pub struct Listener {
        node: Node,
        entropy: Entropy,
        last_tick_ms: u64,
        reported_peak: usize,
    }

    impl Listener {
        /// The heap, the log and the seed, and an engine listening on the air.
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

            let now = now_ms();
            // No bitrate yet: it is what the engine budgets our own announces
            // against, and the board does not announce.
            let node = Node::new(0, now);
            defmt::info!(
                "node: listening while no host is attached; heap {=usize} of {=usize} bytes",
                HEAP.used(),
                HEAP_SIZE
            );
            Self {
                node,
                entropy: Entropy::from_seed(seed),
                last_tick_ms: now,
                reported_peak: 0,
            }
        }

        /// A packet came off the air and went wherever it goes.
        pub fn heard(&mut self, packet: &[u8], signal: Signal, host: impl FnOnce() -> Host) {
            if host() != Host::None {
                return;
            }
            let out = self
                .node
                .heard(packet, Some(signal), now_ms(), &mut self.entropy);
            self.report(out);
        }

        /// Called on every pass of the modem loop; runs the engine's
        /// housekeeping as often as it asks for it.
        pub fn tick(&mut self, host: impl FnOnce() -> Host) {
            if host() != Host::None {
                return;
            }
            let now = now_ms();
            if now.saturating_sub(self.last_tick_ms) < TICK_MS {
                return;
            }
            self.last_tick_ms = now;
            let out = self.node.tick(now, &mut self.entropy);
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
                    self.node.paths()
                );
            }
        }
    }

    fn now_ms() -> u64 {
        Instant::now().as_millis()
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
    pub fn heard(&mut self, _packet: &[u8], _signal: Signal, _host: impl FnOnce() -> Host) {}

    #[inline(always)]
    pub fn tick(&mut self, _host: impl FnOnce() -> Host) {}
}
