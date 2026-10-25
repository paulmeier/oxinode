//! Bring-up image for the Super IO's buzzer.
//!
//! The buzzer sits behind a load switch: P0.22 drives an NMOS that gates a
//! high-side PMOS feeding a switched 3V3 rail, so the pin powers the buzzer
//! rather than sounding it. What is on the far side of the switch is not
//! known -- the Super IO has no schematic -- and this image is how it is
//! found out, by ear:
//!
//! * P0.22 **held high**. An active buzzer, which has its own oscillator,
//!   sounds a continuous tone; a passive one clicks once as the rail comes
//!   up, and once as it goes down.
//! * P0.22 **driven with a square wave** by the PWM peripheral. A passive
//!   buzzer sounds at that frequency, if the load switch turns on and off
//!   fast enough to follow it.
//!
//! Like `qspi`, this image exposes a **single** CDC-ACM port and it is a log
//! port, so that DTR can hold the report until a terminal is attached and can
//! still take the 1200-baud touch. See `docs/architecture/images.md` for the
//! key table.
//!
//! With nobody at a terminal it plays a fixed cycle instead, with the LED off,
//! so the board can be listened to on its battery with a meter in the lead.
//!
//! Between sounds the pin is driven **low**, never left floating: nothing
//! says the switch's gate has a pull-down, and a floating gate is a rail
//! that may be half on. (Until 2026-10-25 it was not: a dropped `Output`
//! disconnects its pin, and the image dropped one after each sound.)

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals::{P0_22, PWM0};
use embassy_nrf::pwm::{DutyCycle, Prescaler, SimpleConfig, SimplePwm};
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals, Peri};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, State};
use embassy_usb::driver::Driver as UsbDriverTrait;
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::{boot, usb_log};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

/// Same prototyping VID as the other images, with its own PID.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0007;

/// How long the image waits for a terminal before it starts the cycle.
const TERMINAL_WAIT: Duration = Duration::from_secs(5);

/// The PWM's clock: 16 MHz divided by 16. Every frequency below divides it
/// into a whole number of ticks, so the one asked for is the one played.
const PWM_CLOCK_HZ: u32 = 1_000_000;

/// The sweep, in hertz: wide enough to bracket a small buzzer's resonance,
/// which for the usual 12 mm parts is somewhere between 2 and 4 kHz.
const SWEEP_HZ: [u32; 16] = [
    500, 1_000, 1_500, 2_000, 2_300, 2_500, 2_700, 3_000, 3_200, 3_500, 4_000, 4_500, 5_000, 6_000,
    7_000, 8_000,
];

/// The length sweep, in milliseconds: how long the rail has to be held up
/// before a beep is a tone rather than a tap. The switch and the buzzer's
/// own oscillator both take time to start, and 80 ms was measured as taps.
const SWEEP_MS: [u64; 6] = [50, 100, 150, 200, 300, 500];

/// The candidate alert: two beeps of this length, this far apart.
const BEEP: Duration = Duration::from_millis(80);

/// The buzzer's pin and the PWM peripheral that can drive it, lent out one
/// use at a time.
struct Buzzer<'d> {
    pwm: Peri<'d, PWM0>,
    pin: Peri<'d, P0_22>,
}

impl Buzzer<'_> {
    /// The rail off, with the pin driven low.
    fn off(&mut self) -> Output<'_> {
        Output::new(self.pin.reborrow(), Level::Low, OutputDrive::Standard)
    }

    /// The pin driven low, and left so. Dropping an `Output` disconnects
    /// its pin, which would leave the switch's gate floating until the next
    /// sound, so this one is never dropped: forgetting it frees nothing,
    /// and the next use of the pin reconfigures it.
    fn park(&mut self) {
        core::mem::forget(self.off());
    }

    /// The rail held on for `length`.
    async fn hold(&mut self, length: Duration) {
        let mut pin = self.off();
        pin.set_high();
        Timer::after(length).await;
        pin.set_low();
        core::mem::forget(pin);
    }

    /// A square wave at `hz` for `length`, then the pin low again.
    async fn square(&mut self, hz: u32, length: Duration) {
        let top = (PWM_CLOCK_HZ / hz) as u16;
        let mut config = SimpleConfig::default();
        config.max_duty = top;
        config.prescaler = Prescaler::Div16;
        config.ch0_idle_level = Level::Low;
        {
            let mut pwm = SimplePwm::new_1ch(self.pwm.reborrow(), self.pin.reborrow(), &config);
            pwm.set_duty(0, DutyCycle::normal(top / 2));
            Timer::after(length).await;
            // Dropping it leaves the pin an undriven input; `park` drives
            // it again.
        }
        self.park();
    }
}

/// Play one thing, and say what it is first.
async fn play(buzzer: &mut Buzzer<'_>, what: Sound) {
    match what {
        Sound::Held(length) => {
            defmt::info!("P0.22 held high for {=u64} ms", length.as_millis());
            buzzer.hold(length).await;
        }
        Sound::Square(hz, length) => {
            defmt::info!(
                "P0.22 square wave at {=u32} Hz for {=u64} ms",
                hz,
                length.as_millis()
            );
            buzzer.square(hz, length).await;
        }
        Sound::PatternHeld => {
            defmt::info!("alert pattern, held: two {=u64} ms beeps", BEEP.as_millis());
            buzzer.hold(BEEP).await;
            Timer::after(BEEP).await;
            buzzer.hold(BEEP).await;
        }
        Sound::PatternSquare(hz) => {
            defmt::info!(
                "alert pattern at {=u32} Hz: two {=u64} ms beeps",
                hz,
                BEEP.as_millis()
            );
            buzzer.square(hz, BEEP).await;
            Timer::after(BEEP).await;
            buzzer.square(hz, BEEP).await;
        }
    }
}

#[derive(Clone, Copy)]
enum Sound {
    Held(Duration),
    Square(u32, Duration),
    PatternHeld,
    PatternSquare(u32),
}

/// The cycle with no terminal, in this order and with these gaps, so it can
/// be told apart by ear with nothing to read: held, then 2 kHz, then 4 kHz,
/// then the two patterns, then a long held phase for a meter.
const CYCLE: [(Sound, u64); 7] = [
    (Sound::Held(Duration::from_secs(1)), 2),
    (Sound::Square(2_000, Duration::from_secs(1)), 2),
    (Sound::Square(4_000, Duration::from_secs(1)), 2),
    (Sound::PatternHeld, 2),
    (Sound::PatternSquare(4_000), 2),
    (Sound::Held(Duration::from_secs(10)), 2),
    (Sound::Square(4_000, Duration::from_secs(10)), 5),
];

/// The cycle, until a terminal opens the port or a key arrives.
async fn cycle<'d, D: UsbDriverTrait<'d>>(
    buzzer: &mut Buzzer<'_>,
    led: &mut Led<'_>,
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
) {
    led.off();
    defmt::info!("cycling: a key or a terminal stops it");
    let mut buf = [0u8; 64];
    'outer: loop {
        for (sound, gap_s) in CYCLE {
            play(buzzer, sound).await;
            let until = Instant::now() + Duration::from_secs(gap_s);
            while Instant::now() < until {
                if usb_log::is_bootloader_touch(rx, control) {
                    boot::reboot_to_bootloader();
                }
                if control.dtr() {
                    if let Some(n) =
                        usb_log::read_for(rx, &mut buf, Duration::from_millis(20)).await
                    {
                        if n > 0 {
                            break 'outer;
                        }
                    }
                } else {
                    Timer::after(Duration::from_millis(20)).await;
                }
            }
        }
    }
    led.on();
    defmt::info!("cycling stopped");
}

/// The next key from the terminal, checking for the 1200-baud touch on
/// every wake.
async fn next_key<'d, D: UsbDriverTrait<'d>>(
    rx: &mut Receiver<'d, D>,
    control: &ControlChanged<'d>,
) -> u8 {
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

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    let serial = board::take_device_serial();

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode buzzer bring-up");
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
    let (mut log_tx, mut log_rx, control) = logs.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);
    let mut buzzer = Buzzer {
        pwm: p.PWM0,
        pin: p.P0_22,
    };
    // Driven low from the start, before anything waits.
    buzzer.park();

    let run_usb = usb.run();
    let pump = usb_log::pump(&mut log_tx, || control.dtr());

    let bring_up = async {
        let waited = Instant::now();
        let mut ticks = 0u32;
        while !control.dtr() {
            if usb_log::is_bootloader_touch(&log_rx, &control) {
                boot::reboot_to_bootloader();
            }
            if waited.elapsed() >= TERMINAL_WAIT {
                cycle(&mut buzzer, &mut led, &mut log_rx, &control).await;
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
            "oxinode buzzer bring-up: serial {=str}, image at {=u32:#x}",
            serial,
            boot::APP_FLASH_ORIGIN
        );
        defmt::info!(
            "keys: h held 1 s, H held 10 s, 1-8 square at N kHz, s sweep, w lengths, p/P alert held/at 4 kHz, c cycle"
        );

        loop {
            let one = Duration::from_secs(1);
            match next_key(&mut log_rx, &control).await {
                b'h' => play(&mut buzzer, Sound::Held(one)).await,
                b'H' => play(&mut buzzer, Sound::Held(Duration::from_secs(10))).await,
                k @ b'1'..=b'8' => {
                    let hz = u32::from(k - b'0') * 1_000;
                    play(&mut buzzer, Sound::Square(hz, one)).await;
                }
                b's' => {
                    for hz in SWEEP_HZ {
                        play(&mut buzzer, Sound::Square(hz, Duration::from_millis(400))).await;
                        Timer::after(Duration::from_millis(400)).await;
                    }
                    defmt::info!("sweep done");
                }
                b'w' => {
                    for ms in SWEEP_MS {
                        play(&mut buzzer, Sound::Held(Duration::from_millis(ms))).await;
                        Timer::after(Duration::from_secs(2)).await;
                    }
                    defmt::info!("length sweep done");
                }
                b'p' => play(&mut buzzer, Sound::PatternHeld).await,
                b'P' => play(&mut buzzer, Sound::PatternSquare(4_000)).await,
                b'c' => cycle(&mut buzzer, &mut led, &mut log_rx, &control).await,
                b'R' => boot::reboot(),
                b'b' => boot::reboot_to_bootloader(),
                b'\r' | b'\n' => {}
                other => defmt::warn!("console: unknown command {=u8:#04x}", other),
            }
        }
    };

    join3(run_usb, pump, bring_up).await;
}
