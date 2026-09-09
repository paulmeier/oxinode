//! Bring-up image for the Bluetooth stack.
//!
//! It starts MPSL and the SoftDevice Controller, reports
//! what they say about themselves, and advertises as `RNode XXXX`. It accepts
//! a connection and logs what happens to it.
//!
//! It serves the Nordic UART Service and answers the RNode protocol on it —
//! everything except the radio, which this image does not have. So Sideband
//! finds the device, detects it, reads its firmware version, and then fails
//! at radio initialisation, which is the honest result: bytes went both ways
//! through the whole stack and were understood, on a board with no radio.
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
use embassy_futures::join::{join, join4};
use embassy_futures::select::{select, Either};
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, interrupt, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use nrf_sdc::{self as sdc, mpsl};
use oxinode::ble::{self, Vbus};
use oxinode::board::{self, Led};
use oxinode::nus;
use oxinode::{boot, usb_log};
use oxinode_core::ble as interop;
use oxinode_core::rnode::command::error;
use oxinode_core::rnode::outbox::Outbox;
use oxinode_core::rnode::protocol::{Action, Protocol};
use oxinode_core::rnode::store::DeviceStore;
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
    // image binds here, and not MPSL's handler alone, which is what the first
    // version of this image bound and what stormed. See `oxinode::ble::Vbus`.
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

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0005;

// # Why this image handles faults and the others do not
//
// A fault and a spin loop are the same thing from the outside: a board that
// stops answering. The other images never need to tell them apart, because
// every stall has a log line just before it. This one is bringing up a binary
// blob that runs with interrupts of its own, and "it went quiet" is all the
// symptom it gives.
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

/// How long the controller gets to start before its address is taken.
///
/// A successful bring-up is over in milliseconds, and the longest thing it
/// could legitimately wait for is a crystal, at a quarter of a second. Two
/// seconds is an order of magnitude past that.
const STALL_AFTER_MS: u32 = 2_000;

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
static START: Signal<CriticalSectionRawMutex, u8> = Signal::new();

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    // Read and clear before anything else can fault, so this is the *previous*
    // run's verdict rather than this one's.
    let last_fault = ble::fault::take();
    let last_stall = ble::stall::take();
    let p = embassy_nrf::init(ble::embassy_config());

    // Before the USB driver is built, because its constructor enables the
    // interrupt and lowering the priority afterwards leaves a window where a
    // transfer can delay the radio.
    ble::yield_to_mpsl(&[interrupt::USBD]);

    // Before USB, before anything waits on a timer: if this says the clock is
    // dead then every stall after it is explained, and none of them are
    // Bluetooth's fault.
    ble::report_clock();
    ble::report_lfclk();
    ble::fault::report(last_fault);
    ble::stall::report(last_stall);

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
    let vbus = &vbus;

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
        defmt::info!(
            "ble: send a byte to bring the controller up: 'i' for the internal RC, anything else for the crystal; 'Q' reboots"
        );
        let key = START.wait().await;

        // Said again here, because the boot-time copy goes out before any
        // terminal can be open to read it, and this is the first moment one
        // is certain to be. A short pause lets the pump deliver it before the
        // bring-up below can stop the world.
        ble::fault::report(last_fault);
        ble::stall::report(last_stall);
        defmt::info!(
            "vbus: the bootloader left interrupt enables {=u32:#010x}",
            vbus.inherited()
        );
        Timer::after(Duration::from_millis(200)).await;

        // The crystal is the board's clock and the default. The RC oscillator
        // stays selectable because it was the control that showed the failure
        // was a race and not a configuration -- see
        // docs/architecture/bluetooth.md -- and a control is worth keeping.
        let source = if key == b'i' {
            ble::LfSource::InternalRc
        } else {
            ble::LfSource::Crystal
        };
        ble::fault::mark(match source {
            ble::LfSource::Crystal => ble::fault::BRINGUP_CRYSTAL,
            ble::LfSource::InternalRc => ble::fault::BRINGUP_RC,
        });
        stage_led.on();
        // If the bring-up is still inside `mpsl_init` when this fires, the
        // address it is at comes back on the next boot. See `ble::stall`.
        ble::stall::arm(STALL_AFTER_MS);
        let built = ble::bring_up(mpsl_p, sdc_p, rng, Irqs, source);
        ble::stall::disarm();
        stage_led.off();
        ble::fault::mark(ble::fault::NONE);

        let Some((mpsl, controller)) = built else {
            return;
        };
        join(mpsl.run(), run(controller, device_id)).await;
    };

    join4(
        usb.run(),
        pump,
        // The 1200-baud touch and the blink share this image's one port, so
        // they are joined with the Bluetooth work rather than raced against
        // it: a board that cannot be reflashed from the keyboard is a board
        // that needs its reset button.
        idle(&mut led, &mut log_rx, &log_control),
        bluetooth,
    )
    .await;
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
        .set_io_capabilities(IoCapabilities::DisplayOnly)
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
    // The same protocol core the USB port runs, with an empty EEPROM: this
    // image has no flash driver and provisioning over Bluetooth is not a
    // thing anyone does. What matters is that detect, version and the rest of
    // the conversation are answered by the real thing.
    let mut protocol = Protocol::with_storage(DeviceStore::new(), device_id);
    let mut outbox = Outbox::<{ nus::OUTBOX }>::new();

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
            let conn = match conn.with_attribute_server(&server) {
                Ok(conn) => conn,
                Err(e) => {
                    defmt::error!("ble: could not attach the GATT server: {}", e);
                    continue;
                }
            };
            let end = nus::session(&conn, &server, &mut protocol, &mut outbox, &mut NoRadio).await;
            defmt::info!("ble: session over: {}", end);
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

/// Whether the VBUS comparator currently sees a cable, straight from the
/// register — the LED's one bit must not depend on anything that could be
/// what is broken.
fn vbus_present() -> bool {
    embassy_nrf::pac::POWER.usbregstatus().read().vbusdetect()
}

/// What this image does with a command that needed a radio: says so.
///
/// The same answer `rnode` gives when its radio did not come up, and for the
/// same reason -- a host told "hardware initialisation error" looks at the
/// radio, and a host left waiting looks at the cable.
struct NoRadio;

impl nus::Act for NoRadio {
    async fn act(
        &mut self,
        protocol: &mut Protocol,
        action: Action<'_>,
        outbox: &mut Outbox<{ nus::OUTBOX }>,
    ) {
        match action {
            Action::None | Action::Redraw => {}
            // Nothing to persist to, and nothing to reset for.
            Action::Persist => defmt::info!("nus: a write the image cannot keep"),
            Action::Reset => defmt::info!("nus: reset requested; this image stays"),
            Action::ReportDisplay => {
                defmt::info!("nus: display read on an image with no panel")
            }
            _ => protocol.report_error(error::INITRADIO, outbox),
        }
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
        let period = if vbus_present() { 25 } else { 5 };
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
            // Handled here rather than where `START` is consumed, because
            // that future moves on once the stack is up and would never see
            // it: a reboot has to be reachable from any state.
            Either::First(Some(n)) if n > 0 && buf[0] == b'Q' => {
                defmt::info!("ble: rebooting");
                Timer::after(Duration::from_millis(100)).await;
                boot::reboot();
            }
            Either::First(Some(n)) if n > 0 => START.signal(buf[0]),
            Either::First(_) => {}
            Either::Second(()) => {}
        }
    }
}
