//! Bringing the LR1121 from power-on to a radio that will accept a
//! configuration.
//!
//! Each step is there because bring-up on hardware established it was needed,
//! and `docs/hardware/radio.md` says which:
//!
//! 1. **Reset, and wait for BUSY.** 191 ms on this part — two hundred times
//!    what an SX126x takes, and the reason the first timeout written for it was
//!    wrong by a factor of two.
//! 2. **`SetTcxoMode`.** The 32 MHz oscillator does not start without it; the
//!    chip reports `hf_xosc_start` forever.
//! 3. **A full calibration.** The boot-time calibrations ran without a working
//!    32 MHz reference, so they have to be redone once there is one.
//! 4. **The RF switch masks.** Without them a transmission produces a clean
//!    `TxDone` into a dead antenna port, which is the failure that looks most
//!    like success.
//! 5. **Interrupt routing.** DIO9 is the only LR1121 pin that leaves the
//!    module, and nothing is on it until it is asked for.
//! 6. **The DC-DC regulator.** Measured worth 0.8 °C of die temperature under
//!    load, and it only works in standby RC.
//!
//! # Why `src/bin/radio.rs` still has its own copy
//!
//! Because the two images want different things from the same sequence. That
//! one is a diagnostic: its job is to report the evidence at every step —
//! the BUSY trace, the raw `GetVersion` bytes, whether a command was merely
//! accepted or actually did something — and to keep going when a step fails so
//! the next one can be tried anyway. This one's job is to come up or say why
//! not. Sharing the code would mean one of the two doing its job badly.

use embassy_time::{with_timeout, Duration};
use lr11xx::ops::{Calibrate, Interrupt, RfSwitchConfig, TcxoMode, TcxoTune};
use lr11xx::Lr11xx;
use oxinode_core::lr1121::{irq as irq_bits, rf_switch, tcxo, ResetVerdict};

use crate::radio::{RadioIrq, RadioReset};

/// Why the radio did not come up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum BringUpError {
    /// BUSY never behaved the way a chip coming out of reset does. Everything
    /// after this would hang, because `lr11xx` waits on BUSY without a timeout
    /// of its own.
    NotReset,
    /// A command did not answer inside its bound.
    Timeout(&'static str),
    /// A command failed.
    Radio,
    /// The 32 MHz oscillator did not start, or the die temperature says it is
    /// not running. Everything the radio does is timed by it.
    NoOscillator,
    /// The interrupt line was high with every interrupt cleared, so it cannot
    /// be trusted to mean anything.
    InterruptStuck,
}

/// Bring the radio up, or explain why not.
///
/// Takes ownership of the reset lines because BUSY has to be handed to
/// `lr11xx`, which owns it from here on.
pub async fn bring_up<'d, S>(
    spi: S,
    mut reset: RadioReset<'d>,
    irq: &mut RadioIrq<'_>,
) -> Result<Lr11xx<S, embassy_nrf::gpio::Input<'d>>, BringUpError>
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
{
    let trace = reset.cycle().await;
    let verdict = ResetVerdict::of(&trace);
    defmt::info!(
        "radio: reset -- {=str} (busy fell after {=u32} us)",
        verdict.summary(),
        trace.fell_after_us.unwrap_or(0)
    );
    if !verdict.can_proceed() {
        // Refused rather than attempted. `lr11xx` waits on BUSY with no
        // timeout, so handing it a chip whose BUSY never falls hangs the
        // driver -- and on a board with no debug probe that is a silent board.
        return Err(BringUpError::NotReset);
    }

    // Attaching is itself a transaction with the chip, so it is bounded like
    // everything else here.
    let mut dev = match with_timeout(
        Duration::from_millis(500),
        Lr11xx::new(spi, reset.into_busy()),
    )
    .await
    {
        Err(_) => return Err(BringUpError::Timeout("attach")),
        Ok(Err(_)) => return Err(BringUpError::Radio),
        Ok(Ok(dev)) => dev,
    };

    // The oscillator, and the calibration that has to follow it.
    let start = async {
        dev.clear_errors().await?;
        dev.set_tcxo_mode(
            TcxoMode::builder()
                .with_delay(arbitrary_int::u24::new(tcxo::STARTUP_STEPS))
                // 3.0 V. Sweeping all eight supply codes against a
                // receiver showed the oscillator starts on every one and
                // the frequency does not care, so this is the board's
                // documented value rather than a tuned one.
                .with_tune(TcxoTune::V3p0)
                .build(),
        )
        .await?;
        dev.calibrate(Calibrate::ALL).await?;
        let errors = dev.errors().await?;
        let temp = dev.temp().await?;
        Ok::<_, lr11xx::Error>((errors, temp))
    };
    let (errors, temp) = match with_timeout(Duration::from_millis(500), start).await {
        Err(_) => return Err(BringUpError::Timeout("tcxo")),
        Ok(Err(_)) => return Err(BringUpError::Radio),
        Ok(Ok(v)) => v,
    };
    // Two independent checks, because either alone can pass while the
    // oscillator is stopped: the error flag is cleared by the calibration that
    // follows it, and `GetTemp` is *timed* by the oscillator, so a stopped
    // clock reads as an absurd temperature rather than as an error.
    if errors.raw_value() != 0 || !tcxo::temperature_is_plausible(temp) {
        defmt::error!(
            "radio: oscillator did not start -- errors {}, die {=f32} C",
            errors,
            temp
        );
        return Err(BringUpError::NoOscillator);
    }
    defmt::info!("radio: 32 MHz oscillator running, die {=f32} C", temp);

    // The antenna switch. This step cannot check itself: a wrong mask gives a
    // clean TxDone into a dead port. These masks were proved on a receiver,
    // which is the only instrument that can tell the difference.
    let switch = async {
        dev.set_dio_as_rf_switch(RfSwitchConfig::new_with_raw_value(
            rf_switch::BASE_DUO.to_raw(),
        ))
        .await?;
        dev.status().await
    };
    match with_timeout(Duration::from_millis(200), switch).await {
        Err(_) => return Err(BringUpError::Timeout("rf switch")),
        Ok(Err(_)) => return Err(BringUpError::Radio),
        Ok(Ok(_)) => {}
    }

    // Interrupts. Cleared first, or a stale flag holds the line high from the
    // moment it becomes an output and the idle check below fails for a reason
    // that has nothing to do with the routing.
    let route = async {
        dev.clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
            .await?;
        dev.set_dio_irq(
            Interrupt::new_with_raw_value(irq_bits::DIO9_MASK),
            Interrupt::new_with_raw_value(irq_bits::DIO11_MASK),
        )
        .await
    };
    match with_timeout(Duration::from_millis(200), route).await {
        Err(_) => return Err(BringUpError::Timeout("irq routing")),
        Ok(Err(_)) => return Err(BringUpError::Radio),
        Ok(Ok(_)) => {}
    }
    if irq.is_asserted() {
        defmt::error!("radio: the interrupt line is high with everything cleared");
        return Err(BringUpError::InterruptStuck);
    }

    // The switching regulator. Only accepted in standby RC -- in any other mode
    // the chip takes the command and then reports CMD_FAIL on the next status,
    // which is the same silent failure SetTxCw produces without a packet type.
    let regulator = async {
        dev.standby(false).await?;
        dev.set_reg_mode(true).await?;
        dev.status().await
    };
    match with_timeout(Duration::from_millis(200), regulator).await {
        Err(_) => return Err(BringUpError::Timeout("regulator")),
        Ok(Err(_)) => return Err(BringUpError::Radio),
        Ok(Ok((status, _))) => {
            if status.stat1().command_status() == Ok(lr11xx::ops::CommandStatus::Fail) {
                // Not fatal. It costs efficiency, not correctness, and a modem
                // that refuses to start over a regulator choice would be worse
                // than one that runs warm.
                defmt::warn!("radio: SetRegMode refused; running on the LDO");
            }
        }
    }

    defmt::info!("radio: up");
    Ok(dev)
}

// The tune code this uses and the one `oxinode-core` records for the board have
// to be the same voltage. They are separate enumerations -- one the crate's,
// one ours -- so nothing but an assertion connects them.
const _: () = assert!(tcxo::TUNE_3V0 == TcxoTune::V3p0 as u8);
