//! The Bluetooth LE stack: what it needs, and what it takes away.
//!
//! Three layers, from the bottom:
//!
//! * **MPSL** (`nrf-mpsl`) owns the radio's *time*. It arbitrates the RADIO
//!   peripheral between protocols and keeps the link timing that BLE depends
//!   on. It is a binary blob from Nordic.
//! * **The SoftDevice Controller** (`nrf-sdc`) is the Bluetooth link layer,
//!   also a blob, speaking HCI. Note that this is *not* the S140 SoftDevice
//!   sitting in flash at `0x1000`: that one stays disabled exactly as it has
//!   always been, and we keep linking above it. See the README.
//! * **TrouBLE** (`trouble-host`) is the host — L2CAP, ATT, GATT, and the
//!   security manager — and is ordinary Rust.
//!
//! # The four things it takes
//!
//! Everything in this module exists because the controller is not a library
//! you call: it is a co-resident that claims hardware.
//!
//! 1. **Peripherals.** `RTC0`, `TIMER0`, `TEMP`, `RADIO`, and PPI channels 17
//!    to 31. `embassy-time` here runs on `RTC1`, so the clock we already
//!    depend on is not one of them.
//! 2. **Interrupt priorities.** See [`APP_PRIORITY`].
//! 3. **The critical section.** See the `ble` feature in `Cargo.toml`: masking
//!    every interrupt, which is what a single-core Cortex-M implementation
//!    does, stops the clocks the link layer keeps its timing with.
//! 4. **`CLOCK_POWER`.** See [`Vbus`].
//!
//! There is a fifth that this module cannot solve, and it is written down here
//! because it is invisible from the outside: **`NVMC` stalls the CPU.** An
//! erase of one flash page takes about 85 ms during which the core does not
//! execute, and MPSL cannot keep a connection alive through that. Provisioning
//! writes the device record with `NVMC` directly. Until that moves onto
//! `nrf_mpsl::Flash` — which schedules the write inside a timeslot — a
//! provisioning run while a phone is connected will drop the connection.

use core::sync::atomic::{AtomicPtr, Ordering};

use embassy_nrf::config::Config;
use embassy_nrf::interrupt::{self, InterruptExt, Priority};
use embassy_nrf::mode::Blocking;
use embassy_nrf::rng::Rng;
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;

use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;

/// Priority every application interrupt runs at in a BLE image.
///
/// MPSL sets its own: `RADIO`, `RTC0` and `TIMER0` to `P0`, and both
/// `CLOCK_POWER` and its low-priority software interrupt to `P4`. What it
/// cannot do is lower *ours*, and the NVIC's reset value is zero — the same
/// `P0` the link layer runs at.
///
/// Two interrupts at equal priority do not preempt each other, so a USB
/// transfer left at `P0` does not interrupt the radio handler; it *delays* it
/// by however long the USB handler runs. That is worse, not better: a link
/// layer that misses its window loses the connection event, and enough of
/// those lose the connection. So everything of ours moves down.
///
/// `embassy-nrf` sets a priority for exactly two things, GPIOTE and the time
/// driver, both from its `Config` — see [`embassy_config`]. Every other bound
/// interrupt keeps whatever the NVIC had, which is why [`yield_to_mpsl`]
/// exists.
///
/// `P2` rather than `P4`: it leaves room below us for MPSL's own low-priority
/// processing, which is where it does work that is allowed to be interrupted.
pub const APP_PRIORITY: Priority = Priority::P2;

/// The board's usual `embassy-nrf` configuration, with priorities MPSL can
/// live with.
///
/// Only the two priorities differ from [`crate::board::embassy_config`]; the
/// oscillator choices are the same, and have to be — see [`lfclk_config`].
pub fn embassy_config() -> Config {
    let mut config = crate::board::embassy_config();
    config.gpiote_interrupt_priority = APP_PRIORITY;
    config.time_interrupt_priority = APP_PRIORITY;
    config
}

/// Move the interrupts this image binds out of the link layer's way.
///
/// Call once, after `embassy_nrf::init` and before the peripherals that use
/// them are constructed — a driver may enable its interrupt in its
/// constructor, and lowering the priority of an interrupt that has already
/// fired at `P0` is a fix applied too late.
///
/// Listing them by hand is unpleasant and is the only way: an interrupt is
/// bound by `bind_interrupts!` in each image, and nothing hands back the set.
/// The cost of forgetting one is a link that works until that peripheral is
/// busy, which is the kind of fault that gets blamed on the phone.
pub fn yield_to_mpsl(irqs: &[interrupt::Interrupt]) {
    for irq in irqs {
        irq.set_priority(APP_PRIORITY);
    }
}

/// Low-frequency clock configuration, handed to MPSL at init.
///
/// The Base Duo has a real 32.768 kHz crystal that is known to run, so
/// this says so. The alternative — the internal RC — drifts by a couple of
/// hundred ppm, and BLE spends that drift on longer receive windows: the
/// controller widens every one to cover the worst case it is told about. A
/// crystal is a factor of ten better and the part is already fitted.
///
/// `accuracy_ppm` is a promise to the controller, not a measurement of the
/// part. 20 ppm is the usual specification for a 32.768 kHz watch crystal.
/// Claiming better than the part delivers loses packets at the edges of the
/// window; claiming worse only costs current.
///
/// Which oscillator MPSL is told to keep time with.
#[derive(Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum LfSource {
    /// The 32.768 kHz crystal the board actually has.
    Crystal,
    /// The internal RC oscillator, calibrated against temperature.
    ///
    /// Worse in every way that matters — a couple of hundred ppm against the
    /// crystal's twenty, and BLE spends that drift on wider receive windows —
    /// but it needs no external part and takes a different path through MPSL's
    /// initialiser. It is here as a control, not as a configuration anyone
    /// would choose.
    InternalRc,
}

/// Low-frequency clock configuration, handed to MPSL at init.
///
/// # What this does and does not explain
///
/// `skip_wait_lfclk_started` is `false`, which is the conventional setting,
/// and it is right for the reason the disassembly gives rather than the one
/// the header suggests. MPSL's clock initialiser asks first whether the clock
/// is already running with the requested source. `embassy_nrf::init` has
/// already started LFXO — `embassy-time` runs on `RTC1` and could not keep
/// time otherwise — so the answer is yes, and the wait that follows finds its
/// condition already true and returns at once. Nothing is waited for.
///
/// This is recorded because it was suspected at length and is not, on its own,
/// the problem: three settings of that field were tried on hardware and all
/// three reached the same point. Which is why [`LfSource`] exists — the
/// remaining question is not when MPSL waits but which oscillator it is
/// waiting on.
///
/// `accuracy_ppm` is a promise to the controller, not a measurement of the
/// part. 20 ppm is the usual specification for a 32.768 kHz watch crystal.
/// Claiming better than the part delivers loses packets at the edges of the
/// receive window; claiming worse only costs current.
pub fn lfclk_config(source: LfSource) -> mpsl::raw::mpsl_clock_lfclk_cfg_t {
    match source {
        LfSource::Crystal => mpsl::raw::mpsl_clock_lfclk_cfg_t {
            source: mpsl::raw::MPSL_CLOCK_LF_SRC_XTAL as u8,
            // Both are calibration parameters for the RC oscillator, and the
            // header requires them to be zero for any other source.
            rc_ctiv: 0,
            rc_temp_ctiv: 0,
            accuracy_ppm: 20,
            skip_wait_lfclk_started: false,
        },
        LfSource::InternalRc => mpsl::raw::mpsl_clock_lfclk_cfg_t {
            source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
            // The values the header names as recommended: calibrate every four
            // seconds, and unconditionally every second interval.
            rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
            rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
            accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
            skip_wait_lfclk_started: false,
        },
    }
}

/// Say what the low-frequency clock is doing, before handing it to MPSL.
///
/// `LFCLKSTAT` reports whether the clock is running and which source it
/// settled on; `EVENTS_LFCLKSTARTED` is the flag MPSL looks for. Printed
/// because [`lfclk_config`] depends on all three being what this claims, and a
/// wrong assumption there does not produce an error — it produces a board that
/// stops.
pub fn report_lfclk() {
    let clock = embassy_nrf::pac::CLOCK;
    let stat = clock.lfclkstat().read();
    defmt::info!(
        "lfclk: running={=bool} src={=u8} started_event={=u32} inten={=u32:#05x}",
        stat.state(),
        stat.src() as u8,
        clock.events_lfclkstarted().read(),
        power::read(power::INTENSET),
    );
}

/// USB VBUS detection on the interrupt MPSL owns.
///
/// # The conflict
///
/// On the nRF52840 the `POWER` and `CLOCK` peripherals share one interrupt
/// vector *and* one interrupt-enable register, with disjoint bits.
/// `embassy-usb` normally binds the vector through `HardwareVbusDetect`, which
/// is how the images without Bluetooth learn that a cable was plugged in. MPSL
/// needs the same vector for the clock, and only one handler can be bound.
///
/// # What happens if `POWER` is left to nobody
///
/// It is not left to nobody. When the bootloader starts the application
/// **from DFU** — which is every boot that follows a flash — its own USB
/// stack has run, and it hands over with `USBDETECTED`, `USBREMOVED` and
/// `USBPWRRDY` still enabled in the shared word: `0x380`, read back at boot.
/// After a plain press of reset the word reads `0`. `USBPWRRDY` is latched
/// from the moment the regulator comes up.
///
/// Images without Bluetooth never notice, because `HardwareVbusDetect` clears
/// those events. The first Bluetooth image bound the vector to MPSL's handler
/// alone, and the moment `mpsl_init` unmasked it the line was high with
/// nothing to lower it — 841,653 handler entries in two seconds, at the
/// lowest priority there is, so USB control transfers kept working while
/// everything in thread mode stopped. It took a captured program counter to
/// see. And it looked like a race for a week of resets only because every
/// boot after a flash hung and every boot after the reset button worked,
/// and nobody was keeping track of which was which.
///
/// So this handler services `POWER` and then hands `CLOCK` to MPSL: the
/// events are cleared and folded into a `SoftwareVbusDetect`, which is what
/// `embassy-usb` was designed to be fed by exactly this. The polling version
/// that stood here before is gone; it was a workaround for a conflict that
/// turns out to be a shared line, not a shared owner.
pub struct Vbus {
    detect: &'static SoftwareVbusDetect,
    inherited: u32,
}

/// The one detector, for the handler to reach.
static DETECT: AtomicPtr<SoftwareVbusDetect> = AtomicPtr::new(core::ptr::null_mut());

/// `POWER` register offsets from the shared base. Raw, because the point of
/// the handler is what is *in* the registers, and the PAC's view of `POWER`
/// and `CLOCK` as two peripherals hides that they are one address space.
mod power {
    pub const BASE: usize = 0x4000_0000;
    pub const EVENTS_USBDETECTED: usize = 0x11C;
    pub const EVENTS_USBREMOVED: usize = 0x120;
    pub const EVENTS_USBPWRRDY: usize = 0x124;
    pub const INTENSET: usize = 0x304;
    pub const INTENCLR: usize = 0x308;
    pub const USBREGSTATUS: usize = 0x438;
    /// The three this module wants.
    pub const USB_INTS: u32 = (1 << 7) | (1 << 8) | (1 << 9);

    pub fn read(offset: usize) -> u32 {
        // SAFETY: memory-mapped registers inside the POWER/CLOCK block.
        unsafe { core::ptr::read_volatile((BASE + offset) as *const u32) }
    }
    pub fn write(offset: usize, value: u32) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((BASE + offset) as *mut u32, value) }
    }
}

impl Vbus {
    /// Take ownership of the `POWER` side of the shared interrupt.
    ///
    /// Clears whatever the bootloader left enabled, clears any latched event,
    /// seeds the detector from the live register, and then enables exactly
    /// the three USB events — so nothing is inherited, and the handler below
    /// is the only thing that ever sees them.
    ///
    /// Call once per image, before MPSL is initialised; MPSL is what unmasks
    /// the vector, and it must find `POWER` already quiet.
    pub fn take() -> Self {
        static VBUS: StaticCell<SoftwareVbusDetect> = StaticCell::new();
        let inherited = power::read(power::INTENSET);
        // The whole word, not just the `POWER` half. The bootloader's clock
        // driver leaves `HFCLKSTARTED` and `LFCLKSTARTED` enabled too, and
        // `embassy_nrf::init` leaves both of those events latched -- so
        // unmasking the vector below with those bits still set is a storm
        // before `main` has printed a line. That is what the first build of
        // this function did on every soft reboot. MPSL enables the `CLOCK`
        // bits it wants when it initialises; nothing before that needs any.
        power::write(power::INTENCLR, 0xFFFF_FFFF);
        for ev in [
            power::EVENTS_USBDETECTED,
            power::EVENTS_USBREMOVED,
            power::EVENTS_USBPWRRDY,
        ] {
            power::write(ev, 0);
        }
        let status = power::read(power::USBREGSTATUS);
        let (detected, ready) = (status & 1 != 0, status & 2 != 0);
        let detect: &'static SoftwareVbusDetect =
            VBUS.init(SoftwareVbusDetect::new(detected, ready));
        DETECT.store(detect as *const _ as *mut _, Ordering::Release);
        power::write(power::INTENSET, power::USB_INTS);
        // Unmask the vector now, at an application priority; MPSL re-sets the
        // priority to its own when it initialises. Without this the regulator
        // transition that `usb.run()` waits for is latched but never
        // delivered, and the board never enumerates -- which the first build
        // of this function established.
        interrupt::CLOCK_POWER.set_priority(APP_PRIORITY);
        interrupt::CLOCK_POWER.unpend();
        unsafe { interrupt::CLOCK_POWER.enable() };
        defmt::info!(
            "vbus: inherited interrupt enables {=u32:#010x}, now {=u32:#05x}; detected={=bool} ready={=bool}",
            inherited,
            power::USB_INTS,
            detected,
            ready
        );
        Self { detect, inherited }
    }

    /// What was in the shared interrupt-enable word when this image took it
    /// over: the bootloader's leavings.
    pub fn inherited(&self) -> u32 {
        self.inherited
    }

    /// The handle to give to `embassy_nrf::usb::Driver::new`.
    pub fn detector(&self) -> &'static SoftwareVbusDetect {
        self.detect
    }

    /// The `POWER` half of the shared handler. Cheap when nothing is set,
    /// which is every call MPSL's clock work causes.
    ///
    /// # Safety
    /// Interrupt context only.
    pub unsafe fn on_interrupt() {
        let detect = DETECT.load(Ordering::Acquire);
        let fire = |offset: usize, f: &dyn Fn(&SoftwareVbusDetect)| {
            if power::read(offset) != 0 {
                power::write(offset, 0);
                if !detect.is_null() {
                    // SAFETY: points at a `StaticCell` that lives forever.
                    f(unsafe { &*detect });
                }
            }
        };
        fire(power::EVENTS_USBDETECTED, &|d| d.detected(true));
        fire(power::EVENTS_USBREMOVED, &|d| d.detected(false));
        fire(power::EVENTS_USBPWRRDY, &|d| d.ready());
    }
}

/// The one handler for the one vector: `POWER` first, then MPSL's `CLOCK`.
///
/// Bind this to `CLOCK_POWER`, and also declare — by hand, since
/// `bind_interrupts!` will not — that the image binds
/// `mpsl::ClockInterruptHandler` there. That is the marker trait MPSL's
/// constructor asks for, and this handler calls exactly what it promises.
pub struct PowerAndClockHandler;

impl interrupt::typelevel::Handler<interrupt::typelevel::CLOCK_POWER> for PowerAndClockHandler {
    unsafe fn on_interrupt() {
        stall::CLOCK_IRQS.fetch_add(1, Ordering::Relaxed);
        unsafe {
            Vbus::on_interrupt();
            mpsl::raw::MPSL_IRQ_CLOCK_Handler();
        }
    }
}

/// Memory handed to the SoftDevice Controller, in bytes.
///
/// The controller allocates its connection state, buffers and advertising sets
/// out of one block given to it at `sdc_enable`, and the size it needs depends
/// on every `support_*` and count set on the builder. `sdc::Builder` will
/// refuse to start if this is too small and log the figure it wanted, so
/// [`build_controller`] logs `required_memory()` on the way past whether or not
/// it fits: guessing here is expected, and the log turns the guess into a
/// number.
///
/// This is the starting guess for one peripheral connection with data-length
/// extension and the 2M PHY. Tighten it once hardware has reported the real
/// figure.
pub const SDC_MEM: usize = 4096;

/// Link-layer packet size, in bytes.
///
/// 27 is what a BLE connection starts at, and 251 is the ceiling that
/// data-length extension raises it to. The difference is close to a factor of
/// five on throughput, because each packet costs a fixed turnaround either
/// way, and a KISS stream carrying Reticulum traffic is exactly the case that
/// notices. Both ends have to agree; a phone that will not do DLE simply stays
/// at 27.
const LL_PACKET_SIZE: u16 = 251;

/// How many link-layer packets the controller buffers in each direction.
///
/// Four is two connection events' worth at the usual two packets per event, so
/// the host can be a whole event late without the link going idle. More would
/// buy latency tolerance we have no evidence of needing, out of the block
/// [`SDC_MEM`] has to cover.
const LL_PACKET_COUNT: u8 = 4;

/// Start the controller.
///
/// Everything switched on here is a peripheral-side feature, because that is
/// the only role this board plays: it is advertised to and connected to, and
/// never scans or initiates. The central half of the controller library is
/// a third of a megabyte that would never be called, and is left out at the
/// crate-feature level rather than here.
pub fn build_controller(
    peripherals: sdc::Peripherals<'static>,
    rng: &'static mut Rng<'static, Blocking>,
    mpsl: &'static MultiprotocolServiceLayer<'static>,
    mem: &'static mut sdc::Mem<SDC_MEM>,
) -> Result<sdc::SoftdeviceController<'static>, sdc::Error> {
    let builder = sdc::Builder::new()?
        .support_adv()
        .support_peripheral()
        .support_dle_peripheral()
        .support_le_2m_phy()
        .support_phy_update_peripheral()
        .peripheral_count(1)?
        .adv_count(1)?
        .buffer_cfg(
            LL_PACKET_SIZE,
            LL_PACKET_SIZE,
            LL_PACKET_COUNT,
            LL_PACKET_COUNT,
        )?;

    match builder.required_memory() {
        Ok(needed) => defmt::info!("sdc: needs {=usize} bytes, has {=usize}", needed, SDC_MEM),
        Err(e) => defmt::warn!("sdc: could not ask how much memory it needs: {}", e),
    }

    builder.build(peripherals, rng, mpsl, mem)
}

/// Report whether the clock everything else waits on is actually working.
///
/// # Why an image needs this at all
///
/// A board whose `embassy-time` timers never fire does not look broken. USB
/// still enumerates, because that is driven by the `USBD` interrupt and by
/// `usb.run()` being woken by it; control transfers are still answered, so the
/// port opens. Everything that waits on a `Timer`, though — the log pump, the
/// bootloader-touch check, every poll loop in the image — stops, and stops
/// silently. The board is then a serial port that says nothing and accepts
/// nothing, which reads as "the Bluetooth stack broke" and is not.
///
/// So this asks two separate questions and prints both answers:
///
/// * **Is the counter running?** `Instant::now()` twice around a busy wait of
///   known length. Zero means `RTC1` is not counting, which means `LFCLK` is
///   not running, which is a clock problem and not a timer problem.
/// * **Is the interrupt live?** A counter that runs while its interrupt is
///   masked gives exactly the symptom above, so the `NVIC` state is printed
///   next to it.
pub fn report_clock() {
    let before = embassy_time::Instant::now();
    // ~10 ms at 64 MHz. `delay` counts cycles and needs no clock of its own,
    // which is the point: it is the one measure here that cannot be affected
    // by the thing being measured.
    cortex_m::asm::delay(640_000);
    let after = embassy_time::Instant::now();
    defmt::info!(
        "clock: rtc advanced {=u64} us across a ~10000 us busy wait",
        after.as_micros().saturating_sub(before.as_micros())
    );

    use cortex_m::peripheral::NVIC;
    defmt::info!(
        "nvic: rtc1 enabled={=bool} prio={=u8}, usbd enabled={=bool} prio={=u8}",
        NVIC::is_enabled(interrupt::RTC1),
        NVIC::get_priority(interrupt::RTC1),
        NVIC::is_enabled(interrupt::USBD),
        NVIC::get_priority(interrupt::USBD),
    );
}

/// Log what the two blobs say they are.
///
/// Both revisions are git hashes of Nordic's own build. They are worth a line
/// in the log because the crate version does not identify them: `nrf-sdc-sys`
/// vendors the binaries, and a bug found here has to be reported against the
/// controller build rather than against the wrapper.
pub fn report_versions() {
    match MultiprotocolServiceLayer::build_revision() {
        Ok(rev) => defmt::info!("mpsl: build {=[u8]:02x}", rev.as_slice()),
        Err(e) => defmt::warn!("mpsl: no build revision: {}", e),
    }
    match sdc::SoftdeviceController::build_revision() {
        Ok(rev) => defmt::info!("sdc: build {=[u8]:02x}", rev.as_slice()),
        Err(e) => defmt::warn!("sdc: no build revision: {}", e),
    }
}

/// Start MPSL and the SoftDevice Controller, or say why not.
///
/// Straight-line and synchronous: there is nothing to await, and nothing that
/// could usefully be awaited. `irqs` is the image's `bind_interrupts!` struct,
/// which has to bind `RADIO`, `TIMER0` and `RTC0` to
/// `mpsl::HighPrioInterruptHandler`, one software interrupt to
/// `mpsl::LowPrioInterruptHandler`, and `CLOCK_POWER` to
/// [`PowerAndClockHandler`] -- plus the hand-written `Binding` to MPSL's own
/// clock handler that the constructor's bound asks for.
///
/// Call once per image; the statics inside are `StaticCell`s.
pub fn bring_up<T, I>(
    mpsl_p: mpsl::Peripherals<'static>,
    sdc_p: sdc::Peripherals<'static>,
    rng: embassy_nrf::Peri<'static, embassy_nrf::peripherals::RNG>,
    irqs: I,
    source: LfSource,
) -> Option<(
    &'static MultiprotocolServiceLayer<'static>,
    sdc::SoftdeviceController<'static>,
)>
where
    T: interrupt::typelevel::Interrupt,
    I: interrupt::typelevel::Binding<T, mpsl::LowPrioInterruptHandler>
        + interrupt::typelevel::Binding<interrupt::typelevel::RADIO, mpsl::HighPrioInterruptHandler>
        + interrupt::typelevel::Binding<interrupt::typelevel::TIMER0, mpsl::HighPrioInterruptHandler>
        + interrupt::typelevel::Binding<interrupt::typelevel::RTC0, mpsl::HighPrioInterruptHandler>
        + interrupt::typelevel::Binding<
            interrupt::typelevel::CLOCK_POWER,
            mpsl::ClockInterruptHandler,
        >,
{
    defmt::info!("mpsl: init on {}", source);
    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = match MultiprotocolServiceLayer::new::<T, I>(mpsl_p, irqs, lfclk_config(source)) {
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
    static CONTROLLER_MEM: StaticCell<sdc::Mem<{ SDC_MEM }>> = StaticCell::new();
    defmt::info!("sdc: init");
    let controller = match build_controller(sdc_p, rng, mpsl, CONTROLLER_MEM.init(sdc::Mem::new()))
    {
        Ok(controller) => controller,
        Err(e) => {
            defmt::error!("sdc: would not start: {}", e);
            return None;
        }
    };
    defmt::info!("sdc: running");
    report_versions();
    Some((mpsl, controller))
}

/// How many connections the host keeps state for.
///
/// One. A phone connects to a modem; nothing in the RNode protocol has a
/// second host in mind, and what two of them would even mean has to be decided
/// before there is any point in paying for it.
pub const CONNECTIONS: usize = 1;

/// L2CAP channels the host keeps state for.
///
/// Two fixed channels are in use on a GATT-only peripheral: ATT, and the
/// signalling channel. The security manager's channel makes three, and is
/// counted now so that enabling pairing later is not also a resource change.
pub const L2CAP_CHANNELS: usize = 3;

/// Where a fault or an unhandled interrupt is recorded across the reset.
///
/// `GPREGRET2` keeps its value through a soft reset and is not used by
/// anything else: the bootloader's own mailbox is `GPREGRET`, which
/// [`crate::boot::reboot_to_bootloader`] writes. So this is a free byte that
/// survives exactly the reboot the fault handler performs.
pub mod fault {
    /// Nothing was recorded.
    pub const NONE: u8 = 0;
    /// A hard fault. There is no room for the faulting address, only the fact.
    pub const HARD_FAULT: u8 = 0x40;
    /// An interrupt fired that no handler was bound to. The low six bits are
    /// its number, which is what says who enabled it.
    pub const UNHANDLED: u8 = 0x80;
    /// The controller's initialisation was entered, on the crystal, and not
    /// left.
    ///
    /// Written before the call and cleared after it, so a board that comes up
    /// wearing one of these knows the *previous* boot did not come back. See
    /// [`mark`] — an image that reads this and does something different is a
    /// board that recovers from a hang by being reset once, rather than by
    /// being double-tapped into its bootloader.
    pub const BRINGUP_CRYSTAL: u8 = 0x01;
    /// The same, on the internal RC oscillator.
    pub const BRINGUP_RC: u8 = 0x02;

    /// Record a code and reboot into the bootloader.
    ///
    /// # Why a fault reboots rather than halts
    ///
    /// The same reason the panic handler does — see `src/lib.rs`. A halted
    /// image is a USB device that never enumerates, and on a board with no
    /// debug probe that is indistinguishable from a spin loop, from a stopped
    /// clock, and from a dead chip. Every one of them is "the board went
    /// quiet", and telling them apart is most of the work.
    ///
    /// Landing in the bootloader says *fault*, and leaves the board
    /// reflashable from the keyboard instead of from the reset button.
    pub fn record_and_reboot(code: u8) -> ! {
        embassy_nrf::pac::POWER
            .gpregret2()
            .write(|w| w.0 = code as u32);
        crate::boot::reboot_to_bootloader()
    }

    /// Record a code without rebooting: a breadcrumb, not a verdict.
    pub fn mark(code: u8) {
        embassy_nrf::pac::POWER
            .gpregret2()
            .write(|w| w.0 = code as u32);
    }

    /// Read and clear whatever the last run recorded.
    pub fn take() -> u8 {
        let power = embassy_nrf::pac::POWER;
        let code = power.gpregret2().read().0 as u8;
        power.gpregret2().write(|w| w.0 = NONE as u32);
        code
    }

    /// Log it, in the words that say what to do about it.
    pub fn report(code: u8) {
        match code & 0xC0 {
            NONE if code == BRINGUP_CRYSTAL => defmt::error!(
                "fault: the previous run hung in the controller bring-up, on the crystal"
            ),
            NONE if code == BRINGUP_RC => defmt::error!(
                "fault: the previous run hung in the controller bring-up, on the RC oscillator"
            ),
            NONE => defmt::info!("fault: none recorded on the previous run"),
            HARD_FAULT => defmt::error!("fault: the previous run took a HARD FAULT"),
            UNHANDLED => defmt::error!(
                "fault: the previous run took interrupt {=u8} with no handler bound",
                code & 0x3F
            ),
            _ => defmt::warn!("fault: unrecognised code {=u8:#04x}", code),
        }
    }
}

/// Where the controller's initialiser stops, when it stops.
///
/// The one fact a hang does not give up from outside is *where* `mpsl_init`
/// spins. Everything else about the hang — that it is not a fault, not an
/// assertion, not the executor, not the clock — was established by exclusion.
/// This module gets the address.
///
/// # How
///
/// `TIMER1` is armed before the call, at a priority above everything of ours
/// and below the link layer's, and disarmed after it. If the call returns in
/// time nothing happens. If it does not, the timer's handler runs in the
/// middle of whatever `mpsl_init` is doing, reads the program counter that
/// was interrupted out of the exception frame, writes it somewhere a reset
/// does not clear, and resets into the application — which reads it back and
/// says so.
///
/// The handler is written in assembly rather than Rust because the exception
/// frame sits at the stack pointer *at entry*, and a compiled function's
/// prologue moves the stack pointer before any Rust code can read it. Two
/// instructions are enough: which stack was in use (bit 2 of `EXC_RETURN`),
/// then a branch into Rust with the frame's address as the first argument.
///
/// # What it costs to be wrong
///
/// If the spin holds interrupts disabled, the timer never fires and the board
/// hangs exactly as before, telling you that much. If MPSL later claims
/// `TIMER1` for something, this has to move. It does not: the controller takes
/// `TIMER0`, and `embassy-time` here is on `RTC1`.
pub mod stall {
    use core::mem::MaybeUninit;

    use embassy_nrf::interrupt::{self, InterruptExt, Priority};
    use embassy_nrf::pac;

    /// Kept in RAM the linker does not zero and reset does not clear.
    ///
    /// The MBR and the dormant SoftDevice use the bottom of RAM when they run
    /// at all, and a reset that goes straight to the application gives them
    /// no reason to. This sits above 20 KB of `.data` and `.bss`.
    #[link_section = ".uninit.OXINODE_STALL"]
    static mut RECORD: MaybeUninit<Record> = MaybeUninit::uninit();

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Record {
        magic: u32,
        pc: u32,
        lr: u32,
        xpsr: u32,
        /// Times the `CLOCK_POWER` handler ran between arming and capture.
        clock_irqs: u32,
        /// The shared `POWER`/`CLOCK` interrupt enable word, read back
        /// through `INTENSET`.
        inten: u32,
        /// `EVENTS_*` at `0x100 + 4n`, for `n` in `0..10`: HFCLKSTARTED,
        /// LFCLKSTARTED, POFWARN, DONE, CTTO, SLEEPENTER, SLEEPEXIT,
        /// USBDETECTED, USBREMOVED, USBPWRRDY.
        events: [u32; 10],
        /// NVIC priority of `CLOCK_POWER` at the moment of capture.
        clock_prio: u32,
    }

    /// Bumped by the `CLOCK_POWER` handler in the bring-up image, so the
    /// record can say how many times it ran. A stalled board whose clock
    /// handler ran ten thousand times in two seconds is not stalled; it is
    /// being interrupted to death.
    pub static CLOCK_IRQS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

    /// `POWER` and `CLOCK` share one base address and one interrupt enable
    /// register, with disjoint bits. Read raw, because the question is
    /// "what is in the register", not "what does the driver think".
    const POWER_CLOCK: usize = 0x4000_0000;
    fn reg(offset: usize) -> u32 {
        // SAFETY: a read of a memory-mapped register inside the POWER/CLOCK
        // block, which exists on every nRF52840.
        unsafe { core::ptr::read_volatile((POWER_CLOCK + offset) as *const u32) }
    }

    /// Anything else in uninitialised RAM is noise, and noise does not spell
    /// this.
    const MAGIC: u32 = 0x5354_414C; // "STAL"

    /// What the previous run recorded, if anything.
    #[derive(Clone, Copy, defmt::Format)]
    pub struct Stall {
        /// The address that was executing when the timer fired.
        pub pc: u32,
        /// The return address at that moment, which names the caller when
        /// `pc` is inside a helper.
        pub lr: u32,
        /// The program status, whose low nine bits say which exception was
        /// active — zero means thread mode, which is where a spin is expected
        /// to be.
        pub xpsr: u32,
        /// See [`CLOCK_IRQS`].
        pub clock_irqs: u32,
        /// See [`Record::inten`].
        pub inten: u32,
        /// See [`Record::events`].
        pub events: [u32; 10],
        /// See [`Record::clock_prio`].
        pub clock_prio: u32,
    }

    /// Arm the timer: `millis` from now, the address is taken.
    ///
    /// 1 MHz from the 16 MHz timer clock, 32-bit, so the count is in
    /// microseconds and cannot wrap for over an hour.
    pub fn arm(millis: u32) {
        let t = pac::TIMER1;
        t.tasks_stop().write_value(1);
        t.tasks_clear().write_value(1);
        t.mode()
            .write(|w| w.set_mode(pac::timer::vals::Mode::Timer));
        t.bitmode()
            .write(|w| w.set_bitmode(pac::timer::vals::Bitmode::_32bit));
        t.prescaler().write(|w| w.set_prescaler(4));
        t.cc(0).write_value(millis.saturating_mul(1_000));
        t.events_compare(0).write_value(0);
        t.intenset().write(|w| w.set_compare(0, true));
        // Above every application interrupt, so a USB transfer in progress
        // cannot delay it; below the link layer's own, so it cannot corrupt
        // the thing it is observing.
        interrupt::TIMER1.set_priority(Priority::P1);
        interrupt::TIMER1.unpend();
        unsafe { interrupt::TIMER1.enable() };
        CLOCK_IRQS.store(0, core::sync::atomic::Ordering::Relaxed);
        t.tasks_start().write_value(1);
    }

    /// The call returned: stand down.
    pub fn disarm() {
        let t = pac::TIMER1;
        t.tasks_stop().write_value(1);
        t.intenclr().write(|w| w.set_compare(0, true));
        interrupt::TIMER1.disable();
        t.events_compare(0).write_value(0);
    }

    /// Read and clear whatever the previous run left.
    pub fn take() -> Option<Stall> {
        // SAFETY: single-threaded, at boot, before anything else touches it.
        let record = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RECORD)) };
        let record = unsafe { record.assume_init() };
        unsafe {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(RECORD),
                MaybeUninit::new(Record { magic: 0, ..record }),
            );
        }
        (record.magic == MAGIC).then_some(Stall {
            pc: record.pc,
            lr: record.lr,
            xpsr: record.xpsr,
            clock_irqs: record.clock_irqs,
            inten: record.inten,
            events: record.events,
            clock_prio: record.clock_prio,
        })
    }

    /// Say what was found, in a form that can be looked up in a disassembly.
    pub fn report(stall: Option<Stall>) {
        match stall {
            None => defmt::info!("stall: nothing recorded by the previous run"),
            Some(s) => {
                defmt::error!(
                    "stall: the previous bring-up was still at pc={=u32:#010x} lr={=u32:#010x} xpsr={=u32:#010x} when the timer fired",
                    s.pc,
                    s.lr,
                    s.xpsr
                );
                defmt::error!(
                    "stall: CLOCK_POWER ran {=u32} times, prio {=u32}, inten={=u32:#010x}",
                    s.clock_irqs,
                    s.clock_prio,
                    s.inten
                );
                defmt::error!(
                    "stall: events hfclk={=u32} lfclk={=u32} pofwarn={=u32} done={=u32} ctto={=u32} sleepin={=u32} sleepout={=u32} usbdet={=u32} usbrem={=u32} usbrdy={=u32}",
                    s.events[0], s.events[1], s.events[2], s.events[3], s.events[4],
                    s.events[5], s.events[6], s.events[7], s.events[8], s.events[9]
                );
            }
        }
    }

    /// The Rust half of the handler. `frame` is the exception frame:
    /// `r0 r1 r2 r3 r12 lr pc xpsr`, in that order.
    #[no_mangle]
    extern "C" fn oxinode_stall_capture(frame: *const u32) -> ! {
        // SAFETY: the frame was pushed by the hardware on exception entry and
        // is eight words long; only the last three are read.
        let (lr, pc, xpsr) = unsafe {
            (
                core::ptr::read_volatile(frame.add(5)),
                core::ptr::read_volatile(frame.add(6)),
                core::ptr::read_volatile(frame.add(7)),
            )
        };
        let mut events = [0u32; 10];
        for (n, slot) in events.iter_mut().enumerate() {
            *slot = reg(0x100 + 4 * n);
        }
        let record = Record {
            magic: MAGIC,
            pc,
            lr,
            xpsr,
            clock_irqs: CLOCK_IRQS.load(core::sync::atomic::Ordering::Relaxed),
            // INTENSET reads back the enabled mask; this block has no INTEN.
            inten: reg(0x304),
            events,
            clock_prio: cortex_m::peripheral::NVIC::get_priority(interrupt::CLOCK_POWER) as u32,
        };
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(RECORD), MaybeUninit::new(record));
        }
        // Into the application, not the bootloader: RAM survives this and the
        // bootloader is what would overwrite it.
        crate::boot::reboot()
    }

    // The vector-table entry for TIMER1. `device.x` provides it as a weak
    // alias of `DefaultHandler`; a strong definition here replaces it.
    core::arch::global_asm!(
        ".section .text.oxinode_stall_timer1, \"ax\", %progbits",
        ".global TIMER1",
        ".type TIMER1, %function",
        ".thumb_func",
        "TIMER1:",
        // Bit 2 of EXC_RETURN says which stack the frame was pushed on.
        "    tst lr, #4",
        "    ite eq",
        "    mrseq r0, msp",
        "    mrsne r0, psp",
        "    b oxinode_stall_capture",
    );
}
