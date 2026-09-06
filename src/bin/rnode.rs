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
use embassy_futures::join::join3;
use embassy_futures::select::{select, Either};
use embassy_futures::yield_now;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, spim};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::modem::Modem;
use oxinode::{boot, bringup, radio, usb_log};
use oxinode_core::lr1121::config::ValidConfig;
use oxinode_core::rnode::command::{self, error};
use oxinode_core::rnode::kiss;
use oxinode_core::rnode::protocol::{Action, Protocol, Sink};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
});

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0003;

/// How much unsent response can pile up.
///
/// One worst-case data frame is 1019 bytes: 508 bytes of payload where every
/// byte needs escaping, plus the command and two delimiters. This holds that
/// and a little more, so a full packet can always be queued whole — which
/// matters because [`Outbox`] drops whole frames rather than truncating them.
const OUTBOX: usize = 2048;

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

    let run_usb = usb.run();
    // `|| true` rather than waiting for DTR: this image's log is continuous
    // rather than a one-shot startup sequence, and the port that has DTR is
    // the KISS port, not this one.
    let pump = usb_log::pump(&mut log_tx, || true);

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

        let mut dev = match bringup::bring_up(spi, reset, &mut irq).await {
            Ok(dev) => dev,
            Err(e) => {
                // Answer anyway. A host that connects to a board whose radio is
                // dead should be told so rather than left waiting: the protocol
                // has a frame for exactly this, and it makes Reticulum say
                // "hardware initialisation error" instead of timing out.
                defmt::error!("radio did not come up: {}", e);
                serve_without_a_radio(&mut kiss_tx, &mut kiss_rx, &control, &mut led).await;
            }
        };

        led.on();
        run(
            &mut dev,
            &mut irq,
            &mut kiss_tx,
            &mut kiss_rx,
            &control,
            &mut led,
        )
        .await
    };

    join3(run_usb, pump, modem).await;
}

/// The main loop: host bytes in one direction, radio packets in the other.
#[allow(clippy::too_many_arguments)]
async fn run<'d, D, S, B>(
    dev: &mut lr11xx::Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    tx: &mut Sender<'d, D>,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
) -> !
where
    D: UsbDriverTrait<'d>,
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let mut protocol = Protocol::new();
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut outbox = Outbox::<OUTBOX>::new();
    let mut usb_buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    let mut rx_buf = [0u8; kiss::HW_MTU];
    // What the radio is currently programmed with, so a configuration is not
    // reprogrammed on every packet -- and, more to the point, so that a change
    // is applied exactly once and can be logged when it happens.
    let mut applied: Option<ValidConfig> = None;
    let mut receiving = false;
    let mut last_blink = Instant::now();

    loop {
        // A touch can arrive at any moment, including while a packet is in the
        // air, so it is level-triggered and re-checked on every wake rather
        // than waited for on an edge.
        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }

        let event = {
            let host = rx.read_packet(&mut usb_buf);
            // A bounded wait rather than an indefinite one, so that the loop
            // also serves as a housekeeping tick.
            let radio_irq = irq.wait_asserted(Duration::from_millis(50));
            select(host, radio_irq).await
        };

        match event {
            Either::First(Ok(n)) => {
                for &byte in &usb_buf[..n] {
                    match decoder.feed(byte) {
                        kiss::Step::Pending => {}
                        kiss::Step::Error(e) => {
                            defmt::warn!("kiss: {=str}", e.message());
                        }
                        kiss::Step::Frame => {
                            let command = command::decode(decoder.command(), decoder.payload());
                            let action = protocol.handle(command, &mut outbox);
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
            // The host closed the port. Not an error: it will come back.
            Either::First(Err(_)) => {}

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

        outbox.flush(tx).await;

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
async fn serve_without_a_radio<'d, D>(
    tx: &mut Sender<'d, D>,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
    led: &mut Led<'_>,
) -> !
where
    D: UsbDriverTrait<'d>,
{
    let mut protocol = Protocol::new();
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let mut outbox = Outbox::<OUTBOX>::new();
    let mut buf = [0u8; usb_log::MAX_PACKET_SIZE as usize];
    loop {
        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }
        // Fast blink: alive, enumerated, no radio.
        led.on();
        let read = rx.read_packet(&mut buf);
        if let Either::First(Ok(n)) = select(read, Timer::after(Duration::from_millis(100))).await {
            for &byte in &buf[..n] {
                if decoder.feed(byte) == kiss::Step::Frame {
                    let command = command::decode(decoder.command(), decoder.payload());
                    let action = protocol.handle(command, &mut outbox);
                    if action != Action::None {
                        protocol.report_error(error::INITRADIO, &mut outbox);
                    }
                }
            }
        }
        led.off();
        Timer::after(Duration::from_millis(100)).await;
        outbox.flush(tx).await;
    }
}

/// Response frames waiting to go to the host.
///
/// Whole frames or nothing. A byte ring that dropped its oldest bytes — which
/// is right for a log, and is what `logbuf` does — would splice two frames
/// together here and hand the host a packet made of two halves. So an
/// overflowing frame is dropped entire and counted.
struct Outbox<const N: usize> {
    buf: [u8; N],
    len: usize,
    dropped: u32,
}

impl<const N: usize> Outbox<N> {
    const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            dropped: 0,
        }
    }

    /// Write everything queued, then report anything lost.
    async fn flush<'d, D: UsbDriverTrait<'d>>(&mut self, tx: &mut Sender<'d, D>) {
        let max = usb_log::MAX_PACKET_SIZE as usize;
        let mut sent = 0;
        while sent < self.len {
            let end = (sent + max).min(self.len);
            // Bounded, because `write_packet` waits for the host to ask for
            // the data and a host that has closed the port never will. Without
            // a bound the modem stops servicing the radio the moment somebody
            // disconnects, and only starts again if they come back.
            match with_timeout(
                Duration::from_millis(500),
                tx.write_packet(&self.buf[sent..end]),
            )
            .await
            {
                Ok(Ok(())) => {}
                // The host went away mid-frame. Everything queued is part of a
                // conversation nobody is having; drop it rather than delivering
                // half a frame to whoever connects next.
                Ok(Err(_)) | Err(_) => {
                    self.len = 0;
                    self.dropped += 1;
                    return;
                }
            }
            sent = end;
        }
        // A full-size final packet needs a zero-length packet behind it, or the
        // host waits for the rest of a transfer that is already complete.
        if self.len % max == 0 && self.len != 0 {
            let _ = with_timeout(Duration::from_millis(500), tx.write_packet(&[])).await;
        }
        self.len = 0;
        if self.dropped > 0 {
            defmt::warn!("outbox: {=u32} frames dropped", self.dropped);
            self.dropped = 0;
        }
    }
}

impl<const N: usize> Sink for Outbox<N> {
    fn frame(&mut self, cmd: u8, payload: &[u8]) {
        let need = command::response_len(cmd, payload);
        if self.len + need > N {
            self.dropped += 1;
            return;
        }
        match command::encode_response(cmd, payload, &mut self.buf[self.len..]) {
            Some(n) => self.len += n,
            None => self.dropped += 1,
        }
    }
}

// The outbox must hold a worst-case data frame whole, or a packet of entirely
// escapable bytes -- which encrypted traffic eventually produces -- could never
// be delivered at all.
const _: () = assert!(OUTBOX >= 2 * kiss::HW_MTU + 3);
