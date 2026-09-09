//! The RNode image: a Reticulum host opens the serial port and finds a modem.
//!
//! Two CDC-ACM ports and, since phase 8, a Bluetooth peripheral. The USB
//! **first** port carries the KISS stream and is the one
//! `rnsd` is pointed at; the **second** carries the defmt log. That order is
//! the contract with the host — it decides which tty gets the lower number —
//! and it is also forced by DTR, which is only visible on the first CDC
//! function of a composite device. The KISS port needs DTR for the 1200-baud
//! bootloader touch, so it has to be first, and the log port therefore cannot
//! have it.
//!
//! # Bluetooth is a second pipe
//!
//! The Nordic UART Service carries the identical KISS stream. It does not get
//! its own protocol instance: the modem loop is the one owner of the protocol
//! and the radio, exactly as it was for USB alone, and Bluetooth is one more
//! place bytes come from and go to -- through a pipe in each direction, pumped
//! by `oxinode::nus::pump`, with its own decoder and its own outbox so that a
//! frame arriving on one transport can never be spliced into a frame arriving
//! on the other.
//!
//! Who gets a frame nobody asked for -- a received packet, a modem error -- is
//! decided by one rule: **a connected phone is the host.** Answers to commands
//! always go back the way the command came, so `rnodeconf` over USB keeps
//! working while a phone is on the line; unsolicited frames go to the phone
//! while there is one, and to USB otherwise.
//!
//! # What this image does that `radio` does not
//!
//! `radio` is a diagnostic: it reports the evidence for every bring-up step and
//! keeps going when one fails, so the next can be tried anyway. This one comes
//! up or says why not, and then talks to a host. Neither is a better version of
//! the other.
//!
//! # The panel is an interface now
//!
//! Phase 11. The modem loop owns a [`Ui`]: the navigator from `oxinode-core`,
//! the frame the controller has been sent, and the frame the next page is
//! drawn into. Once per redraw the loop copies what the screens are allowed
//! to know out of the protocol and its own bookkeeping into a
//! [`screens::State`] -- plain values -- and hands that to the renderer, which
//! reaches into nothing. Gestures from the pad go to the navigator, and a
//! menu item the user picks comes back as a [`ui::Action`] for
//! [`perform`] to carry out.
//!
//! # The panel is a second controller
//!
//! Phase 12. An RNode is host-controlled, and Reticulum checks its
//! configuration against the device exactly once, when it brings the
//! interface up; a change underneath it afterwards is a debug line in its
//! log and nothing else. So the rule is: **a live session on USB or
//! Bluetooth owns the live radio configuration.** While one is there, every
//! action that would change the radio -- an edit, the toggle, a reset --
//! opens the notice that says so instead. With nobody on the line the panel
//! edits the configuration through the same setters and the same validation
//! the host gets, and in TNC mode the edit goes into the stored configuration
//! too, so it is what the board boots with. See `docs/phase-12-settings.md`.
//!
//! # The GPS is a third task
//!
//! Phase 14. The receiver has a task of its own, `oxinode::gps::serve`,
//! beside the Bluetooth one: it follows the mode switch and the menu, drives
//! the load switch, probes for the module and parses what it sends. The modem
//! loop meets it at two points only -- a copy of the `Position` for the
//! screen, and the `GPS On/Off` menu item, which raises a signal. Nothing on
//! the radio's path waits for the receiver.
//!
//! # Idling in standby XOSC
//!
//! Phase 3 measured the oscillator startup as a fixed 5 ms charged to the first
//! operation that needs the 32 MHz reference. Idling in standby RC would pay it
//! on every packet — a tenth of the airtime at SF7 — so the modem idles with
//! the crystal running instead. The cost is a milliamp; the alternative is a
//! tenth of the channel.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::join::{join, join3, join5};
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_futures::yield_now;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, buffered_uarte, interrupt, peripherals, saadc, spim, twim};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::pipe::{Pipe, Reader, Writer};
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Sender, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use nrf_sdc::{self as sdc, mpsl};
use oxinode::battery::Sense;
use oxinode::ble::{self, Vbus};
use oxinode::board::{self, Led};
use oxinode::display::{self, Boost, Panel};
use oxinode::gps;
use oxinode::modem::{CsmaReport, Modem, RxReport, TxOutcome, TxReport};
use oxinode::nus;
use oxinode::pad;
use oxinode::store::Storage;
use oxinode::{boot, bringup, radio, usb_log};
use oxinode_core::ble as interop;
use oxinode_core::edit::Editor;
use oxinode_core::lr1121::config::{ValidConfig, DEFAULT};
use oxinode_core::lr1121::csma::Backoff;
use oxinode_core::rnode::air::{self, Reassembler, Sequence, Split};
use oxinode_core::rnode::command::{self, error};
use oxinode_core::rnode::display as rnode_display;
use oxinode_core::rnode::eeprom;
use oxinode_core::rnode::kiss;
use oxinode_core::rnode::outbox::Outbox;
use oxinode_core::rnode::protocol::{Action, PanelChange, Protocol, Sink};
use oxinode_core::rnode::store::DeviceStore;
use oxinode_core::screens::{self, Air, Host, Identity};
use oxinode_core::sh1107;
use oxinode_core::status::Bluetooth;
use oxinode_core::ui::{self, Nav, NavExt};
use static_cell::StaticCell;
use trouble_host::prelude::*;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
    SAADC => saadc::InterruptHandler;
    // Phase 14: the GPS UART.
    UARTE0 => buffered_uarte::InterruptHandler<peripherals::UARTE0>;
    // The link layer's own. MPSL sets their priorities itself.
    RADIO => mpsl::HighPrioInterruptHandler;
    TIMER0 => mpsl::HighPrioInterruptHandler;
    RTC0 => mpsl::HighPrioInterruptHandler;
    EGU0_SWI0 => mpsl::LowPrioInterruptHandler;
    // `POWER` and `CLOCK` share this vector, and the bootloader leaves USB
    // power interrupts enabled when it starts the application from DFU. One
    // handler services both, or the second one storms. See `oxinode::ble::Vbus`.
    CLOCK_POWER => ble::PowerAndClockHandler;
});

// `MultiprotocolServiceLayer::new` asks for a binding to *its* handler type,
// and `PowerAndClockHandler` calls exactly that handler, so the promise the
// marker trait makes is kept.
unsafe impl
    interrupt::typelevel::Binding<interrupt::typelevel::CLOCK_POWER, mpsl::ClockInterruptHandler>
    for Irqs
{
}

// A fault must look different from a hang. See `src/bin/ble.rs` for why these
// are here and `oxinode::ble::fault` for what they record.
#[cortex_m_rt::exception]
unsafe fn HardFault(_frame: &cortex_m_rt::ExceptionFrame) -> ! {
    ble::fault::record_and_reboot(ble::fault::HARD_FAULT)
}

#[cortex_m_rt::exception]
unsafe fn DefaultHandler(irqn: i16) {
    let code = if irqn >= 0 {
        ble::fault::UNHANDLED | (irqn as u8 & 0x3F)
    } else {
        ble::fault::HARD_FAULT
    };
    ble::fault::record_and_reboot(code)
}

/// Whether a phone is on the line. Written by the Bluetooth task, read by the
/// modem loop to decide where unsolicited frames go and what the title bar
/// says.
static BLE_CONNECTED: AtomicBool = AtomicBool::new(false);
/// Whether the stack came up at all. Also for the title bar.
static BLE_UP: AtomicBool = AtomicBool::new(false);
/// `Forget Phones` was chosen on the panel. The stored bonds are already
/// gone by the time this is raised; this is for the copy the host stack
/// holds, which only the Bluetooth task can reach. Latched, so a choice made
/// during a connection is honoured once the connection ends.
static FORGET_BONDS: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Bytes from the phone to the modem loop. The same size as the USB pipe and
/// for the same reason: a full one is back-pressure, not loss.
const BLE_IN: usize = 1024;
/// Bytes from the modem loop to the phone. Sized like an outbox, because it is
/// where the modem loop's Bluetooth outbox drains to without waiting.
const BLE_OUT: usize = 4096;
const _: () = assert!(BLE_OUT >= OUTBOX);

/// How long the controller gets to start before its address is taken and the
/// board restarts itself. See `oxinode::ble::stall`.
const STALL_AFTER_MS: u32 = 2_000;

/// Which transport a command arrived on, for the one action that has to flush
/// before it acts.
#[derive(Clone, Copy)]
enum Via {
    Usb,
    Ble,
}

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0003;

/// How much unsent response can pile up.
///
/// The largest frame is no longer a packet. `CMD_DISP_READ` answers with 1024
/// bytes of screen, which escapes to 2051 in the worst case — and a picture is
/// exactly the kind of data that is full of `0xC0`. This holds that and a data
/// frame behind it, because [`Outbox`] drops whole frames rather than
/// truncating them, and a display read that could never fit would never be
/// answered at all.
const OUTBOX: usize = 4096;

/// The boot configuration, validated: what the reassembler's age is set from
/// until a host or the stored record applies one.
const DEFAULT_VALID: ValidConfig = match ValidConfig::new(DEFAULT) {
    Ok(v) => v,
    Err(_) => panic!("the default configuration is valid"),
};

/// How long the EEPROM has to stay still before it is written to flash.
///
/// `rnodeconf` provisions a board with 155 single-byte writes six milliseconds
/// apart. Committing each of them would mean 155 page erases — thirteen
/// seconds of stalled CPU to absorb one second of commands — so the image is
/// held in RAM and written out once the writes stop.
///
/// The cost is that this much of a provisioning run is lost if the board is
/// unplugged at exactly the wrong moment. That is survivable and visible: the
/// host reads the image back immediately afterwards and would find it short.
/// Losing it because the flash could not keep up would not be.
const PERSIST_IDLE: Duration = Duration::from_millis(250);

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    // Read and clear before anything else can fault, so this is the previous
    // run's verdict rather than this one's.
    let last_fault = ble::fault::take();
    let last_stall = ble::stall::take();
    let p = embassy_nrf::init(ble::embassy_config());
    // Before any of these peripherals is built: their constructors enable
    // their interrupts, and the NVIC's reset priority is the one the link
    // layer runs at.
    ble::yield_to_mpsl(&[
        interrupt::USBD,
        interrupt::SPI2,
        interrupt::TWISPI0,
        interrupt::SAADC,
        interrupt::UARTE0,
    ]);

    let spi = radio::new_spi(p.SPI2, Irqs, p.P1_13, p.P1_15, p.P1_14, p.P1_12);
    let reset = radio::RadioReset::new(p.P1_10, p.P1_11);
    let mut irq = radio::RadioIrq::new(p.P1_08);

    // Takes over the `POWER` half of the shared interrupt before MPSL unmasks
    // it; see the binding above.
    let vbus = Vbus::take();
    let driver = Driver::new(p.USBD, Irqs, vbus.detector());
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    // Reticulum does not match on this string, but a person looking at a list
    // of serial ports does.
    config.product = Some("oxinode RNode");
    config.serial_number = Some(serial);
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    static CONFIG_DESC: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static DATA_STATE: StaticCell<State> = StaticCell::new();
    static LOG_STATE: StaticCell<State> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 512]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );

    // Order is the contract: the KISS port first, so it enumerates as the
    // lower-numbered tty and so that it is the one with DTR.
    let data = CdcAcmClass::new(
        &mut builder,
        DATA_STATE.init(State::new()),
        usb_log::MAX_PACKET_SIZE,
    );
    let (mut kiss_tx, mut kiss_rx, control) = data.split_with_control();

    let logs = CdcAcmClass::new(
        &mut builder,
        LOG_STATE.init(State::new()),
        usb_log::MAX_PACKET_SIZE,
    );
    let (mut log_tx, _, log_control) = logs.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    // Phase 10: the navigation pad. Claimed whether or not a Super IO is
    // attached -- with the pull-ups on, an unconnected line reads released,
    // and an idle driver costs nothing. The channel is what the modem loop
    // reads gestures from.
    let mut pad_pins = pad::Pins::new(p.P0_21, p.P0_17, p.P1_05, p.P0_16, p.P0_10, p.P0_15);
    let mode_switch = pad::ModeSwitch::new(p.P1_09, p.P0_12);
    let pad_events = pad::Events::new();
    let nav_pad = pad::run(&mut pad_pins, &pad_events);

    // Phase 14: the GPS. A task of its own that follows the mode switch and
    // the menu, probes for the module when powered, and publishes what it
    // knows for the Position screen; see `oxinode::gps`.
    static GPS_RX: StaticCell<[u8; gps::RX_BUFFER]> = StaticCell::new();
    static GPS_TX: StaticCell<[u8; gps::TX_BUFFER]> = StaticCell::new();
    let receiver = gps::Gps::new(
        p.P1_01,
        p.UARTE0,
        p.TIMER2,
        p.PPI_CH10,
        p.PPI_CH11,
        p.PPI_GROUP0,
        p.P0_20,
        p.P0_19,
        Irqs,
        GPS_RX.init([0; gps::RX_BUFFER]),
        GPS_TX.init([0; gps::TX_BUFFER]),
    );
    let gps_task = gps::serve(receiver, &mode_switch);

    // Phase 11: the cell through its divider on P0.31, and the charger's
    // status line. Read once per redraw, for the Home screen's battery row
    // and the title bar's percentage.
    let mut battery = Sense::new(p.SAADC, Irqs, p.P0_31, p.P1_02);

    // Read before anything else touches it. What comes back is either a record
    // this firmware wrote, or the state of a board nobody has provisioned --
    // there is no third answer; see `oxinode::store`.
    let mut storage = Storage::new(p.NVMC);
    let device = storage.load();
    let mcu_id = board::device_id();
    // The phones this board already knows, handed to the host stack at boot.
    // Copied out because `device` moves into the modem loop.
    let stored_bonds = device.bonds;

    let mut boost = Boost::new(p.P0_23);
    static TWIM_RAM: StaticCell<[u8; 256]> = StaticCell::new();
    let mut i2c = Some(display::new_i2c(
        p.TWISPI0,
        Irqs,
        p.P0_24,
        p.P0_25,
        TWIM_RAM.init([0; 256]),
    ));

    let run_usb = usb.run();
    // Held until a terminal opens the log port. This used to be `|| true` on
    // the grounds that the log is continuous rather than a startup sequence,
    // and that cost the startup sequence: the host discards what arrives on a
    // port nobody has open, and a board re-enumerates faster than a terminal
    // can be attached to it, so the lines that say what the hardware is --
    // `UICR.NFCPINS`, the mode switch, the panel address -- were gone every
    // time. The ring holds 4 KB, which is the whole of boot; with nobody ever
    // listening it simply fills and drops the oldest, as it always did.
    let pump = usb_log::pump(&mut log_tx, || log_control.dtr());

    // The Bluetooth controller, unconditionally: the product image does not
    // wait for a keystroke. The stall guard stays, because a controller that
    // stops with the executor is a board that has to restart itself and say
    // why, and the guard is what makes both happen.
    ble::fault::report(last_fault);
    ble::stall::report(last_stall);
    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );
    ble::stall::arm(STALL_AFTER_MS);
    let stack_parts = ble::bring_up(mpsl_p, sdc_p, p.RNG, Irqs, ble::LfSource::Crystal);
    ble::stall::disarm();
    BLE_UP.store(stack_parts.is_some(), Ordering::Relaxed);

    // One pipe in each direction between the phone and the modem loop. See
    // the module docs: Bluetooth is a source and a sink of bytes and nothing
    // else, and the loop owns everything that understands them.
    static BLE_RX: StaticCell<Pipe<NoopRawMutex, BLE_IN>> = StaticCell::new();
    static BLE_TX: StaticCell<Pipe<NoopRawMutex, BLE_OUT>> = StaticCell::new();
    let (mut ble_reader, ble_writer) = BLE_RX.init(Pipe::new()).split();
    let (ble_out_reader, ble_out_writer) = BLE_TX.init(Pipe::new()).split();

    let bluetooth = async {
        let Some((mpsl, controller)) = stack_parts else {
            defmt::error!("ble: no controller; this board is USB only until it is reset");
            core::future::pending::<()>().await;
            unreachable!()
        };
        join(
            mpsl.run(),
            serve_bluetooth(
                controller,
                mcu_id,
                stored_bonds,
                &ble_writer,
                &ble_out_reader,
            ),
        )
        .await;
    };

    // Everything the host sends goes through here, and the reason is
    // cancellation.
    //
    // The modem loop has to wait on two things at once -- host bytes and the
    // radio's interrupt -- and `select` drops whichever future loses. Dropping
    // `read_packet` mid-flight loses whatever it had already taken from the
    // endpoint, and the loop cancelled it every 50 ms whether or not the radio
    // was doing anything. The symptom was a setter going missing perhaps one
    // round in three: the host would see no reply, decide its configuration had
    // not taken, and refuse the interface.
    //
    // So the only thing that ever awaits `read_packet` is the task below, which
    // never selects on anything and therefore never cancels it. The pipe is
    // what the modem loop waits on instead, and a cancelled pipe read consumes
    // nothing.
    static HOST_RX: StaticCell<Pipe<NoopRawMutex, 1024>> = StaticCell::new();
    let host_rx = HOST_RX.init(Pipe::new());
    let (mut host_reader, host_writer) = host_rx.split();

    let feed_host_rx = async {
        let mut buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
        loop {
            match kiss_rx.read_packet(&mut buf).await {
                // `write_all` on a full pipe waits for the modem loop to catch
                // up, which is back-pressure rather than loss -- and the
                // endpoint NAKs while we are not reading it, so the host simply
                // retries.
                Ok(n) => {
                    // `Writer::write` may take fewer bytes than offered when
                    // the pipe is nearly full, so this loops until the packet
                    // is in. Truncating instead would drop the tail of a
                    // frame, which is the failure this whole arrangement
                    // exists to remove.
                    let mut rest = &buf[..n];
                    while !rest.is_empty() {
                        let written = host_writer.write(rest).await;
                        rest = &rest[written..];
                    }
                }
                // The host closed the port. It will be back.
                Err(_) => Timer::after(Duration::from_millis(50)).await,
            }
        }
    };

    let modem = async {
        defmt::info!(
            "oxinode RNode: serial {=str}, image at {=u32:#x}",
            serial,
            boot::APP_FLASH_ORIGIN
        );

        // Let enumeration finish before the radio bring-up starts.
        //
        // `wait_connection` resolves at SET_CONFIGURATION -- when the host has
        // enumerated the device, not when anybody opens the tty -- so this
        // normally costs a hundred milliseconds. The radio's own bring-up takes
        // 191 ms in the reset alone and a further 50 ms of calibration, and
        // sharing the executor with enumeration for that long is a needless
        // risk on a stack that has to answer control transfers promptly.
        //
        // The timeout is the other half. A board on battery with no host must
        // still bring its radio up, so this waits for enumeration *or* two
        // seconds, whichever comes first, and never depends on a host being
        // there at all.
        match select(
            kiss_tx.wait_connection(),
            Timer::after(Duration::from_secs(2)),
        )
        .await
        {
            Either::First(()) => defmt::info!("usb: enumerated"),
            Either::Second(()) => defmt::info!("usb: not enumerated after 2 s; continuing anyway"),
        }

        // The panel, if there is one. A board with no Super IO attached is a
        // perfectly good modem, so nothing here is allowed to be fatal -- and
        // the display is brought up before the radio because it costs 50 ms
        // against the radio's 250, and because a status page saying "no radio"
        // is worth more than a blank screen.
        boost.on().await;
        let mut panel = match display::scan(i2c.as_mut().unwrap()).await.display {
            Some(address) => {
                let mut panel = Panel::new(i2c.take().unwrap(), address);
                match panel.init(status_intensity()).await {
                    Ok(()) => {
                        defmt::info!("panel: {=u8:#04x}, 128x128", address);
                        Some(panel)
                    }
                    Err(e) => {
                        defmt::error!("panel: init failed: {}", e);
                        None
                    }
                }
            }
            None => {
                defmt::info!("panel: none on the bus; running without a display");
                None
            }
        };

        // Reported after the radio rather than at the top of boot, because the
        // log ring is small and the first hundred milliseconds of it are long
        // gone by the time a host has enumerated and opened the port.
        defmt::info!(
            "board: nav pad usable={=bool} (UICR.NFCPINS), regulator={=u8}.{=u8} V",
            board::nfc_pins_are_gpio(),
            board::regulator_decivolts().unwrap_or(0) / 10,
            board::regulator_decivolts().unwrap_or(0) % 10,
        );
        mode_switch.report();

        // The ADC's offset calibration, then one reading for the log, so
        // the divider arithmetic can be checked against a meter without a
        // screen: the count is what the ADC said, the rest is the core's.
        battery.calibrate().await;
        let count = battery.raw().await;
        match oxinode_core::battery::reading(count) {
            Some(cell) => defmt::info!(
                "battery: count {=i16}, {=u32} mV, {=u8}%, charging={=bool}",
                count,
                cell.millivolts,
                cell.percent,
                battery.is_charging()
            ),
            None => defmt::info!(
                "battery: count {=i16}, {=u32} mV: no cell; charging={=bool}",
                count,
                oxinode_core::battery::cell_millivolts(count),
                battery.is_charging()
            ),
        }

        let mut dev = match bringup::bring_up(spi, reset, &mut irq).await {
            Ok(dev) => dev,
            Err(e) => {
                // Answer anyway. A host that connects to a board whose radio is
                // dead should be told so rather than left waiting: the protocol
                // has a frame for exactly this, and it makes Reticulum say
                // "hardware initialisation error" instead of timing out.
                defmt::error!("radio did not come up: {}", e);
                // Provisioning still works. A board whose radio is dead can
                // still be given an identity, and telling somebody to fix the
                // radio first would be inventing a dependency that is not
                // there.
                serve_without_a_radio(
                    &mut kiss_tx,
                    &mut host_reader,
                    &mut ble_reader,
                    &ble_out_writer,
                    &control,
                    &mut led,
                    &mut storage,
                    device,
                    mcu_id,
                    panel.as_mut(),
                    &pad_events,
                    &mut battery,
                )
                .await;
            }
        };

        led.on();
        run(
            &mut dev,
            &mut irq,
            &mut kiss_tx,
            &mut host_reader,
            &mut ble_reader,
            &ble_out_writer,
            &control,
            &mut led,
            &mut storage,
            device,
            mcu_id,
            panel.as_mut(),
            &pad_events,
            &mut battery,
        )
        .await
    };

    join5(
        run_usb,
        pump,
        feed_host_rx,
        modem,
        join3(bluetooth, nav_pad, gps_task),
    )
    .await;
}

/// Advertise, and pump bytes for every connection that comes.
///
/// One connection at a time, which is what the controller was sized for and
/// what an RNode means: a modem has a host, not an audience. While a phone is
/// connected the board is not advertising, so a second one sees nothing
/// until the first has gone.
async fn serve_bluetooth(
    controller: sdc::SoftdeviceController<'static>,
    device_id: u64,
    stored_bonds: [Option<oxinode_core::rnode::store::Bond>; oxinode_core::rnode::store::MAX_BONDS],
    to_modem: &Writer<'_, NoopRawMutex, BLE_IN>,
    from_modem: &Reader<'_, NoopRawMutex, BLE_OUT>,
) {
    let address = interop::static_random_address(device_id);
    let name = interop::advertised_name(device_id);
    let name = core::str::from_utf8(&name).expect("advertised_name is ASCII");

    static RESOURCES: StaticCell<
        HostResources<DefaultPacketPool, { ble::CONNECTIONS }, { ble::L2CAP_CHANNELS }>,
    > = StaticCell::new();
    let stack = trouble_host::new(controller, RESOURCES.init(HostResources::new()))
        .set_random_address(Address::random(address))
        // A screen and no keyboard: the board shows six digits and the phone
        // types them. That is the Passkey Entry method, which is the one that
        // gives MITM protection -- see `nus::NusService` for why that is
        // required rather than merely preferred.
        .set_io_capabilities(IoCapabilities::DisplayOnly)
        .build();
    for bond in stored_bonds.iter().flatten() {
        match stack.add_bond_information(nus::restore(bond)) {
            Ok(()) => defmt::info!("ble: remembered {=[u8; 6]:02x}", bond.addr),
            Err(e) => defmt::warn!("ble: could not restore a bond: {}", e),
        }
    }
    let mut runner = stack.runner();
    let mut peripheral = stack.peripheral();

    let server = match nus::Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name,
        appearance: &appearance::UNKNOWN,
    })) {
        Ok(server) => server,
        Err(e) => {
            defmt::error!("ble: could not build the GATT server: {}", e);
            return;
        }
    };
    defmt::info!("ble: {=str}, address {=[u8; 6]:02x}", name, address);

    // Flags and the name in the advertisement; the service UUID in the scan
    // response. All three will not fit in one legacy packet, and the name is
    // what Reticulum's scan filters on.
    let mut adv_data = [0u8; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut adv_data,
    )
    .expect("advertisement does not fit");
    let mut scan_data = [0u8; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteServiceUuids128(&[
            interop::uuid_le_bytes(interop::NUS_SERVICE),
        ])],
        &mut scan_data,
    )
    .expect("scan response does not fit");

    let advertise = async {
        loop {
            let advertiser = match peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv_data[..adv_len],
                        scan_data: &scan_data[..scan_len],
                    },
                )
                .await
            {
                Ok(advertiser) => advertiser,
                Err(_) => {
                    defmt::error!("ble: could not start advertising");
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
            };
            let conn = match select(advertiser.accept(), FORGET_BONDS.wait()).await {
                Either::First(Ok(conn)) => conn,
                Either::First(Err(_)) => continue,
                // Dropping the accept stops advertising; the loop starts it
                // again, now with nobody remembered.
                Either::Second(()) => {
                    forget_bonds(&stack);
                    continue;
                }
            };
            let conn = match conn.with_attribute_server(&server) {
                Ok(conn) => conn,
                Err(e) => {
                    defmt::error!("ble: could not attach the GATT server: {}", e);
                    continue;
                }
            };
            BLE_CONNECTED.store(true, Ordering::Relaxed);
            let end = nus::pump(&conn, &server, to_modem, from_modem).await;
            BLE_CONNECTED.store(false, Ordering::Relaxed);
            defmt::info!("ble: session over: {}", end);
        }
    };

    match select(runner.run(), advertise).await {
        Either::First(Err(_)) => defmt::error!("ble: the host stack stopped"),
        Either::First(Ok(())) | Either::Second(()) => {}
    }
}

/// Drop every bond the host stack holds.
///
/// The identities are copied out first, because removing while iterating
/// would be removing from under the borrow. Eight is more than the record
/// holds; a stack that somehow held more would forget the first eight and
/// the rest at the next boot, when only the record is reloaded.
fn forget_bonds<C: Controller, P: PacketPool>(stack: &Stack<'_, C, P>) {
    let mut identities = heapless::Vec::<trouble_host::Identity, 8>::new();
    stack.with_bond_information(|bonds| {
        for bond in bonds {
            let _ = identities.push(bond.identity);
        }
    });
    for identity in identities {
        match stack.remove_bond_information(identity) {
            Ok(()) => defmt::info!(
                "ble: forgot {=[u8; 6]:02x}",
                identity.addr.addr.into_inner()
            ),
            Err(e) => defmt::warn!("ble: could not forget a bond: {}", e),
        }
    }
}

/// Hand a Bluetooth outbox to the pump, without waiting for the phone.
///
/// `try_write` rather than `write`: the modem loop must never block on a
/// phone. What does not fit stays in the outbox for the next pass, and a
/// phone that has gone gets its frames dropped, counted, rather than a modem
/// loop that stops servicing the radio.
fn push_to_ble(outbox: &mut Outbox<OUTBOX>, out: &Writer<'_, NoopRawMutex, BLE_OUT>) {
    if !BLE_CONNECTED.load(Ordering::Relaxed) {
        outbox.abandon();
    }
    while !outbox.is_empty() {
        match out.try_write(outbox.pending()) {
            Ok(n) if n > 0 => outbox.consume(n),
            _ => break,
        }
    }
    let dropped = outbox.take_dropped();
    if dropped > 0 {
        defmt::warn!("ble outbox: {=u32} frames dropped", dropped);
    }
}

/// The passkey to show, if a pairing is in progress.
fn passkey_state() -> Option<u32> {
    match nus::PASSKEY.load(Ordering::Relaxed) {
        nus::NO_PASSKEY => None,
        key => Some(key),
    }
}

/// What the title bar should say about Bluetooth.
fn bluetooth_state() -> Bluetooth {
    if BLE_CONNECTED.load(Ordering::Relaxed) {
        Bluetooth::Connected
    } else if BLE_UP.load(Ordering::Relaxed) {
        Bluetooth::Advertising
    } else {
        Bluetooth::Absent
    }
}

/// The main loop: host bytes in one direction, radio packets in the other.
#[allow(clippy::too_many_arguments)]
async fn run<'d, D, S, B>(
    dev: &mut lr11xx::Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    tx: &mut Sender<'d, D>,
    host: &mut Reader<'_, NoopRawMutex, 1024>,
    ble: &mut Reader<'_, NoopRawMutex, BLE_IN>,
    ble_out: &Writer<'_, NoopRawMutex, BLE_OUT>,
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
    storage: &mut Storage<'_>,
    device: DeviceStore,
    mcu_id: u64,
    mut panel: Option<&mut Panel<'_>>,
    pad_events: &pad::Events,
    battery: &mut Sense<'_>,
) -> !
where
    D: UsbDriverTrait<'d>,
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let mut protocol = Protocol::with_storage(device, mcu_id);
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut outbox = Outbox::<OUTBOX>::new();
    // The phone's own decoder and outbox: a frame in progress on one
    // transport is never spliced with the other's.
    let mut ble_decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut ble_outbox = Outbox::<OUTBOX>::new();
    let mut ble_buf = [0u8; 256];
    let mut usb_buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    let mut rx_buf = [0u8; kiss::HW_MTU];
    // What comes off the air is frames, and what the host gets is packets:
    // the header byte stripped, and a packet that came as two frames joined.
    // The age it holds a first half for is set from the configuration when
    // one is applied.
    let mut air_in = Reassembler::new(air::max_age_us(&DEFAULT_VALID));
    // The sequence nibble the next packet goes out under. Seeded from the
    // chip's ID rather than the clock, which reads the same at every boot.
    let mut sequence = Sequence::new(mcu_id as u8 ^ (mcu_id >> 8) as u8);
    // When the EEPROM was last changed, and therefore when it should be
    // written out. See `PERSIST_IDLE`.
    let mut dirty_since: Option<Instant> = None;

    // The interface: see `Ui`.
    let mut ui = Ui::new();
    let mut last_signal: Option<(i16, i8)> = None;
    // When a frame last passed between the modem and a host, on either
    // transport and in either direction, for the Home screen's "talking".
    let mut last_host_frame: Option<Instant> = None;
    let facts = Facts::new(mcu_id);
    // What the radio is currently programmed with, so a configuration is not
    // reprogrammed on every packet -- and, more to the point, so that a change
    // is applied exactly once and can be logged when it happens.
    let mut applied: Option<ValidConfig> = None;
    let mut receiving = false;
    let mut last_blink = Instant::now();

    // TNC mode. A device with a stored configuration is supposed to come up on
    // air by itself -- that is the whole point of `rnodeconf --tnc`, and the
    // only reason to store a configuration at all. Nothing here waits for a
    // host, and a host that connects later reconfigures it as it would any
    // other board.
    let resume = protocol.resume_stored_config();
    if resume != Action::None {
        defmt::info!("tnc: resuming the stored configuration");
        act(
            dev,
            irq,
            &mut protocol,
            resume,
            &mut applied,
            &mut receiving,
            &mut outbox,
            &mut rx_buf,
            &mut last_signal,
            &mut air_in,
            &mut sequence,
        )
        .await;
    } else if let Some(reason) = protocol.last_error() {
        defmt::warn!(
            "tnc: the stored configuration is not one this radio can do: {=str}",
            reason.message()
        );
    }

    loop {
        // A touch can arrive at any moment, including while a packet is in the
        // air, so it is level-triggered and re-checked on every wake rather
        // than waited for on an edge.
        if usb_log::is_bootloader_touch_tx(tx, control) {
            // Anything unwritten goes to flash first. A reflash is exactly
            // when losing a provisioning would be least welcome, and the
            // record survives the flash itself -- see `oxinode::store`.
            commit(storage, &protocol);
            boot::reboot_to_bootloader();
        }

        let event = {
            // Reading the pipe rather than the endpoint. `select` drops the
            // loser, and a cancelled pipe read consumes nothing -- whereas a
            // cancelled `read_packet` loses whatever it had already taken.
            let from_host = host.read(&mut usb_buf);
            let from_phone = ble.read(&mut ble_buf);
            // A bounded wait rather than an indefinite one, so that the loop
            // also serves as a housekeeping tick.
            let radio_irq = irq.wait_asserted(Duration::from_millis(50));
            select3(from_host, from_phone, radio_irq).await
        };

        // Bytes from either host go through the same steps; only the decoder
        // and the outbox differ, so that answers go back the way the command
        // came. `Reset` is the one action that has to flush before it acts,
        // and it flushes the transport that asked.
        let (bytes, dec, out, via) = match &event {
            Either3::First(n) => (&usb_buf[..*n], &mut decoder, &mut outbox, Via::Usb),
            Either3::Second(n) => (&ble_buf[..*n], &mut ble_decoder, &mut ble_outbox, Via::Ble),
            Either3::Third(_) => (&usb_buf[..0], &mut decoder, &mut outbox, Via::Usb),
        };
        for &byte in bytes {
            match dec.feed(byte) {
                kiss::Step::Pending => {}
                kiss::Step::Error(e) => {
                    defmt::warn!("kiss: {=str}", e.message());
                }
                kiss::Step::Frame => {
                    last_host_frame = Some(Instant::now());
                    let command = command::decode(dec.command(), dec.payload());
                    match protocol.handle(command, out) {
                        // Not a radio action, and not "write it out now"
                        // either: the timer restarts on every write, so a
                        // burst of them costs one erase.
                        Action::Persist => dirty_since = Some(Instant::now()),
                        // Everything queued is written before the reset,
                        // because the host asked for this and will look at the
                        // result afterwards.
                        Action::Reset => {
                            defmt::info!("reset requested by the host");
                            match via {
                                Via::Usb => flush(out, tx).await,
                                Via::Ble => {
                                    push_to_ble(out, ble_out);
                                    // Give the pump a moment to notify it.
                                    Timer::after(Duration::from_millis(200)).await;
                                }
                            }
                            commit(storage, &protocol);
                            boot::reboot();
                        }
                        // Force the next pass to redraw rather than waiting
                        // for the tick.
                        Action::Redraw => ui.redraw_now(),
                        // The pixels the host wants are the ones on the panel,
                        // and those are here rather than in the protocol.
                        Action::ReportDisplay => {
                            let mut image = [0u8; rnode_display::DISP_LEN];
                            rnode_display::read_display(&ui.live, &mut image);
                            out.frame(command::cmd::DISP_READ, &image);
                        }
                        action => {
                            act(
                                dev,
                                irq,
                                &mut protocol,
                                action,
                                &mut applied,
                                &mut receiving,
                                out,
                                &mut rx_buf,
                                &mut last_signal,
                                &mut air_in,
                                &mut sequence,
                            )
                            .await;
                        }
                    }
                }
            }
        }

        match event {
            Either3::First(_) | Either3::Second(_) => {}
            Either3::Third(Ok(())) => {
                if !receiving {
                    // The line is asserted and nothing is listening for it, so
                    // clearing it is the only thing that will put it down --
                    // and it *must* go down. `wait_asserted` is level
                    // triggered: on a line that is already high it returns
                    // immediately, forever, and this loop would then spin
                    // without ever yielding. Nothing else in the executor gets
                    // polled after that, USB included, and the board goes on
                    // enumerating while answering nothing.
                    //
                    // That is not hypothetical. It is what happened the first
                    // time this image was run against a host: `start_rx`
                    // failed, `receiving` stayed false, the chip's error
                    // interrupt stayed up, and both serial ports stopped
                    // opening while the device still showed as connected.
                    let mut modem = Modem::new(dev, irq);
                    if let Err(e) = modem.clear_interrupts().await {
                        defmt::error!("could not clear a stuck interrupt: {}", e);
                    }
                } else {
                    let mut modem = Modem::new(dev, irq);
                    // A frame nobody asked for goes to whoever is the host
                    // right now: the phone if there is one, USB otherwise.
                    let listener = if BLE_CONNECTED.load(Ordering::Relaxed) {
                        &mut ble_outbox
                    } else {
                        &mut outbox
                    };
                    match modem.receive(&mut rx_buf, Duration::from_millis(20)).await {
                        Ok(Some(report)) => {
                            led.off();
                            last_signal = Some((report.rssi_dbm, report.snr_quarter_db));
                            if heard(&mut air_in, &mut protocol, &report, &rx_buf, listener) {
                                last_host_frame = Some(Instant::now());
                            }
                            led.on();
                        }
                        Ok(None) => {}
                        Err(e) => {
                            defmt::error!("rx: {}", e);
                            protocol.report_error(error::MODEM_TIMEOUT, listener);
                        }
                    }
                }
            }
            Either3::Third(Err(_)) => {
                // Housekeeping tick. Nothing to do but keep the light moving,
                // which is the only sign of life this board has when the log
                // port is not open.
                if last_blink.elapsed() > Duration::from_secs(2) {
                    last_blink = Instant::now();
                    if protocol.radio_is_on() {
                        led.on();
                    } else {
                        led.off();
                    }
                }
            }
        }

        // Gestures from the pad go to the navigator; what it hands back is
        // done here, where the storage and the panel are.
        let mut chosen = heapless::Vec::<ui::Action, { pad::QUEUE }>::new();
        if ui.drain(pad_events, &mut chosen) {
            if let Some(panel) = panel.as_deref_mut() {
                ui.wake(panel).await;
            }
        }
        for action in chosen {
            let change = perform(
                action,
                &mut ui,
                panel.as_deref_mut(),
                storage,
                &mut protocol,
                host_of(control),
            )
            .await;
            if change.persist {
                dirty_since = Some(Instant::now());
            }
            if change.radio != Action::None {
                act(
                    dev,
                    irq,
                    &mut protocol,
                    change.radio,
                    &mut applied,
                    &mut receiving,
                    &mut outbox,
                    &mut rx_buf,
                    &mut last_signal,
                    &mut air_in,
                    &mut sequence,
                )
                .await;
            }
        }

        // Nobody has touched the pad for a minute: put the panel out. The
        // page keeps being drawn into `live` underneath, so the wake shows
        // what is true then rather than what was true at the sleep.
        if ui.idle() {
            ui.sleep(panel.as_deref_mut()).await;
        }

        // Draw, and send at most two pages of it. A full repaint is 218 ms on
        // this bus and the radio cannot be left that long, so the panel is
        // filled in over several passes -- under half a second for a whole
        // screen, and never away for more than 28 ms at a time.
        if let Some(panel) = panel.as_deref_mut() {
            if ui.due() {
                if protocol.external().enabled() {
                    protocol.external().draw(&mut ui.scratch);
                } else {
                    let state = facts.state(
                        &protocol,
                        air_of(&protocol, receiving),
                        host_of(control),
                        is_talking(last_host_frame),
                        last_signal,
                        battery.read().await,
                        battery.is_charging(),
                    );
                    screens::render(&mut ui.scratch, &mut ui.nav, &state);
                }
                ui.commit();
            }
            ui.flush(panel).await;
        }

        // A phone that has just paired goes into the record with everything
        // else, on the same timer. The write is one page erase, about 85 ms
        // of stalled CPU, inside a live connection: iOS's supervision timeout
        // is measured in seconds, so the link survives it. What would not
        // survive is a reset before it is written -- see `nus::take_new_bond`.
        if let Some(bond) = nus::take_new_bond() {
            protocol.store_mut().add_bond(bond);
            dirty_since = Some(Instant::now());
            // Redraw promptly: the passkey box has to go.
            ui.redraw_now();
        }

        // Write the EEPROM out once it has stopped changing. Checked on every
        // iteration rather than on the housekeeping tick, because during a
        // provisioning run there are host bytes arriving and the tick never
        // fires.
        if let Some(since) = dirty_since {
            if since.elapsed() > PERSIST_IDLE {
                commit(storage, &protocol);
                dirty_since = None;
            }
        }

        flush(&mut outbox, tx).await;
        push_to_ble(&mut ble_outbox, ble_out);

        // Belt and braces. Every path above is *supposed* to await something
        // that can actually pend, but a loop that can complete an iteration
        // with every future already resolved starves the executor rather than
        // merely running hot -- and the failure looks like dead hardware, not
        // like a busy one. One unconditional yield costs nothing and removes
        // the whole class.
        yield_now().await;
    }
}

/// Carry out whatever the protocol decided.
///
/// The last two arguments are for a transmission: the wait for a clear
/// channel is spent listening, and what it hears has to go somewhere.
/// A frame off the air: through the reassembler, and if it completes a
/// packet, to the host with its signal. Says whether it did.
///
/// The signal is per packet rather than per frame -- the mean of two for a
/// packet that came as two -- and quarter-dB throughout: the chip reports
/// it that way and the protocol carries it that way, so nothing is rounded
/// in between.
fn heard<S: Sink>(
    air_in: &mut Reassembler,
    protocol: &mut Protocol,
    report: &RxReport,
    rx_buf: &[u8],
    out: &mut S,
) -> bool {
    let signal = air::Signal {
        rssi_dbm: report.rssi_dbm,
        snr_quarter_db: report.snr_quarter_db,
    };
    match air_in.feed(&rx_buf[..report.len], signal, Instant::now().as_micros()) {
        Some(packet) => {
            protocol.received(
                packet.signal.rssi_dbm,
                packet.signal.snr_quarter_db,
                packet.payload,
                out,
            );
            true
        }
        None => {
            defmt::debug!(
                "rx: {=usize} bytes, half of a split packet; holding it",
                report.len
            );
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn act<S, B>(
    dev: &mut lr11xx::Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    protocol: &mut Protocol,
    action: Action<'_>,
    applied: &mut Option<ValidConfig>,
    receiving: &mut bool,
    outbox: &mut Outbox<OUTBOX>,
    rx_buf: &mut [u8],
    last_signal: &mut Option<(i16, i8)>,
    air_in: &mut Reassembler,
    sequence: &mut Sequence,
) where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    match action {
        Action::None => {}

        // Handled where the command was decoded, because none of these is
        // anything to do with the radio. Reported rather than ignored:
        // reaching here would mean a caller had forgotten one.
        Action::Persist | Action::Reset | Action::Redraw | Action::ReportDisplay => {
            defmt::error!("a storage or display action reached the radio path");
        }

        Action::Reconfigure => {
            let Some(valid) = protocol.valid_config() else {
                // Unreachable: the protocol only asks for this after
                // validating. Reported rather than ignored, because if it ever
                // happened the alternative is a radio configured with nothing.
                defmt::error!("reconfigure asked for with an invalid configuration");
                return;
            };
            defmt::info!(
                "config: {=u32} Hz ({=u32} Hz commanded), SF{=u8} BW{=u32} CR4/{=u8}, {=i8} dBm",
                valid.frequency_hz,
                valid.commanded_frequency_hz(),
                valid.spreading_factor,
                valid.bandwidth_hz,
                valid.coding_rate,
                valid.tx_power_dbm
            );
            let mut modem = Modem::new(dev, irq);
            if let Err(e) = modem.apply(&valid).await {
                defmt::error!("apply failed: {}", e);
                protocol.report_error(error::INITRADIO, outbox);
                *applied = None;
                *receiving = false;
                return;
            }
            if let Err(e) = modem.start_rx(&valid).await {
                defmt::error!("start_rx failed: {}", e);
                protocol.report_error(error::INITRADIO, outbox);
                *receiving = false;
                return;
            }
            *applied = Some(valid);
            *receiving = true;
            // The other half of a packet from the old channel is not coming
            // on the new one.
            air_in.set_max_age_us(air::max_age_us(&valid));
            air_in.clear();
        }

        Action::Standby => {
            let mut modem = Modem::new(dev, irq);
            if let Err(e) = modem.standby().await {
                defmt::error!("standby failed: {}", e);
            }
            *receiving = false;
            if let Some(reason) = protocol.last_error() {
                // With the numbers, because "above the rating" on its own
                // sends whoever reads it to the host's settings screen
                // without telling them what to type there.
                let c = protocol.config();
                defmt::warn!(
                    "radio stays off: {=str} (asked for {=u32} Hz, BW {=u32}, SF{=u8}, CR4/{=u8}, {=i8} dBm)",
                    reason.message(),
                    c.frequency_hz,
                    c.bandwidth_hz,
                    c.spreading_factor,
                    c.coding_rate,
                    c.tx_power_dbm
                );
            }
        }

        Action::Transmit(payload) => {
            let Some(valid) = *applied else {
                defmt::error!("asked to transmit before the radio was configured");
                return;
            };
            // One header byte in front, and two frames if it does not fit
            // in one; see `oxinode_core::rnode::air`. The decoder's capacity
            // is the longest packet a frame pair carries, so a packet that
            // does not split cannot reach here.
            let Some(split) = Split::new(payload, sequence.take()) else {
                defmt::error!(
                    "tx: {=usize} bytes is more than two frames; dropped",
                    payload.len()
                );
                return;
            };
            let mut frame = [0u8; air::FRAME_MAX];
            let Some(n) = split.frame(0, &mut frame) else {
                return;
            };
            // The wait for a clear channel is spent listening, and a packet
            // heard during it goes to the host that asked for this
            // transmission -- the same way its answers do. Then the
            // transmission is tried again, with the wait so far remembered.
            let mut backoff = Backoff::new(&valid, n as u8, Instant::now().as_ticks());
            let sent = loop {
                let mut modem = Modem::new(dev, irq);
                match modem
                    .transmit(&valid, &frame[..n], &mut backoff, rx_buf)
                    .await
                {
                    Ok(TxOutcome::Sent(report)) => break Ok(report),
                    Ok(TxOutcome::Heard(report)) => {
                        defmt::debug!(
                            "rx while waiting to transmit: {=usize} bytes, rssi {=i16} dBm",
                            report.len,
                            report.rssi_dbm
                        );
                        *last_signal = Some((report.rssi_dbm, report.snr_quarter_db));
                        heard(air_in, protocol, &report, rx_buf, outbox);
                    }
                    Err(e) => break Err(e),
                }
            };
            // The second frame follows the first with no wait: the receiver
            // is holding the first half, and a stock RNode holds it until
            // any other packet arrives and throws it away.
            let sent = match sent {
                Ok(report) if split.frames() == 2 => {
                    let Some(n) = split.frame(1, &mut frame) else {
                        return;
                    };
                    let mut modem = Modem::new(dev, irq);
                    match modem.send(&valid, &frame[..n], CsmaReport::default()).await {
                        Ok(second) => Ok(TxReport {
                            elapsed_us: report.elapsed_us + second.elapsed_us,
                            airtime_us: report.airtime_us + second.airtime_us,
                            ..report
                        }),
                        Err(e) => Err(e),
                    }
                }
                other => other,
            };
            match sent {
                Ok(report) => {
                    defmt::debug!(
                        "tx: {=usize} bytes in {=usize} frames, {=u32} us, after {=u32} senses ({=u32} busy) and {=u32} us waiting",
                        payload.len(),
                        split.frames(),
                        report.elapsed_us,
                        report.csma.senses,
                        report.csma.busy,
                        report.csma.waited_us
                    );
                    if report.csma.forced {
                        defmt::warn!(
                            "tx: the channel never read clear; sent after {=u32} us regardless",
                            report.csma.waited_us
                        );
                    }
                    // Counted and acknowledged before going back to receive, so
                    // a host using flow control is released as early as it can
                    // be rather than after the restart. A modem that never
                    // sends CMD_READY works perfectly until somebody turns
                    // flow control on, and then stops after one packet.
                    protocol.transmitted(outbox);
                }
                Err(e) => {
                    defmt::error!("tx failed: {}", e);
                    protocol.report_error(error::TXFAILED, outbox);
                }
            }
            // Transmitting leaves the chip in standby, so receive has to be
            // restarted explicitly or the modem goes deaf after its first
            // packet -- the kind of fault that looks like a range problem.
            if *receiving {
                let mut modem = Modem::new(dev, irq);
                if let Err(e) = modem.start_rx(&valid).await {
                    defmt::error!("could not return to receive: {}", e);
                    *receiving = false;
                }
            }
        }
    }
}

/// Answer a host when the radio never came up.
///
/// It still gets a detect response and a firmware version, because a host that
/// cannot detect the device reports "could not detect device" — which points at
/// the cable. Answering and then failing to configure points at the radio,
/// which is where the fault actually is.
#[allow(clippy::too_many_arguments)]
async fn serve_without_a_radio<'d, D>(
    tx: &mut Sender<'d, D>,
    host: &mut Reader<'_, NoopRawMutex, 1024>,
    ble: &mut Reader<'_, NoopRawMutex, BLE_IN>,
    ble_out: &Writer<'_, NoopRawMutex, BLE_OUT>,
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
    storage: &mut Storage<'_>,
    device: DeviceStore,
    mcu_id: u64,
    mut panel: Option<&mut Panel<'_>>,
    pad_events: &pad::Events,
    battery: &mut Sense<'_>,
) -> !
where
    D: UsbDriverTrait<'d>,
{
    let mut protocol = Protocol::with_storage(device, mcu_id);
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut outbox = Outbox::<OUTBOX>::new();
    let mut ble_decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut ble_outbox = Outbox::<OUTBOX>::new();
    let mut buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    let mut ble_buf = [0u8; 256];
    let mut dirty_since: Option<Instant> = None;
    // The same interface as with a radio, and every screen says "no radio"
    // where the radio would be -- which is the only place somebody holding
    // the board can be told.
    let mut ui = Ui::new();
    let mut last_host_frame: Option<Instant> = None;
    let facts = Facts::new(mcu_id);

    loop {
        if usb_log::is_bootloader_touch_tx(tx, control) {
            commit(storage, &protocol);
            boot::reboot_to_bootloader();
        }
        // Fast blink: alive, enumerated, no radio.
        led.on();
        let event = select3(
            host.read(&mut buf),
            ble.read(&mut ble_buf),
            Timer::after(Duration::from_millis(100)),
        )
        .await;
        let (bytes, dec, out, via) = match &event {
            Either3::First(n) => (&buf[..*n], &mut decoder, &mut outbox, Via::Usb),
            Either3::Second(n) => (&ble_buf[..*n], &mut ble_decoder, &mut ble_outbox, Via::Ble),
            Either3::Third(()) => (&buf[..0], &mut decoder, &mut outbox, Via::Usb),
        };
        {
            for &byte in bytes {
                if dec.feed(byte) == kiss::Step::Frame {
                    last_host_frame = Some(Instant::now());
                    let command = command::decode(dec.command(), dec.payload());
                    match protocol.handle(command, out) {
                        Action::None => {}
                        // Provisioning has nothing to do with the radio, and
                        // works here exactly as it does when one came up. A
                        // board with a dead radio can still be given an
                        // identity, and refusing would be inventing a
                        // dependency that is not there.
                        Action::Persist => dirty_since = Some(Instant::now()),
                        Action::Reset => {
                            match via {
                                Via::Usb => flush(out, tx).await,
                                Via::Ble => {
                                    push_to_ble(out, ble_out);
                                    Timer::after(Duration::from_millis(200)).await;
                                }
                            }
                            commit(storage, &protocol);
                            boot::reboot();
                        }
                        // The display works whether or not the radio does, and
                        // a host reading the screen should get the screen.
                        Action::Redraw => ui.redraw_now(),
                        Action::ReportDisplay => {
                            let mut image = [0u8; rnode_display::DISP_LEN];
                            rnode_display::read_display(&ui.live, &mut image);
                            out.frame(command::cmd::DISP_READ, &image);
                        }
                        // Anything that needed the radio: say why it cannot
                        // happen, rather than leaving the host to time out.
                        _ => protocol.report_error(error::INITRADIO, out),
                    }
                }
            }
        }
        if let Some(since) = dirty_since {
            if since.elapsed() > PERSIST_IDLE {
                commit(storage, &protocol);
                dirty_since = None;
            }
        }
        let mut chosen = heapless::Vec::<ui::Action, { pad::QUEUE }>::new();
        if ui.drain(pad_events, &mut chosen) {
            if let Some(panel) = panel.as_deref_mut() {
                ui.wake(panel).await;
            }
        }
        for action in chosen {
            let change = perform(
                action,
                &mut ui,
                panel.as_deref_mut(),
                storage,
                &mut protocol,
                host_of(control),
            )
            .await;
            if change.persist {
                dirty_since = Some(Instant::now());
            }
            // The setting has landed and, in TNC mode, been stored; the
            // screen shows it. What cannot happen is the radio being told.
            if change.radio != Action::None {
                defmt::info!("ui: no radio to apply that to");
            }
        }
        if ui.idle() {
            ui.sleep(panel.as_deref_mut()).await;
        }
        if let Some(panel) = panel.as_deref_mut() {
            if ui.due() {
                if protocol.external().enabled() {
                    protocol.external().draw(&mut ui.scratch);
                } else {
                    let state = facts.state(
                        &protocol,
                        Air::NoRadio,
                        host_of(control),
                        is_talking(last_host_frame),
                        None,
                        battery.read().await,
                        battery.is_charging(),
                    );
                    screens::render(&mut ui.scratch, &mut ui.nav, &state);
                }
                ui.commit();
            }
            ui.flush(panel).await;
        }
        led.off();
        Timer::after(Duration::from_millis(100)).await;
        flush(&mut outbox, tx).await;
        push_to_ble(&mut ble_outbox, ble_out);
    }
}

/// How often the screen is redrawn.
///
/// Half a second: fast enough that a packet counter looks live, slow enough
/// that the rendering and the diff are lost in the noise next to the radio.
/// A gesture brings the next redraw forward, so the interface never waits on
/// this.
const RENDER_INTERVAL: Duration = Duration::from_millis(500);

/// How long after the last frame the Home screen still says "talking".
const TALKING_FOR: Duration = Duration::from_secs(10);

/// How long the panel stays on with nobody touching the pad.
///
/// A minute, measured from boot and from the last gesture. The OLED is the
/// one part of the board that wears with use and the only one a person has
/// to be looking at to be worth lighting; a modem left on a desk has neither.
/// The next press wakes it and is otherwise ignored, exactly as after
/// `Sleep Screen`.
const IDLE_SLEEP: Duration = Duration::from_secs(60);

/// Contrast the panel starts at: the SH1107's own power-on value, so a host
/// that never sends `CMD_DISP_INT` gets what the part was designed for.
const fn status_intensity() -> u8 {
    0x80
}

/// The panel's side of the interface.
///
/// `live` is what the controller has been sent; `scratch` is where a page is
/// drawn before being committed by comparison, so an update costs the pages
/// that changed rather than the pages that were redrawn -- see
/// `sh1107::Frame::copy_from`. A screen change is therefore not a full-panel
/// flush: the title and the strip and the rows that differ, and nothing
/// else. Nothing here marks the whole panel dirty except the user, from the
/// menu item that exists for it.
struct Ui {
    nav: Nav,
    live: sh1107::Frame,
    scratch: sh1107::Frame,
    /// `Sleep Screen` was chosen, or the pad went untouched for
    /// `IDLE_SLEEP`: the panel is off until the next gesture, which wakes
    /// it and is otherwise ignored.
    asleep: bool,
    last_render: Instant,
    /// When the pad was last touched, for the idle sleep. Starts at boot,
    /// so a board nobody picks up goes dark a minute after it lights.
    last_gesture: Instant,
}

impl Ui {
    fn new() -> Self {
        Ui {
            nav: ui::nav(),
            live: sh1107::Frame::new(),
            scratch: sh1107::Frame::new(),
            asleep: false,
            last_render: Instant::now() - RENDER_INTERVAL,
            last_gesture: Instant::now(),
        }
    }

    /// Whether the pad has been left alone long enough to put the panel out.
    fn idle(&self) -> bool {
        !self.asleep && self.last_gesture.elapsed() >= IDLE_SLEEP
    }

    /// Whether the tick has come round.
    fn due(&self) -> bool {
        self.last_render.elapsed() >= RENDER_INTERVAL
    }

    /// Bring the next redraw forward to the next pass.
    fn redraw_now(&mut self) {
        self.last_render = Instant::now() - RENDER_INTERVAL;
    }

    /// The page in `scratch` is the one to show: mark what changed.
    fn commit(&mut self) {
        self.last_render = Instant::now();
        self.live.copy_from(&self.scratch);
    }

    /// Send at most two dirty pages, unless the screen is asleep.
    async fn flush(&mut self, panel: &mut Panel<'_>) {
        if self.asleep || self.live.is_clean() {
            return;
        }
        if let Err(e) = panel.flush_pages(&mut self.live, 2).await {
            defmt::error!("panel: {}", e);
        }
    }

    /// Take every waiting gesture: a sleeping screen wakes and swallows the
    /// press, an awake one hands it to the navigator. Returns whether the
    /// screen has to be woken.
    fn drain(
        &mut self,
        events: &pad::Events,
        chosen: &mut heapless::Vec<ui::Action, { pad::QUEUE }>,
    ) -> bool {
        let mut woke = false;
        let taken = pad::drain(events, |input| {
            defmt::debug!("ui: {=str}", input.name());
            if self.asleep {
                self.asleep = false;
                woke = true;
            } else if let Some(action) = self.nav.handle(input) {
                // The channel and this vector are the same size, so a push
                // cannot fail; if it ever did, dropping the action would be
                // the right thing anyway.
                let _ = chosen.push(action);
            }
        });
        if taken > 0 {
            self.last_gesture = Instant::now();
            self.redraw_now();
        }
        woke
    }

    /// Put the panel out. Its RAM keeps the page, so waking is one command
    /// and the pages that changed in the meantime.
    async fn sleep(&mut self, panel: Option<&mut Panel<'_>>) {
        self.asleep = true;
        if let Some(panel) = panel {
            if let Err(e) = panel.power(false).await {
                defmt::error!("panel: could not sleep: {}", e);
            }
        }
    }

    /// Put the panel back on after a sleep.
    async fn wake(&mut self, panel: &mut Panel<'_>) {
        if let Err(e) = panel.power(true).await {
            defmt::error!("panel: could not wake: {}", e);
        }
        // Whatever changed while it was off has been accumulating in `live`
        // and goes out on the next flushes.
        self.redraw_now();
    }
}

/// Do what a menu item asked for.
///
/// The phase 12 rule is applied here and nowhere else in the firmware: an
/// action that would change the live radio while a host has the line gets
/// the notice instead of being done. Everything else is done as asked. What
/// the radio has to do about it, and whether the record changed, are handed
/// back for the loop, which is where the modem and the persist timer are.
async fn perform(
    action: ui::Action,
    ui: &mut Ui,
    panel: Option<&mut Panel<'_>>,
    storage: &mut Storage<'_>,
    protocol: &mut Protocol,
    host: Host,
) -> PanelChange {
    if let Some(lock) = host.lock() {
        if action.changes_the_radio() {
            defmt::info!(
                "ui: {=str} refused, a host has the radio ({=str})",
                action.name(),
                lock.headline()
            );
            ui.nav.notice(lock);
            return PanelChange::NONE;
        }
    }
    match action {
        // A full repaint, which is the one thing that is allowed to cost
        // one: the user asked.
        ui::Action::Redraw => {
            ui.live.mark_all_dirty();
            ui.redraw_now();
            PanelChange::NONE
        }
        ui::Action::SleepScreen => {
            ui.sleep(panel).await;
            PanelChange::NONE
        }
        // Both restarts write the record first, for the reason the host's
        // reset does: a reboot is exactly when losing a provisioning would
        // be least welcome.
        ui::Action::Reboot => {
            defmt::info!("ui: reboot");
            commit(storage, protocol);
            boot::reboot();
        }
        ui::Action::Bootloader => {
            defmt::info!("ui: bootloader");
            commit(storage, protocol);
            boot::reboot_to_bootloader();
        }
        // Nobody has the radio, or this would have been the notice above.
        ui::Action::Edit(field) => {
            defmt::info!("ui: editing {=str}", field.name());
            ui.nav.edit(Editor::open(field, *protocol.config()));
            PanelChange::NONE
        }
        ui::Action::Set(setting) => {
            let change = protocol.set_from_panel(setting);
            defmt::info!(
                "ui: set {=str}; stored={=bool}",
                setting.name(),
                change.persist
            );
            change
        }
        ui::Action::ToggleRadio => {
            let radio = protocol.toggle_from_panel();
            defmt::info!(
                "ui: radio {=str}",
                if protocol.radio_is_on() { "on" } else { "off" }
            );
            PanelChange {
                radio,
                persist: false,
            }
        }
        ui::Action::ResetRadioConfig => {
            let radio = protocol.reset_from_panel();
            defmt::info!(
                "ui: reset config to {=str}",
                if protocol.is_tnc() {
                    "stored"
                } else {
                    "default"
                }
            );
            PanelChange {
                radio,
                persist: false,
            }
        }
        ui::Action::SaveRadioConfig => {
            defmt::info!("ui: save config; the board is a TNC");
            protocol.save_from_panel()
        }
        // The record first, so a reset before the stack has caught up
        // still forgets them; then the stack, which only its own task can
        // reach.
        ui::Action::ForgetBonds => {
            defmt::info!("ui: forget phones");
            protocol.store_mut().clear_bonds();
            FORGET_BONDS.signal(());
            PanelChange {
                radio: Action::None,
                persist: true,
            }
        }
        // The receiver is the GPS task's; this only asks.
        ui::Action::ToggleGps => {
            defmt::info!("ui: gps on/off");
            gps::toggle();
            PanelChange::NONE
        }
    }
}

/// What the screens know that never changes: the two names the board has.
struct Facts {
    serial: [u8; 16],
    ble_name: [u8; interop::NAME_LEN],
}

impl Facts {
    fn new(mcu_id: u64) -> Self {
        Facts {
            serial: oxinode_core::serial::hex_u64(mcu_id),
            ble_name: interop::advertised_name(mcu_id),
        }
    }

    /// Copy what the screens are allowed to know out of the modem loop.
    ///
    /// Plain values, every one. The renderer is handed this and nothing
    /// else, which is what keeps the render path out of the modem.
    #[allow(clippy::too_many_arguments)]
    fn state(
        &self,
        protocol: &Protocol,
        air: Air,
        host: Host,
        talking: bool,
        last_signal: Option<(i16, i8)>,
        battery: Option<oxinode_core::battery::Reading>,
        charging: bool,
    ) -> screens::State {
        let (rx_count, tx_count) = protocol.counters();
        let link = bluetooth_state();
        screens::State {
            home: screens::Home {
                host,
                talking,
                air,
                rx_count,
                tx_count,
                last_rssi_dbm: last_signal.map(|(rssi, _)| rssi),
                last_snr_quarter_db: last_signal.map(|(_, snr)| snr),
                uptime_s: Some(Instant::now().as_secs().min(u32::MAX as u64) as u32),
                battery,
                charging,
            },
            radio: screens::Radio {
                config: *protocol.config(),
                air,
                tnc: protocol.is_tnc(),
            },
            bluetooth: screens::BluetoothScreen {
                link,
                name: (link != Bluetooth::Absent).then_some(self.ble_name),
                passkey: passkey_state(),
                bonded: protocol.store().bonds().count() as u8,
            },
            position: gps::position(),
            system: screens::System {
                version: env!("CARGO_PKG_VERSION"),
                serial: Some(self.serial),
                identity: identity_of(protocol),
                free_ram: board::free_ram_bytes(),
            },
        }
    }
}

/// What the radio is doing, from what it was asked and whether that took.
fn air_of(protocol: &Protocol, receiving: bool) -> Air {
    if receiving {
        Air::Receiving
    } else if protocol.radio_is_on() {
        // Asked for and valid, and the chip would not take it.
        Air::Failed
    } else if let Some(reason) = protocol.last_error() {
        Air::Refused(reason)
    } else {
        Air::Off
    }
}

/// Who has the line: a connected phone, else whoever has the KISS port open.
fn host_of(control: &ControlChanged<'_>) -> Host {
    if BLE_CONNECTED.load(Ordering::Relaxed) {
        Host::Bluetooth
    } else if control.dtr() {
        Host::Usb
    } else {
        Host::None
    }
}

/// Whether a frame has passed recently enough to call the link live.
fn is_talking(last_host_frame: Option<Instant>) -> bool {
    last_host_frame.is_some_and(|at| at.elapsed() < TALKING_FOR)
}

/// What the device can say about its own identity. It cannot check the RSA
/// signature -- that needs the host's public key -- so "signed" means one is
/// stored; the checksum is the part it verifies itself.
fn identity_of(protocol: &Protocol) -> Identity {
    match protocol.rom().status() {
        eeprom::Status::Unprovisioned => Identity::None,
        eeprom::Status::ChecksumMismatch => Identity::BadChecksum,
        eeprom::Status::Provisioned => {
            if protocol.store().device_signature.is_some() {
                Identity::Signed
            } else {
                Identity::Unsigned
            }
        }
    }
}

/// Write the device record out, and say so if it will not go.
///
/// There is nothing useful to do about a failure here, which is why this
/// returns nothing: the host is about to read the image back and will find it
/// unchanged. What matters is that the log says which of the two happened,
/// because "the provisioning did not stick" and "the flash write failed" look
/// identical from the other end of the serial port.
fn commit(storage: &mut Storage<'_>, protocol: &Protocol) {
    if let Err(e) = storage.save(protocol.store()) {
        defmt::error!("store: could not write the device record: {}", e);
    }
}

/// Write everything an outbox holds to a USB endpoint, then report anything lost.
///
/// The buffer itself is `oxinode_core::rnode::outbox::Outbox`, shared with the
/// Bluetooth transport; this is the part that knows about 64-byte packets.
async fn flush<'d, D: UsbDriverTrait<'d>, const N: usize>(
    outbox: &mut Outbox<N>,
    tx: &mut Sender<'d, D>,
) {
    let max = usb_log::MAX_PACKET_SIZE as usize;
    let total = outbox.len();
    while !outbox.is_empty() {
        let n = outbox.len().min(max);
        // Bounded, because `write_packet` waits for the host to ask for the
        // data and a host that has closed the port never will. Without a
        // bound the modem stops servicing the radio the moment somebody
        // disconnects, and only starts again if they come back.
        match with_timeout(
            Duration::from_millis(500),
            tx.write_packet(&outbox.pending()[..n]),
        )
        .await
        {
            Ok(Ok(())) => outbox.consume(n),
            // The host went away mid-frame. Everything queued is part of a
            // conversation nobody is having; drop it rather than delivering
            // half a frame to whoever connects next.
            Ok(Err(_)) | Err(_) => {
                outbox.abandon();
                break;
            }
        }
    }
    // A full-size final packet needs a zero-length packet behind it, or the
    // host waits for the rest of a transfer that is already complete.
    if total != 0 && total % max == 0 && outbox.is_empty() {
        let _ = with_timeout(Duration::from_millis(500), tx.write_packet(&[])).await;
    }
    let dropped = outbox.take_dropped();
    if dropped > 0 {
        defmt::warn!("outbox: {=u32} frames dropped", dropped);
    }
}

// The outbox must hold a worst-case data frame whole, or a packet of entirely
// escapable bytes -- which encrypted traffic eventually produces -- could never
// be delivered at all.
const _: () = assert!(OUTBOX >= 2 * kiss::HW_MTU + 3);
// And a worst-case display read, which is larger.
const _: () = assert!(OUTBOX >= 2 * rnode_display::DISP_LEN + 3);
