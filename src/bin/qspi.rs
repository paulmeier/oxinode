//! Bring-up image for the W25Q128 QSPI flash.
//!
//! It reads the chip's ID before waking it (which says whether the last boot
//! left it asleep) and after, reads its status registers, and looks at the
//! test sector: erased, holding a pattern an earlier boot wrote (and how
//! much of it survived), or holding something else. Then it takes
//! single-character commands. See `docs/architecture/images.md` for the key
//! table and `oxinode_core::w25q` for the pattern.
//!
//! With nobody at a terminal it measures instead: the chip in standby and in
//! deep power-down for ten seconds each, over and over, with the LED off, so
//! a meter in the battery lead shows the difference as a square wave.
//!
//! Like `radio`, this image exposes a **single** CDC-ACM port and it is a log
//! port, so that DTR can hold the report until a terminal is attached and can
//! still take the 1200-baud touch. See that image for the full reasoning.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, qspi, Peri};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::ext_flash::{self, Bitbang};
use oxinode::{boot, usb_log};
use oxinode_core::w25q::{self, Found, Identity, Status, Verdict};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    QSPI => qspi::InterruptHandler<peripherals::QSPI>;
});

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0006;

/// How long the image waits for a terminal before it starts measuring.
const TERMINAL_WAIT: Duration = Duration::from_secs(5);
/// Each half of the measuring cycle.
const PHASE: Duration = Duration::from_secs(10);

/// The test sector, in RAM: EasyDMA reads and writes it, four bytes at a time.
#[repr(C, align(4))]
struct Sector([u8; w25q::SECTOR as usize]);

/// The chip's six pins and the peripheral, lent out one use at a time:
/// either to the GPIOs or to the QSPI peripheral, never both.
struct Flash<'d> {
    qspi: Peri<'d, peripherals::QSPI>,
    sck: Peri<'d, peripherals::P0_03>,
    cs: Peri<'d, peripherals::P0_26>,
    io0: Peri<'d, peripherals::P0_30>,
    io1: Peri<'d, peripherals::P0_29>,
    io2: Peri<'d, peripherals::P0_28>,
    io3: Peri<'d, peripherals::P0_02>,
}

impl Flash<'_> {
    fn bitbang(&mut self) -> Bitbang<'_> {
        Bitbang::new(
            self.sck.reborrow(),
            self.cs.reborrow(),
            self.io0.reborrow(),
            self.io1.reborrow(),
            self.io2.reborrow(),
            self.io3.reborrow(),
        )
    }

    /// The peripheral. Only for a chip that is awake: see `ext_flash`.
    fn qspi(&mut self, quad: bool) -> qspi::Qspi<'_> {
        qspi::Qspi::new(
            self.qspi.reborrow(),
            Irqs,
            self.sck.reborrow(),
            self.cs.reborrow(),
            self.io0.reborrow(),
            self.io1.reborrow(),
            self.io2.reborrow(),
            self.io3.reborrow(),
            ext_flash::config(quad),
        )
    }
}

fn log_id(when: &str, id: Identity) {
    let verdict = match id.verdict() {
        Verdict::W25q128 => "a W25Q128JV, 16 MB",
        Verdict::Other => "not the part the schematic calls for",
        Verdict::Silent => "nothing driving the line: asleep, unpowered or not there",
    };
    defmt::info!(
        "id {=str}: {=u8:02x} {=u8:02x} {=u8:02x}, {=str}",
        when,
        id.manufacturer,
        id.memory_type,
        id.capacity,
        verdict
    );
}

/// Both status registers, on the GPIOs. Not through the peripheral, which
/// sends a write enable ahead of every custom instruction and would always
/// show the latch set.
fn status(flash: &mut Flash<'_>) -> Status {
    let mut bus = flash.bitbang();
    let mut sr1 = [0u8];
    let mut sr2 = [0u8];
    bus.read(w25q::READ_STATUS_1, &mut sr1);
    bus.read(w25q::READ_STATUS_2, &mut sr2);
    Status {
        sr1: sr1[0],
        sr2: sr2[0],
    }
}

fn log_status(s: Status) {
    defmt::info!(
        "status: SR1 {=u8:#04x} SR2 {=u8:#04x}: busy {=bool}, write enable {=bool}, quad enable {=bool}",
        s.sr1,
        s.sr2,
        s.busy(),
        s.write_enabled(),
        s.quad_enabled()
    );
}

/// Read the test sector and say what is there.
async fn look(flash: &mut Flash<'_>, buf: &mut Sector, quad: bool) {
    let started = Instant::now();
    let read = flash.qspi(quad).read(w25q::TEST_SECTOR, &mut buf.0).await;
    let us = started.elapsed().as_micros() as u32;
    if let Err(e) = read {
        defmt::error!("read: {}", defmt::Debug2Format(&e));
        return;
    }
    let lines = if quad { "four lines" } else { "one line" };
    match w25q::check(&buf.0) {
        Found::Erased => defmt::info!(
            "sector {=u32:#x}: erased (all 0xff); read on {=str} in {=u32} us",
            w25q::TEST_SECTOR,
            lines,
            us
        ),
        Found::Pattern {
            seed,
            wrong,
            first_wrong,
        } => {
            if wrong == 0 {
                defmt::info!(
                    "sector {=u32:#x}: pattern {=u32:#010x}, every byte right; read on {=str} in {=u32} us",
                    w25q::TEST_SECTOR,
                    seed,
                    lines,
                    us
                );
            } else {
                defmt::error!(
                    "sector {=u32:#x}: pattern {=u32:#010x}, {=usize} of {=usize} bytes wrong, the first at {=usize}; read on {=str}",
                    w25q::TEST_SECTOR,
                    seed,
                    wrong,
                    buf.0.len() - w25q::HEADER,
                    first_wrong.unwrap_or(0),
                    lines
                );
            }
        }
        Found::Foreign => defmt::warn!(
            "sector {=u32:#x}: neither erased nor a pattern this wrote; starts {=[u8]:02x}",
            w25q::TEST_SECTOR,
            buf.0[..16]
        ),
    }
}

/// Wait for an erase or a program to finish, and clear the write enable
/// latch behind it.
///
/// The peripheral's READY comes when an erase or a write has been *started*,
/// not finished; the next operation is what waits. A custom instruction
/// waits for the busy bit to clear before it is sent, so a write disable is
/// the wait, and leaves the latch as a status read expects to find it.
async fn settle(q: &mut qspi::Qspi<'_>) {
    if let Err(e) = q
        .custom_instruction(w25q::WRITE_DISABLE, &[], &mut [])
        .await
    {
        defmt::error!("write disable: {}", defmt::Debug2Format(&e));
    }
}

/// Erase the test sector, waiting until the chip has finished. Microseconds.
async fn erase_sector(q: &mut qspi::Qspi<'_>) -> Option<u32> {
    let started = Instant::now();
    if let Err(e) = q.erase(w25q::TEST_SECTOR).await {
        defmt::error!("erase: {}", defmt::Debug2Format(&e));
        return None;
    }
    settle(q).await;
    Some(started.elapsed().as_micros() as u32)
}

/// Erase the test sector, write a new pattern, and read it back.
async fn write(flash: &mut Flash<'_>, buf: &mut Sector) {
    let seed = Instant::now().as_ticks() as u32 ^ board::device_id() as u32;
    {
        let mut q = flash.qspi(false);
        let Some(erase_us) = erase_sector(&mut q).await else {
            return;
        };
        w25q::fill(&mut buf.0, seed);
        let started = Instant::now();
        if let Err(e) = q.write(w25q::TEST_SECTOR, &buf.0).await {
            defmt::error!("write: {}", defmt::Debug2Format(&e));
            return;
        }
        settle(&mut q).await;
        defmt::info!(
            "sector {=u32:#x}: erased in {=u32} us, pattern {=u32:#010x} written in {=u32} us",
            w25q::TEST_SECTOR,
            erase_us,
            seed,
            started.elapsed().as_micros() as u32
        );
    }
    // Scribble on the buffer, so what is checked is what came back.
    buf.0.fill(0);
    look(flash, buf, false).await;
}

async fn erase(flash: &mut Flash<'_>) {
    if let Some(us) = erase_sector(&mut flash.qspi(false)).await {
        defmt::info!(
            "sector {=u32:#x}: erased in {=u32} us",
            w25q::TEST_SECTOR,
            us
        );
    }
}

/// The next key from the terminal, checking for the 1200-baud touch on
/// every wake, with the chip left as `asleep` says and its pins driven.
async fn next_key<'d, D: UsbDriverTrait<'d>>(
    flash: &mut Flash<'_>,
    asleep: bool,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
) -> u8 {
    let mut bus = flash.bitbang();
    if asleep {
        bus.sleep();
    }
    let mut buf = [0u8; 64];
    loop {
        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }
        match usb_log::read_for(rx, &mut buf, Duration::from_millis(20)).await {
            Some(n) if n > 0 => return buf[0],
            _ => {}
        }
    }
}

/// Standby and deep power-down, a [`PHASE`] each, until a terminal opens
/// the port or a key arrives. The LED is off throughout: it draws more than
/// the difference being measured.
async fn measure<'d, D: UsbDriverTrait<'d>>(
    flash: &mut Flash<'_>,
    led: &mut Led<'_>,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
) {
    led.off();
    let mut bus = flash.bitbang();
    bus.wake();
    let mut asleep = false;
    let mut since = Instant::now();
    defmt::info!(
        "measuring: standby and deep power-down, {=u64} s each; a key or a terminal stops it",
        PHASE.as_secs()
    );
    let mut buf = [0u8; 64];
    loop {
        if usb_log::is_bootloader_touch(rx, control) {
            boot::reboot_to_bootloader();
        }
        if since.elapsed() >= PHASE {
            since = Instant::now();
            asleep = !asleep;
            if asleep {
                bus.sleep();
            } else {
                bus.wake();
            }
            defmt::info!(
                "measuring: {=str}",
                if asleep { "deep power-down" } else { "standby" }
            );
        }
        if control.dtr() {
            if let Some(n) = usb_log::read_for(rx, &mut buf, Duration::from_millis(20)).await {
                if n > 0 {
                    break;
                }
            }
        } else {
            Timer::after(Duration::from_millis(20)).await;
        }
    }
    bus.wake();
    led.on();
    defmt::info!("measuring stopped; the chip is awake");
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode qspi bring-up");
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
    static SECTOR: StaticCell<Sector> = StaticCell::new();

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
    let buf = SECTOR.init(Sector([0; w25q::SECTOR as usize]));
    let mut flash = Flash {
        qspi: p.QSPI,
        sck: p.P0_03,
        cs: p.P0_26,
        io0: p.P0_30,
        io1: p.P0_29,
        io2: p.P0_28,
        io3: p.P0_02,
    };

    let run_usb = usb.run();
    let pump = usb_log::pump(&mut log_tx, || control.dtr());

    let bring_up = async {
        // Before anything else talks to the chip: whether the last boot left
        // it asleep. A reset of the nRF52 does not wake it.
        let before = flash.bitbang().jedec();

        // A terminal, or after a while the measuring cycle, which is what a
        // board on its battery with a meter in the lead wants.
        let waited = Instant::now();
        let mut ticks = 0u32;
        while !control.dtr() {
            if usb_log::is_bootloader_touch(&log_rx, &control) {
                boot::reboot_to_bootloader();
            }
            if waited.elapsed() >= TERMINAL_WAIT {
                measure(&mut flash, &mut led, &mut log_rx, &control).await;
                break;
            }
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
            "oxinode qspi bring-up: serial {=str}, image at {=u32:#x}",
            serial,
            boot::APP_FLASH_ORIGIN
        );
        log_id("at boot, before waking", before);
        let mut bus = flash.bitbang();
        bus.wake();
        let awake = bus.jedec();
        drop(bus);
        log_id("awake", awake);
        let s = status(&mut flash);
        log_status(s);
        if awake.verdict() == Verdict::W25q128 {
            look(&mut flash, buf, false).await;
        }

        let mut asleep = false;
        loop {
            let key = next_key(&mut flash, asleep, &mut log_rx, &control).await;
            match key {
                b'i' => {
                    let mut bus = flash.bitbang();
                    let now = bus.jedec();
                    bus.wake();
                    let woken = bus.jedec();
                    drop(bus);
                    log_id("as it was", now);
                    log_id("after waking", woken);
                    log_status(status(&mut flash));
                    asleep = false;
                }
                b'r' | b'w' | b'x' | b'q' if asleep => {
                    defmt::warn!("the chip is asleep; `a` wakes it");
                }
                b'r' => look(&mut flash, buf, false).await,
                b'w' => write(&mut flash, buf).await,
                b'x' => erase(&mut flash).await,
                b'q' => {
                    if status(&mut flash).quad_enabled() {
                        look(&mut flash, buf, true).await;
                    } else {
                        defmt::warn!("quad enable is clear; four-line reads would not work");
                    }
                }
                b's' => {
                    let mut bus = flash.bitbang();
                    bus.sleep();
                    log_id("after B9h", bus.jedec());
                    asleep = true;
                }
                b'a' => {
                    let mut bus = flash.bitbang();
                    bus.wake();
                    log_id("after ABh", bus.jedec());
                    asleep = false;
                }
                b'm' => {
                    measure(&mut flash, &mut led, &mut log_rx, &control).await;
                    asleep = false;
                }
                b'R' => boot::reboot(),
                b'b' => boot::reboot_to_bootloader(),
                b'\r' | b'\n' => {}
                other => defmt::warn!("console: unknown command {=u8:#04x}", other),
            }
        }
    };

    join3(run_usb, pump, bring_up).await;
}
