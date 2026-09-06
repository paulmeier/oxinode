//! oxinode -- RNode-compatible LoRa modem firmware for the muzi.works Base Duo.
//!
//! This crate is the shared support library; the actual firmware images live in
//! `src/bin/`. See the README for what does and does not work yet.

#![no_std]

pub mod board;
pub mod boot;

use core::panic::PanicInfo;

/// Halt on panic.
///
/// There is no debug probe on this board and, until the USB CDC image comes up,
/// no way to report anything either. Halting with interrupts off at least makes
/// the failure obvious (the LED stops, USB drops off the bus) rather than
/// letting a half-initialised radio keep transmitting. Recovery is a double-tap
/// of the reset button into the UF2 bootloader.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    cortex_m::interrupt::disable();
    loop {
        cortex_m::asm::wfi();
    }
}
