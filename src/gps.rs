//! The GPS: a load switch, a UART, and the task that owns them.
//!
//! Three pins, all on the Super IO's side of the castellations:
//!
//! | signal | pin | what it is |
//! |---|---|---|
//! | `GPS_EN` | P1.01 | a load switch, **active high**, rated 500 mA; not a module enable |
//! | module TX | P0.20 | the receiver's output, so the nRF52840's `RXD` |
//! | module RX | P0.19 | the receiver's input, so the nRF52840's `TXD` |
//!
//! The two sources name the UART from opposite ends -- the schematic calls
//! P0.20 `UART_GPS_TX`, the variant calls it `GPS_RX_PIN` -- and they agree
//! once each is read from its own end: the module transmits on P0.20. That
//! is what is tried first, and it is not trusted: the probe below listens on
//! each pin at each baud rate until a sentence with a good checksum arrives,
//! and logs which one it was. See `docs/hardware/gps.md` for the answer the
//! board gave.
//!
//! # What this module does and does not decide
//!
//! Nothing about the bytes. Framing, checksums, the sentences, the fix and
//! its age are [`oxinode_core::gps`], where they are tested against captured
//! sentences; so is the rule for whether the receiver should be on at all.
//! This is the part that has to own peripherals: it drives the switch, opens
//! the UART, feeds what arrives to the core, and copies the core's answer
//! into a place the modem loop can read without waiting.
//!
//! # A task of its own
//!
//! The modem loop never touches the receiver. [`serve`] runs beside it,
//! like the Bluetooth task, and the two meet at exactly two points: the
//! screen's copy of the [`Position`], published here and read by
//! [`position`]; and the menu's `GPS On/Off`, raised by [`toggle`]. The
//! mode switch is read here too, every quarter second, because the
//! receiver is what it controls.
//!
//! # Power
//!
//! Off means the UART is *dropped*, not idle. The receiver's output floats
//! when its rail is cut, and a UART left listening to a floating line burns
//! current on the input and fills the ring with noise; dropping the driver
//! disconnects both pins. On is the reverse: the switch first, then a
//! listener.

use core::cell::Cell;

use embassy_futures::select::{select, select3, Either, Either3};
use embassy_nrf::buffered_uarte::{self, BufferedUarte};
use embassy_nrf::gpio::{AnyPin, Level, Output, OutputDrive};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{
    P0_19, P0_20, P1_01, PPI_CH10, PPI_CH11, PPI_GROUP0, TIMER2, UARTE0,
};
use embassy_nrf::uarte::{Baudrate, Config};
use embassy_nrf::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use oxinode_core::gps::{self, Attempt, Control, Position, Receiver, Status, PROBE_WINDOW_MS};
use oxinode_core::pad::Millis;

use crate::pad::ModeSwitch;

/// The receive ring. Two DMA halves, so it must be even; at 9600 baud this
/// is half a second of sentences, and the task reads it many times a second.
pub const RX_BUFFER: usize = 512;
/// Nothing is sent. The driver wants a buffer anyway.
pub const TX_BUFFER: usize = 16;

/// How often the switch is read, and the receiver's age recomputed, while
/// nothing arrives.
const TICK: Duration = Duration::from_millis(250);

/// How often the receiver's state goes to the log while it is on, so a
/// bring-up can be followed from the log port alone.
const REPORT_EVERY: Duration = Duration::from_secs(30);

/// The screen's copy. Written here, read by the modem loop once per redraw.
static POSITION: Mutex<CriticalSectionRawMutex, Cell<Position>> =
    Mutex::new(Cell::new(Position::OFF));

/// `GPS On/Off` was chosen on the panel.
static TOGGLE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// What the receiver knows right now. Plain values; never waits.
pub fn position() -> Position {
    POSITION.lock(|p| p.get())
}

fn publish(position: Position) {
    POSITION.lock(|p| p.set(position));
}

/// Flip the receiver's power, from the panel.
pub fn toggle() {
    TOGGLE.signal(());
}

fn now() -> Millis {
    Instant::now().as_millis()
}

/// Publish for the screen, and log a change of state: silent to searching
/// to fix and back are the moments a person waiting on the board wants to
/// know about, and once a fix is had, where it is.
fn report(receiver: &Receiver, last: &mut Status, last_report: &mut Instant) {
    let position = receiver.position(now());
    publish(position);
    let periodic = last_report.elapsed() >= REPORT_EVERY;
    if position.status == *last && !periodic {
        return;
    }
    *last = position.status;
    *last_report = Instant::now();
    let (good, bad) = receiver.counts();
    match position.fix {
        Some(fix) => defmt::info!(
            "gps: {=str}; sats {=u8}/{=u8}; lat {=i32} lon {=i32} udeg, alt {=i32} dm, {=u32} s old; {=u32} sentences, {=u32} bad",
            position.status.word(),
            position.sats_used.unwrap_or(0),
            position.sats_in_view.unwrap_or(0),
            fix.lat_udeg,
            fix.lon_udeg,
            fix.alt_dm.unwrap_or(0),
            position.fix_age_s.unwrap_or(0),
            good,
            bad
        ),
        None => defmt::info!(
            "gps: {=str}; sats {=u8}/{=u8}; no fix; {=u32} sentences, {=u32} bad",
            position.status.word(),
            position.sats_used.unwrap_or(0),
            position.sats_in_view.unwrap_or(0),
            good,
            bad
        ),
    }
}

/// The pins and peripherals the receiver needs, owned for the life of the
/// task so the UART can be opened and closed as often as the power is.
pub struct Gps<'d, I> {
    enable: Output<'d>,
    uarte: Peri<'d, UARTE0>,
    timer: Peri<'d, TIMER2>,
    ppi_a: Peri<'d, PPI_CH10>,
    ppi_b: Peri<'d, PPI_CH11>,
    group: Peri<'d, PPI_GROUP0>,
    p0_20: Peri<'d, P0_20>,
    p0_19: Peri<'d, P0_19>,
    irq: I,
    rx: &'d mut [u8; RX_BUFFER],
    tx: &'d mut [u8; TX_BUFFER],
}

impl<'d, I> Gps<'d, I>
where
    I: Binding<embassy_nrf::interrupt::typelevel::UARTE0, buffered_uarte::InterruptHandler<UARTE0>>
        + Copy
        + 'd,
{
    /// Claim the load switch, off, and everything the UART will need.
    ///
    /// `TIMER2` and two PPI channels with a group are the buffered driver's:
    /// it counts received bytes with the timer so the ring can be read
    /// before a DMA transfer ends. `TIMER0` is the link layer's and `TIMER1`
    /// is the stall guard's; channels 17 to 31 are the link layer's too.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        enable: Peri<'d, P1_01>,
        uarte: Peri<'d, UARTE0>,
        timer: Peri<'d, TIMER2>,
        ppi_a: Peri<'d, PPI_CH10>,
        ppi_b: Peri<'d, PPI_CH11>,
        group: Peri<'d, PPI_GROUP0>,
        p0_20: Peri<'d, P0_20>,
        p0_19: Peri<'d, P0_19>,
        irq: I,
        rx: &'d mut [u8; RX_BUFFER],
        tx: &'d mut [u8; TX_BUFFER],
    ) -> Self {
        Gps {
            enable: Output::new(enable, Level::Low, OutputDrive::Standard),
            uarte,
            timer,
            ppi_a,
            ppi_b,
            group,
            p0_20,
            p0_19,
            irq,
            rx,
            tx,
        }
    }

    /// Open the UART one way: the expected pins or the other way round, at
    /// one baud rate. Dropping what comes back closes it and releases both
    /// pins.
    fn open(&mut self, attempt: Attempt) -> BufferedUarte<'_> {
        let (rxd, txd): (Peri<'_, AnyPin>, Peri<'_, AnyPin>) = if attempt.swapped {
            (self.p0_19.reborrow().into(), self.p0_20.reborrow().into())
        } else {
            (self.p0_20.reborrow().into(), self.p0_19.reborrow().into())
        };
        let mut config = Config::default();
        config.baudrate = baudrate(attempt.baud);
        BufferedUarte::new(
            self.uarte.reborrow(),
            self.timer.reborrow(),
            self.ppi_a.reborrow(),
            self.ppi_b.reborrow(),
            self.group.reborrow(),
            rxd,
            txd,
            self.irq,
            config,
            &mut self.rx[..],
            &mut self.tx[..],
        )
    }
}

/// The peripheral's divisor for a rate the core names. The core's table is
/// what is probed; this is the only place it meets the register.
fn baudrate(baud: u32) -> Baudrate {
    match baud {
        4_800 => Baudrate::Baud4800,
        38_400 => Baudrate::Baud38400,
        57_600 => Baudrate::Baud57600,
        115_200 => Baudrate::Baud115200,
        _ => Baudrate::Baud9600,
    }
}

/// The pin the module was heard on, for the log.
fn rx_pin(attempt: Attempt) -> &'static str {
    if attempt.swapped {
        "P0.19"
    } else {
        "P0.20"
    }
}

/// Run the receiver: follow the switch and the menu, probe for the module
/// when powered, and keep the screen's copy current.
pub async fn serve<I>(mut gps: Gps<'_, I>, switch: &ModeSwitch<'_>) -> !
where
    I: Binding<embassy_nrf::interrupt::typelevel::UARTE0, buffered_uarte::InterruptHandler<UARTE0>>
        + Copy,
{
    let mut control = Control::new();
    let mut receiver = Receiver::new();
    let mut buf = [0u8; 128];
    if control.switch(switch.read()) {
        defmt::info!("gps: the switch is at GPS ON");
    }

    loop {
        // Off: the switch low, no UART, and nothing to do but watch for a
        // reason to change that.
        gps.enable.set_low();
        receiver.power(false);
        publish(receiver.position(now()));
        while !control.is_on() {
            match select(TOGGLE.wait(), Timer::after(TICK)).await {
                Either::First(()) => {
                    control.toggle();
                    defmt::info!("gps: menu: on");
                }
                Either::Second(()) => {
                    if control.switch(switch.read()) {
                        defmt::info!("gps: switch moved to GPS ON");
                    }
                }
            }
        }

        // On: the rail first, then listen. The probe walks every pin order
        // and rate until a sentence checks, and starts over if none does,
        // because a module that is slow to wake is not one to give up on.
        defmt::info!("gps: power on (P1.01 high)");
        gps.enable.set_high();
        receiver.power(true);
        publish(receiver.position(now()));
        let mut heard: Option<Attempt> = None;
        let mut round = 0u32;
        let mut last_status = Status::Off;
        let mut last_report = Instant::now();
        'on: loop {
            for attempt in gps::attempts() {
                let started = Instant::now();
                let mut uart = gps.open(attempt);
                loop {
                    match select3(uart.read(&mut buf), Timer::after(TICK), TOGGLE.wait()).await {
                        Either3::First(Ok(n)) => {
                            receiver.feed(&buf[..n], now());
                            if heard.is_none() && receiver.heard() {
                                heard = Some(attempt);
                                defmt::info!(
                                    "gps: NMEA at {=u32} baud, module TX on {=str}",
                                    attempt.baud,
                                    rx_pin(attempt)
                                );
                            }
                        }
                        // The ring filled before it was read: bytes were
                        // lost, and the lexer will drop the sentence they
                        // were part of. Not fatal, and worth knowing about.
                        Either3::First(Err(_)) => defmt::warn!("gps: uart overrun"),
                        Either3::Second(()) => {
                            if control.switch(switch.read()) && !control.is_on() {
                                defmt::info!("gps: switch moved to Power ON");
                                break 'on;
                            }
                            if heard.is_none() && started.elapsed().as_millis() > PROBE_WINDOW_MS {
                                let (_, bad) = receiver.counts();
                                defmt::debug!(
                                    "gps: nothing at {=u32} baud on {=str} ({=u32} bad lines)",
                                    attempt.baud,
                                    rx_pin(attempt),
                                    bad
                                );
                                break;
                            }
                        }
                        Either3::Third(()) => {
                            if !control.toggle() {
                                defmt::info!("gps: menu: off");
                                break 'on;
                            }
                        }
                    }
                    report(&receiver, &mut last_status, &mut last_report);
                }
                // Next attempt: the UART is reopened on other terms, and
                // the receiver's counts start again so the probe's question
                // -- anything readable *here*? -- is asked afresh.
                drop(uart);
                receiver.power(true);
            }
            round += 1;
            if round == 1 {
                defmt::warn!("gps: nothing heard on either pin at any rate; still trying");
            }
        }
        defmt::info!("gps: power off");
    }
}
