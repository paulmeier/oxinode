//! The battery sense: P0.31 through the divider, and the charger's two
//! status lines.
//!
//! Three pins, and nothing else on this board says anything about power:
//!
//! | signal | pin | what it is |
//! |---|---|---|
//! | `VBAT` sense | P0.31 (`AIN7`) | the cell through 806 kΩ / 1.5 MΩ, so 0.65048 of it |
//! | `BAT_CHG_STATUS` | P1.02 | the BQ25185's `STAT2`, open drain, **low while charging**; 100 kΩ pull-up on the board, and the red LED |
//! | `BAT_CHG_STAT1` | P0.27 | the BQ25185's `STAT1`, open drain, **low on a fault**; no pull-up on the board |
//!
//! The two together are the chip's whole status table, which is why both
//! are read: on `STAT2` alone a latched-off fault looks exactly like
//! charging. Whether there is anything on the USB connector to charge from
//! comes from the chip's own regulator status, not from a pin.
//!
//! The divider ratio, the ADC's full scale, the percentage table, the
//! smoothing and the charger's table are all in [`oxinode_core::battery`],
//! where they can be tested. This is the part that has to own a peripheral:
//! it asks the ADC for one count and hands it over.
//!
//! # How the ADC is set up, and why it matters
//!
//! Gain 1/6 against the internal 0.6 V reference: the peripheral's own
//! default for a single-ended channel, and the only setting whose full scale
//! -- 3.6 V -- is above what a full cell puts on the pin. The core pins the
//! same numbers, so a change here without one there is a wrong voltage on
//! the screen rather than a compile error; the reading is cross-checked
//! against the log at bring-up for that reason.
//!
//! The peripheral averages sixteen conversions per sample -- its own
//! oversampling, in a burst, so one request is still one request -- which
//! takes about a fifth of a millisecond, plus a calibration once at start.
//! It is read once per redraw, which is twice a second, and never on the
//! radio's path. The average across redraws, and the dead band that keeps
//! the screen still, are the core's [`battery::Smoothed`].

use embassy_nrf::gpio::{Input, Pull};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{P0_27, P0_31, P1_02, SAADC};
use embassy_nrf::saadc::{
    self, ChannelConfig, Config, Gain, Oversample, Reference, Resolution, Saadc,
};
use embassy_nrf::Peri;
use oxinode_core::battery::{self, Charger, Reading, Smoothed};

use crate::board;

/// The cell sense and the charger's status lines.
pub struct Sense<'d> {
    adc: Saadc<'d, 1>,
    /// `STAT2`: low while charging.
    stat2: Input<'d>,
    /// `STAT1`: low on a fault.
    stat1: Input<'d>,
    smoothed: Smoothed,
    /// The last state reported, so a change can go to the log.
    last: Option<Charger>,
}

impl<'d> Sense<'d> {
    /// Claim the ADC on P0.31 and the status lines on P1.02 and P0.27.
    ///
    /// Both status lines get the chip's pull-up: the charger's outputs are
    /// open drain, so without one a line the charger is not pulling low
    /// floats, and its state would read as noise. `STAT2` has a pull-up on
    /// the board as well; `STAT1` has only this one.
    pub fn new(
        saadc: Peri<'d, SAADC>,
        irq: impl Binding<embassy_nrf::interrupt::typelevel::SAADC, saadc::InterruptHandler> + 'd,
        vbat: Peri<'d, P0_31>,
        stat2: Peri<'d, P1_02>,
        stat1: Peri<'d, P0_27>,
    ) -> Self {
        let mut config = Config::default();
        config.resolution = Resolution::_12bit;
        // Sixteen conversions averaged in hardware per sample. Single
        // channel, which is the only configuration the peripheral's
        // oversampling is defined for.
        config.oversample = Oversample::Over16x;
        let mut channel = ChannelConfig::single_ended(vbat);
        // Explicit, although these are the defaults, because the core's
        // arithmetic depends on exactly these two.
        channel.gain = Gain::Gain1_6;
        channel.reference = Reference::Internal;
        let adc = Saadc::new(saadc, irq, config, [channel]);
        Self {
            adc,
            stat2: Input::new(stat2, Pull::Up),
            stat1: Input::new(stat1, Pull::Up),
            smoothed: Smoothed::new(),
            last: None,
        }
    }

    /// Run the ADC's offset calibration. Once, before the first reading.
    pub async fn calibrate(&mut self) {
        self.adc.calibrate().await;
    }

    /// One reading of the cell, smoothed over the readings before it, or
    /// `None` if there is no cell to read.
    pub async fn read(&mut self) -> Option<Reading> {
        let mut buf = [0i16; 1];
        self.adc.sample(&mut buf).await;
        self.smoothed.update(battery::reading(buf[0]))
    }

    /// The raw count, for the bring-up log.
    pub async fn raw(&mut self) -> i16 {
        let mut buf = [0i16; 1];
        self.adc.sample(&mut buf).await;
        buf[0]
    }

    /// What the charger says it is doing. A change goes to the log with
    /// the raw lines, so a state the screen names can be checked against
    /// the cable and the red LED.
    pub fn charger(&mut self) -> Charger {
        let (stat1_low, stat2_low) = (self.stat1.is_low(), self.stat2.is_low());
        let input = board::usb_power_present();
        let state = Charger::decode(stat1_low, stat2_low, input);
        if self.last != Some(state) {
            defmt::info!(
                "charger: {=str} (STAT1 low={=bool}, STAT2 low={=bool}, USB power={=bool})",
                state.word(),
                stat1_low,
                stat2_low,
                input
            );
            self.last = Some(state);
        }
        state
    }
}
