//! Phase 3 bring-up image for the LR1121.
//!
//! Currently at step 4: it configures the SPI bus, reports what the peripheral
//! actually claimed, pulses NRESET and reports what BUSY did about it, asks the
//! chip what it is, starts its 32 MHz oscillator off the board's TCXO, and
//! tells it which of its own DIOs drive the antenna switch. See
//! `docs/phase-3-radio.md` for what each step adds.
//!
//! **Nothing here transmits.** No carrier, no packet, no antenna port
//! energised.
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

use arbitrary_int::u24;
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_futures::select::{select3, Either3};
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, spim};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use lr11xx::ops::Interrupt;
use lr11xx::ops::Interrupt as Irq;
use lr11xx::ops::{
    Calibrate, CodingRate, LoRaBandwidth, LoRaModulation, LoRaPacket, PaConfig, PacketType,
    RampTime, RfSwitchConfig, SpreadingFactor, TcxoMode, TcxoTune, TxParams,
};
use lr11xx::Lr11xx;
use oxinode::board::{self, Led};
use oxinode::{boot, radio, usb_log};
use oxinode_core::lr1121::{irq as irq_bits, lora, pa, rf_switch, tcxo, ResetVerdict};
use oxinode_core::meshtastic;
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
    let mut irq = radio::RadioIrq::new(p.P1_08);

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
    let (mut log_tx, mut log_rx, control) = logs.split_with_control();

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
                                defmt::info!("step 3 done");
                                set_regulator(&mut dev, true).await;
                                if start_tcxo(&mut dev, tcxo::TUNE_3V0, true).await {
                                    configure_rf_switch(&mut dev).await;
                                    route_interrupts(&mut dev, &mut irq, irq_bits::DIO9_MASK).await;
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

        console(&mut log_rx, &control, radio, &mut irq).await
    };

    join3(run_usb, pump, bring_up).await;
}

/// Step 4: start the 32 MHz oscillator from the board's 3.0 V TCXO, and prove
/// it started.
///
/// Bounded as a whole. `lr11xx` waits for BUSY with no timeout of its own, so
/// any command here that never completes would take the board silent — the
/// failure step 2 exists to prevent. A cancelled SPI transaction leaves the
/// driver in an undefined state, which is why nothing is attempted afterwards
/// except saying so.
async fn start_tcxo<S, B>(dev: &mut Lr11xx<S, B>, tune_code: u8, use_tcxo: bool) -> bool
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let millivolts = tcxo::tune_millivolts(tune_code).unwrap_or(0);
    defmt::info!(
        "tcxo: tune code {=u8:#04x} = {=u16} mV, delay {=u32} steps ({=u32} us at 30.52 us per step)",
        tune_code,
        millivolts,
        tcxo::STARTUP_STEPS,
        tcxo::us_for_steps(tcxo::STARTUP_STEPS)
    );

    let sequence = async {
        // Clear first, so the errors read at the end are fresh evidence rather
        // than the `hf_xosc_start` already latched from before the TCXO was
        // configured.
        dev.clear_errors().await?;

        let started = Instant::now();
        let cfg = TcxoMode::builder()
            .with_delay(u24::new(tcxo::STARTUP_STEPS))
            .with_tune(tune_from_code(tune_code))
            .build();
        // Skippable on purpose. The module's reference oscillator starts at
        // every supply voltage `SetTcxoMode` can select, from 1.6 V to 3.3 V,
        // with no measurable effect on frequency -- which is not how a TCXO fed
        // from DIO3 would behave. So the question becomes whether the chip
        // needs to be told about an external oscillator at all, or whether
        // there is a crystal here being driven in the wrong mode.
        if use_tcxo {
            dev.set_tcxo_mode(cfg).await?;
        } else {
            defmt::info!("tcxo: SKIPPED -- letting the chip drive a crystal instead");
        }
        let tcxo_us = started.elapsed().as_micros() as u32;

        // The calibrations that ran at boot did so without a working 32 MHz
        // oscillator, so they have to be redone -- which is also what the
        // crate's own note on `hf_xosc_start` says to do.
        let calibrating = Instant::now();
        dev.calibrate(Calibrate::ALL).await?;
        let calib_us = calibrating.elapsed().as_micros() as u32;

        let errors = dev.errors().await?;
        let temp = dev.temp().await?;
        let vbat = dev.vbat().await?;
        Ok::<_, lr11xx::Error>((errors, temp, vbat, tcxo_us, calib_us))
    };

    // Generous against a few milliseconds of oscillator startup and
    // calibration, and still far short of a board that has gone quiet.
    let ok = match with_timeout(Duration::from_millis(500), sequence).await {
        Err(_) => {
            defmt::error!(
                "tcxo: no reply within 500 ms -- a command left BUSY high and the \
             driver has no timeout of its own. The radio is now in an unknown \
                 state; reflash rather than trusting anything after this"
            );
            false
        }
        Ok(Err(e)) => {
            defmt::error!("tcxo: {}", e);
            false
        }
        Ok(Ok((errors, temp, vbat, tcxo_us, calib_us))) => {
            if errors.raw_value() == 0 {
                defmt::info!("tcxo: errors clear -- the 32 MHz oscillator started");
            } else {
                defmt::error!(
                    "tcxo: {} still set after SetTcxoMode and a full calibration",
                    errors
                );
                if errors.hf_xosc_start() {
                    defmt::error!(
                        "tcxo: hf_xosc_start persists -- the delay may be too \
                         short for this TCXO, or DIO3 is not supplying it"
                    );
                }
            }

            if tcxo::temperature_is_plausible(temp) {
                defmt::info!("tcxo: die {=f32} C, vbat {=f32} V", temp, vbat);
            } else {
                defmt::error!(
                    "tcxo: die temperature {=f32} C is not a number to believe -- \
                     GetTemp runs off the 32 MHz oscillator, so this is what a \
                     clock that is not running looks like",
                    temp
                );
            }

            // Timed separately to answer a question the datasheet wording
            // leaves open: whether the delay field is a timeout that ends when
            // the oscillator is detected, or a fixed wait paid every time. If
            // SetTcxoMode's own cost tracks the programmed delay, it is a wait.
            defmt::info!(
                "tcxo: SetTcxoMode took {=u32} us for a {=u32} us delay; \
                 calibration took {=u32} us",
                tcxo_us,
                tcxo::us_for_steps(tcxo::STARTUP_STEPS),
                calib_us
            );

            errors.raw_value() == 0 && tcxo::temperature_is_plausible(temp)
        }
    };

    defmt::info!("step 4 done");
    ok
}

/// Step 5: tell the chip which of its own DIOs drive the antenna switch.
///
/// This step cannot check itself, and the way it fails is the reason it is
/// worth being careful about. A wrong configuration produces a clean `TxDone`
/// while nothing reaches the connector: the chip is doing exactly what it was
/// told, and what it was told is a fact about copper it cannot see. The only
/// instrument that can disagree is a receiver, which is step 7a.
///
/// So all this reports is that the command was accepted, and it says so in
/// those words rather than in words that sound like success.
async fn configure_rf_switch<S, B>(dev: &mut Lr11xx<S, B>)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let masks = rf_switch::BASE_DUO;
    let raw = masks.to_raw();

    defmt::info!(
        "rfsw: enable {=u8:#04x}, standby {=u8:#04x}, rx {=u8:#04x}, tx {=u8:#04x}, tx_hp {=u8:#04x}, tx_hf {=u8:#04x} (word {=u64:#018x})",
        masks.enable,
        masks.standby,
        masks.rx,
        masks.tx,
        masks.tx_hp,
        masks.tx_hf,
        raw
    );

    // Built from the packed word rather than through `RfSwitchConfig`'s
    // builder: the builder has no field for bits 16..=23, the high-frequency TX
    // state. See `oxinode_core::lr1121::rf_switch` for why that is harmless
    // here and would not be on a different board.
    let cfg = RfSwitchConfig::new_with_raw_value(raw);

    let sequence = async {
        let prior = dev.set_dio_as_rf_switch(cfg).await?;
        // SetDioAsRfSwitch's own verdict arrives on the next command, so ask
        // for one rather than assuming.
        let (status, _) = dev.status().await?;
        Ok::<_, lr11xx::Error>((prior, status))
    };
    match with_timeout(Duration::from_millis(200), sequence).await {
        Err(_) => defmt::error!("rfsw: no reply within 200 ms; the radio is in an unknown state"),
        Ok(Err(e)) => defmt::error!("rfsw: {}", e),
        Ok(Ok((prior, status))) => {
            defmt::info!("rfsw: prior {}, after {}", prior, status);
            defmt::info!(
                "rfsw: command accepted -- which is NOT proof that anything reaches the antenna. A wrong switch mask gives a clean TxDone into a dead port. Only step 7a, on an SDR, can tell the difference"
            );
        }
    }

    defmt::info!("step 5 done");
}

/// The bench console: single-character commands on the same serial port the log
/// comes out of.
///
/// It also owns the 1200-baud bootloader touch, and owns it **level-triggered**
/// rather than edge-triggered. The earlier version waited on
/// `control_changed()` in a task of its own, which meant a touch arriving while
/// the bring-up sequence was running — a quarter of a second, most of it the
/// LR1121's 191 ms reset — could be missed, and a missed touch is a walk to the
/// reset button. Here the condition is re-checked on every wake, including the
/// 200 ms timer, so nothing can slip between edges.
async fn console<'d, D, S, B>(
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
    mut radio: Option<Lr11xx<S, B>>,
    irq: &mut radio::RadioIrq<'_>,
) -> !
where
    D: UsbDriverTrait<'d>,
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    defmt::info!(
        "console: 1/2/3 = CW at {=i8}/{=i8}/{=i8} dBm, v/n = CW at {=u32}/{=u32} Hz, 0 = stop, r = reboot, g = next TCXO voltage, x = restart without TCXO mode, j = provoke IRQ, p/o = send a LoRa packet warm/cold, m/u = DC-DC/LDO, t = temperature, y = listen (h/i = -60/+60 kHz), z = sweep, k/l = route nothing/normal to DIO9, ? = status, b = bootloader",
        CW_LEVELS[0],
        CW_LEVELS[1],
        CW_LEVELS[2],
        pa::CW_SWEEP_HZ[0],
        pa::CW_SWEEP_HZ[1]
    );

    let mut buf = [0u8; 64];
    let mut cw_until: Option<Instant> = None;
    let mut tune_code = tcxo::TUNE_3V0;
    let mut ticks: u32 = 0;

    loop {
        let event = select3(
            rx.read_packet(&mut buf),
            control.control_changed(),
            Timer::after(Duration::from_millis(200)),
        )
        .await;

        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }

        // Enforced here rather than trusted to the host. A carrier that outlives
        // the thing that asked for it is the failure worth designing against:
        // an unmodulated carrier sitting in 902-928 MHz is not something a
        // crashed terminal or an unplugged cable should be able to leave behind.
        if let Some(deadline) = cw_until {
            if Instant::now() >= deadline {
                cw_until = None;
                defmt::warn!(
                    "cw: {=u32} ms burst limit reached, stopping",
                    pa::CW_MAX_BURST_MS
                );
                stop_cw(radio.as_mut()).await;
            }
        }

        match event {
            Either3::First(Ok(n)) => {
                for &byte in &buf[..n] {
                    match byte {
                        b'0' | b's' => {
                            cw_until = None;
                            stop_cw(radio.as_mut()).await;
                        }
                        b'1' | b'2' | b'3' => {
                            let dbm = CW_LEVELS[(byte - b'1') as usize];
                            if start_cw(radio.as_mut(), dbm, pa::CW_TEST_HZ).await {
                                cw_until = Some(
                                    Instant::now()
                                        + Duration::from_millis(pa::CW_MAX_BURST_MS as u64),
                                );
                            }
                        }
                        // Reboot the radio's own firmware and redo the boot
                        // configuration, so a prelude can be tested against a
                        // chip that has not already been put right by an
                        // earlier attempt. Without this every experiment after
                        // the first one passes for the wrong reason.
                        b'r' => {
                            if let Some(dev) = radio.as_mut() {
                                cw_until = None;
                                reinit(dev, tune_code, true).await;
                            }
                        }
                        // Restart the radio WITHOUT telling it there is an
                        // external oscillator, so the chip tries to drive a
                        // crystal instead. If the oscillator starts anyway,
                        // TCXO mode was never the right configuration.
                        b'x' => {
                            if let Some(dev) = radio.as_mut() {
                                cw_until = None;
                                reinit(dev, tune_code, false).await;
                            }
                        }
                        // Step to the next TCXO supply voltage and restart the
                        // radio on it. The transmitter is 73 ppm low, which is
                        // far outside what a TCXO should do, and 3.0 V was
                        // taken from the plan rather than from the board -- so
                        // the voltage is swept against a receiver instead of
                        // being assumed.
                        b'g' => {
                            if let Some(dev) = radio.as_mut() {
                                cw_until = None;
                                tune_code = (tune_code + 1) % tcxo::TUNE_CODES.len() as u8;
                                reinit(dev, tune_code, true).await;
                            }
                        }
                        // The frequency-error experiment. The receiver stays
                        // tuned to one centre for the whole run -- moving it
                        // with the transmitter makes the two hypotheses
                        // algebraically identical -- so these two carriers are
                        // as far apart as one capture window allows.
                        b'v' | b'n' => {
                            let hz = pa::CW_SWEEP_HZ[usize::from(byte == b'n')];
                            if start_cw(radio.as_mut(), pa::LP_MAX_DBM, hz).await {
                                cw_until = Some(
                                    Instant::now()
                                        + Duration::from_millis(pa::CW_MAX_BURST_MS as u64),
                                );
                            }
                        }
                        // Step 7b: a real LoRa packet, awaiting TxDone.
                        b'p' | b'o' => {
                            send_packet(radio.as_mut(), irq, byte == b'p').await;
                            cw_until = None;
                        }
                        // Step 6: provoke an interrupt with no RF at all.
                        b'j' => provoke_irq(radio.as_mut(), irq).await,
                        // Negative control. Route NOTHING to DIO9, then provoke
                        // the same interrupt: the line must stay down. Without
                        // this, a DIO9 that signals every interrupt regardless
                        // of the mask passes the positive test exactly as well
                        // as a correctly configured one.
                        b'k' | b'l' => {
                            if let Some(dev) = radio.as_mut() {
                                let mask = if byte == b'k' { 0 } else { irq_bits::DIO9_MASK };
                                route_interrupts(dev, irq, mask).await;
                            }
                        }
                        // Listen on the second bench board's channel. The
                        // offset argument steps the commanded frequency, which
                        // is how the two boards' clocks get compared.
                        b'y' | b'h' | b'i' => {
                            let offset_hz: i32 = match byte {
                                b'h' => -60_000,
                                b'i' => 60_000,
                                _ => 0,
                            };
                            receive(radio.as_mut(), irq, offset_hz).await;
                        }
                        b'z' => sweep_rx(radio.as_mut(), irq).await,
                        // Step 8: the regulator, and a way to compare the two.
                        b'm' | b'u' => {
                            if let Some(dev) = radio.as_mut() {
                                cw_until = None;
                                set_regulator(dev, byte == b'm').await;
                            }
                        }
                        b't' => sample_thermals(radio.as_mut()).await,
                        b'?' => report(radio.as_mut(), cw_until.is_some()).await,
                        b'b' => boot::reboot_to_bootloader(),
                        b'\r' | b'\n' => {}
                        other => defmt::warn!("console: unknown command {=u8:#04x}", other),
                    }
                }
            }
            Either3::First(Err(_)) => {} // host went away; wait for the next one
            Either3::Second(()) => {}
            Either3::Third(()) => {
                // Roughly every five seconds, and only when nothing is keyed --
                // a log line in the middle of a measurement is noise on the
                // serial port, not on the air, but it still muddles a trace.
                ticks += 1;
                if ticks % 25 == 0 && cw_until.is_none() {
                    match radio.as_mut() {
                        Some(dev) => {
                            defmt::info!("idle, busy {}", dev.busy().ok());
                        }
                        None => defmt::info!("idle, no radio attached"),
                    }
                }
            }
        }
    }
}

/// Output powers the console's `1`, `2` and `3` keys select, lowest first.
///
/// Lowest first is the order the plan asks for, and the order that makes the
/// SDR measurement conclusive: three distinct levels at one frequency, stepped
/// in a known direction, is evidence that the transmitter is under control.
/// A single burst is only evidence that *something* appeared.
const CW_LEVELS: [i8; 3] = [pa::LP_MIN_DBM, 0, pa::LP_MAX_DBM];

/// Key an unmodulated carrier. Returns whether it started.
async fn start_cw<S, B>(dev: Option<&mut Lr11xx<S, B>>, dbm: i8, hz: u32) -> bool
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::error!("cw: no radio attached");
        return false;
    };

    // Refuse rather than clamp. Clamping transmits something nobody asked for,
    // which is the wrong instinct when the thing being transmitted is power
    // into an antenna.
    if !pa::low_power_pa_accepts(dbm) {
        defmt::error!(
            "cw: {=i8} dBm is outside the low-power PA's range ({=i8} to {=i8})",
            dbm,
            pa::LP_MIN_DBM,
            pa::LP_MAX_DBM
        );
        return false;
    }
    if !pa::is_in_us915(hz) {
        defmt::error!("cw: {=u32} Hz is outside US915", hz);
        return false;
    }

    // Every one of these returns the status of the *previous* command, not its
    // own -- which is why the last one in a chain can fail silently. That is
    // exactly what happened here the first time: `set_tx_cw` was rejected, the
    // chain reported success, and the chip sat in standby while the log
    // claimed a carrier was up. So each status is logged, and a `status()` read
    // afterwards catches the last command's own verdict.
    let sequence = async {
        // `SetPacketType` first, and it is not optional.
        //
        // `set_tx_cw`'s own documentation says the frequency and the PA
        // configuration "have to be called prior to this command", and says
        // nothing about a packet type -- which for an *unmodulated carrier* is
        // the reasonable reading. The chip disagrees. Without it, `SetTxCw` is
        // refused with `command_status: Fail` and a latched `cmd_error`, while
        // `GetErrors` stays clean, and the chip sits in standby with the log
        // cheerfully reporting a carrier. Bisected against a freshly rebooted
        // radio, three times each way: with a packet type it keys, without one
        // it never does. LoRa is chosen only because something must be.
        defmt::debug!(
            "cw: packet type -> {}",
            dev.set_packet_type(PacketType::LoRa).await?
        );
        defmt::debug!("cw: freq -> {}", dev.set_rf_frequency(hz).await?);
        // The low-power PA on the internal regulator. Built from
        // `oxinode_core::lr1121::pa` rather than through `PaConfig`'s builder,
        // which declares `vbat` and `hp` at the same bit.
        defmt::debug!(
            "cw: pa -> {}",
            dev.set_pa_config(PaConfig::new_with_raw_value(pa::LOW_POWER.to_raw()))
                .await?
        );
        defmt::debug!(
            "cw: tx params -> {}",
            dev.set_tx_params(
                TxParams::builder()
                    .with_ramp_time(RampTime::Us48)
                    .with_tx_power(dbm)
                    .build(),
            )
            .await?
        );
        defmt::debug!("cw: set_tx_cw -> {}", dev.set_tx_cw().await?);
        let (status, interrupt) = dev.status().await?;
        let errors = dev.errors().await?;
        Ok::<_, lr11xx::Error>((status, interrupt, errors))
    };

    match with_timeout(Duration::from_millis(500), sequence).await {
        Ok(Ok((status, interrupt, errors))) => {
            defmt::info!(
                "cw: after set_tx_cw -- {}, {}, {}",
                status,
                interrupt,
                errors
            );
            defmt::info!(
                "cw: requested {=u32} Hz, {=i8} dBm, low-power PA (stops itself after {=u32} ms)",
                hz,
                dbm,
                pa::CW_MAX_BURST_MS
            );
            // The only answer that matters: did the chip actually leave standby?
            if status.stat2().chip_mode() == Ok(lr11xx::ops::ChipMode::Tx) {
                defmt::info!("cw: chip is in Tx -- it is keyed");
            } else {
                defmt::error!("cw: chip is NOT in Tx; nothing is radiating");
            }
            true
        }
        Ok(Err(e)) => {
            defmt::error!("cw: {}", e);
            false
        }
        Err(_) => {
            defmt::error!("cw: no reply within 200 ms; the radio may still be keyed");
            false
        }
    }
}

/// Drop back to standby on the RC oscillator, which stops any transmission.
async fn stop_cw<S, B>(dev: Option<&mut Lr11xx<S, B>>)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else { return };
    match with_timeout(Duration::from_millis(200), dev.standby(false)).await {
        Ok(Ok(_)) => defmt::info!("cw: OFF (standby RC)"),
        Ok(Err(e)) => defmt::error!("cw: stop failed, {}", e),
        Err(_) => defmt::error!("cw: stop timed out -- the carrier may still be up"),
    }
}

/// Say what the radio thinks it is doing.
async fn report<S, B>(dev: Option<&mut Lr11xx<S, B>>, keyed: bool)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::info!("status: no radio attached");
        return;
    };
    match with_timeout(Duration::from_millis(200), dev.status()).await {
        Ok(Ok((status, interrupt))) => {
            defmt::info!("status: keyed {=bool}, {}, {}", keyed, status, interrupt)
        }
        Ok(Err(e)) => defmt::error!("status: {}", e),
        Err(_) => defmt::error!("status: no reply within 200 ms"),
    }
}

/// Restart the LR1121's own firmware and redo the boot configuration.
///
/// Exists for one reason: several of the one-time commands in this bring-up
/// persist until the radio is reset, so once an experiment has succeeded every
/// later experiment succeeds too — for the wrong reason. Without a way back to
/// a virgin chip, a bisect measures nothing.
async fn reinit<S, B>(dev: &mut Lr11xx<S, B>, tune_code: u8, use_tcxo: bool)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    defmt::info!("reinit: rebooting the radio firmware");
    match with_timeout(Duration::from_millis(1000), dev.reboot(false)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            defmt::error!("reinit: reboot failed, {}", e);
            return;
        }
        Err(_) => {
            defmt::error!("reinit: reboot did not complete within 1 s");
            return;
        }
    }
    // The radio takes ~191 ms to boot; wait comfortably past that before
    // speaking to it again.
    Timer::after(Duration::from_millis(300)).await;

    set_regulator(dev, true).await;
    if start_tcxo(dev, tune_code, use_tcxo).await {
        configure_rf_switch(dev).await;
    }
    defmt::info!("reinit: back to the step 5 state");
}

/// Map a `SetTcxoMode` tune code to the crate's enum.
///
/// The codes are the chip's and are dense from 0 to 7; `oxinode_core` holds the
/// table and tests that it stays that way, so this is only the translation.
fn tune_from_code(code: u8) -> TcxoTune {
    match code {
        0x00 => TcxoTune::V1p6,
        0x01 => TcxoTune::V1p7,
        0x02 => TcxoTune::V1p8,
        0x03 => TcxoTune::V2p2,
        0x04 => TcxoTune::V2p4,
        0x05 => TcxoTune::V2p7,
        0x07 => TcxoTune::V3p3,
        _ => TcxoTune::V3p0,
    }
}

/// Step 6: tell the chip which interrupts to signal on DIO9, and check the line
/// is where it should be before anything has fired.
async fn route_interrupts<S, B>(
    dev: &mut Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    dio9_mask: u32,
) where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    defmt::info!(
        "irq: DIO9 -> P{=u8}.{=u8}, mask {=u32:#010x}; DIO11 mask {=u32:#010x} (it does not leave the module)",
        radio::IRQ.0,
        radio::IRQ.1,
        dio9_mask,
        irq_bits::DIO11_MASK
    );

    let sequence = async {
        // Clear first: a stale flag would hold the line high from the moment it
        // becomes an output, and the idle check below would fail for a reason
        // that has nothing to do with the routing.
        dev.clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
            .await?;
        dev.set_dio_irq(
            Interrupt::new_with_raw_value(dio9_mask),
            Interrupt::new_with_raw_value(irq_bits::DIO11_MASK),
        )
        .await
    };

    match with_timeout(Duration::from_millis(200), sequence).await {
        Err(_) => {
            defmt::error!("irq: no reply within 200 ms");
            return;
        }
        Ok(Err(e)) => {
            defmt::error!("irq: {}", e);
            return;
        }
        Ok(Ok(_)) => {}
    }

    if irq.is_asserted() {
        defmt::error!("irq: P1.08 is already high with every interrupt cleared");
    } else {
        defmt::info!("irq: P1.08 idle low");
    }
    defmt::info!("step 6 configured; press j to prove the line actually moves");
}

/// Raise an interrupt deliberately and watch P1.08 follow it.
///
/// The trigger is `SetTxCw` issued without a packet type — the refusal found at
/// step 7a, which latches `cmd_error`. It is worth its weight here: it is the
/// only interrupt that can be raised **without transmitting anything**, so the
/// interrupt path is testable before there is a packet to send, and testable
/// again later without putting a carrier in the air.
///
/// Three things are checked, and the middle one is the one that matters:
/// the line is low beforehand, it *rises* — waited on asynchronously, not
/// polled — and it falls again when the interrupt is cleared. A line stuck high
/// would pass a naive "is it high" test and fail this one.
async fn provoke_irq<S, B>(dev: Option<&mut Lr11xx<S, B>>, irq: &mut radio::RadioIrq<'_>)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::error!("irq: no radio attached");
        return;
    };

    if with_timeout(
        Duration::from_millis(200),
        dev.clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED)),
    )
    .await
    .is_err()
    {
        defmt::error!("irq: could not clear before the test");
        return;
    }

    if irq.is_asserted() {
        defmt::error!("irq: line still high after clearing; it is stuck, not idle");
        return;
    }

    // Provoke. The chip is freshly booted or has had its packet type cleared,
    // so this is refused -- which is the point.
    let started = Instant::now();
    let provoke = with_timeout(Duration::from_millis(200), dev.set_tx_cw()).await;
    if provoke.is_err() {
        defmt::error!("irq: the provoking command did not complete");
        return;
    }

    let rose = irq.wait_asserted(Duration::from_millis(200)).await;
    match rose {
        Ok(()) => defmt::info!(
            "irq: P1.08 rose {=u32} us after the command -- the async wait woke",
            started.elapsed().as_micros() as u32
        ),
        Err(_) => defmt::warn!(
            "irq: P1.08 did not rise within 200 ms -- correct if nothing is routed to it"
        ),
    }

    // What actually fired, from the chip's own view.
    match with_timeout(Duration::from_millis(200), dev.status()).await {
        Ok(Ok((_, pending))) => {
            let raw = pending.raw_value();
            defmt::info!("irq: pending {=u32:#010x}", raw);
            for (b, name) in irq_bits::NAMES {
                if raw & b != 0 {
                    defmt::info!("irq:   {=str}", name);
                }
            }
        }
        _ => defmt::error!("irq: could not read what fired"),
    }

    if rose.is_err() {
        defmt::info!("irq: (the chip's own pending flags above say whether it fired at all)");
    }

    match with_timeout(
        Duration::from_millis(200),
        dev.clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED)),
    )
    .await
    {
        Ok(Ok(_)) => match irq.wait_cleared(Duration::from_millis(200)).await {
            Ok(()) => {
                defmt::info!("irq: P1.08 fell again once cleared -- the line follows the chip")
            }
            Err(_) => defmt::error!("irq: P1.08 stayed high after clearing; it is stuck high"),
        },
        _ => defmt::error!("irq: could not clear afterwards"),
    }
}

/// Step 7b: transmit one LoRa packet and wait for `TxDone` on the interrupt
/// line.
///
/// The airtime is computed beforehand, in `oxinode_core::lr1121::lora`, and
/// used two ways: as the chip's own transmit timeout, and as the number the
/// measured `TxDone` latency is checked against. That second use is the point.
/// A `TxDone` that arrives immediately, or after some unrelated interval, would
/// otherwise look exactly like a successful transmission — and this board has
/// already shown once that a command can report success while nothing happens.
async fn send_packet<S, B>(
    dev: Option<&mut Lr11xx<S, B>>,
    irq: &mut radio::RadioIrq<'_>,
    warm: bool,
) where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::error!("tx: no radio attached");
        return;
    };

    let airtime_us = lora::bench::AIRTIME_US;
    defmt::info!(
        "tx: SF{=u8} BW{=u32} CR4/5, {=u8}-byte payload, {=u16}-symbol preamble, sync {=u8:#04x}",
        lora::bench::SF,
        lora::bench::BANDWIDTH_HZ,
        lora::bench::PAYLOAD_LEN,
        lora::bench::PREAMBLE,
        lora::bench::SYNC_WORD
    );
    defmt::info!("tx: computed airtime {=u32} us", airtime_us);

    // Recognisable on a receiver, and exactly PAYLOAD_LEN bytes.
    let payload: [u8; lora::bench::PAYLOAD_LEN as usize] = *b"oxinode 7b test\n";

    // The chip's own timeout, in 32.768 kHz ticks. Three times the airtime:
    // long enough that a healthy packet never trips it, short enough that a
    // stuck transmitter gives up rather than holding the channel.
    let ticks = (airtime_us as u64 * 3 * 32_768 / 1_000_000) as u32;

    let configure = async {
        dev.set_packet_type(PacketType::LoRa).await?;
        dev.set_rf_frequency(pa::CW_TEST_HZ).await?;
        dev.set_lora_modulation(
            LoRaModulation::builder()
                .with_sf(SpreadingFactor::SF7)
                .with_bwl(LoRaBandwidth::KHz125)
                .with_cr(CodingRate::Short45)
                .with_low_data_rate_optimize(lora::low_data_rate_optimize(
                    lora::bench::SF,
                    lora::bench::BANDWIDTH_HZ,
                ))
                .build(),
        )
        .await?;
        dev.set_lora_packet(
            LoRaPacket::builder()
                .with_preamble_length(lora::bench::PREAMBLE)
                .with_header_implicit(false)
                .with_payload_length(lora::bench::PAYLOAD_LEN)
                .with_crc(true)
                .with_invert_iq(false)
                .build(),
        )
        .await?;
        dev.set_lora_sync_word(lora::bench::SYNC_WORD).await?;
        dev.set_pa_config(PaConfig::new_with_raw_value(pa::LOW_POWER.to_raw()))
            .await?;
        dev.set_tx_params(
            TxParams::builder()
                .with_ramp_time(RampTime::Us48)
                .with_tx_power(pa::LP_MAX_DBM)
                .build(),
        )
        .await?;
        dev.clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
            .await?;
        dev.write_buffer8(&payload).await?;
        if warm {
            // Standby on the crystal rather than the RC oscillator, so the
            // 32 MHz reference is already running when SetTx is issued. See
            // below: it is worth 5 ms.
            dev.standby(true).await?;
        }
        Ok::<(), lr11xx::Error>(())
    };

    match with_timeout(Duration::from_millis(500), configure).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            defmt::error!("tx: configuration failed, {}", e);
            return;
        }
        Err(_) => {
            defmt::error!("tx: configuration did not complete within 500 ms");
            return;
        }
    }

    if irq.is_asserted() {
        defmt::error!("tx: the interrupt line is high before transmitting; aborting");
        return;
    }

    let started = Instant::now();
    match with_timeout(Duration::from_millis(200), dev.set_tx(u24::new(ticks))).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            defmt::error!("tx: SetTx rejected, {}", e);
            return;
        }
        Err(_) => {
            defmt::error!("tx: SetTx did not complete");
            return;
        }
    }

    // Generously past the airtime, so a late TxDone is still caught and
    // reported as late rather than as missing.
    let deadline = Duration::from_micros(airtime_us as u64 * 4);
    match irq.wait_asserted(deadline).await {
        Ok(()) => {
            let elapsed = started.elapsed().as_micros() as u32;
            defmt::info!(
                "tx: interrupt after {=u32} us against {=u32} us of computed airtime",
                elapsed,
                airtime_us
            );
            // What the latency should be, rather than just the airtime.
            //
            // Transmitting from Standby RC pays the TCXO startup delay inside
            // the measurement, because step 4 established that the delay is a
            // fixed wait charged to the first operation that needs the
            // oscillator -- and SetTx is that operation. From Standby XOSC the
            // reference is already running and the delay is not paid at all.
            //
            // Predicting it rather than widening the tolerance is what makes
            // this check worth having: a 5 ms discrepancy that is explained is
            // evidence, and a 5 ms tolerance that hides it is not.
            let startup = if warm { 0 } else { tcxo::STARTUP_US };
            let expected = airtime_us + startup;
            defmt::info!(
                "tx: expected {=u32} us = {=u32} airtime + {=u32} oscillator startup ({=str})",
                expected,
                airtime_us,
                startup,
                if warm { "standby XOSC" } else { "standby RC" }
            );
            // Five percent of the airtime, plus a millisecond for the SetTx
            // transaction, the PLL lock and the PA ramp.
            let slack = airtime_us / 20 + 1_000;
            if elapsed + slack >= expected && elapsed <= expected + slack {
                defmt::info!("tx: within {=u32} us of prediction", slack);
            } else {
                defmt::error!(
                    "tx: {=u32} us off prediction, outside the {=u32} us allowance",
                    elapsed.abs_diff(expected),
                    slack
                );
            }
        }
        Err(_) => {
            defmt::error!(
                "tx: no interrupt within {=u32} us",
                deadline.as_micros() as u32
            );
        }
    }

    match with_timeout(Duration::from_millis(200), dev.status()).await {
        Ok(Ok((status, pending))) => {
            let raw = pending.raw_value();
            defmt::info!("tx: pending {=u32:#010x}, {}", raw, status);
            for (b, name) in irq_bits::NAMES {
                if raw & b != 0 {
                    defmt::info!("tx:   {=str}", name);
                }
            }
            if raw & irq_bits::bit::TX_DONE != 0 {
                defmt::info!("tx: TxDone -- a packet went out");
            } else {
                defmt::error!("tx: TxDone is NOT set; whatever raised the line, it was not this");
            }
        }
        _ => defmt::error!("tx: could not read what fired"),
    }

    let _ = with_timeout(
        Duration::from_millis(200),
        dev.clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED)),
    )
    .await;
}

/// Step 8: choose the switching regulator over the LDO.
///
/// Every LR1110-family design does this, and the reasoning given is always
/// transmit current: the LDO burns the difference between the supply and what
/// the PA needs as heat, and a DC-DC converter does not. One command.
///
/// It only works in Standby RC — in any other mode the chip accepts it and
/// then reports `CMD_FAIL` on the next `GetStatus`, which is the same silent
/// failure `SetTxCw` produced at step 7a. So the mode is forced first rather
/// than assumed, and the result is read back rather than trusted.
async fn set_regulator<S, B>(dev: &mut Lr11xx<S, B>, dcdc: bool)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let sequence = async {
        dev.standby(false).await?;
        dev.set_reg_mode(dcdc).await?;
        let (status, _) = dev.status().await?;
        Ok::<_, lr11xx::Error>(status)
    };
    match with_timeout(Duration::from_millis(200), sequence).await {
        Ok(Ok(status)) => {
            let accepted = status.stat1().command_status() != Ok(lr11xx::ops::CommandStatus::Fail);
            if accepted {
                defmt::info!(
                    "reg: {=str}",
                    if dcdc {
                        "DC-DC converter selected"
                    } else {
                        "LDO selected"
                    }
                );
            } else {
                defmt::error!("reg: SetRegMode was refused; the chip was not in standby RC");
            }
        }
        Ok(Err(e)) => defmt::error!("reg: {}", e),
        Err(_) => defmt::error!("reg: no reply within 200 ms"),
    }
}

/// Read the die temperature and supply voltage.
///
/// The only two things this board can say about its own power, and therefore
/// the only evidence available for whether the regulator choice does anything.
/// `GetTemp` quantises to about 0.39 °C, which is worth knowing before drawing
/// conclusions from small differences.
async fn sample_thermals<S, B>(dev: Option<&mut Lr11xx<S, B>>)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::error!("thermal: no radio attached");
        return;
    };
    let sequence = async {
        let temp = dev.temp().await?;
        let vbat = dev.vbat().await?;
        Ok::<_, lr11xx::Error>((temp, vbat))
    };
    match with_timeout(Duration::from_millis(200), sequence).await {
        Ok(Ok((temp, vbat))) => defmt::info!("thermal: die {=f32} C, vbat {=f32} V", temp, vbat),
        Ok(Err(e)) => defmt::error!("thermal: {}", e),
        Err(_) => defmt::error!("thermal: no reply within 200 ms"),
    }
}

/// Listen on the second bench board's Meshtastic channel for a few seconds.
///
/// This is the first time anything in oxinode has receive. It exists because
/// the only radio peer available is a Base Duo running stock Meshtastic, and
/// hearing it answers a question the SDR could not: **is the 73 ppm error this
/// board, or the design?**
///
/// The two boards carry the same module, so if the error is a property of the
/// design they share it, and each will hear the other at the frequency both
/// were told to use. If instead this board is uniquely bad, it will only hear
/// the other one when commanded roughly 66 kHz away from where the other one
/// thinks it is transmitting. `offset_hz` is what makes that measurable.
async fn receive<S, B>(
    dev: Option<&mut Lr11xx<S, B>>,
    irq: &mut radio::RadioIrq<'_>,
    offset_hz: i32,
) where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::error!("rx: no radio attached");
        return;
    };
    listen(dev, irq, offset_hz, Duration::from_secs(20), true).await;
}

/// Step the receive frequency across a range and count what arrives at each.
///
/// Three fixed offsets could not answer the question. At 250 kHz bandwidth LoRa
/// tolerates so much frequency error that 0 and ±60 kHz all receive perfectly,
/// so the measurement said nothing at all.
///
/// What does answer it is the **centre** of the window: find where reception
/// dies on each side, and the midpoint is where the two boards' clocks agree.
/// If the 73 ppm error belongs to the module design, both boards share it, they
/// agree at zero offset, and the window is centred on 0. If instead this board
/// is uniquely bad, the window is centred near +66 kHz — the amount this board
/// must be told to add to land where the other one actually is.
async fn sweep_rx<S, B>(dev: Option<&mut Lr11xx<S, B>>, irq: &mut radio::RadioIrq<'_>)
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let Some(dev) = dev else {
        defmt::error!("rx: no radio attached");
        return;
    };
    defmt::info!("sweep: 13 steps of 40 kHz, 8 s each; keep the peer transmitting");
    for step in -6i32..=6 {
        let offset = step * 40_000;
        let heard = listen(dev, irq, offset, Duration::from_secs(8), false).await;
        defmt::info!("sweep: {=i32} Hz -> {=u32} packets", offset, heard);
    }
    defmt::info!("sweep: done");
}

/// Configure for the peer's channel at `offset_hz` and count packets for
/// `dwell`. Returns how many arrived.
async fn listen<S, B>(
    dev: &mut Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    offset_hz: i32,
    dwell: Duration,
    verbose: bool,
) -> u32
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let tuned = (meshtastic::US_LONG_FAST_HZ as i64 + offset_hz as i64) as u32;
    if verbose {
        defmt::info!(
            "rx: {=u32} Hz ({=i32} Hz off Meshtastic US LongFast), SF{=u8} BW{=u32} CR4/5, sync {=u8:#04x}",
            tuned,
            offset_hz,
            meshtastic::long_fast::SF,
            meshtastic::long_fast::BANDWIDTH_HZ,
            meshtastic::long_fast::SYNC_WORD
        );
    }

    let configure = async {
        dev.set_packet_type(PacketType::LoRa).await?;
        dev.set_rf_frequency(tuned).await?;
        dev.set_lora_modulation(
            LoRaModulation::builder()
                .with_sf(SpreadingFactor::SF11)
                .with_bwl(LoRaBandwidth::KHz250)
                .with_cr(CodingRate::Short45)
                .with_low_data_rate_optimize(lora::low_data_rate_optimize(
                    meshtastic::long_fast::SF,
                    meshtastic::long_fast::BANDWIDTH_HZ,
                ))
                .build(),
        )
        .await?;
        dev.set_lora_packet(
            LoRaPacket::builder()
                .with_preamble_length(meshtastic::long_fast::PREAMBLE)
                .with_header_implicit(false)
                // Explicit header: the length comes off the air, and this is
                // the ceiling rather than the expected size.
                .with_payload_length(255)
                .with_crc(true)
                .with_invert_iq(false)
                .build(),
        )
        .await?;
        dev.set_lora_sync_word(meshtastic::long_fast::SYNC_WORD)
            .await?;
        // Sensitivity matters more than current here, and it is what the peer
        // is using.
        dev.set_rx_boosted(true).await?;
        dev.clear_irq(Irq::new_with_raw_value(irq_bits::ALL_NAMED))
            .await?;
        // 0xFFFFFF: stay in RX until told otherwise, receiving repeatedly.
        dev.set_rx(u24::new(0xff_ffff)).await?;
        Ok::<(), lr11xx::Error>(())
    };

    match with_timeout(Duration::from_millis(500), configure).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            defmt::error!("rx: configuration failed, {}", e);
            return 0;
        }
        Err(_) => {
            defmt::error!("rx: configuration did not complete");
            return 0;
        }
    }
    if verbose {
        defmt::info!("rx: listening");
    }

    let until = Instant::now() + dwell;
    let mut heard = 0u32;
    while Instant::now() < until {
        if irq.wait_asserted(Duration::from_millis(500)).await.is_err() {
            continue;
        }
        let report = async {
            let (_, pending) = dev.status().await?;
            let raw = pending.raw_value();
            let mut buf = [0u8; 64];
            let mut len = 0usize;
            let mut rssi = 0i16;
            let mut snr = 0i16;
            if raw & irq_bits::bit::RX_DONE != 0 {
                let status = dev.rx_buffer_status().await?;
                len = (status.payload_length() as usize).min(buf.len());
                dev.read_buffer8(status.offset(), &mut buf[..len]).await?;
                let pkt = dev.lora_packet_status().await?;
                // RSSI in dBm is -RssiPkt/2; SNR in dB is SnrPkt/4.
                rssi = -(pkt.rssi() as i16) / 2;
                snr = (pkt.snr() as i16 + 2) / 4;
            }
            dev.clear_irq(Irq::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            Ok::<_, lr11xx::Error>((raw, buf, len, rssi, snr))
        };
        match with_timeout(Duration::from_millis(300), report).await {
            Ok(Ok((raw, buf, len, rssi, snr))) => {
                if verbose {
                    for (b, name) in irq_bits::NAMES {
                        if raw & b != 0 {
                            defmt::info!("rx: irq {=str}", name);
                        }
                    }
                }
                if raw & irq_bits::bit::RX_DONE != 0 {
                    heard += 1;
                    if verbose {
                        defmt::info!(
                            "rx: PACKET {=u32}: {=usize} bytes, RSSI {=i16} dBm, SNR {=i16} dB",
                            heard,
                            len,
                            rssi,
                            snr
                        );
                        defmt::info!("rx: bytes {=[u8]:02x}", buf[..len.min(32)]);
                    }
                }
            }
            Ok(Err(e)) => defmt::error!("rx: {}", e),
            Err(_) => defmt::error!("rx: read timed out"),
        }
    }

    let _ = with_timeout(Duration::from_millis(200), dev.standby(false)).await;
    if verbose {
        defmt::info!(
            "rx: done at {=i32} Hz offset -- {=u32} packets",
            offset_hz,
            heard
        );
    }
    heard
}
