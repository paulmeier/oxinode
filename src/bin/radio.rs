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
use lr11xx::ops::{
    Calibrate, PaConfig, PacketType, RampTime, RfSwitchConfig, TcxoMode, TcxoTune, TxParams,
};
use lr11xx::Lr11xx;
use oxinode::board::{self, Led};
use oxinode::modem::{Modem, TxOutcome};
use oxinode::{boot, radio, usb_log};
use oxinode_core::lr1121::config::{self, RadioConfig, ValidConfig};
use oxinode_core::lr1121::csma::Backoff;
use oxinode_core::lr1121::{irq as irq_bits, lora, pa, reference, rf_switch, tcxo, ResetVerdict};
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
        "console bench: 1/2/3 = CW at {=i8}/{=i8}/{=i8} dBm, v/n = CW at {=u32}/{=u32} Hz, 0 = stop, r = reboot radio, g = next TCXO voltage, x = restart without TCXO mode, j = provoke IRQ, k/l = route nothing/normal to DIO9, m/u = DC-DC/LDO, t = temperature, b = bootloader",
        CW_LEVELS[0],
        CW_LEVELS[1],
        CW_LEVELS[2],
        pa::CW_SWEEP_HZ[0],
        pa::CW_SWEEP_HZ[1]
    );
    defmt::info!(
        "console config: S = spreading factor, W = bandwidth, C = coding rate, P = power, [ / ] = frequency -/+ 100 kHz, R = reference correction, N = sync word, M = Meshtastic LongFast preset, D = oxinode default, A = apply, ? = show"
    );
    defmt::info!(
        "console radio: p = send a packet, y = listen 20 s (h/i = -60/+60 kHz), z = coarse frequency sweep, E = fine sweep of the window's upper edge"
    );

    let mut buf = [0u8; 64];
    let mut cw_until: Option<Instant> = None;
    let mut tune_code = tcxo::TUNE_3V0;
    let mut ticks: u32 = 0;
    // Phase 4's whole point: the radio's parameters are a value that lives
    // here and changes, not constants compiled into the transmit path.
    let mut cfg = config::DEFAULT;

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
                        // A real LoRa packet, with whatever the config keys
                        // below have been set to.
                        b'p' => {
                            cw_until = None;
                            tx_packet(radio.as_mut(), irq, &cfg).await;
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
                        // Listen with the current configuration. The offset
                        // moves the wanted frequency, so it composes with the
                        // reference correction rather than replacing it.
                        b'y' | b'h' | b'i' => {
                            let offset_hz: i32 = match byte {
                                b'h' => -60_000,
                                b'i' => 60_000,
                                _ => 0,
                            };
                            cw_until = None;
                            if let (Some(dev), Some(valid)) = (radio.as_mut(), validate(&cfg)) {
                                rx_listen(
                                    dev,
                                    irq,
                                    &valid,
                                    offset_hz,
                                    Duration::from_secs(20),
                                    true,
                                )
                                .await;
                            }
                        }
                        b'z' | b'E' => {
                            cw_until = None;
                            let plan = if byte == b'z' {
                                SWEEP_COARSE
                            } else {
                                SWEEP_EDGE
                            };
                            rx_sweep(radio.as_mut(), irq, &cfg, plan).await;
                        }
                        // The configuration editor. Each key cycles one
                        // parameter and prints the result; nothing reaches the
                        // chip until a transmit, a receive, or `A`.
                        b'S' | b'W' | b'C' | b'P' | b'[' | b']' | b'R' | b'N' | b'M' | b'D' => {
                            edit_config(&mut cfg, byte);
                            match validate(&cfg) {
                                Some(valid) => log_config(&valid),
                                // `validate` has already said which limit was
                                // hit. Reported and kept rather than reverted:
                                // a host sets one parameter at a time and is
                                // entitled to pass through invalid states.
                                None => defmt::warn!("config: held, but not usable as it stands"),
                            }
                        }
                        // Program the current configuration into the chip
                        // without transmitting or receiving, so that a
                        // configuration can be checked on its own.
                        b'A' => {
                            if let (Some(dev), Some(valid)) = (radio.as_mut(), validate(&cfg)) {
                                let mut modem = Modem::new(dev, irq);
                                match modem.apply(&valid).await {
                                    Ok(()) => {
                                        log_config(&valid);
                                        defmt::info!("config: applied");
                                    }
                                    Err(e) => defmt::error!("config: apply failed, {}", e),
                                }
                            }
                        }
                        // Step 8: the regulator, and a way to compare the two.
                        b'm' | b'u' => {
                            if let Some(dev) = radio.as_mut() {
                                cw_until = None;
                                set_regulator(dev, byte == b'm').await;
                            }
                        }
                        b't' => sample_thermals(radio.as_mut()).await,
                        b'?' => {
                            if let Some(valid) = validate(&cfg) {
                                log_config(&valid);
                            }
                            report(radio.as_mut(), cw_until.is_some()).await;
                        }
                        b'b' => boot::reboot_to_bootloader(),
                        b'\r' | b'\n' => {}
                        other => defmt::warn!("console: unknown command {=u8:#04x}", other),
                    }
                }
            }
            // The host went away. Waiting here rather than falling through:
            // `read_packet` on a disabled endpoint returns immediately and
            // forever, so looping straight back would spin without ever
            // yielding and starve the USB task that would bring the endpoint
            // back. See `usb_log::read_for`.
            Either3::First(Err(_)) => Timer::after(Duration::from_millis(200)).await,
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

/// Phase 4: transmit one LoRa packet with whatever the console is configured
/// for, and check the `TxDone` latency against what that configuration predicts.
///
/// Phase 3 did this with the parameters compiled in. The check is the same and
/// the point of it is the same — a `TxDone` that arrives immediately, or after
/// some unrelated interval, is a `TxDone` that did not come from a packet — but
/// now the prediction comes from the configuration rather than from a constant,
/// which means it also tests that the configuration reached the chip.
///
/// The tolerance is a prediction rather than a widened window. Transmitting is
/// always done from Standby XOSC here, so the 5 ms oscillator startup phase 3
/// measured is paid before `SetTx` rather than inside the measurement.
async fn tx_packet<S, B>(
    dev: Option<&mut Lr11xx<S, B>>,
    irq: &mut radio::RadioIrq<'_>,
    config: &RadioConfig,
) where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let (Some(dev), Some(valid)) = (dev, validate(config)) else {
        return;
    };

    // Recognisable on a receiver, and short enough for any spreading factor.
    let payload = b"oxinode phase 4";
    let airtime_us = valid.airtime_us(payload.len() as u8);
    log_config(&valid);
    defmt::info!(
        "tx: {=usize}-byte payload, computed airtime {=u32} us",
        payload.len(),
        airtime_us
    );

    let mut modem = Modem::new(dev, irq);
    if let Err(e) = modem.apply(&valid).await {
        defmt::error!("tx: apply failed, {}", e);
        return;
    }
    // A bring-up image has nobody to hand a heard packet to; it is noted
    // and the transmission tried again.
    let mut backoff = Backoff::new(&valid, payload.len() as u8, Instant::now().as_ticks());
    let mut rx_buf = [0u8; config::MAX_PAYLOAD as usize];
    let sent = loop {
        match modem
            .transmit(&valid, payload, &mut backoff, &mut rx_buf)
            .await
        {
            Ok(TxOutcome::Sent(report)) => break Ok(report),
            Ok(TxOutcome::Heard(report)) => {
                defmt::info!(
                    "tx: heard {=usize} bytes while waiting; trying again",
                    report.len
                );
            }
            Err(e) => break Err(e),
        }
    };
    match sent {
        Err(e) => defmt::error!("tx: {}", e),
        Ok(report) => {
            defmt::info!(
                "tx: interrupt after {=u32} us against {=u32} us of computed airtime, pending {=u32:#010x}",
                report.elapsed_us,
                report.airtime_us,
                report.pending
            );
            // Five percent of the airtime, plus a millisecond for the SetTx
            // transaction, the PLL lock and the PA ramp.
            let slack = report.airtime_us / 20 + 1_000;
            if report.elapsed_us.abs_diff(report.airtime_us) <= slack {
                defmt::info!("tx: TxDone within {=u32} us of prediction", slack);
            } else {
                defmt::error!(
                    "tx: {=u32} us off prediction, outside the {=u32} us allowance",
                    report.elapsed_us.abs_diff(report.airtime_us),
                    slack
                );
            }
        }
    }
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

/// Listen with the current configuration for a fixed dwell, counting packets.
///
/// Returns how many arrived, so the sweep below can use it as a measurement
/// rather than as a log line.
async fn rx_listen<S, B>(
    dev: &mut Lr11xx<S, B>,
    irq: &mut radio::RadioIrq<'_>,
    config: &ValidConfig,
    offset_hz: i32,
    dwell: Duration,
    verbose: bool,
) -> u32
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    // The offset moves the *wanted* frequency, so it composes with the
    // reference correction rather than fighting it: sweeping a corrected
    // configuration sweeps around the corrected centre, which is the whole
    // measurement in step 5.
    let mut shifted = **config;
    shifted.frequency_hz = (shifted.frequency_hz as i64 + offset_hz as i64) as u32;
    let Some(shifted) = validate(&shifted) else {
        return 0;
    };
    // Always, even when the caller is a sweep and wants one line per step. A
    // sweep whose steps are labelled only by their offset is trusting that the
    // offset reached the chip, which is the one thing the sweep exists to
    // establish about everything else.
    defmt::info!(
        "rx: tuned {=u32} Hz wanted, {=u32} Hz commanded (offset {=i32} Hz)",
        shifted.frequency_hz,
        shifted.commanded_frequency_hz(),
        offset_hz
    );
    if verbose {
        log_config(&shifted);
    }

    let mut modem = Modem::new(dev, irq);
    if let Err(e) = modem.apply(&shifted).await {
        defmt::error!("rx: apply failed, {}", e);
        return 0;
    }
    if let Err(e) = modem.start_rx(&shifted).await {
        defmt::error!("rx: {}", e);
        return 0;
    }
    if verbose {
        defmt::info!("rx: listening");
    }

    let until = Instant::now() + dwell;
    let mut heard = 0u32;
    let mut buf = [0u8; 64];
    while Instant::now() < until {
        match modem.receive(&mut buf, Duration::from_millis(500)).await {
            Err(e) => defmt::error!("rx: {}", e),
            Ok(None) => {}
            Ok(Some(report)) => {
                heard += 1;
                if verbose {
                    defmt::info!(
                        "rx: PACKET {=u32}: {=usize} bytes, RSSI {=i16} dBm, SNR {=i16} dB",
                        heard,
                        report.len,
                        report.rssi_dbm,
                        report.snr_db
                    );
                    defmt::info!("rx: bytes {=[u8]:02x}", buf[..report.len.min(32)]);
                }
            }
        }
    }
    let _ = modem.standby().await;
    if verbose {
        defmt::info!(
            "rx: done at {=i32} Hz offset -- {=u32} packets",
            offset_hz,
            heard
        );
    }
    heard
}

/// Step the receive frequency across a range and count what arrives at each.
///
/// Phase 3 used this to establish that the 73 ppm error belongs to the module:
/// against a second Base Duo the window came out centred on zero, which it
/// could not be if only one of the two boards were wrong.
///
/// Phase 4 uses the same sweep for the opposite purpose. With the reference
/// correction on, this board is deliberately 73 ppm away from the peer, so the
/// window must move *down* by about 67 kHz. That is a prediction with a sign
/// and a magnitude, and it is the only check available that the correction does
/// what the arithmetic says — the alternative would be believing a number
/// because it was derived carefully.
async fn rx_sweep<S, B>(
    dev: Option<&mut Lr11xx<S, B>>,
    irq: &mut radio::RadioIrq<'_>,
    config: &RadioConfig,
    plan: SweepPlan,
) where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    let (Some(dev), Some(valid)) = (dev, validate(config)) else {
        return;
    };
    log_config(&valid);
    defmt::info!(
        "sweep: {=u32} steps of {=i32} Hz from {=i32} Hz, {=u64} s each; keep the peer transmitting",
        plan.steps,
        plan.step_hz,
        plan.start_hz,
        plan.dwell_s
    );
    if valid.correct_reference {
        defmt::info!(
            "sweep: the correction is ON, so against another nRFLR1121 every edge should sit {=i32} Hz lower than with it off",
            -reference::correction_hz(valid.frequency_hz)
        );
    } else {
        defmt::info!(
            "sweep: the correction is OFF, so this is the baseline the other run moves against"
        );
    }
    for step in 0..plan.steps {
        let offset = plan.start_hz + step as i32 * plan.step_hz;
        let heard = rx_listen(
            dev,
            irq,
            &valid,
            offset,
            Duration::from_secs(plan.dwell_s),
            false,
        )
        .await;
        defmt::info!("sweep: {=i32} Hz -> {=u32} packets", offset, heard);
    }
    defmt::info!("sweep: done");
}

/// Where a sweep starts, how far it steps, and for how long.
#[derive(Clone, Copy)]
struct SweepPlan {
    start_hz: i32,
    step_hz: i32,
    steps: u32,
    dwell_s: u64,
}

/// The reconnaissance sweep: ±700 kHz in 100 kHz steps.
///
/// Phase 3 swept ±240 kHz in 40 kHz steps and found both edges inside it. On
/// this bench it no longer does — the boards have moved apart, the peer arrives
/// at −69 dBm rather than −45 dBm, and the window is wider than ±300 kHz. That
/// is a fact about how much frequency error a LoRa receiver tolerates when it
/// has 60 dB of margin over its sensitivity, and it means a sweep that assumes
/// phase 3's answer measures nothing.
///
/// So this one is for finding where the edges *are*. [`SWEEP_EDGE`] is for
/// measuring one once it has been found.
const SWEEP_COARSE: SweepPlan = SweepPlan {
    start_hz: -700_000,
    step_hz: 100_000,
    steps: 15,
    dwell_s: 8,
};

/// The fine sweep: the window's upper edge, in 20 kHz steps.
///
/// The coarse sweep cannot answer phase 4's question. LoRa's frequency
/// tolerance at SF11 and 250 kHz is wider than the 67 kHz correction, so both
/// runs receive at every offset in the middle and the tables come out looking
/// identical. What moves is not whether the middle works — it is where the
/// window *ends*.
///
/// So this sweeps only the upper edge, at a fifth of the reconnaissance step.
/// The prediction has a sign and a magnitude: with the correction on, the edge
/// must sit 66.5 kHz *lower*, because the correction moves this board away from
/// a peer that carries the same error. An edge that does not move, or moves the
/// other way, falsifies the correction rather than being explained away.
///
/// The range runs +220 kHz to +400 kHz because both edges have to be inside it
/// — corrected at +254 kHz and uncorrected at +328 kHz, as measured. A range
/// that brackets only one of them yields a bound rather than a number, which is
/// how the first attempt at this went. Sweeping the middle of a window measures
/// nothing at all.
///
/// The dwell is 24 seconds and not 8, which is the more important of the two
/// numbers. A peer beaconing every two seconds gives three or four packets in
/// eight seconds, and locating an edge from counts that small is guesswork —
/// a first pass put two nominally identical runs a whole step apart. Twelve
/// packets a point costs three times the wall clock and is the difference
/// between an edge and an impression of one.
const SWEEP_EDGE: SweepPlan = SweepPlan {
    start_hz: 220_000,
    step_hz: 20_000,
    steps: 10,
    dwell_s: 24,
};

/// Apply one console key to the configuration.
///
/// Every parameter with a small domain cycles rather than being typed, because
/// this console reads raw bytes off a serial port with no line editing, and a
/// key that always does something is easier to use — and much easier to
/// describe in a bring-up log — than a number that has to be parsed and might
/// not be.
///
/// Frequency is the exception, since its domain is 26 MHz wide, so it steps.
fn edit_config(config: &mut RadioConfig, key: u8) {
    match key {
        b'S' => {
            config.spreading_factor = if config.spreading_factor >= lora::SF_MAX {
                lora::SF_MIN
            } else {
                config.spreading_factor + 1
            };
        }
        b'W' => {
            let next = lora::BANDWIDTHS
                .iter()
                .position(|(_, hz)| *hz == config.bandwidth_hz)
                .map_or(0, |i| (i + 1) % lora::BANDWIDTHS.len());
            config.bandwidth_hz = lora::BANDWIDTHS[next].1;
        }
        b'C' => {
            config.coding_rate = if config.coding_rate >= config::CR_MAX {
                config::CR_MIN
            } else {
                config.coding_rate + 1
            };
        }
        b'P' => {
            let next = POWER_LADDER
                .iter()
                .position(|dbm| *dbm == config.tx_power_dbm)
                .map_or(0, |i| (i + 1) % POWER_LADDER.len());
            config.tx_power_dbm = POWER_LADDER[next];
        }
        // Saturating at the band edges rather than wrapping. A frequency key
        // held down should stop at 928 MHz, not reappear at 902.
        b'[' => {
            config.frequency_hz = config
                .frequency_hz
                .saturating_sub(FREQUENCY_STEP_HZ)
                .max(pa::US915_MIN_HZ);
        }
        b']' => {
            config.frequency_hz = config
                .frequency_hz
                .saturating_add(FREQUENCY_STEP_HZ)
                .min(pa::US915_MAX_HZ);
        }
        b'R' => config.correct_reference = !config.correct_reference,
        // Two sync words, because there are two things on this bench worth
        // talking to: an RNode uses the private-network value and the
        // Meshtastic board next to it does not.
        b'N' => {
            config.sync_word = if config.sync_word == config::SYNC_WORD_PRIVATE {
                meshtastic::long_fast::SYNC_WORD
            } else {
                config::SYNC_WORD_PRIVATE
            };
        }
        // The peer board's settings, in one key.
        //
        // The correction goes *off* with this preset, and that is the whole
        // point of it being a preset: the peer carries the same 73 ppm error
        // this board does, so correcting for it would move this board 67 kHz
        // away from the only radio on the bench that answers.
        b'M' => {
            *config = RadioConfig {
                frequency_hz: meshtastic::US_LONG_FAST_HZ,
                bandwidth_hz: meshtastic::long_fast::BANDWIDTH_HZ,
                spreading_factor: meshtastic::long_fast::SF,
                coding_rate: 5,
                preamble_symbols: meshtastic::long_fast::PREAMBLE,
                sync_word: meshtastic::long_fast::SYNC_WORD,
                correct_reference: false,
                ..config::DEFAULT
            };
        }
        b'D' => *config = config::DEFAULT,
        _ => {}
    }
}

/// Output powers the `P` key steps through.
///
/// The last two are above what the low-power PA can produce, so they select the
/// high-power one — which nothing on this board has ever measured. They are
/// included because the module is rated for 20 dBm and a modem that can only
/// reach 14 is not using the hardware, and [`log_config`] says loudly when one
/// of them is selected.
const POWER_LADDER: [i8; 7] = [pa::LP_MIN_DBM, -10, 0, 7, pa::LP_MAX_DBM, 17, 20];

/// How far the `[` and `]` keys move the frequency.
const FREQUENCY_STEP_HZ: u32 = 100_000;

/// Validate a configuration, logging why not.
fn validate(config: &RadioConfig) -> Option<ValidConfig> {
    match ValidConfig::new(*config) {
        Ok(valid) => Some(valid),
        Err(e) => {
            defmt::error!("config: refused -- {=str}", e.message());
            None
        }
    }
}

/// Print a configuration in the units a person reasons in.
///
/// Both frequencies, always. The commanded one is what the chip is told and the
/// wanted one is what should come out of the antenna; phase 3 spent a long time
/// on the 73 ppm between them, and a status line that showed only one of the two
/// would make that distinction invisible again.
fn log_config(config: &ValidConfig) {
    defmt::info!(
        "config: {=u32} Hz wanted, {=u32} Hz commanded ({=str}), SF{=u8} BW{=u32} CR4/{=u8}, {=i8} dBm",
        config.frequency_hz,
        config.commanded_frequency_hz(),
        if config.correct_reference {
            "corrected"
        } else {
            "uncorrected"
        },
        config.spreading_factor,
        config.bandwidth_hz,
        config.coding_rate,
        config.tx_power_dbm
    );
    defmt::info!(
        "config: preamble {=u16}, sync {=u8:#04x}, crc {=bool}, {=str} header, {=u32} bps, {=u32} us airtime for 16 bytes",
        config.preamble_symbols,
        config.sync_word,
        config.crc,
        if config.implicit_header {
            "implicit"
        } else {
            "explicit"
        },
        config.bitrate_bps(),
        config.airtime_us(16)
    );
    if !config.is_rnode_representable() {
        defmt::warn!(
            "config: an RNode host could not ask for this; SF must be 7-12 and power >= 0"
        );
    }
    if config.pa_config().pa_sel != 0 {
        defmt::warn!(
            "config: {=i8} dBm selects the HIGH-POWER PA, which nothing on this board has ever measured. Phase 3 only ever keyed the low-power one. Attach an antenna and expect a number you have not seen before",
            config.tx_power_dbm
        );
    }
}
