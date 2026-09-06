//! Board facts for the muzi.works Base Duo (nRF52840).
//!
//! Pin assignments are taken from the board's own Meshtastic variant definition
//! (`variants/nrf52840/muzi_base/variant.h`). Pin numbers are hardware facts,
//! not borrowed code.
//!
//! Only what the current phase actually drives is declared here. Radio, flash,
//! display, GPS and IMU pins are documented in the README until the phase that
//! needs them arrives -- an unused `const` is just a comment that can go stale
//! without anyone noticing.

use embassy_nrf::config::{Config, HfclkSource, LfclkSource};
use embassy_nrf::gpio::{Level, Output, OutputDrive, Pin};
use embassy_nrf::Peri;
use static_cell::StaticCell;

/// Both user LEDs are wired active low (`LED_STATE_ON 0` upstream): the pin
/// sinks current, so driving it low lights the LED.
pub const LED_ON: Level = Level::Low;
/// See [`LED_ON`].
pub const LED_OFF: Level = Level::High;

/// Standard `embassy-nrf` init config for this board.
///
/// Both oscillators are external parts on the Base Duo. The 32.768 kHz crystal
/// (`USE_LFXO` upstream) is what makes `embassy-time` accurate enough for LoRa
/// airtime accounting later on; the internal RC would drift by a couple of
/// percent. The 32 MHz HFXO is mandatory for USB, and asking for it here rather
/// than per-image keeps the two firmware binaries behaving identically.
pub fn embassy_config() -> Config {
    let mut config = Config::default();
    config.lfclk_source = LfclkSource::ExternalXtal;
    config.hfclk_source = HfclkSource::ExternalXtal;
    config
}

/// An active-low user LED.
pub struct Led<'d> {
    pin: Output<'d>,
}

impl<'d> Led<'d> {
    /// Claim a pin as an LED, initially off.
    pub fn new(pin: Peri<'d, impl Pin>) -> Self {
        Self {
            pin: Output::new(pin, LED_OFF, OutputDrive::Standard),
        }
    }

    pub fn on(&mut self) {
        self.pin.set_level(LED_ON);
    }

    pub fn off(&mut self) {
        self.pin.set_level(LED_OFF);
    }

    pub fn toggle(&mut self) {
        self.pin.toggle();
    }
}

/// Claim the device serial string, derived from the nRF52840's factory device
/// ID so the host names the port consistently and two boards never collide.
///
/// **Call this exactly once per image.** It hands out a `StaticCell`, and
/// `init` panics on a second call — which on this board means a USB device that
/// never enumerates. Bind the result once and pass it around.
pub fn take_device_serial() -> &'static str {
    static SERIAL: StaticCell<[u8; 16]> = StaticCell::new();

    let ficr = embassy_nrf::pac::FICR;
    let id = ((ficr.deviceid(1).read() as u64) << 32) | ficr.deviceid(0).read() as u64;

    let buf = SERIAL.init(oxinode_core::serial::hex_u64(id));
    // `hex_u64` only ever emits ASCII hex digits, which its own tests assert.
    core::str::from_utf8(buf).expect("hex_u64 produced non-ASCII")
}
