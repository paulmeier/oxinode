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
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, twim};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::class::cdc_acm::{ControlChanged, Receiver};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::display::{self, Boost, Panel};
use oxinode::{boot, usb_log};
use oxinode_core::sh1107;
use static_cell::StaticCell;

/// Blink, and watch for the 1200-baud touch.
///
/// This image has one serial port and it is this one, so the touch has to be
/// checked here or reflashing means walking over to the reset button. It can
/// arrive at any moment, so it is re-checked on every wake rather than waited
/// for on an edge.
async fn idle<'d, D: UsbDriverTrait<'d>>(
    led: &mut Led<'_>,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
) -> ! {
    let mut buf = [0u8; 64];
    let mut ticks = 0u32;
    loop {
        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }
        // 20 ms, for the reason in `bring_up`: the touch window is short and
        // missing it costs a walk to the reset button.
        if ticks % 25 == 0 {
            led.toggle();
        }
        ticks = ticks.wrapping_add(1);
        let _ = usb_log::read_for(rx, &mut buf, Duration::from_millis(20)).await;
    }
}

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
    let mut p = embassy_nrf::init(board::embassy_config());

    // The rail is claimed before the bus, and starts off, so the first scan
    // below is a real control rather than a scan of a panel that happened to
    // already be powered.
    let mut boost = Boost::new(p.P0_23);

    // The nRF52's EasyDMA cannot read from flash, so the driver needs
    // somewhere in RAM to stage a write whose source is a `const`. Command
    // sequences are exactly that.
    static TWIM_RAM: StaticCell<[u8; 256]> = StaticCell::new();

    // Both bus lines are looked at as plain inputs before anything drives
    // them. See `display::line_levels`.
    let idle_levels = display::line_levels(p.P0_24.reborrow(), p.P0_25.reborrow());

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
        //
        // Sampled every 20 ms, and the touch is checked on every one of them.
        // A 1200-baud open/close is over in tens of milliseconds, so a loop
        // that looked once a second could miss the window entirely -- and this
        // image did, twice, rebooting into the bootloader seconds after the
        // flasher had given up waiting for it. Two trips to the reset button.
        let mut ticks = 0u32;
        while !control.dtr() {
            if usb_log::is_bootloader_touch(&log_rx, &control) {
                boot::reboot_to_bootloader();
            }
            // Slow blink: enumerated, waiting for someone to look.
            if ticks % 50 == 0 {
                led.on();
            } else if ticks % 50 == 3 {
                led.off();
            }
            ticks = ticks.wrapping_add(1);
            Timer::after(Duration::from_millis(20)).await;
        }
        led.on();

        defmt::info!(
            "oxinode display bring-up: serial {=str}, image at {=u32:#x}",
            serial,
            boot::APP_FLASH_ORIGIN
        );

        defmt::info!(
            "i2c: at boot, SDA high {=bool}, SCL high {=bool}",
            idle_levels.0,
            idle_levels.1
        );
        if !idle_levels.0 || !idle_levels.1 {
            defmt::warn!(
                "i2c: a line was low with nothing driving it -- either the bus is held \
                 or the Super IO board is not attached and the pins are floating"
            );
        }

        // Free the bus if something is holding SDA, before the peripheral ever
        // sees it. A stuck line makes every transaction fail identically.
        boost.on().await;
        Timer::after(Duration::from_millis(50)).await;
        let after_boost = display::line_levels(p.P0_24.reborrow(), p.P0_25.reborrow());
        defmt::info!(
            "i2c: with the 12 V rail on, SDA high {=bool}, SCL high {=bool}",
            after_boost.0,
            after_boost.1
        );
        if !after_boost.0 {
            let freed = display::recover_bus(p.P0_24.reborrow(), p.P0_25.reborrow()).await;
            defmt::warn!(
                "i2c: SDA was held low; clocked it out -- freed {=bool}",
                freed
            );
        }

        let mut i2c = display::new_i2c(p.TWISPI0, Irqs, p.P0_24, p.P0_25, TWIM_RAM.init([0; 256]));

        if display::check_pin_selection() {
            defmt::info!("i2c: TWIM0 up at 100 kHz, external pull-ups");
        } else {
            defmt::error!("i2c: peripheral did not claim the pins we asked for");
        }

        // Before any scan: ask the two addresses an SH1107 can use, one
        // question at a time, and report exactly what came back. A scan that
        // reduces every failure to "did not answer" cannot tell "nobody there"
        // apart from "the peripheral refused", and this bus has now produced
        // both.
        defmt::info!("boost: on (P0.{=u8})", display::BOOST_EN.1);
        for address in display::SH1107_ADDRESSES {
            let read = display::probe(&mut i2c, address, false).await;
            let write = display::probe(&mut i2c, address, true).await;
            let read_again = display::probe(&mut i2c, address, false).await;
            defmt::info!(
                "probe {=u8:#04x}: read {=str}, write {=str}, read again {=str}",
                address,
                read,
                write,
                read_again
            );
        }

        defmt::info!("scanning the whole bus");
        let lit = display::scan(&mut i2c).await;
        let dark = lit;

        let _ = dark;
        let Some(addr) = lit.display else {
            defmt::error!("step 1 failed: no SH1107 answered. Check the Super IO board is seated.");
            idle(&mut led, &mut log_rx, &control).await
        };
        defmt::info!("step 1 done: display controller at {=u8:#04x}", addr);

        // ---- step 2: say something to it -------------------------------
        let mut panel = Panel::new(i2c, addr);
        if let Err(e) = panel.init(0x80).await {
            defmt::error!("panel: init failed: {}", e);
            idle(&mut led, &mut log_rx, &control).await
        }
        defmt::info!("panel: initialised, 128x128, external VPP, display on");

        // Every pixel, straight from the controller's own test mode. This
        // needs no RAM to be right -- only power, an address and glass -- so
        // a screen that stays dark here is a supply or a panel, and one that
        // lights here and shows nothing later is this firmware's fault.
        if let Err(e) = panel.all_on(true).await {
            defmt::error!("panel: entire-display-on failed: {}", e);
        }
        defmt::info!("panel: ENTIRE DISPLAY ON for 3 s -- the whole screen should be lit");
        Timer::after(Duration::from_secs(3)).await;
        let _ = panel.all_on(false).await;

        // The test pattern. Asymmetric in three separate ways, because the
        // first version of the axis mapping was wrong and looked *almost*
        // right: see `oxinode_core::sh1107`.
        let mut frame = sh1107::Frame::new();
        // A border, to show how much of the panel the mapping actually covers.
        frame.frame_rect(0, 0, sh1107::WIDTH, sh1107::HEIGHT, true);
        // The origin: a solid block at what this firmware calls (0, 0).
        frame.rect(4, 4, 16, 16, true);
        // A wide, short bar, well above centre. Horizontal if the mapping is
        // right, vertical if the page and column axes are the other way round,
        // and there is no way to mistake one for the other.
        frame.rect(4, 40, 92, 8, true);
        // A staircase down the lower half. Each step is a different length, so
        // a picture that is mirrored or upside down cannot be read as correct.
        for step in 0..6usize {
            frame.rect(8, 72 + step * 8, 8 + step * 16, 4, true);
        }
        // And one lone block hard against the bottom-right, opposite the
        // origin square.
        frame.rect(108, 108, 12, 12, true);

        let started = Instant::now();
        match panel.flush(&mut frame).await {
            Ok(()) => defmt::info!(
                "panel: test pattern sent, {=u32} us for {=usize} bytes at 100 kHz",
                started.elapsed().as_micros() as u32,
                sh1107::BUFFER_LEN
            ),
            Err(e) => defmt::error!("panel: flush failed: {}", e),
        }

        defmt::info!("step 2 done. Expected: a border on all four edges, a solid");
        defmt::info!("  square just inside the TOP-LEFT, a horizontal bar above centre,");
        defmt::info!("  a staircase widening DOWNWARDS, and a small block bottom-right.");
        Timer::after(Duration::from_secs(5)).await;

        // ---- step 3: something worth looking at -------------------------
        //
        // Plausible values rather than real ones: this image has no radio. The
        // point here is the layout and the refresh, both of which are the same
        // whatever the numbers are.
        let mut status = oxinode_core::status::Status {
            name: [
                serial.as_bytes()[12],
                serial.as_bytes()[13],
                serial.as_bytes()[14],
                serial.as_bytes()[15],
            ],
            frequency_hz: 915_000_000,
            bandwidth_hz: 125_000,
            spreading_factor: 8,
            coding_rate: 5,
            tx_power_dbm: 17,
            radio_on: true,
            tnc: true,
            provisioned: true,
            rx_count: 0,
            tx_count: 7,
            last_rssi_dbm: Some(-69),
            last_snr_quarter_db: Some(45),
        };

        // Rendered into a scratch frame and committed by comparison, so an
        // update sends the pages that changed rather than the pages that were
        // redrawn. See `Frame::copy_from`.
        let mut scratch = sh1107::Frame::new();
        oxinode_core::status::render(&status, &mut scratch);
        frame.copy_from(&scratch);
        let started = Instant::now();
        match panel.flush(&mut frame).await {
            Ok(()) => defmt::info!(
                "panel: status page drawn, full flush {=u32} us",
                started.elapsed().as_micros() as u32
            ),
            Err(e) => defmt::error!("panel: status flush failed: {}", e),
        }
        defmt::info!("step 3 done: the panel should show a status page, title bar at the top.");

        // Now tick the received-packet counter once a second. Only the pages
        // that change are sent, so this also measures what a partial update
        // costs -- which is the number that decides whether the display can be
        // refreshed while the radio is busy.
        loop {
            if usb_log::is_bootloader_touch(&log_rx, &control) {
                boot::reboot_to_bootloader();
            }
            led.toggle();
            Timer::after(Duration::from_secs(1)).await;

            status.rx_count = status.rx_count.wrapping_add(1);
            oxinode_core::status::render(&status, &mut scratch);
            frame.copy_from(&scratch);
            let started = Instant::now();
            match panel.flush(&mut frame).await {
                Ok(()) => {
                    if status.rx_count % 5 == 0 {
                        defmt::info!(
                            "panel: partial update {=u32} us (rx {=u32})",
                            started.elapsed().as_micros() as u32,
                            status.rx_count
                        );
                    }
                }
                Err(e) => defmt::error!("panel: update failed: {}", e),
            }
        }
    };

    join3(run_usb, pump, bring_up).await;
}
