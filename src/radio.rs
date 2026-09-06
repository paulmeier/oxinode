//! The SPI link to the LR1121 (phase 3, step 1).
//!
//! This module only builds the bus. Reset, BUSY and anything that talks to the
//! chip belong to the steps after this one — see `docs/phase-3-radio.md`.
//!
//! # There is nothing to probe
//!
//! U1 on this board is an Elecrow nRFLR1121 module: the nRF52840 and the LR1121
//! are two dies inside one package, and all four SPI signals are routed
//! *inside* it. The pin numbers below are still that MCU's own GPIOs and still
//! correct, but they never reach board copper, so no oscilloscope, logic
//! analyser or continuity check can be brought to bear on any of this.
//!
//! That is why [`check_pin_selection`] exists. Reading the peripheral's `PSEL`
//! registers back is the only external evidence available at this step that the
//! SPIM claimed the pins we meant rather than four others — and transposing
//! MISO and MOSI is easy to do, because [`Spim::new`] takes them in that order.

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{P1_12, P1_13, P1_14, P1_15, SPI2};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{interrupt, pac, Peri};
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;
use oxinode_core::gpio::{psel, psel_decode};

/// `(port, pin)` for each signal, in the order the datasheet names them.
pub const NSS: (u8, u8) = (1, 12);
/// See [`NSS`].
pub const SCK: (u8, u8) = (1, 13);
/// See [`NSS`].
pub const MOSI: (u8, u8) = (1, 14);
/// See [`NSS`].
pub const MISO: (u8, u8) = (1, 15);

/// 1 MHz to start with.
///
/// The LR1121 will take far more than this and the link is a few millimetres of
/// substrate, so signal integrity is not the reason to be conservative —
/// there is simply no benefit to going faster before anything works, and a
/// slow clock removes one variable from a debug session that has no probe.
pub const FREQUENCY: spim::Frequency = spim::Frequency::M1;

/// The SPI bus with chip-select attached, in the shape `lr11xx` asks for.
pub type RadioSpi<'d> = ExclusiveDevice<Spim<'d>, Output<'d>, Delay>;

/// Bring up the SPIM and wrap it with chip-select.
///
/// SPIM2 rather than SPIM3: SPIM3 is the only instance that reaches 32 MHz, and
/// it is also the one carrying an nRF52840 EasyDMA anomaly around CPU writes to
/// the RAM holding an in-flight TX buffer — believed to be anomaly 198, though
/// that number has not been checked against the errata sheet here. `embassy-nrf`
/// implements no workaround for it either way. At 1 MHz nothing SPIM3 offers is
/// worth having, so the quiet instance is the better trade; it also leaves both
/// TWI/SPI-shared instances free for the phase-7 display.
pub fn new_spi<'d>(
    spim: Peri<'d, SPI2>,
    irq: impl Binding<interrupt::typelevel::SPI2, spim::InterruptHandler<SPI2>> + 'd,
    sck: Peri<'d, P1_13>,
    miso: Peri<'d, P1_15>,
    mosi: Peri<'d, P1_14>,
    nss: Peri<'d, P1_12>,
) -> RadioSpi<'d> {
    let mut config = spim::Config::default();
    config.frequency = FREQUENCY;
    // Mode 0 and MSB-first are what the LR1121 expects. Both happen to be the
    // `embassy-nrf` defaults; stated anyway, because a default that changes
    // underneath us would show up as a radio that never answers.
    config.mode = spim::MODE_0;
    config.bit_order = spim::BitOrder::MsbFirst;
    // `lr11xx` documents that MOSI must be held low during a read. On SPIM the
    // byte sent when the TX buffer runs short is this one, so this is that
    // requirement, spelled as a register value.
    config.orc = 0x00;

    let spi = Spim::new(spim, irq, sck, miso, mosi, config);

    // Idle high: the LR1121 latches on the falling edge of NSS.
    let cs = Output::new(nss, Level::High, OutputDrive::Standard);

    // The error type here is `Infallible` -- driving a GPIO cannot fail.
    ExclusiveDevice::new(spi, cs, Delay).unwrap()
}

/// Read SPIM2's `PSEL` registers back and check they name the pins we asked
/// for, logging whatever was actually found.
///
/// Call this *after* [`new_spi`]; before it the registers hold reset values and
/// this will report every signal as disconnected.
///
/// This proves the peripheral claimed the right MCU pins. It proves nothing
/// whatsoever about the LR1121 on the other end — that starts at step 3.
pub fn check_pin_selection() -> bool {
    let r = pac::SPIM2;
    let mut ok = true;
    for (name, want, got) in [
        ("SCK", SCK, r.psel().sck().read().0),
        ("MISO", MISO, r.psel().miso().read().0),
        ("MOSI", MOSI, r.psel().mosi().read().0),
    ] {
        match psel_decode(got) {
            Some((port, pin)) if (port, pin) == want => {
                defmt::info!("spi {=str}: P{=u8}.{=u8}", name, port, pin);
            }
            Some((port, pin)) => {
                defmt::error!(
                    "spi {=str}: peripheral has P{=u8}.{=u8}, expected P{=u8}.{=u8}",
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
                    "spi {=str}: disconnected (PSEL {=u32:#010x}), expected P{=u8}.{=u8}",
                    name,
                    got,
                    want.0,
                    want.1
                );
                ok = false;
            }
        }
    }
    // NSS is a plain GPIO, not a SPIM signal, so it has no PSEL to read back.
    // Its number is checked on the host instead, in `oxinode_core::gpio`.
    defmt::info!("spi NSS: P{=u8}.{=u8} (GPIO)", NSS.0, NSS.1);
    let _ = psel(NSS.0, NSS.1);
    ok
}

/// `lr11xx::Lr11xx::new` takes an async `SpiDevice<u8>`. Proving that here means
/// step 2 cannot discover a trait mismatch after the wiring is already written.
const fn _assert_spi_device<S: embedded_hal_async::spi::SpiDevice<u8>>() {}
const _: () = _assert_spi_device::<RadioSpi<'static>>();
