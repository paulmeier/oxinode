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
//!   since phase 1, and we keep linking above it. See the README.
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
//! execute, and MPSL cannot keep a connection alive through that. Phase 6
//! writes the device record with `NVMC` directly. Until that moves onto
//! `nrf_mpsl::Flash` — which schedules the write inside a timeslot — a
//! provisioning run while a phone is connected will drop the connection.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_nrf::config::Config;
use embassy_nrf::interrupt::{self, InterruptExt, Priority};
use embassy_nrf::mode::Blocking;
use embassy_nrf::rng::Rng;
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_time::{Duration, Timer};
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

/// Make a pending interrupt wake `WFE`, whether or not it is enabled.
///
/// # Why the controller needs this and embassy does not provide it
///
/// MPSL waits for things by executing `WFE` in a loop. Disassembling its wait
/// helper shows a **plain** `WFE` on this part: no `SEV` before it and no
/// `SEVONPEND` management around it. That is only correct in an environment
/// where `SEVONPEND` is already set, and in Nordic's own — Zephyr — it is.
///
/// `embassy-executor` does not set it. Its own `WFE` is woken by the `SEV`
/// its pender issues, so it never needed to.
///
/// The difference decides whether the Bluetooth controller starts. Without
/// `SEVONPEND`, a `WFE` only wakes for an interrupt the NVIC will actually
/// take — and MPSL disables its own during initialisation, so the event it is
/// waiting for pends and does not wake it. What does wake it is any *other*
/// enabled interrupt that happens to fire: USB traffic, mostly. So the
/// bring-up succeeds when the host is busy and sleeps forever when it is
/// quiet, which is exactly the behaviour observed — the same configuration
/// coming up one time and hanging the next.
///
/// Call once, before `mpsl_init`, and then leave it alone: the MPSL header
/// warns that changing `SEVONPEND` *during* initialisation can deadlock.
///
/// The cost to everything else is that `embassy-executor` wakes from `WFE`
/// slightly more often than it needs to and finds nothing to do. That is a
/// little current, against a Bluetooth stack that starts.
pub fn set_sevonpend() {
    const SEVONPEND: u32 = 1 << 4;
    // SAFETY: `SCR` is a plain read/write system register, and this is a
    // single bit set with interrupts in whatever state the caller had.
    unsafe {
        let scb = &*cortex_m::peripheral::SCB::PTR;
        scb.scr.modify(|scr| scr | SEVONPEND);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

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
/// The Base Duo has a real 32.768 kHz crystal and phase 1 proved it runs, so
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
        "lfclk: running={=bool} src={=u8} started_event={=u32}",
        stat.state(),
        stat.src() as u8,
        clock.events_lfclkstarted().read(),
    );
}

/// USB VBUS detection for an image that cannot have the `CLOCK_POWER`
/// interrupt.
///
/// # The conflict
///
/// On the nRF52840 the `POWER` and `CLOCK` peripherals share one interrupt.
/// `embassy-usb` normally binds it through `HardwareVbusDetect`, which is how
/// every image before this one learns that a cable was plugged in. MPSL needs
/// the same interrupt for the low-frequency clock, and only one handler can be
/// bound to a vector.
///
/// # Why this polls
///
/// The obvious repair is `SoftwareVbusDetect` fed from MPSL's power events,
/// and MPSL does not have any: its clock handler services `CLOCK`, and the
/// `USBDETECTED`, `USBREMOVED` and `USBPWRRDY` events belong to `POWER`.
/// Enabling those in `INTENSET` would deliver them to *MPSL's* handler, which
/// would not clear them — an interrupt that re-fires forever, at priority
/// zero, on a device that then does nothing else at all.
///
/// So nothing is enabled and `USBREGSTATUS` is read instead. It is a status
/// register with the same two bits the events announce, it belongs to `POWER`
/// rather than to the clock MPSL was told to manage, and reading it costs one
/// load. The price is latency: a cable plugged in is noticed up to
/// [`Self::INTERVAL`] late, once, before enumeration that takes a hundred
/// times longer.
pub struct Vbus {
    detect: &'static SoftwareVbusDetect,
    /// Mirrors what was last reported, because `SoftwareVbusDetect` is
    /// write-only from here: it has no getter, and reporting the same state
    /// repeatedly would wake the USB stack forever.
    reported_detected: AtomicBool,
    reported_ready: AtomicBool,
}

impl Vbus {
    /// How often `USBREGSTATUS` is read.
    ///
    /// 20 ms is the same interval the log pump and the bootloader-touch check
    /// already poll at, and is far below the millisecond budget of anything it
    /// delays.
    pub const INTERVAL: Duration = Duration::from_millis(20);

    /// Read the current state and hand back something `embassy-usb` can use.
    ///
    /// Seeded from the register rather than from `false`, so a board that is
    /// already plugged in — which is every board being flashed — enumerates
    /// without waiting for the first poll.
    ///
    /// Call this once per image; it hands out a `StaticCell`.
    pub fn take() -> Self {
        static VBUS: StaticCell<SoftwareVbusDetect> = StaticCell::new();
        let (detected, ready) = Self::read();
        Self {
            detect: VBUS.init(SoftwareVbusDetect::new(detected, ready)),
            reported_detected: AtomicBool::new(detected),
            reported_ready: AtomicBool::new(ready),
        }
    }

    /// The handle to give to `embassy_nrf::usb::Driver::new`.
    pub fn detector(&self) -> &'static SoftwareVbusDetect {
        self.detect
    }

    /// Watch the register forever. Join this with the rest of the image.
    pub async fn run(&self) -> ! {
        loop {
            Timer::after(Self::INTERVAL).await;
            let (detected, ready) = Self::read();

            if detected != self.reported_detected.swap(detected, Ordering::Relaxed) {
                // `detected` also clears the ready flag inside
                // `SoftwareVbusDetect`, which is why the mirror below is
                // cleared too rather than left claiming the rail is up.
                self.detect.detected(detected);
                self.reported_ready.store(false, Ordering::Relaxed);
                defmt::info!("vbus: {=bool}", detected);
            }
            if ready && !self.reported_ready.swap(true, Ordering::Relaxed) {
                self.detect.ready();
            }
        }
    }

    /// Whether the VBUS comparator currently sees a cable.
    ///
    /// Exposed so an image can say so without USB — see the blink rate in
    /// `src/bin/ble.rs`. If this ever reads `false` on a board that is being
    /// powered through its USB socket, then it is this that is wrong and not
    /// anything above it.
    pub fn present() -> bool {
        Self::read().0
    }

    /// `(VBUS present, regulator output ready)`.
    fn read() -> (bool, bool) {
        let status = embassy_nrf::pac::POWER.usbregstatus().read();
        (status.vbusdetect(), status.outputrdy())
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

/// How many connections the host keeps state for.
///
/// One. A phone connects to a modem; nothing in the RNode protocol has a
/// second host in mind, and phase 8 step 3 has to decide what two of them even
/// means before there is any point in paying for it.
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
