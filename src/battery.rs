//! The battery sense: P0.31 through the divider, and the charger's status line.
//!
//! Two pins, and nothing else on this board says anything about power:
//!
//! | signal | pin | what it is |
//! |---|---|---|
//! | `VBAT` sense | P0.31 (`AIN7`) | the cell through 806 kΩ / 1.5 MΩ, so 0.65048 of it |
//! | charger status | P1.02 | the BQ25185's `STAT` output, **low while charging** |
//!
//! The divider ratio, the ADC's full scale and the percentage table are all
//! in [`oxinode_core::battery`], where they can be tested. This is the part
//! that has to own a peripheral: it asks the ADC for one 12-bit count and
//! hands it over.
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
//! A conversion takes about ten microseconds plus a calibration once at
//! start. It is read once per redraw, which is twice a second, and never on
//! the radio's path.

use embassy_nrf::gpio::{Input, Pull};
use embassy_nrf::interrupt::typelevel::Binding;
use embassy_nrf::peripherals::{P0_31, P1_02, SAADC};
use embassy_nrf::saadc::{self, ChannelConfig, Config, Gain, Reference, Resolution, Saadc};
use embassy_nrf::Peri;
use oxinode_core::battery::{self, Reading};

/// The cell sense and the charger's status line.
pub struct Sense<'d> {
    adc: Saadc<'d, 1>,
    charging: Input<'d>,
}

impl<'d> Sense<'d> {
    /// Claim the ADC on P0.31 and the status line on P1.02.
    ///
    /// The status line gets the chip's pull-up: the charger's `STAT` is an
    /// open-drain output, so without one it floats when the charger is not
    /// pulling it low, and "not charging" would read as noise.
    pub fn new(
        saadc: Peri<'d, SAADC>,
        irq: impl Binding<embassy_nrf::interrupt::typelevel::SAADC, saadc::InterruptHandler> + 'd,
        vbat: Peri<'d, P0_31>,
        stat: Peri<'d, P1_02>,
    ) -> Self {
        let mut config = Config::default();
        config.resolution = Resolution::_12bit;
        let mut channel = ChannelConfig::single_ended(vbat);
        // Explicit, although these are the defaults, because the core's
        // arithmetic depends on exactly these two.
        channel.gain = Gain::Gain1_6;
        channel.reference = Reference::Internal;
        let adc = Saadc::new(saadc, irq, config, [channel]);
        Self {
            adc,
            charging: Input::new(stat, Pull::Up),
        }
    }

    /// Run the ADC's offset calibration. Once, before the first reading.
    pub async fn calibrate(&mut self) {
        self.adc.calibrate().await;
    }

    /// One reading of the cell, or `None` if there is no cell to read.
    pub async fn read(&mut self) -> Option<Reading> {
        let mut buf = [0i16; 1];
        self.adc.sample(&mut buf).await;
        battery::reading(buf[0])
    }

    /// The raw count, for the bring-up log.
    pub async fn raw(&mut self) -> i16 {
        let mut buf = [0i16; 1];
        self.adc.sample(&mut buf).await;
        buf[0]
    }

    /// Whether the charger says it is charging.
    pub fn is_charging(&self) -> bool {
        self.charging.is_low()
    }
}
