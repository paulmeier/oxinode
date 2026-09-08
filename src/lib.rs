//! oxinode -- RNode-compatible LoRa modem firmware for the muzi.works Base Duo.
//!
//! This crate is the shared support library; the actual firmware images live in
//! `src/bin/`. See the README for what does and does not work yet.

#![no_std]

// Exactly one `critical-section` implementation may be linked, and picking the
// wrong one for the build is not a link error -- it is a Bluetooth stack that
// loses connections for no visible reason. See `[features]` in Cargo.toml.
#[cfg(all(feature = "ble", feature = "cs-single-core"))]
compile_error!(
    "a `ble` build must not use the single-core critical section: build with \
     --no-default-features --features ble"
);
#[cfg(not(any(feature = "ble", feature = "cs-single-core")))]
compile_error!("no critical-section implementation selected: enable `ble` or `cs-single-core`");

pub mod battery;
#[cfg(feature = "ble")]
pub mod ble;
pub mod board;
pub mod boot;
pub mod bringup;
pub mod display;
pub mod logger;
pub mod modem;
#[cfg(feature = "ble")]
pub mod nus;
pub mod pad;
pub mod radio;
pub mod store;
pub mod usb_log;

use core::panic::PanicInfo;

/// Reboot into the UF2 bootloader on panic.
///
/// The obvious thing is to halt, and that is what this did first. It is the
/// wrong choice on this board: a halted image is a USB device that never
/// enumerates, so the only way back is physically double-tapping the reset
/// button. With no debug probe, a panic in a headless test is otherwise just a
/// board that went quiet, and every one costs a trip to the bench.
///
/// Rebooting into the bootloader instead leaves the board sitting in DFU,
/// enumerated and ready to be reflashed over serial — recoverable from the
/// keyboard. It is a stable resting place, not a loop: the bootloader stays put
/// rather than re-running the image that just failed.
///
/// The cost is that the panic message goes with it. Nothing would have survived
/// the reset to read it anyway, and the alternative was a board that says
/// nothing *and* cannot be reflashed without getting up.
///
/// Worth revisiting before anything ships: a fielded RNode should probably
/// reset into its application and keep trying rather than wait in a bootloader.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    crate::boot::reboot_to_bootloader()
}
