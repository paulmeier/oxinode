//! The I²C link to the Super IO board's OLED (phase 7, step 1).
//!
//! Three pins and a rail. `SDA` and `SCL` reach the panel through the Base
//! Duo's castellations, and **P0.23 enables a 12 V boost converter** that the
//! panel needs to light at all — a low pin gives a display that answers on the
//! bus and shows nothing, which is the most confusing failure this board has to
//! offer.
//!
//! # What can be proved at this step
//!
//! Unlike the radio, this bus *does* reach copper, and unlike the radio the
//! device at the far end has an address rather than a chip ID. So the evidence
//! available here is better than phase 3's:
//!
//! * the peripheral's own `PSEL` registers say which pins it claimed —
//!   [`check_pin_selection`], the same trick `radio` uses;
//! * an address scan says what is actually out there, and how many of them.
//!   Nothing about the display's *geometry* can be discovered this way; see
//!   `docs/phase-7-display.md` for why that needs a person looking at it.

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{P0_23, P0_24, P0_25, TWISPI0};
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{pac, Peri};
use embassy_time::{with_timeout, Duration, Timer};
use oxinode_core::gpio::psel_decode;

/// `(port, pin)` for each signal, from the board's Meshtastic variant and
/// cross-checked against the Rev 01 schematic.
pub const SDA: (u8, u8) = (0, 24);
/// See [`SDA`].
pub const SCL: (u8, u8) = (0, 25);
/// The 12 V boost enable. **Active high**, and the panel is dark without it.
pub const BOOST_EN: (u8, u8) = (0, 23);

/// 100 kHz.
///
/// Standard mode, and the same reasoning as the radio's 1 MHz SPI: there is no
/// benefit to going faster before anything works, and a slow clock removes a
/// variable from a debug session with no logic analyser on it. A full 1024-byte
/// frame at 100 kHz costs about 92 ms, which is why [`crate::display`] will
/// want 400 kHz once the panel is proven — but not before.
pub const FREQUENCY: twim::Frequency = twim::Frequency::K100;

/// Addresses an SH1107 can be strapped to. Which one is a board choice, so it
/// is discovered rather than assumed — see [`scan`].
pub const SH1107_ADDRESSES: [u8; 2] = [0x3C, 0x3D];

/// How long any one bus transaction may take before it is called a failure.
///
/// A device that holds SCL low never releases the peripheral, and an `await`
/// that can hang forever in the middle of bring-up is the difference between a
/// diagnostic and a brick.
const TRANSACTION_TIMEOUT: Duration = Duration::from_millis(50);

/// The 12 V rail that lights the panel.
///
/// Held as a value rather than set and forgotten, because dropping it would
/// return the pin to an input and put the panel out — so the owner of the
/// display has to own this too.
pub struct Boost<'d> {
    pin: Output<'d>,
}

impl<'d> Boost<'d> {
    /// Claim P0.23, initially **off**.
    pub fn new(pin: Peri<'d, P0_23>) -> Self {
        Self {
            pin: Output::new(pin, Level::Low, OutputDrive::Standard),
        }
    }

    /// Turn the rail on and give the converter time to come up.
    ///
    /// The wait is not from a datasheet — there is no boost part named in the
    /// sources this project has — so it is deliberately generous. A panel
    /// addressed before its supply is stable answers on the bus and then shows
    /// nothing, which looks exactly like a driver bug.
    pub async fn on(&mut self) {
        self.pin.set_high();
        Timer::after(Duration::from_millis(50)).await;
    }

    pub fn off(&mut self) {
        self.pin.set_low();
    }

    pub fn is_on(&self) -> bool {
        self.pin.is_set_high()
    }
}

/// Bring up the TWIM on the display bus.
///
/// The board carries 5.1 kΩ pull-ups on both lines, so the internal ones are
/// left off: enabling them in parallel would drop the effective pull-up to
/// about 4.3 kΩ, which is still legal but is a change to the bus nobody asked
/// for.
pub fn new_i2c<'d>(
    twim: Peri<'d, TWISPI0>,
    irq: impl Binding<embassy_nrf::interrupt::typelevel::TWISPI0, twim::InterruptHandler<TWISPI0>> + 'd,
    sda: Peri<'d, P0_24>,
    scl: Peri<'d, P0_25>,
    ram_buffer: &'d mut [u8],
) -> Twim<'d> {
    let mut config = twim::Config::default();
    config.frequency = FREQUENCY;
    config.sda_pullup = false;
    config.scl_pullup = false;
    Twim::new(twim, irq, sda, scl, config, ram_buffer)
}

/// Read the peripheral's `PSEL` registers back and report what it claimed.
///
/// Call *after* [`new_i2c`]. Cheaper than it looks: SDA and SCL are easy to
/// transpose, and a transposed I²C bus fails by every address NACKing, which
/// is indistinguishable from "the expansion board is not plugged in".
pub fn check_pin_selection() -> bool {
    let r = pac::TWIM0;
    let mut ok = true;
    for (name, want, got) in [
        ("SDA", SDA, r.psel().sda().read().0),
        ("SCL", SCL, r.psel().scl().read().0),
    ] {
        match psel_decode(got) {
            Some((port, pin)) if (port, pin) == want => {
                defmt::info!("i2c {=str}: P{=u8}.{=u8}", name, port, pin);
            }
            Some((port, pin)) => {
                defmt::error!(
                    "i2c {=str}: peripheral has P{=u8}.{=u8}, expected P{=u8}.{=u8}",
                    name,
                    port,
                    pin,
                    want.0,
                    want.1
                );
                ok = false;
            }
            None => {
                defmt::error!(
                    "i2c {=str}: disconnected (PSEL {=u32:#010x}), expected P{=u8}.{=u8}",
                    name,
                    got,
                    want.0,
                    want.1
                );
                ok = false;
            }
        }
    }
    ok
}

/// What a scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct Scan {
    /// How many addresses answered.
    pub found: usize,
    /// The first address that answered, if any.
    pub first: Option<u8>,
    /// The first address that answered *and* is one an SH1107 can use.
    pub display: Option<u8>,
}

/// Walk the 7-bit address space and report who answers.
///
/// A one-byte read rather than a zero-length write: the nRF52's TWIM will not
/// start a transaction with an empty buffer, and a device that is present
/// answers the address byte whatever comes after it. An SH1107 read returns its
/// status register, which is harmless and is thrown away.
///
/// The reserved ranges at both ends are skipped, because addressing them is not
/// a question with a meaningful answer.
pub async fn scan(i2c: &mut Twim<'_>) -> Scan {
    let mut result = Scan {
        found: 0,
        first: None,
        display: None,
    };
    for address in 0x08..=0x77u8 {
        let mut byte = [0u8; 1];
        let answered = matches!(
            with_timeout(TRANSACTION_TIMEOUT, i2c.read(address, &mut byte)).await,
            Ok(Ok(()))
        );
        if answered {
            defmt::info!("i2c: {=u8:#04x} answered", address);
            result.found += 1;
            if result.first.is_none() {
                result.first = Some(address);
            }
            if result.display.is_none() && SH1107_ADDRESSES.contains(&address) {
                result.display = Some(address);
            }
        }
    }
    match (result.found, result.display) {
        (0, _) => defmt::error!(
            "i2c: nothing on the bus. Is the Super IO board attached, and P0.23 high?"
        ),
        (n, Some(addr)) => defmt::info!("i2c: {=usize} device(s); display at {=u8:#04x}", n, addr),
        (n, None) => defmt::warn!(
            "i2c: {=usize} device(s), none at an SH1107 address ({=u8:#04x} or {=u8:#04x})",
            n,
            SH1107_ADDRESSES[0],
            SH1107_ADDRESSES[1]
        ),
    }
    result
}
