//! Things that have to happen because we boot from behind a SoftDevice and a
//! UF2 bootloader rather than from address zero.

// Re-exported so images can report where they were linked; a mismatch with
// what the bootloader expects is otherwise invisible until nothing boots.
include!(concat!(env!("OUT_DIR"), "/app_flash_origin.rs"));

/// Point the CPU's vector table at *our* vector table.
///
/// On reset the MBR runs first, then hands over to the S140 SoftDevice at
/// 0x1000, which -- because we never enable it -- forwards to the application
/// at [`APP_FLASH_ORIGIN`]. `cortex-m-rt` does not touch VTOR, so unless we set
/// it, VTOR is left wherever the MBR/SoftDevice put it and interrupts may
/// dispatch through someone else's table. Setting it explicitly is one store
/// and is correct whether or not the SoftDevice already did it for us.
///
/// Call this as the first statement of `main`, before `embassy_nrf::init`
/// enables any interrupt.
///
/// # Safety
/// Safe to call once, early, with interrupts disabled or none yet enabled.
/// Calling it after interrupts are live races with dispatch.
pub fn relocate_vector_table() {
    // SAFETY: VTOR is a plain write-any-time register; the constraint is
    // temporal (see above), not memory safety. `SCB::PTR` is a valid MMIO
    // pointer on every Cortex-M.
    unsafe {
        (*cortex_m::peripheral::SCB::PTR)
            .vtor
            .write(APP_FLASH_ORIGIN);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Restart the application.
///
/// What `CMD_RESET` asks for. Distinct from [`reboot_to_bootloader`]: this
/// comes back up running oxinode, having left `GPREGRET` alone. The host
/// expects the serial port to disappear and re-enumerate, and says so — it
/// closes the port, waits, and finds the device again by its USB serial
/// number.
pub fn reboot() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}

/// Magic value the Adafruit nRF52 bootloader looks for in `GPREGRET` to stay in
/// UF2 mass-storage mode instead of booting the application.
///
/// Verified on hardware: a 1200-baud open/close on the USB serial port brings
/// the board up in its bootloader.
const DFU_MAGIC_UF2_RESET: u32 = 0x57;

/// Reset into the UF2 bootloader, so a new image can be flashed without
/// physically double-tapping the reset button.
///
/// `GPREGRET` is one of the few registers that survives a soft reset, which is
/// exactly why the bootloader uses it as a mailbox. We can write it directly
/// because the SoftDevice is disabled; if it were enabled this would have to go
/// through `sd_power_gpregret_set`.
pub fn reboot_to_bootloader() -> ! {
    // POWER is unclaimed here: the SoftDevice is never enabled, so nothing else
    // owns this register, and we reset immediately afterwards anyway.
    embassy_nrf::pac::POWER
        .gpregret()
        .write(|w| w.0 = DFU_MAGIC_UF2_RESET);
    cortex_m::peripheral::SCB::sys_reset()
}
