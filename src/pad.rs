//! The navigation pad driver: six switches, interrupt-driven, onto a channel.
//!
//! The timing -- debounce, auto-repeat, what counts as a press -- is all in
//! [`oxinode_core::pad`], where it is tested with a clock that is a number.
//! This module is the part that needs a board: which pins, the pull-ups, the
//! `PORT` interrupt, and the channel the render loop reads.
//!
//! # The pins
//!
//! From muzi's Super IO specification, via the Meshtastic variant that names
//! them `TB_*` because it reuses a trackball driver for the pad (the README
//! explains why that is the wrong driver). All six active low, with the chip's
//! own pull-ups: the switches simply short a line to ground.
//!
//! | switch | pin   |
//! |--------|-------|
//! | up     | P0.21 |
//! | down   | P0.17 |
//! | left   | P1.05 |
//! | right  | P0.16 |
//! | OK     | P0.10 |
//! | back   | P0.15 |
//!
//! **P0.10 is an NFC pin.** It is a GPIO only while `UICR.NFCPINS` says so,
//! and this driver does not check -- `board::nfc_pins_are_gpio` does, and the
//! product image logs it at boot. If that line says `false`, an OK that never
//! registers is not a bug in here. See the README before touching the UICR.
//!
//! # Interrupts, and being quiet
//!
//! Every wait on a pin goes through the GPIO `PORT` event: `embassy-nrf` sets
//! the pin's `SENSE` to the level it is *not* at, and the chip raises one
//! interrupt when it gets there. No timer runs while nothing is pressed. When
//! something is, the core's `deadline` says when the next thing is due -- the
//! end of a debounce, or the next repeat -- and the driver sleeps until then or
//! until the next edge, whichever is first.
//!
//! Every pin is re-read on every wake rather than trusting the edge that
//! caused it. Six register reads cost nothing, and it means a missed edge --
//! one that landed between the read and the re-arm -- is caught on the next
//! wake instead of leaving a switch believed stuck. The core treats a re-read
//! that agrees as not an edge, so this costs no bounces in the statistics.
//!
//! # The channel
//!
//! Gestures go onto a bounded channel and the render loop takes them off. If
//! the loop falls behind and the channel fills, the newest gesture is dropped
//! and counted: a burst of presses can make the interface late, but it cannot
//! make it *do* eight things after the finger has left the switch. The core's
//! own no-catch-up rule on repeats makes that a rare event, and the count is
//! logged so it does not stay invisible.

use embassy_futures::select::{select, select_array, Either};
use embassy_nrf::gpio::{Input, Pull};
use embassy_nrf::peripherals::{P0_10, P0_12, P0_15, P0_16, P0_17, P0_21, P1_05, P1_09};
use embassy_nrf::Peri;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, TryReceiveError};
use embassy_time::{Instant, Timer};
use oxinode_core::pad::{Key, Kind, Mode, Pad};
use oxinode_core::ui;

/// How many gestures can wait for the render loop.
///
/// The loop runs every 50 ms at the slowest and a repeat comes every 125 ms,
/// so in normal use this never holds more than one. Eight covers a burst of
/// six simultaneous presses with room to spare, without inviting a queue that
/// keeps acting after the user has stopped.
pub const QUEUE: usize = 8;

/// The channel from the pad to whoever draws. `NoopRawMutex` because both
/// ends live on the one executor, as everything else in the product image
/// does.
pub type Events = Channel<NoopRawMutex, ui::Input, QUEUE>;

/// The six switches, claimed as pulled-up inputs.
pub struct Pins<'d> {
    /// In [`Key::ALL`] order.
    inputs: [Input<'d>; 6],
}

impl<'d> Pins<'d> {
    /// Claim the pad's pins. The parameters are typed, so a transposed pin is
    /// a compile error rather than a switch that does something else.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        up: Peri<'d, P0_21>,
        down: Peri<'d, P0_17>,
        left: Peri<'d, P1_05>,
        right: Peri<'d, P0_16>,
        ok: Peri<'d, P0_10>,
        back: Peri<'d, P0_15>,
    ) -> Self {
        Pins {
            inputs: [
                Input::new(up, Pull::Up),
                Input::new(down, Pull::Up),
                Input::new(left, Pull::Up),
                Input::new(right, Pull::Up),
                Input::new(ok, Pull::Up),
                Input::new(back, Pull::Up),
            ],
        }
    }

    /// Whether each switch reads pressed right now, in [`Key::ALL`] order.
    /// Active low, so pressed is low.
    fn levels(&self) -> [bool; 6] {
        let mut levels = [false; 6];
        for (level, pin) in levels.iter_mut().zip(&self.inputs) {
            *level = pin.is_low();
        }
        levels
    }
}

/// Run the pad forever, feeding `events`.
///
/// Meant to be joined with the rest of the image rather than spawned, like
/// everything else in it.
pub async fn run(pins: &mut Pins<'_>, events: &Events) -> ! {
    let mut pad = Pad::new();
    let mut dropped: u32 = 0;

    loop {
        let now = Instant::now().as_millis();
        for (key, pressed) in Key::ALL.into_iter().zip(pins.levels()) {
            pad.level(key, pressed, now);
        }

        for event in pad.poll(now) {
            match event.kind {
                Kind::Press => defmt::info!(
                    "pad: {=str} press, settled in {=u16} ms after {=u8} bounce(s)",
                    event.key.name(),
                    event.settle_ms,
                    event.bounces
                ),
                Kind::Repeat => defmt::debug!("pad: {=str} repeat", event.key.name()),
            }
            if events.try_send(event.input()).is_err() {
                dropped = dropped.wrapping_add(1);
                defmt::warn!(
                    "pad: {=str} dropped, the render loop is behind ({=u32} so far)",
                    event.key.name(),
                    dropped
                );
            }
        }

        // Sleep until the next edge on any pin, or the pad's next deadline
        // if it has one. Each pin waits for the level it is *not* at as of
        // this instant, so an edge between the read above and the arm here
        // returns immediately rather than being lost.
        let edges = select_array(pins.inputs.each_mut().map(|pin| async move {
            if pin.is_low() {
                pin.wait_for_high().await
            } else {
                pin.wait_for_low().await
            }
        }));
        match pad.deadline() {
            None => {
                edges.await;
            }
            Some(at) => {
                // Either way, go round and poll: an edge is a level to
                // report, and the deadline is something due.
                let _: Either<_, _> = select(edges, Timer::at(Instant::from_millis(at))).await;
            }
        }
    }
}

/// Take every gesture that is waiting, without blocking.
///
/// For the render loop: drain, act, draw. Returns the number taken, so the
/// caller knows whether anything happened without inspecting each one.
pub fn drain(events: &Events, mut each: impl FnMut(ui::Input)) -> usize {
    let mut taken = 0;
    loop {
        match events.try_receive() {
            Ok(input) => {
                each(input);
                taken += 1;
            }
            Err(TryReceiveError::Empty) => return taken,
        }
    }
}

/// The three-position mode switch on the Super IO: P1.09 and P0.12.
///
/// Read with no pull, as Meshtastic reads it, because the Super IO's own
/// resistors are unknown -- there is no schematic for that board -- and an
/// internal pull fighting an external one would read the same in every
/// position. What each position drives is a hypothesis until it is read on
/// the board in all three; see [`Mode`] and the phase 10 notes.
pub struct ModeSwitch<'d> {
    mode1: Input<'d>,
    mode2: Input<'d>,
}

impl<'d> ModeSwitch<'d> {
    pub fn new(mode1: Peri<'d, P1_09>, mode2: Peri<'d, P0_12>) -> Self {
        ModeSwitch {
            mode1: Input::new(mode1, Pull::None),
            mode2: Input::new(mode2, Pull::None),
        }
    }

    /// The raw levels, `(P1.09, P0.12)`, `true` for high. This is the fact;
    /// [`read`](Self::read) is the reading of it.
    pub fn levels(&self) -> (bool, bool) {
        (self.mode1.is_high(), self.mode2.is_high())
    }

    /// The position, on the assumption that a line is driven *high* in its
    /// position. That is the reading Meshtastic's use implies: it treats
    /// P0.12 -- the line it names for the middle position -- low as "GPS
    /// off", and the middle position is the one that switches the GPS on. It
    /// is the opposite polarity to the six switches beside it, and it is a
    /// hypothesis until the log of [`levels`](Self::levels) has been read in
    /// all three positions. If it is wrong, the fix is two `!` here.
    pub fn read(&self) -> Mode {
        let (mode1, mode2) = self.levels();
        Mode::decode(mode1, mode2)
    }

    /// Log both the levels and the reading, once, for the boot log.
    pub fn report(&self) {
        let (mode1, mode2) = self.levels();
        defmt::info!(
            "board: mode switch P1.09={=str} P0.12={=str} -> {=str} (polarity unconfirmed)",
            if mode1 { "high" } else { "low" },
            if mode2 { "high" } else { "low" },
            self.read().name()
        );
    }
}
