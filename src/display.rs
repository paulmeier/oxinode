//! The I²C link to the Super IO board's OLED.
//!
//! Three pins and a rail. `SDA` and `SCL` reach the panel through the Base
//! Duo's castellations, and **P0.23 enables a 12 V boost converter** that the
//! panel needs to light at all — a low pin gives a display that answers on the
//! bus and shows nothing, which is the most confusing failure this board has to
//! offer.
//!
//! # What can be proved here
//!
//! Unlike the radio, this bus *does* reach copper, and unlike the radio the
//! device at the far end has an address rather than a chip ID. So the evidence
//! available here is better than the radio's:
//!
//! * the peripheral's own `PSEL` registers say which pins it claimed —
//!   [`check_pin_selection`], the same trick `radio` uses;
//! * an address scan says what is actually out there, and how many of them.
//!   Nothing about the display's *geometry* can be discovered this way; see
//!   `docs/hardware/display.md` for why that needs a person looking at it.

use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{P0_23, P0_24, P0_25, TWISPI0};
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{pac, Peri};
use embassy_time::{with_timeout, Duration, Timer};
use oxinode_core::gpio::psel_decode;
use oxinode_core::sh1107;

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

/// What the two bus lines are doing, before any peripheral drives them.
///
/// This is the cheapest and most direct thing that can be asked of an I²C bus
/// and it should have been the first: both lines idle high through their
/// pull-ups when nothing is talking. **SDA low with SCL high is a stuck bus** —
/// a device part way through a read, holding the line until it gets more
/// clocks — and in that state every transaction fails with no clue as to why.
///
/// Read with the internal pull-ups *off*, so a high reading is the board's own
/// 5.1 kΩ resistors and not ours. If the expansion board is not attached, both
/// lines float and this reports whatever the pin last saw.
pub fn line_levels(sda: Peri<'_, P0_24>, scl: Peri<'_, P0_25>) -> (bool, bool) {
    let sda = Input::new(sda, Pull::None);
    let scl = Input::new(scl, Pull::None);
    (sda.is_high(), scl.is_high())
}

/// Free a bus whose SDA is being held low, by clocking the holder out of it.
///
/// The standard recovery: pulse SCL up to nine times with SDA released, which
/// lets whatever is mid-byte finish its transfer and release the line, then
/// issue a STOP. Returns whether SDA came back up.
///
/// Nine because that is one byte plus the acknowledge bit — the longest a
/// device can be waiting.
pub async fn recover_bus(sda: Peri<'_, P0_24>, scl: Peri<'_, P0_25>) -> bool {
    let sda_in = Input::new(sda, Pull::None);
    // Open-drain, so the "high" half of each pulse is the board's pull-up
    // doing the work rather than us fighting whatever is holding the line.
    let mut clock = Output::new(scl, Level::High, OutputDrive::Standard0Disconnect1);
    for _ in 0..9 {
        if sda_in.is_high() {
            break;
        }
        clock.set_low();
        Timer::after(Duration::from_micros(5)).await;
        clock.set_high();
        Timer::after(Duration::from_micros(5)).await;
    }
    sda_in.is_high()
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

/// Disable and re-enable the TWIM, leaving its pin and frequency settings alone.
///
/// The nRF52's TWIM can be left in a state it does not come out of after a
/// transaction that ends in a NACK, and `embassy-nrf` implements no workaround
/// for it. The symptom is not an error — it is *inconsistency*: the same
/// address answers a read one moment and not the next, a write succeeds and
/// then the identical write NACKs, and a bus scan disagrees with a probe run
/// half a millisecond earlier. All three of those happened here before this
/// existed, and between them they had the display at 0x3d, then at 0x3c, then
/// nowhere.
///
/// Cycling `ENABLE` is enough: `PSEL` and `FREQUENCY` are separate registers
/// and survive it. A scan NACKs a hundred times by design, so it does this
/// after every one.
pub fn reset_peripheral() {
    let r = pac::TWIM0;
    r.enable()
        .write(|w| w.set_enable(pac::twim::vals::Enable::Disabled));
    // The peripheral needs the write to land before it is turned back on.
    cortex_m::asm::dsb();
    r.enable()
        .write(|w| w.set_enable(pac::twim::vals::Enable::Enabled));
    cortex_m::asm::dsb();
}

/// A short name for a bus error, for the log.
///
/// The difference matters more than it looks: an address NACK is nobody home,
/// a data NACK is somebody who stopped listening, and anything else is the
/// peripheral itself objecting — which is not a statement about the bus at all.
pub const fn error_name(e: &twim::Error) -> &'static str {
    match e {
        twim::Error::AddressNack => "address-nack",
        twim::Error::DataNack => "data-nack",
        twim::Error::Overrun => "overrun",
        twim::Error::Transmit => "transmit",
        twim::Error::Receive => "receive",
        twim::Error::Timeout => "timeout",
        twim::Error::TxBufferTooLong => "tx-too-long",
        twim::Error::RxBufferTooLong => "rx-too-long",
        twim::Error::RAMBufferTooSmall => "ram-buffer-too-small",
        // The enum is marked non-exhaustive upstream, so this arm is required
        // and is not dead code: a new variant would otherwise stop this
        // building on a routine dependency bump.
        _ => "unknown",
    }
}

/// Ask one address one question, and say exactly what came back.
///
/// The peripheral is cycled first, so the answer is about the bus rather than
/// about whatever the previous transaction left behind. See
/// [`reset_peripheral`].
pub async fn probe(i2c: &mut Twim<'_>, address: u8, write: bool) -> &'static str {
    reset_peripheral();
    let mut byte = [0u8; 1];
    let outcome = if write {
        with_timeout(
            TRANSACTION_TIMEOUT,
            i2c.write(address, &[sh1107::control::COMMANDS]),
        )
        .await
    } else {
        with_timeout(TRANSACTION_TIMEOUT, i2c.read(address, &mut byte)).await
    };
    match outcome {
        Ok(Ok(())) => "ok",
        Ok(Err(e)) => error_name(&e),
        Err(_) => "host-timeout",
    }
}

/// What one address did when it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct Answer {
    pub address: u8,
    /// It acknowledged a one-byte read.
    pub reads: bool,
    /// It acknowledged a one-byte write.
    pub writes: bool,
}

/// What a scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct Scan {
    /// How many addresses answered either way.
    pub found: usize,
    /// The first address that answered a **write** and is one an SH1107 uses.
    ///
    /// Writes, because that is the direction a display is driven in. An
    /// address that only answers reads is not somewhere commands can be sent.
    pub display: Option<u8>,
}

/// Walk the 7-bit address space and report who answers, in both directions.
///
/// **Both directions, and that is not thoroughness for its own sake.** The
/// first version of this scanned with reads alone, found one device, and then
/// failed on the first command with an address NACK — because a great many
/// OLED modules implement writes only. A read-only scan asks a question whose
/// answer does not predict the one that matters.
///
/// The write is a single control byte (`0x00`, "commands follow") with no
/// commands after it: a no-op for an SH1107, and one byte with no data for
/// anything else that might be listening.
///
/// The reserved ranges at both ends are skipped, because addressing them is not
/// a question with a meaningful answer.
pub async fn scan(i2c: &mut Twim<'_>) -> Scan {
    let mut result = Scan {
        found: 0,
        display: None,
    };
    for address in 0x08..=0x77u8 {
        let mut byte = [0u8; 1];
        reset_peripheral();
        let reads = matches!(
            with_timeout(TRANSACTION_TIMEOUT, i2c.read(address, &mut byte)).await,
            Ok(Ok(()))
        );
        reset_peripheral();
        let writes = matches!(
            with_timeout(
                TRANSACTION_TIMEOUT,
                i2c.write(address, &[sh1107::control::COMMANDS])
            )
            .await,
            Ok(Ok(()))
        );
        if !reads && !writes {
            continue;
        }
        defmt::info!(
            "i2c: {=u8:#04x} answered -- read {=bool}, write {=bool}",
            address,
            reads,
            writes
        );
        result.found += 1;
        if result.display.is_none() && writes && SH1107_ADDRESSES.contains(&address) {
            result.display = Some(address);
        }
    }
    match (result.found, result.display) {
        (0, _) => defmt::error!(
            "i2c: nothing on the bus. Is the Super IO board attached, and P0.23 high?"
        ),
        (n, Some(addr)) => defmt::info!(
            "i2c: {=usize} device(s); display accepts writes at {=u8:#04x}",
            n,
            addr
        ),
        (n, None) => defmt::warn!(
            "i2c: {=usize} device(s), but none accepts a write at {=u8:#04x} or {=u8:#04x}",
            n,
            SH1107_ADDRESSES[0],
            SH1107_ADDRESSES[1]
        ),
    }
    result
}

/// Why a transfer to the panel failed.
///
/// `embassy-nrf`'s own error type does not implement `defmt::Format` unless
/// that crate's `defmt` feature is on, which would add formatting for every
/// type it has. These are the four cases that mean different things here, and
/// the distinction is worth keeping: an address NACK is a panel that has gone
/// away, a data NACK is one that stopped mid-transfer, and a timeout is a bus
/// that is being held down by something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum PanelError {
    /// Nobody acknowledged the address.
    NoPanel,
    /// The panel stopped acknowledging part way through a transfer.
    Truncated,
    /// The transfer did not finish in time.
    Timeout,
    /// Anything else the peripheral reported.
    Bus,
}

impl From<twim::Error> for PanelError {
    fn from(e: twim::Error) -> Self {
        match e {
            twim::Error::AddressNack => Self::NoPanel,
            twim::Error::DataNack => Self::Truncated,
            twim::Error::Timeout => Self::Timeout,
            _ => Self::Bus,
        }
    }
}

/// The panel itself: an SH1107 at a known address, on a bus we own.
///
/// Everything about *what* to say lives in [`oxinode_core::sh1107`]; this is
/// the part that has to touch a peripheral, which is also the part that cannot
/// be unit tested. It is kept as thin as that division allows.
pub struct Panel<'d> {
    i2c: Twim<'d>,
    address: u8,
    /// One page plus its control byte. Assembled here rather than passed in,
    /// because EasyDMA needs one contiguous buffer in RAM and the alternative
    /// is a write per byte.
    scratch: [u8; 1 + sh1107::COLUMNS],
}

impl<'d> Panel<'d> {
    pub fn new(i2c: Twim<'d>, address: u8) -> Self {
        Self {
            i2c,
            address,
            scratch: [0; 1 + sh1107::COLUMNS],
        }
    }

    /// The address this panel answered on, for the log.
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Send a run of command bytes.
    ///
    /// One transfer with a single leading control byte, which the datasheet's
    /// Figure 9 allows: with the continuation bit clear, everything after it is
    /// of the same kind. A control byte per command would triple the traffic.
    pub async fn commands(&mut self, commands: &[u8]) -> Result<(), PanelError> {
        // Staged through RAM so the transfer is one DMA rather than the
        // driver's flash-to-RAM copy path.
        self.scratch[0] = sh1107::control::COMMANDS;
        self.scratch[1..1 + commands.len()].copy_from_slice(commands);
        match with_timeout(
            TRANSACTION_TIMEOUT,
            self.i2c
                .write(self.address, &self.scratch[..1 + commands.len()]),
        )
        .await
        {
            Ok(result) => result.map_err(PanelError::from),
            Err(_) => Err(PanelError::Timeout),
        }
    }

    /// Configure the controller and switch the panel on.
    pub async fn init(&mut self, contrast: u8) -> Result<(), PanelError> {
        let mut buf = [0u8; sh1107::INIT_LEN];
        let sequence = sh1107::init_sequence(contrast, &mut buf);
        self.commands(sequence).await
    }

    /// Send every page that has changed, and mark it sent.
    ///
    /// Page by page rather than in one transfer, because the write cursor has
    /// to be repositioned between pages: in page addressing mode the column
    /// address wraps back to zero at the end of a page and the *page* address
    /// stays where it was, so a continuous stream would rewrite one page
    /// sixteen times.
    pub async fn flush(&mut self, frame: &mut sh1107::Frame) -> Result<(), PanelError> {
        self.flush_pages(frame, sh1107::PAGES).await
    }

    /// Send at most `max` dirty pages, and say whether any are left.
    ///
    /// This is what keeps the panel from blocking the radio. A full repaint is
    /// 218 ms on this bus, and a modem that stopped servicing its interrupt for
    /// that long to redraw a status line would be trading the thing it is for
    /// the thing it shows. One page is 14 ms, and a caller that sends a couple
    /// per pass finishes a whole screen in under half a second without ever
    /// being away for long.
    pub async fn flush_pages(
        &mut self,
        frame: &mut sh1107::Frame,
        max: usize,
    ) -> Result<(), PanelError> {
        let mut sent = 0;
        for page in 0..sh1107::PAGES {
            if !frame.is_dirty(page) {
                continue;
            }
            if sent == max {
                break;
            }
            sent += 1;
            self.commands(&sh1107::page_cursor(page)).await?;

            self.scratch[0] = sh1107::control::DATA;
            self.scratch[1..].copy_from_slice(frame.page(page));
            match with_timeout(
                TRANSACTION_TIMEOUT,
                self.i2c.write(self.address, &self.scratch),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    reset_peripheral();
                    return Err(e.into());
                }
                Err(_) => {
                    reset_peripheral();
                    return Err(PanelError::Timeout);
                }
            }
            frame.mark_sent(page);
        }
        Ok(())
    }

    /// Light every pixel regardless of RAM, or stop doing so.
    ///
    /// The one test that separates "the panel is dead" from "the driver is
    /// writing the wrong bytes": it needs no RAM to be correct, only power and
    /// an address.
    pub async fn all_on(&mut self, on: bool) -> Result<(), PanelError> {
        let command = if on {
            sh1107::cmd::ENTIRE_DISPLAY_ON
        } else {
            sh1107::cmd::ENTIRE_DISPLAY_OFF
        };
        self.commands(&[command]).await
    }

    /// Set the contrast, which is the only display parameter a host can change.
    pub async fn contrast(&mut self, value: u8) -> Result<(), PanelError> {
        self.commands(&[sh1107::cmd::CONTRAST, value]).await
    }

    /// Blank the panel without losing what is in its RAM.
    pub async fn power(&mut self, on: bool) -> Result<(), PanelError> {
        let command = if on {
            sh1107::cmd::DISPLAY_ON
        } else {
            sh1107::cmd::DISPLAY_OFF
        };
        self.commands(&[command]).await
    }
}
