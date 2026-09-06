//! Phase 3 bring-up image for the LR1121.
//!
//! Currently at step 3: it configures the SPI bus, reports what the peripheral
//! actually claimed, pulses NRESET and reports what BUSY did about it, then
//! asks the chip what it is. See `docs/phase-3-radio.md` for what each step
//! adds.
//!
//! Unlike `usb-cdc`, this image exposes a **single** CDC-ACM port, and it is a
//! log port. That is not a simplification for its own sake: DTR is only visible
//! on the first CDC function of a composite device, and this image needs DTR
//! twice over —
//!
//!   * to hold the bring-up sequence until a terminal is actually attached, so
//!     that a one-shot startup log is not written into a port with no reader,
//!   * and to accept the 1200-baud touch, so reflashing does not mean walking
//!     over to double-tap the reset button.
//!
//! The cost is that this image cannot also carry a data port. It does not need
//! one; phase 5 is where the two transports have to coexist.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join4;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, spim};
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::{Builder, Config as UsbConfig};
use lr11xx::Lr11xx;
use oxinode::board::{self, Led};
use oxinode::{boot, radio, usb_log};
use oxinode_core::lr1121::ResetVerdict;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    // On nRF52 the POWER and CLOCK peripherals share one interrupt line; the
    // USB driver needs it to see VBUS come and go.
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    // SPI2 is the interrupt line for SPIM2; see `radio::new_spi` for why not
    // SPIM3.
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
});

/// Same prototyping VID as the other images, with a distinct PID so a host that
/// has both plugged in can tell them apart. See `usb_cdc.rs` on why these are
/// not real allocations.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0002;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    // Configure the radio bus first, so the register readback below has
    // something to read. Nothing is transmitted by doing this.
    let mut spi = radio::new_spi(p.SPI2, Irqs, p.P1_13, p.P1_15, p.P1_14, p.P1_12);
    let mut reset = radio::RadioReset::new(p.P1_10, p.P1_11);

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode radio bring-up");
    config.serial_number = Some(serial);
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // One CDC-ACM function is still two interfaces that have to be grouped, so
    // this is still a composite device with an IAD.
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static LOG_STATE: StaticCell<State> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [], // no Microsoft OS descriptors; CDC-ACM is class-driver material
        CONTROL_BUF.init([0; 64]),
    );

    let logs = CdcAcmClass::new(
        &mut builder,
        LOG_STATE.init(State::new()),
        usb_log::MAX_PACKET_SIZE,
    );
    let (mut log_tx, log_rx, control) = logs.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    let run_usb = usb.run();
    let pump = usb_log::pump(&mut log_tx, || control.dtr());

    let bring_up = async {
        // Hold everything until a terminal opens the port. The startup log is
        // the entire output of this image; letting it run into a closed port
        // would throw away the only thing worth reading.
        while !control.dtr() {
            // Slow blink: enumerated, waiting for someone to look.
            led.on();
            Timer::after(Duration::from_millis(60)).await;
            led.off();
            Timer::after(Duration::from_millis(940)).await;
        }
        led.on();

        defmt::info!(
            "oxinode radio bring-up: serial {=str}, image at {=u32:#x}",
            serial,
            boot::APP_FLASH_ORIGIN
        );

        if radio::check_pin_selection() {
            defmt::info!("spi: SPIM2 up, mode 0, MSB first, 1 MHz, ORC 0x00");
        } else {
            defmt::error!("spi: peripheral did not claim the pins we asked for");
        }
        // Step 2. Nothing here transmits; it only drives NRESET and watches
        // BUSY. Both waits are bounded -- a chip whose BUSY never falls must
        // produce a log line, not a silent board.
        let trace = reset.cycle().await;
        defmt::info!(
            "busy: before reset {=bool}, during reset {=bool}, rose {=bool}",
            trace.before_reset,
            trace.during_reset,
            trace.rose
        );
        match trace.fell_after_us {
            Some(us) => defmt::info!(
                "busy: fell {=u32} us after NRESET released (measured norm {=u32})",
                us,
                oxinode_core::lr1121::STARTUP_MEASURED_US
            ),
            None => defmt::error!(
                "busy: still high after {=u32} us",
                oxinode_core::lr1121::BUSY_TIMEOUT_US
            ),
        }

        let verdict = ResetVerdict::of(&trace);
        if verdict.can_proceed() {
            defmt::info!("reset: {=str}", verdict.summary());
        } else {
            defmt::error!("reset: {=str}", verdict.summary());
        }
        defmt::info!("reset: cannot distinguish {=str}", verdict.ambiguity());

        // Step 3: the first thing ever said to the radio.
        //
        // Gated on the reset verdict, and not merely as tidiness. `lr11xx`
        // waits for BUSY with no timeout of its own, so calling into the crate
        // with a BUSY that never falls hangs the driver -- exactly the silent
        // board step 2 was built to avoid. The bounded probe goes first, and
        // the crate is only handed the pins once BUSY has proved itself.
        let mut radio = None;
        if !verdict.can_proceed() {
            defmt::error!("skipping GetVersion: BUSY never reported the chip ready");
        } else {
            match radio::probe_version(&mut spi, &mut reset).await {
                Ok(probe) => {
                    defmt::info!(
                        "version: reply {=u8:#04x} {=u8:#04x} {=u8:#04x} {=u8:#04x}",
                        probe.reply[0],
                        probe.reply[1],
                        probe.reply[2],
                        probe.reply[3]
                    );
                    defmt::info!(
                        "version: stat1 {=u8:#04x} -- {=str}",
                        probe.reply_status_raw,
                        probe.reply_status.summary()
                    );
                    defmt::info!(
                        "chip: {=str}, executing from flash {=bool}, last reset {=str} \
                         (stat {=u8:#04x} {=u8:#04x})",
                        probe.before.1.mode_summary(),
                        probe.before.1.flash,
                        probe.before.1.reset_summary(),
                        probe.command_status[0],
                        probe.command_status[1]
                    );
                    if let Some(v) = probe.verdict.version() {
                        defmt::info!(
                            "version: hw {=u8:#04x}, use case {=u8:#04x}, fw {=u8}.{=u8}{=str}",
                            v.hardware,
                            v.use_case,
                            v.fw_major,
                            v.fw_minor,
                            if v.firmware_matches_bench() {
                                ""
                            } else {
                                " -- differs from the board oxinode was brought up on"
                            }
                        );
                    }
                    if probe.verdict.is_expected() {
                        defmt::info!("version: {=str}", probe.verdict.summary());
                    } else {
                        defmt::error!("version: {=str}", probe.verdict.summary());
                    }
                    defmt::info!("version: next -- {=str}", probe.verdict.next_step());

                    if probe.verdict.is_expected() {
                        // Hand the bus and BUSY to `lr11xx`, which owns them
                        // from here. Its own `new` re-runs GetStatus and
                        // GetVersion, so the crate agreeing is an independent
                        // check on the probe above rather than a repeat of it.
                        match Lr11xx::new(spi, reset.into_busy()).await {
                            Ok(mut dev) => {
                                defmt::info!("lr11xx: driver attached");
                                // Whatever the chip has latched since power-on.
                                // Expected to be non-empty here: nothing has
                                // configured the TCXO yet, so the high-frequency
                                // oscillator has had no chance to start. Step 4
                                // is what should clear it, which makes this the
                                // measurement step 4 is judged against.
                                match dev.errors().await {
                                    Ok(errors) => defmt::info!("lr11xx: errors {}", errors),
                                    Err(e) => defmt::error!("lr11xx: GetErrors failed, {}", e),
                                }
                                radio = Some(dev);
                            }
                            Err(e) => defmt::error!("lr11xx: {}", e),
                        }
                    }
                }
                Err(e) => defmt::error!("version: probe failed, {}", e),
            }
        }

        defmt::info!("step 3 done");

        // Liveness, and something for a terminal that reconnects later.
        let mut ticks: u32 = 0;
        loop {
            Timer::after(Duration::from_millis(5000)).await;
            ticks += 1;
            match radio.as_mut() {
                Some(dev) => defmt::info!("idle, tick {=u32}, busy {}", ticks, dev.busy().ok()),
                None => defmt::info!("idle, tick {=u32}, no radio attached", ticks),
            }
        }
    };

    let touch = async {
        loop {
            control.control_changed().await;
            if usb_log::is_bootloader_touch(&log_rx, &control) {
                boot::reboot_to_bootloader();
            }
        }
    };

    join4(run_usb, pump, bring_up, touch).await;
}
