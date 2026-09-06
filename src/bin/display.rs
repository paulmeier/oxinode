//! Phase 7 bring-up image for the Super IO board's OLED.
//!
//! Currently at step 1: it brings up the I²C peripheral, reports which pins it
//! actually claimed, and scans the bus — once with the panel's 12 V rail off
//! and once with it on. See `docs/phase-7-display.md`.
//!
//! **Nothing here draws anything.** No display initialisation, no pixels.
//!
//! Like `radio`, this image exposes a **single** CDC-ACM port and it is a log
//! port, so that DTR can hold the sequence until a terminal is attached and can
//! still take the 1200-baud touch. See that image for the full reasoning.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_futures::select::select;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, twim};
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::display::{self, Boost};
use oxinode::{boot, usb_log};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0004;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    // The rail is claimed before the bus, and starts off, so the first scan
    // below is a real control rather than a scan of a panel that happened to
    // already be powered.
    let mut boost = Boost::new(p.P0_23);

    // The nRF52's EasyDMA cannot read from flash, so the driver needs somewhere
    // in RAM to stage a write whose source is a `const`. Command sequences are
    // exactly that.
    static TWIM_RAM: StaticCell<[u8; 256]> = StaticCell::new();
    let mut i2c = display::new_i2c(p.TWISPI0, Irqs, p.P0_24, p.P0_25, TWIM_RAM.init([0; 256]));

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode display bring-up");
    config.serial_number = Some(serial);
    config.max_power = 100;
    config.max_packet_size_0 = 64;
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
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );

    let logs = CdcAcmClass::new(
        &mut builder,
        LOG_STATE.init(State::new()),
        usb_log::MAX_PACKET_SIZE,
    );
    let (mut log_tx, mut log_rx, control) = logs.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    let run_usb = usb.run();
    let pump = usb_log::pump(&mut log_tx, || control.dtr());

    let bring_up = async {
        // Hold everything until a terminal opens the port: the startup log is
        // the entire output of this image.
        while !control.dtr() {
            led.on();
            Timer::after(Duration::from_millis(60)).await;
            led.off();
            Timer::after(Duration::from_millis(940)).await;
        }
        led.on();

        defmt::info!(
            "oxinode display bring-up: serial {=str}, image at {=u32:#x}",
            serial,
            boot::APP_FLASH_ORIGIN
        );

        if display::check_pin_selection() {
            defmt::info!("i2c: TWIM0 up at 100 kHz, external pull-ups");
        } else {
            defmt::error!("i2c: peripheral did not claim the pins we asked for");
        }

        // The control. The SH1107's logic runs off 3V3 and only its panel bias
        // comes from the 12 V rail, so a display that answers here would be
        // telling us that "it answered on the bus" is *not* evidence it can
        // show anything -- which is worth knowing before a dark screen gets
        // blamed on a driver.
        defmt::info!("boost: off (P0.{=u8}); scanning", display::BOOST_EN.1);
        let dark = display::scan(&mut i2c).await;

        boost.on().await;
        defmt::info!("boost: on; scanning again");
        let lit = display::scan(&mut i2c).await;

        if dark == lit {
            defmt::info!(
                "boost: the bus answers the same either way -- {=usize} device(s). \
                 So answering proves the controller is alive, not that the panel is lit.",
                lit.found
            );
        } else {
            defmt::warn!(
                "boost: the bus changed with the rail -- {=usize} device(s) off, {=usize} on. \
                 The 12 V rail feeds more than the panel bias.",
                dark.found,
                lit.found
            );
        }

        match lit.display {
            Some(addr) => defmt::info!(
                "step 1 done: display controller at {=u8:#04x}. Nothing has been drawn.",
                addr
            ),
            None => defmt::error!(
                "step 1 failed: no SH1107 answered. Check the Super IO board is seated."
            ),
        }

        // Idle, blinking, so the board is visibly alive -- and watching for the
        // 1200-baud touch, since this image has one port and it is this one.
        // A touch can arrive at any moment, so it is re-checked on every wake
        // rather than waited for on an edge.
        let mut buf = [0u8; 64];
        loop {
            if usb_log::is_bootloader_touch(&log_rx, &control) {
                boot::reboot_to_bootloader();
            }
            led.toggle();
            let _ = select(
                log_rx.read_packet(&mut buf),
                Timer::after(Duration::from_millis(500)),
            )
            .await;
        }
    };

    join3(run_usb, pump, bring_up).await;
}
