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

/// The nRF52840's factory device ID.
///
/// Read-only, set at manufacture, and unique to the chip. It is what the USB
/// serial string is made from, and what phase 6 binds the device hash to so a
/// signature made for one board does not validate another.
pub fn device_id() -> u64 {
    let ficr = embassy_nrf::pac::FICR;
    ((ficr.deviceid(1).read() as u64) << 32) | ficr.deviceid(0).read() as u64
}

/// Claim the device serial string, derived from the nRF52840's factory device
/// ID so the host names the port consistently and two boards never collide.
///
/// **Call this exactly once per image.** It hands out a `StaticCell`, and
/// `init` panics on a second call — which on this board means a USB device that
/// never enumerates. Bind the result once and pass it around.
pub fn take_device_serial() -> &'static str {
    static SERIAL: StaticCell<[u8; 16]> = StaticCell::new();

    let buf = SERIAL.init(oxinode_core::serial::hex_u64(device_id()));
    // `hex_u64` only ever emits ASCII hex digits, which its own tests assert.
    core::str::from_utf8(buf).expect("hex_u64 produced non-ASCII")
}

/// Whether P0.09 and P0.10 are ordinary GPIOs rather than the NFC antenna.
///
/// P0.10 carries `USR_BTN` and the Super IO pad's OK switch, and neither
/// works while the pins belong to the NFC peripheral. Which they are is not a
/// runtime choice: it is `UICR.NFCPINS`, a word in the chip's user information
/// page, and changing it means writing that page rather than setting a
/// register.
///
/// This only *reports*. What writes it, since phase 10, is `embassy_nrf::init`
/// under the `nfc-pins-as-gpio` feature -- the only way `embassy-nrf` names
/// P0.10 at all -- and it does so carefully: a masked word write that clears
/// the `PROTECT` bit and changes nothing else. Flash bits go from 1 to 0
/// without an erase, so the rest of the page, `REGOUT0` included, is never
/// touched; if the bit is already clear the write is a no-op; if it was not,
/// the chip resets once so the change takes. The board shipped running
/// Meshtastic, which builds with `CONFIG_NFCT_PINS_AS_GPIOS`, so the expected
/// answer is `true` before oxinode ever ran. Reading it and saying so is much
/// better than assuming either way, because the failure looks identical to a
/// broken button: the pin simply never changes.
///
/// `PROTECT` is bit 0. Erased flash is all ones, so the factory default is NFC.
pub fn nfc_pins_are_gpio() -> bool {
    embassy_nrf::pac::UICR.nfcpins().read().0 & 1 == 0
}

/// The regulator output voltage `UICR.REGOUT0` selects, in tenths of a volt.
///
/// Reported alongside [`nfc_pins_are_gpio`] because both live in the same
/// erase page. Anything that *erases* that page to rewrite `NFCPINS` has to
/// carry this value across with it: the bootloader programs 3.3 V here, and a
/// page erase that lost it would drop the board to the 1.8 V reset default
/// with the panel's boost converter and the QSPI flash still expecting 3.3.
/// The bit-clearing write `embassy_nrf::init` does is not an erase, and
/// oxinode leaves `Config::dcdc.reg0_voltage` at `None` so it never writes
/// this word either; the log line is there to prove both, every boot.
///
/// `None` when the field holds a reserved encoding.
pub fn regulator_decivolts() -> Option<u8> {
    match embassy_nrf::pac::UICR.regout0().read().0 & 0b111 {
        0 => Some(18),
        1 => Some(21),
        2 => Some(24),
        3 => Some(27),
        4 => Some(30),
        5 => Some(33),
        // 6 is reserved; 7 is the erased default, which means 1.8 V.
        7 => Some(18),
        _ => None,
    }
}

/// Bytes between the end of static data and the stack pointer right now.
///
/// The only honest free-RAM figure on a board with no allocator: everything
/// static is laid out from the bottom of RAM and the stack grows down from
/// the top, so the gap is what the stack has left to grow into. `__sheap` is
/// the linker's name for the end of `.uninit`, which is the top of the static
/// data; the stack pointer is read from the register, which on the main stack
/// this executor runs on is the main stack pointer. The subtraction is in the
/// core, where it is tested not to wrap.
pub fn free_ram_bytes() -> Option<u32> {
    extern "C" {
        static __sheap: u8;
    }
    // Taking the address of a linker symbol is the whole of what this does
    // with it; the symbol is never read, so no `unsafe` is needed.
    let heap_start = core::ptr::addr_of!(__sheap) as u32;
    let stack_pointer = cortex_m::register::msp::read();
    oxinode_core::screens::free_ram(heap_start, stack_pointer)
}
