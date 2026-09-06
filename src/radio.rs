//! The SPI link to the LR1121 (phase 3, step 1).
//!
//! This module owns the bus ([`new_spi`]) and the two control lines `lr11xx`
//! does not manage ([`RadioReset`]). Anything that actually *says* something to
//! the chip belongs to the steps after this one — see `docs/phase-3-radio.md`.
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

use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{P1_10, P1_11, P1_12, P1_13, P1_14, P1_15, SPI2};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{interrupt, pac, Peri};
use embassy_time::{with_timeout, Delay, Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use oxinode_core::gpio::{psel, psel_decode};
use oxinode_core::lr1121::{BusyTrace, BUSY_TIMEOUT_US, RESET_PULSE_US, STARTUP_WINDOW_US};

/// `(port, pin)` for each signal, in the order the datasheet names them.
pub const NSS: (u8, u8) = (1, 12);
/// See [`NSS`].
pub const SCK: (u8, u8) = (1, 13);
/// See [`NSS`].
pub const MOSI: (u8, u8) = (1, 14);
/// See [`NSS`].
pub const MISO: (u8, u8) = (1, 15);
/// See [`NSS`].
pub const NRESET: (u8, u8) = (1, 10);
/// See [`NSS`].
pub const BUSY: (u8, u8) = (1, 11);

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

/// NRESET and BUSY, the two signals `lr11xx` does not manage for us.
///
/// The crate's `Lr11xx::new` takes an SPI device and a BUSY pin and no reset
/// pin at all, so the reset sequence is ours to run — and then the BUSY pin has
/// to be handed over, which is what [`RadioReset::into_busy`] is for.
pub struct RadioReset<'d> {
    nreset: Output<'d>,
    busy: Input<'d>,
}

impl<'d> RadioReset<'d> {
    /// Claim NRESET and BUSY.
    ///
    /// NRESET starts high — released. The LR1121 has already booted by the time
    /// this runs, since it powers up with the board; this only takes ownership
    /// of a line that was previously left alone.
    ///
    /// BUSY is read with no pull. The pin is driven by the LR1121 from inside
    /// the module, so there is nothing to pull against, and adding a pull the
    /// reference design does not have would be inventing a requirement. The
    /// cost is that "we chose the wrong GPIO" reads as noise rather than as a
    /// clean low — which is why `ResetVerdict` names that case explicitly
    /// instead of pretending to have ruled it out.
    pub fn new(nreset: Peri<'d, P1_10>, busy: Peri<'d, P1_11>) -> Self {
        Self {
            nreset: Output::new(nreset, Level::High, OutputDrive::Standard),
            busy: Input::new(busy, Pull::None),
        }
    }

    /// Pulse NRESET and report what BUSY did about it.
    ///
    /// Never hangs. Both waits are bounded, because the failure this is most
    /// likely to meet — BUSY high forever — is precisely the one where the
    /// obvious `wait_for_low().await` never returns and the board goes silent
    /// with no way to ask it why.
    pub async fn cycle(&mut self) -> BusyTrace {
        let before_reset = self.busy.is_high();

        self.nreset.set_low();
        Timer::after(Duration::from_micros(RESET_PULSE_US as u64)).await;
        let during_reset = self.busy.is_high();
        self.nreset.set_high();

        let released = Instant::now();

        // `wait_for_high` returns immediately if the pin is already high, so
        // the common case — BUSY up the moment reset is released — costs
        // nothing. Only a chip that is never busy pays the window.
        let rose = with_timeout(
            Duration::from_micros(STARTUP_WINDOW_US as u64),
            self.busy.wait_for_high(),
        )
        .await
        .is_ok();

        // Likewise, this returns immediately if BUSY is already low. That is
        // the trap: on its own it cannot tell a ready chip from a pin stuck
        // low, which is why `rose` is carried alongside rather than discarded.
        let fell_after_us = with_timeout(
            Duration::from_micros(BUSY_TIMEOUT_US as u64),
            self.busy.wait_for_low(),
        )
        .await
        .ok()
        .map(|()| released.elapsed().as_micros() as u32);

        BusyTrace {
            before_reset,
            during_reset,
            rose,
            fell_after_us,
        }
    }

    /// The current BUSY level, for sampling outside a reset cycle.
    pub fn busy_is_high(&self) -> bool {
        self.busy.is_high()
    }

    /// Hand BUSY to `lr11xx`, which owns it from step 3 onwards.
    ///
    /// This drops NRESET as an `Output`, which returns the pin to its reset
    /// state — an input. The LR1121 holds itself out of reset from there, the
    /// same way it did before this firmware ever touched the line.
    pub fn into_busy(self) -> Input<'d> {
        self.busy
    }
}

/// `lr11xx::Lr11xx::new` takes an async `SpiDevice<u8>`. Proving that here means
/// step 2 cannot discover a trait mismatch after the wiring is already written.
const fn _assert_spi_device<S: embedded_hal_async::spi::SpiDevice<u8>>() {}
const _: () = _assert_spi_device::<RadioSpi<'static>>();

/// The same, for the BUSY pin: `Lr11xx` wants `InputPin + Wait`.
const fn _assert_busy_pin<
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
>() {
}
const _: () = _assert_busy_pin::<Input<'static>>();
