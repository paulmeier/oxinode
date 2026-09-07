//! The RNode image: a Reticulum host opens the serial port and finds a modem.
//!
//! Two CDC-ACM ports. The **first** carries the KISS stream and is the one
//! `rnsd` is pointed at; the **second** carries the defmt log. That order is
//! the contract with the host — it decides which tty gets the lower number —
//! and it is also forced by DTR, which is only visible on the first CDC
//! function of a composite device. The KISS port needs DTR for the 1200-baud
//! bootloader touch, so it has to be first, and the log port therefore cannot
//! have it.
//!
//! # What this image does that `radio` does not
//!
//! `radio` is a diagnostic: it reports the evidence for every bring-up step and
//! keeps going when one fails, so the next can be tried anyway. This one comes
//! up or says why not, and then talks to a host. Neither is a better version of
//! the other.
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

use embassy_executor::Spawner;
use embassy_futures::join::join4;
use embassy_futures::select::{select, Either};
use embassy_futures::yield_now;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, spim, twim};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::pipe::{Pipe, Reader};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Sender, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::display::{self, Boost, Panel};
use oxinode::modem::Modem;
use oxinode::store::Storage;
use oxinode::{boot, bringup, radio, usb_log};
use oxinode_core::lr1121::config::ValidConfig;
use oxinode_core::rnode::command::{self, error};
use oxinode_core::rnode::display as rnode_display;
use oxinode_core::rnode::kiss;
use oxinode_core::rnode::outbox::Outbox;
use oxinode_core::rnode::protocol::{Action, Protocol, Sink};
use oxinode_core::rnode::store::DeviceStore;
use oxinode_core::{sh1107, status};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

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
    let p = embassy_nrf::init(board::embassy_config());

    let spi = radio::new_spi(p.SPI2, Irqs, p.P1_13, p.P1_15, p.P1_14, p.P1_12);
    let reset = radio::RadioReset::new(p.P1_10, p.P1_11);
    let mut irq = radio::RadioIrq::new(p.P1_08);

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
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
    let (mut log_tx, _, _) = logs.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    // Read before anything else touches it. What comes back is either a record
    // this firmware wrote, or the state of a board nobody has provisioned --
    // there is no third answer; see `oxinode::store`.
    let mut storage = Storage::new(p.NVMC);
    let device = storage.load();
    let mcu_id = board::device_id();

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
    // `|| true` rather than waiting for DTR: this image's log is continuous
    // rather than a one-shot startup sequence, and the port that has DTR is
    // the KISS port, not this one.
    let pump = usb_log::pump(&mut log_tx, || true);

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
                    &control,
                    &mut led,
                    &mut storage,
                    device,
                    mcu_id,
                    panel.as_mut(),
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
            &control,
            &mut led,
            &mut storage,
            device,
            mcu_id,
            panel.as_mut(),
        )
        .await
    };

    join4(run_usb, pump, feed_host_rx, modem).await;
}

/// The main loop: host bytes in one direction, radio packets in the other.
#[allow(clippy::too_many_arguments)]
async fn run<'d, D, S, B>(
    dev: &mut lr11xx::Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    tx: &mut Sender<'d, D>,
    host: &mut Reader<'_, NoopRawMutex, 1024>,
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
    storage: &mut Storage<'_>,
    device: DeviceStore,
    mcu_id: u64,
    mut panel: Option<&mut Panel<'_>>,
) -> !
where
    D: UsbDriverTrait<'d>,
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let mut protocol = Protocol::with_storage(device, mcu_id);
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut outbox = Outbox::<OUTBOX>::new();
    let mut usb_buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    let mut rx_buf = [0u8; kiss::HW_MTU];
    // When the EEPROM was last changed, and therefore when it should be
    // written out. See `PERSIST_IDLE`.
    let mut dirty_since: Option<Instant> = None;

    // The panel's state. `live` is what the controller has been sent; `scratch`
    // is where a page is drawn before being committed by comparison, so an
    // update costs the pages that changed rather than the pages that were
    // redrawn. See `sh1107::Frame::copy_from`.
    let mut live = sh1107::Frame::new();
    let mut scratch = sh1107::Frame::new();
    let mut last_render = Instant::now() - RENDER_INTERVAL;
    let mut last_signal: Option<(i16, i8)> = None;
    let mut screen_name = [b'-'; 4];
    for (slot, byte) in screen_name.iter_mut().zip(serial_tail(mcu_id)) {
        *slot = byte;
    }
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
            // A bounded wait rather than an indefinite one, so that the loop
            // also serves as a housekeeping tick.
            let radio_irq = irq.wait_asserted(Duration::from_millis(50));
            select(from_host, radio_irq).await
        };

        match event {
            Either::First(n) => {
                for &byte in &usb_buf[..n] {
                    match decoder.feed(byte) {
                        kiss::Step::Pending => {}
                        kiss::Step::Error(e) => {
                            defmt::warn!("kiss: {=str}", e.message());
                        }
                        kiss::Step::Frame => {
                            let command = command::decode(decoder.command(), decoder.payload());
                            match protocol.handle(command, &mut outbox) {
                                // Not a radio action, and not "write it out
                                // now" either: the timer restarts on every
                                // write, so a burst of them costs one erase.
                                Action::Persist => dirty_since = Some(Instant::now()),
                                // Everything queued is written before the
                                // reset, because the host asked for this and
                                // will look at the result afterwards.
                                Action::Reset => {
                                    defmt::info!("reset requested by the host");
                                    flush(&mut outbox, tx).await;
                                    commit(storage, &protocol);
                                    boot::reboot();
                                }
                                // Force the next pass to redraw rather than
                                // waiting for the tick.
                                Action::Redraw => {
                                    last_render = Instant::now() - RENDER_INTERVAL;
                                }
                                // The pixels the host wants are the ones on
                                // the panel, and those are here rather than in
                                // the protocol.
                                Action::ReportDisplay => {
                                    let mut image = [0u8; rnode_display::DISP_LEN];
                                    rnode_display::read_display(&live, &mut image);
                                    outbox.frame(command::cmd::DISP_READ, &image);
                                }
                                action => {
                                    act(
                                        dev,
                                        irq,
                                        &mut protocol,
                                        action,
                                        &mut applied,
                                        &mut receiving,
                                        &mut outbox,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
            Either::Second(Ok(())) => {
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
                    match modem.receive(&mut rx_buf, Duration::from_millis(20)).await {
                        Ok(Some(report)) => {
                            led.off();
                            last_signal = Some((report.rssi_dbm, report.snr_quarter_db));
                            protocol.received(
                                report.rssi_dbm,
                                // Quarter-dB throughout: the chip reports it
                                // that way and the protocol carries it that
                                // way, so nothing is rounded in between.
                                report.snr_quarter_db,
                                &rx_buf[..report.len],
                                &mut outbox,
                            );
                            led.on();
                        }
                        Ok(None) => {}
                        Err(e) => {
                            defmt::error!("rx: {}", e);
                            protocol.report_error(error::MODEM_TIMEOUT, &mut outbox);
                        }
                    }
                }
            }
            Either::Second(Err(_)) => {
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

        // Draw, and send at most two pages of it. A full repaint is 218 ms on
        // this bus and the radio cannot be left that long, so the panel is
        // filled in over several passes -- under half a second for a whole
        // screen, and never away for more than 28 ms at a time.
        if let Some(panel) = panel.as_deref_mut() {
            if last_render.elapsed() >= RENDER_INTERVAL {
                last_render = Instant::now();
                if protocol.external().enabled() {
                    protocol.external().draw(&mut scratch);
                } else {
                    let (rx, tx_count) = protocol.counters();
                    let config = protocol.config();
                    status::render(
                        &status::Status {
                            name: screen_name,
                            frequency_hz: config.frequency_hz,
                            bandwidth_hz: config.bandwidth_hz,
                            spreading_factor: config.spreading_factor,
                            coding_rate: config.coding_rate,
                            tx_power_dbm: config.tx_power_dbm,
                            radio_on: protocol.radio_is_on(),
                            tnc: protocol.is_tnc(),
                            provisioned: protocol.rom().is_provisioned(),
                            rx_count: rx,
                            tx_count,
                            last_rssi_dbm: last_signal.map(|(rssi, _)| rssi),
                            last_snr_quarter_db: last_signal.map(|(_, snr)| snr),
                        },
                        &mut scratch,
                    );
                }
                live.copy_from(&scratch);
            }
            if !live.is_clean() {
                if let Err(e) = panel.flush_pages(&mut live, 2).await {
                    defmt::error!("panel: {}", e);
                }
            }
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
async fn act<S, B>(
    dev: &mut lr11xx::Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    protocol: &mut Protocol,
    action: Action<'_>,
    applied: &mut Option<ValidConfig>,
    receiving: &mut bool,
    outbox: &mut Outbox<OUTBOX>,
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
        }

        Action::Standby => {
            let mut modem = Modem::new(dev, irq);
            if let Err(e) = modem.standby().await {
                defmt::error!("standby failed: {}", e);
            }
            *receiving = false;
            if let Some(reason) = protocol.last_error() {
                defmt::warn!("radio stays off: {=str}", reason.message());
            }
        }

        Action::Transmit(payload) => {
            let Some(valid) = *applied else {
                defmt::error!("asked to transmit before the radio was configured");
                return;
            };
            let mut modem = Modem::new(dev, irq);
            match modem.transmit(&valid, payload).await {
                Ok(report) => {
                    defmt::debug!(
                        "tx: {=usize} bytes in {=u32} us",
                        payload.len(),
                        report.elapsed_us
                    );
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
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
    storage: &mut Storage<'_>,
    device: DeviceStore,
    mcu_id: u64,
    panel: Option<&mut Panel<'_>>,
) -> !
where
    D: UsbDriverTrait<'d>,
{
    let mut protocol = Protocol::with_storage(device, mcu_id);
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut outbox = Outbox::<OUTBOX>::new();
    let mut buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    let mut dirty_since: Option<Instant> = None;

    // Say so on the panel, which is the only place somebody holding the board
    // can be told.
    if let Some(panel) = panel {
        let mut frame = sh1107::Frame::new();
        let mut page = status::Status::new();
        page.name = *b"DEAD";
        page.provisioned = protocol.rom().is_provisioned();
        status::render(&page, &mut frame);
        oxinode_core::font::draw(&mut frame, 4, 118, "NO RADIO", true);
        let _ = panel.flush(&mut frame).await;
    }

    loop {
        if usb_log::is_bootloader_touch_tx(tx, control) {
            commit(storage, &protocol);
            boot::reboot_to_bootloader();
        }
        // Fast blink: alive, enumerated, no radio.
        led.on();
        let read = host.read(&mut buf);
        if let Either::First(n) = select(read, Timer::after(Duration::from_millis(100))).await {
            for &byte in &buf[..n] {
                if decoder.feed(byte) == kiss::Step::Frame {
                    let command = command::decode(decoder.command(), decoder.payload());
                    match protocol.handle(command, &mut outbox) {
                        Action::None => {}
                        // Provisioning has nothing to do with the radio, and
                        // works here exactly as it does when one came up. A
                        // board with a dead radio can still be given an
                        // identity, and refusing would be inventing a
                        // dependency that is not there.
                        Action::Persist => dirty_since = Some(Instant::now()),
                        Action::Reset => {
                            flush(&mut outbox, tx).await;
                            commit(storage, &protocol);
                            boot::reboot();
                        }
                        // The display works whether or not the radio does, and
                        // a host reading the screen should get the screen.
                        Action::Redraw => {}
                        Action::ReportDisplay => {
                            let mut image = [0u8; rnode_display::DISP_LEN];
                            let mut frame = sh1107::Frame::new();
                            let mut page = status::Status::new();
                            page.name = *b"DEAD";
                            status::render(&page, &mut frame);
                            rnode_display::read_display(&frame, &mut image);
                            outbox.frame(command::cmd::DISP_READ, &image);
                        }
                        // Anything that needed the radio: say why it cannot
                        // happen, rather than leaving the host to time out.
                        _ => protocol.report_error(error::INITRADIO, &mut outbox),
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
        led.off();
        Timer::after(Duration::from_millis(100)).await;
        flush(&mut outbox, tx).await;
    }
}

/// How often the status page is redrawn.
///
/// Half a second: fast enough that a packet counter looks live, slow enough
/// that the rendering and the diff are lost in the noise next to the radio.
const RENDER_INTERVAL: Duration = Duration::from_millis(500);

/// Contrast the panel starts at: the SH1107's own power-on value, so a host
/// that never sends `CMD_DISP_INT` gets what the part was designed for.
const fn status_intensity() -> u8 {
    0x80
}

/// The last four hex digits of the device ID, which is what the host names the
/// port with and therefore what a person can match the board against.
fn serial_tail(mcu_id: u64) -> [u8; 4] {
    let hex = oxinode_core::serial::hex_u64(mcu_id);
    [hex[12], hex[13], hex[14], hex[15]]
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
