//! Phase 1: prove the toolchain, the linker script and the UF2 flashing path.
//!
//! Blinks the green user LED (P1.03). If this runs, then the image is linked at
//! the right offset, the SoftDevice hands control over as expected, the
//! external 32.768 kHz crystal is running (otherwise `embassy-time` stalls and
//! the LED freezes), and `cargo run` can put a new image on the board.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use oxinode::board::{self, Led};
use oxinode::boot;

/// Deliberately asymmetric: a short flash every second is unmistakably "my
/// firmware is running" and is hard to confuse with the bootloader's own
/// LED behaviour or with a board that is simply stuck.
const ON: Duration = Duration::from_millis(60);
const OFF: Duration = Duration::from_millis(940);

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    let mut led = Led::new(p.P1_03);

    loop {
        led.on();
        Timer::after(ON).await;
        led.off();
        Timer::after(OFF).await;
    }
}
