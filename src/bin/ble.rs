//! Phase 8 bring-up image for the Bluetooth stack.
//!
//! Currently at step 1: it starts MPSL and the SoftDevice Controller, reports
//! what they say about themselves, and advertises as `RNode XXXX`. It accepts
//! a connection and logs what happens to it.
//!
//! **There is no GATT server yet.** A phone can see this board and connect to
//! it, and will then find nothing to talk to. That is deliberate: whether the
//! controller can run at all on this board is one question, and whether the
//! service definition is right is another, and answering them in separate
//! images means a failure says which.
//!
//! Like `radio` and `display`, this image exposes a **single** CDC-ACM port
//! and it is a log port, so it can still take the 1200-baud touch.
//!
//! # The order things happen in, and why
//!
//! The controller is started **before USB**, as the first thing after
//! `embassy_nrf::init`. That is not the natural order for an image whose only
//! way of saying anything is USB, and it is deliberate: every working example
//! of `nrf-sdc` brings MPSL up before anything else is running, and an earlier
//! version of this image that started it from inside a task, with USB
//! enumerated and timers armed, hung inside `mpsl_init` and never came back.
//! Whether that difference matters is the question this ordering asks.
//!
//! Everything the controller says is buffered and comes out once USB is up a
//! few milliseconds later, so nothing is lost by going first.
//!
//! # Recovering from a hang without the reset button
//!
//! A spin inside either blob stops the executor, and with it USB, the log, and
//! the 1200-baud touch. So the bring-up leaves a breadcrumb in `GPREGRET2`
//! before it starts and clears it after — see [`oxinode::ble::fault`].
//!
//! A board that comes up wearing that breadcrumb **skips the bring-up
//! entirely** and comes up as an ordinary USB device that says why. So a hang
//! costs one press of reset, not a double-tap into the bootloader: press it
//! once, the board enumerates, and it can be reflashed from the keyboard.
//!
//! # What a run should show
//!
//! ```text
//! fault: none recorded on the previous run
//! mpsl: build <revision>
//! sdc:  build <revision>
//! ble:  address C0:...  name "RNode 4F2A"
//! ble:  advertising
//! ```
//!
//! and then, on a connection, `ble: connected` followed by `ble: disconnected`
//! when the phone gives up on a device with no services.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::{join, join5};
use embassy_futures::select::{select, Either};
use embassy_nrf::interrupt::InterruptExt;
use embassy_nrf::mode::Blocking;
use embassy_nrf::rng::Rng;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, interrupt, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use oxinode::ble::{self, Vbus};
use oxinode::board::{self, Led};
use oxinode::{boot, usb_log};
use oxinode_core::ble as interop;
use static_cell::StaticCell;
use trouble_host::prelude::*;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    // The link layer's own. MPSL sets their priorities itself: the three
    // below to P0, and the two above to P4.
    RADIO => mpsl::HighPrioInterruptHandler;
    TIMER0 => mpsl::HighPrioInterruptHandler;
    RTC0 => mpsl::HighPrioInterruptHandler;
    EGU0_SWI0 => mpsl::LowPrioInterruptHandler;
    // Not `usb::vbus_detect::InterruptHandler`, which is what every other
    // image binds here. See `oxinode::ble::Vbus` for why it cannot be.
    CLOCK_POWER => mpsl::ClockInterruptHandler;
});

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0005;

// # Why this image handles faults and the others do not
//
// A fault and a spin loop are the same thing from the outside: a board that
// stops answering. Phases 1 to 7 never needed to tell them apart, because
// every stall had a log line just before it. This one is bringing up a binary
// blob that runs with interrupts of its own, and "it went quiet" has already
// cost several trips to the reset button.
//
// So both are caught, recorded in a register that survives the reset, and
// turned into a reboot into the bootloader -- which is visible from the host,
// recoverable from the keyboard, and unambiguous: if the board lands in DFU
// it faulted, and if it just goes quiet it is spinning.
#[cortex_m_rt::exception]
unsafe fn HardFault(_frame: &cortex_m_rt::ExceptionFrame) -> ! {
    ble::fault::record_and_reboot(ble::fault::HARD_FAULT)
}

#[cortex_m_rt::exception]
unsafe fn DefaultHandler(irqn: i16) {
    // Exceptions are negative and interrupts are not; only the latter fit in
    // the six bits there is room for, and only the latter are a question of
    // who enabled what.
    let code = if irqn >= 0 {
        ble::fault::UNHANDLED | (irqn as u8 & 0x3F)
    } else {
        ble::fault::HARD_FAULT
    };
    ble::fault::record_and_reboot(code)
}

/// Set when a byte arrives on the log port, which is how the Bluetooth
/// bring-up is started.
///
/// # Why it is not started automatically
///
/// Because a synchronous hang inside either blob stops the executor, and with
/// it the 1200-baud touch that is the only way to reflash this board from the
/// keyboard. An image that starts the stack on its own and then wedges has to
/// be recovered by physically double-tapping reset.
///
/// Requiring a keystroke makes that recoverable: the board comes up, enumerates
/// and sits there. Attach a terminal and press a key to run the sequence; if it
/// hangs, reset, attach, and *do not* press a key -- the board is then an
/// ordinary USB device that takes a touch and a new image.
/// `CriticalSectionRawMutex` rather than `NoopRawMutex` only because a
/// `static` has to be `Sync`; both halves run on the one executor.
static START: Signal<CriticalSectionRawMutex, ()> = Signal::new();

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    // Read and clear before anything else can fault, so this is the *previous*
    // run's verdict rather than this one's.
    let last_fault = ble::fault::take();
    let p = embassy_nrf::init(ble::embassy_config());

    // Before the USB driver is built, because its constructor enables the
    // interrupt and lowering the priority afterwards leaves a window where a
    // transfer can delay the radio.
    ble::yield_to_mpsl(&[interrupt::USBD]);
    // Before anything the controller does. See the function.
    ble::set_sevonpend();

    // Before USB, before anything waits on a timer: if this says the clock is
    // dead then every stall after it is explained, and none of them are
    // Bluetooth's fault.
    ble::report_clock();
    ble::report_lfclk();
    ble::fault::report(last_fault);

    // The blue LED is this image's last channel: everything else -- the log,
    // the bootloader touch, the blink -- needs the executor, and a spin inside
    // either blob stops the executor. Lit for exactly as long as the bring-up
    // below is running.
    let stage_led = Led::new(p.P1_04);

    // The peripherals the two blobs claim. Assembled here because they are
    // holder structs with no side effects, and because it lets the bring-up
    // below own them.
    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );
    let rng = p.RNG;

    let vbus = Vbus::take();
    let driver = Driver::new(p.USBD, Irqs, vbus.detector());
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode BLE bring-up");
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
    let (mut log_tx, mut log_rx, log_control) = logs.split_with_control();
    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    // `|| true` rather than gating on DTR, unlike the other bring-up images.
    // Theirs run a one-shot sequence at boot that would be thrown away if it
    // were drained before a terminal attached. This one runs its sequence when
    // a key is pressed, which cannot happen until a terminal *is* attached, so
    // there is nothing to hold back -- and gating on DTR would make the log
    // depend on a timer, which is one of the things being diagnosed here.
    let pump = usb_log::pump(&mut log_tx, || true);

    let device_id = board::device_id();

    // A shared reference is `Copy`, so the `async move` below takes a copy of
    // this one and `idle` can still borrow the same thing.
    let control = &log_control;

    // # Why the bring-up waits for a keystroke
    //
    // Because it does not reliably return. `mpsl_init` hangs -- no fault, no
    // assert, no reset -- and when it does, the executor stops with it and the
    // board is a USB device that enumerated once and then went silent.
    //
    // An image that attempts it at boot is therefore an image that can come up
    // dead, and a dead board cannot be reflashed from the keyboard. Waiting
    // for a byte means the board is *always* usable when it comes up: attach a
    // terminal and press a key to try the stack, or do not, and reflash it.
    //
    // The bring-up also ran before USB for a while, on the theory that the
    // examples all do it that way and something about a running executor was
    // to blame. It made no difference: it hangs identically either side of
    // enumeration. So it is here, where a failure is at least visible.
    let bluetooth = async move {
        let mut stage_led = stage_led;
        let _ = control;
        if last_fault == ble::fault::BRINGUP_CRYSTAL || last_fault == ble::fault::BRINGUP_RC {
            defmt::warn!("ble: the previous attempt hung; press a key to try again anyway");
        }
        defmt::info!("ble: send any byte on this port to bring the controller up");
        START.wait().await;

        // # Why the internal RC and not the crystal
        //
        // Both have been seen to hang and both have been seen to work, which
        // is the single most useful thing established about this failure: it
        // is a race, not a configuration error. `InternalRc` is the one that
        // has been seen to get all the way to advertising, so it is what this
        // tries. See `docs/phase-8-bluetooth.md`.
        //
        // It would be the wrong choice if it worked: the RC oscillator is
        // specified at 250 ppm against the crystal's 20, and BLE spends that
        // drift on wider receive windows, which costs current at both ends of
        // every connection. It is somewhere to stand, not somewhere to ship.
        let source = ble::LfSource::InternalRc;
        ble::fault::mark(match source {
            ble::LfSource::Crystal => ble::fault::BRINGUP_CRYSTAL,
            ble::LfSource::InternalRc => ble::fault::BRINGUP_RC,
        });
        stage_led.on();
        let built = bring_up(mpsl_p, sdc_p, rng, source);
        stage_led.off();
        ble::fault::mark(ble::fault::NONE);

        let Some((mpsl, controller)) = built else {
            return;
        };
        join(mpsl.run(), run(controller, device_id)).await;
    };

    join5(
        usb.run(),
        pump,
        vbus.run(),
        // The 1200-baud touch and the blink share this image's one port, so
        // they are joined with the Bluetooth work rather than raced against
        // it: a board that cannot be reflashed from the keyboard is a board
        // that needs its reset button.
        idle(&mut led, &mut log_rx, &log_control),
        bluetooth,
    )
    .await;
}

/// Start MPSL and the SoftDevice Controller, or say why not.
///
/// Straight-line and synchronous: there is nothing to await, and nothing that
/// could usefully be awaited. Every log line it writes is buffered until USB
/// comes up a few milliseconds later.
fn bring_up(
    mpsl_p: mpsl::Peripherals<'static>,
    sdc_p: sdc::Peripherals<'static>,
    rng: embassy_nrf::Peri<'static, peripherals::RNG>,
    source: ble::LfSource,
) -> Option<(
    &'static MultiprotocolServiceLayer<'static>,
    sdc::SoftdeviceController<'static>,
)> {
    // MPSL drives its deferred work from this interrupt. It enables it itself
    // at the end of `mpsl_init`, which is too late to be of use to anything
    // `mpsl_init` waits for -- so it is enabled first. The handler is bound and
    // harmless before init, since all it does is wake a waker.
    unsafe { interrupt::EGU0_SWI0.enable() };

    defmt::info!("mpsl: init on {}", source);
    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = match MultiprotocolServiceLayer::new(mpsl_p, Irqs, ble::lfclk_config(source)) {
        Ok(mpsl) => MPSL.init(mpsl),
        Err(e) => {
            defmt::error!("mpsl: would not start: {}", e);
            return None;
        }
    };
    defmt::info!("mpsl: running");

    // Blocking, so the RNG interrupt is not bound at all. The controller pulls
    // random numbers from a callback it makes at its own priority, and an
    // asynchronous source there would be one more thing to get out of MPSL's
    // way for no benefit.
    static RNG: StaticCell<Rng<'static, Blocking>> = StaticCell::new();
    let rng = RNG.init(Rng::new_blocking(rng));
    static SDC_MEM: StaticCell<sdc::Mem<{ ble::SDC_MEM }>> = StaticCell::new();
    defmt::info!("sdc: init");
    let controller = match ble::build_controller(sdc_p, rng, mpsl, SDC_MEM.init(sdc::Mem::new())) {
        Ok(controller) => controller,
        Err(e) => {
            defmt::error!("sdc: would not start: {}", e);
            return None;
        }
    };
    defmt::info!("sdc: running");
    ble::report_versions();
    Some((mpsl, controller))
}

/// Advertise, accept a connection, and say what happened.
///
/// Takes the controller concretely rather than behind `trouble_host::Controller`.
/// The generic version needs a `ControllerCmdSync` bound per HCI command used,
/// which is four here and would grow with every feature; naming the type says
/// the same thing and says it once.
async fn run(controller: sdc::SoftdeviceController<'static>, device_id: u64) {
    let address = interop::static_random_address(device_id);
    if !interop::is_valid_static_random(&address) {
        // Reachable only from an unprogrammed FICR, and worth saying out loud
        // because the symptom is a peripheral every scanner ignores.
        defmt::error!(
            "ble: {=[u8; 6]:02x} is not a usable static random address",
            address
        );
    }
    let name = interop::advertised_name(device_id);
    let name = core::str::from_utf8(&name).expect("advertised_name is ASCII");

    static RESOURCES: StaticCell<
        HostResources<DefaultPacketPool, { ble::CONNECTIONS }, { ble::L2CAP_CHANNELS }>,
    > = StaticCell::new();
    let stack = trouble_host::new(controller, RESOURCES.init(HostResources::new()))
        .set_random_address(Address::random(address))
        .build();

    let mut runner = stack.runner();
    let mut peripheral = stack.peripheral();

    defmt::info!(
        "ble: address {=[u8; 6]:02x} (reversed on the wire), name {=str}",
        address,
        name
    );

    // Flags and the name in the advertisement itself; the service UUID in the
    // scan response. All three will not fit in one legacy packet -- 31 bytes,
    // and a 128-bit UUID costs 18 of them -- and the name is the half that
    // must be in the advertisement, because that is what Reticulum's scan
    // filters on. See `oxinode_core::ble::NAME_PREFIX`.
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
            defmt::info!("ble: advertising");
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

            let conn = match advertiser.accept().await {
                Ok(conn) => conn,
                Err(_) => {
                    defmt::warn!("ble: advertising stopped without a connection");
                    continue;
                }
            };
            defmt::info!(
                "ble: connected to {=[u8; 6]:02x}",
                conn.peer_address().addr.into_inner()
            );

            // Nothing to serve yet, so the only thing to do is watch. A phone
            // that finds no services here disconnects on its own after a few
            // seconds, and seeing that happen is the point of step 1.
            loop {
                match conn.next().await {
                    ConnectionEvent::Disconnected { reason } => {
                        defmt::info!("ble: disconnected, reason {=u8:#04x}", reason.into_inner());
                        break;
                    }
                    ConnectionEvent::ConnectionParamsUpdated {
                        conn_interval,
                        supervision_timeout,
                        ..
                    } => defmt::info!(
                        "ble: interval {=u64} us, supervision timeout {=u64} ms",
                        conn_interval.as_micros(),
                        supervision_timeout.as_millis()
                    ),
                    ConnectionEvent::DataLengthUpdated {
                        max_tx_octets,
                        max_rx_octets,
                        ..
                    } => defmt::info!(
                        "ble: data length tx {=u16} rx {=u16} octets",
                        max_tx_octets,
                        max_rx_octets
                    ),
                    ConnectionEvent::PhyUpdated { tx_phy, rx_phy } => {
                        defmt::info!("ble: phy tx {=u8} rx {=u8}", tx_phy as u8, rx_phy as u8)
                    }
                    _ => {}
                }
            }
        }
    };

    // `runner.run()` is what actually moves bytes between the controller and
    // the host; without it nothing above ever completes. It returns only on a
    // failure of the controller itself.
    match select(runner.run(), advertise).await {
        Either::First(Err(_)) => defmt::error!("ble: the host stack stopped"),
        Either::First(Ok(())) | Either::Second(()) => {}
    }
}

/// Blink, and watch for the 1200-baud touch.
///
/// This image has one serial port and it is this one, so the touch has to be
/// checked here or reflashing means walking over to the reset button.
async fn idle<'d, D: UsbDriverTrait<'d>>(
    led: &mut Led<'_>,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
) {
    let mut buf = [0u8; 64];
    let mut ticks = 0u32;
    loop {
        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }
        // The blink rate carries one bit: whether the VBUS comparator sees a
        // cable. If `Vbus` reads the wrong register the board never
        // enumerates, and then the LED is the only channel left. Slow is
        // normal. Fast means the register says there is no cable, on a board
        // that is being powered through one.
        let period = if Vbus::present() { 25 } else { 5 };
        if ticks % period == 0 {
            led.toggle();
        }
        // Rate-limited, and deliberately reporting the clock: if these arrive
        // one per second the timers are fine, and if they only arrive when
        // something is typed then they are not.
        if ticks % 50 == 0 {
            defmt::info!(
                "idle: tick {=u32}, t={=u64} us",
                ticks,
                embassy_time::Instant::now().as_micros()
            );
        }
        ticks = ticks.wrapping_add(1);

        // Raced against `control_changed`, which is woken by the USB interrupt
        // rather than by a timer. That is what makes the 1200-baud touch above
        // still work on a board whose timers have stopped -- which is the one
        // failure that would otherwise need the reset button.
        match select(
            usb_log::read_for(rx, &mut buf, Duration::from_millis(20)),
            control.control_changed(),
        )
        .await
        {
            Either::First(Some(n)) if n > 0 => START.signal(()),
            Either::First(_) => {}
            Either::Second(()) => {}
        }
    }
}
