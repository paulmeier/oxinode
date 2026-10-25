//! The Super IO's buzzer, sounding the alert for a message.
//!
//! When to sound is `oxinode_core::alert`'s, tested on the host; what this
//! does is play [`PATTERN`] on P0.22 when told to. P0.22 is not the buzzer's
//! signal: it drives an NMOS that gates a high-side PMOS feeding a switched
//! 3V3 rail, and the buzzer is on that rail. It is a passive buzzer, heard
//! with the `buzzer` image: the rail held up only clicks it, and a square
//! wave on P0.22 through the switch sounds it, at 1 to 3 kHz but not at 4,
//! which the switch does not follow. So each beep is the PWM peripheral at
//! [`TONE_HZ`] for the beep's length. See `docs/hardware/board.md`.
//!
//! The pin is driven low from boot, in every build, and between beeps. Left
//! as the reset leaves it, an undriven input, the switch's gate would float,
//! and nothing says the Super IO pulls it down.
//!
//! The pattern plays in a future of its own rather than in the modem loop,
//! so the loop never waits a quarter of a second on it with the radio
//! unserviced. A second request while one plays is the same alert: the
//! signal holds one, not a count.

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals::{P0_22, PWM0};
use embassy_nrf::pwm::{DutyCycle, Prescaler, SimpleConfig, SimplePwm};
use embassy_nrf::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use oxinode_core::alert::{PATTERN, TONE_HZ};

static SOUND: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The PWM's clock: 16 MHz divided by 16.
const PWM_CLOCK_HZ: u32 = 1_000_000;
/// One period of the tone, in PWM clock ticks.
const TOP: u16 = (PWM_CLOCK_HZ / TONE_HZ) as u16;
const _: () = assert!(PWM_CLOCK_HZ % TONE_HZ == 0 && TOP < 0x8000);

/// Sound the pattern once, as soon as the buzzer is free.
pub fn sound() {
    SOUND.signal(());
}

/// The pin low, then the pattern each time [`sound`] asks for it. Never
/// returns.
///
/// The pin is an `Output` held low at every moment but a beep. That is the
/// only way it stays driven: dropping an `Output`, or the PWM driver,
/// disconnects the pin and leaves the switch's gate floating.
pub async fn serve(mut pwm: Peri<'_, PWM0>, mut pin: Peri<'_, P0_22>) -> ! {
    let mut config = SimpleConfig::default();
    config.max_duty = TOP;
    config.prescaler = Prescaler::Div16;
    loop {
        {
            let _low = low(pin.reborrow());
            SOUND.wait().await;
        }
        for (i, &ms) in PATTERN.iter().enumerate() {
            let length = u64::from(ms);
            if i % 2 == 0 {
                let mut tone = SimplePwm::new_1ch(pwm.reborrow(), pin.reborrow(), &config);
                tone.set_duty(0, DutyCycle::normal(TOP / 2));
                Timer::after_millis(length).await;
            } else {
                let _low = low(pin.reborrow());
                Timer::after_millis(length).await;
            }
        }
    }
}

fn low(pin: Peri<'_, P0_22>) -> Output<'_> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}
