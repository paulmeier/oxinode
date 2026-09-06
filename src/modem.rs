//! One place that programs a configuration into the LR1121, and one place each
//! for transmitting and receiving with it.
//!
//! Phase 3's bring-up image talked to the radio directly, in three different
//! places, with the parameters written into each. That was the right shape for
//! answering "does this work at all"; it is the wrong shape for a modem, where
//! the parameters arrive from a host and the question is whether the chip ends
//! up configured the way it was asked to be.
//!
//! Everything here takes an [`oxinode_core::lr1121::config::ValidConfig`] and
//! nothing takes anything else. That is not politeness — it is the reason
//! "somebody forgot to check the frequency was in band" is not a bug that can
//! be written in phase 5.
//!
//! # Two habits from phase 3 that are not optional
//!
//! **Never trust a command's `Result`.** The LR11xx protocol returns the status
//! of the *previous* command, so `lr11xx` returning `Ok` means the command
//! before this one succeeded. A sequence that ends by asking for the status
//! explicitly is the only kind that has actually checked anything.
//!
//! **Bound every wait.** `lr11xx` waits on BUSY with no timeout of its own, so
//! a command that leaves BUSY high hangs the driver, and on a board with no
//! debug probe a hang is indistinguishable from a crash. Every await below is
//! inside a `with_timeout`, and the errors say which one expired.

use embassy_time::{with_timeout, Duration, Instant};
use lr11xx::ops::{
    CodingRate, Interrupt, LoRaBandwidth, LoRaModulation, LoRaPacket, PaConfig, PacketType,
    RampTime, SpreadingFactor, TxParams,
};
use lr11xx::Lr11xx;
use oxinode_core::lr1121::config::{ValidConfig, MAX_PAYLOAD};
use oxinode_core::lr1121::irq as irq_bits;

use crate::radio::RadioIrq;

/// How long a configuration sequence may take before it is called a hang.
///
/// A dozen commands at 1 MHz is well under a millisecond of bus time; the rest
/// is the oscillator startup one of them may pay. Generous against that, and
/// still far short of a board that has gone quiet.
const CONFIGURE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a single short command may take. Reading a status is bus time and
/// nothing else.
const COMMAND_TIMEOUT: Duration = Duration::from_millis(200);

/// Why an operation did not do what it was asked to.
#[derive(Debug, defmt::Format)]
pub enum ModemError {
    /// A command failed, or the bus did.
    Radio(lr11xx::Error),
    /// Something took longer than its bound. The string names which step, so a
    /// hang on a board with no probe still says where it was.
    Timeout(&'static str),
    /// The interrupt line was already high before an operation that needs to
    /// see it rise. Transmitting anyway would time the wrong thing.
    InterruptAlreadyHigh,
    /// The interrupt line never rose.
    NoInterrupt,
    /// The interrupt arrived, and it was not the one being waited for. This is
    /// the failure that a bare `TxDone` timeout reports as success.
    WrongInterrupt(u32),
    /// The payload does not fit in a LoRa packet.
    PayloadTooLong(usize),
}

impl From<lr11xx::Error> for ModemError {
    fn from(e: lr11xx::Error) -> Self {
        Self::Radio(e)
    }
}

/// What a transmission did.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub struct TxReport {
    /// Microseconds from `SetTx` to the interrupt.
    pub elapsed_us: u32,
    /// Microseconds of airtime the configuration predicts.
    pub airtime_us: u32,
    /// The interrupt word that was pending when it fired.
    pub pending: u32,
}

/// What arrived.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub struct RxReport {
    /// How many bytes were written into the caller's buffer.
    pub len: usize,
    /// Packet RSSI in dBm.
    pub rssi_dbm: i16,
    /// Packet SNR in dB, rounded — for logs.
    pub snr_db: i16,
    /// Packet SNR in quarter-dB, as the chip reports it.
    ///
    /// Carried alongside the rounded value rather than derived from it: the
    /// RNode protocol also transports quarter-dB, so recovering it by
    /// multiplying the rounded figure back up would throw away up to half a
    /// decibel for nothing.
    pub snr_quarter_db: i8,
    /// The interrupt word that was pending.
    pub pending: u32,
}

/// The modem: an LR1121 and the line it interrupts on.
///
/// Borrows rather than owns, so the phase 3 bring-up steps that need the raw
/// device can keep it. Phase 5 will want the owning form; the difference is one
/// line and no logic.
pub struct Modem<'a, 'd, S, B>
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    dev: &'a mut Lr11xx<S, B>,
    irq: &'a mut RadioIrq<'d>,
}

impl<'a, 'd, S, B> Modem<'a, 'd, S, B>
where
    S: embedded_hal_async::spi::SpiDevice<u8>,
    B: embedded_hal::digital::InputPin + embedded_hal_async::digital::Wait,
{
    /// Wrap a device and its interrupt line.
    pub fn new(dev: &'a mut Lr11xx<S, B>, irq: &'a mut RadioIrq<'d>) -> Self {
        Self { dev, irq }
    }

    /// Program a configuration into the chip.
    ///
    /// Programs the packet parameters for *reception* — an explicit header and
    /// a 255-byte ceiling — because that is the state a modem idles in.
    /// [`Modem::transmit`] re-programs the length, which it has to do anyway
    /// since the field means the actual length on transmit and the maximum
    /// accepted on receive.
    ///
    /// `SetPacketType` goes first and is not optional. Phase 3 spent a
    /// bisection discovering that: without it `SetTxCw` is rejected with
    /// `cmd_error` while `GetErrors` stays clean, and the crate's own
    /// documentation says only frequency and PA config are needed.
    pub async fn apply(&mut self, config: &ValidConfig) -> Result<(), ModemError> {
        let sequence = async {
            self.dev.set_packet_type(PacketType::LoRa).await?;
            self.dev
                .set_rf_frequency(config.commanded_frequency_hz())
                .await?;
            self.dev.set_lora_modulation(modulation(config)).await?;
            self.dev
                .set_lora_packet(packet(config, MAX_PAYLOAD))
                .await?;
            self.dev.set_lora_sync_word(config.sync_word).await?;
            self.dev
                .set_pa_config(PaConfig::new_with_raw_value(config.pa_config().to_raw()))
                .await?;
            self.dev
                .set_tx_params(
                    TxParams::builder()
                        .with_ramp_time(RampTime::Us48)
                        .with_tx_power(config.tx_power_dbm)
                        .build(),
                )
                .await?;
            // Sensitivity over receive current. An RNode is a base station on a
            // desk far more often than it is a battery node, and the extra
            // 2 dB is worth more than the milliamp.
            self.dev.set_rx_boosted(true).await?;
            // Ask for a status explicitly: everything above returned the status
            // of the command before it.
            let (status, _) = self.dev.status().await?;
            Ok::<_, lr11xx::Error>(status)
        };
        match with_timeout(CONFIGURE_TIMEOUT, sequence).await {
            Err(_) => Err(ModemError::Timeout("apply")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(_)) => Ok(()),
        }
    }

    /// Send a packet, and wait for the chip to say it went.
    ///
    /// Returns when `TxDone` is pending, and returns an error rather than a
    /// report if some *other* interrupt fired — which is the case a bare
    /// timeout on the interrupt line reports as success.
    pub async fn transmit(
        &mut self,
        config: &ValidConfig,
        payload: &[u8],
    ) -> Result<TxReport, ModemError> {
        if payload.len() > MAX_PAYLOAD as usize {
            return Err(ModemError::PayloadTooLong(payload.len()));
        }
        let len = payload.len() as u8;
        let airtime_us = config.airtime_us(len);

        // The chip's own timeout, in 32.768 kHz ticks, at three times the
        // airtime: long enough that a healthy packet never trips it, short
        // enough that a stuck transmitter gives the channel back. Saturated at
        // the field's width rather than wrapped -- at SF12 and 62.5 kHz three
        // airtimes is over eight minutes and does not fit in 24 bits, and a
        // wrap there would be a timeout of a few milliseconds on the slowest
        // configuration the chip offers.
        let ticks = ((airtime_us as u64 * 3 * 32_768 / 1_000_000) as u32).min(0x00ff_fffe);

        let prepare = async {
            self.dev.set_lora_packet(packet(config, len)).await?;
            self.dev
                .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            self.dev.write_buffer8(payload).await?;
            // Standby on the crystal rather than the RC oscillator, so the
            // 32 MHz reference is already running when SetTx is issued. Phase 3
            // measured that difference at 5 ms, charged to whichever operation
            // first needs the oscillator -- and if that is SetTx, the 5 ms
            // lands inside the airtime measurement.
            self.dev.standby(true).await?;
            Ok::<(), lr11xx::Error>(())
        };
        match with_timeout(CONFIGURE_TIMEOUT, prepare).await {
            Err(_) => return Err(ModemError::Timeout("transmit: prepare")),
            Ok(Err(e)) => return Err(ModemError::Radio(e)),
            Ok(Ok(())) => {}
        }

        if self.irq.is_asserted() {
            return Err(ModemError::InterruptAlreadyHigh);
        }

        let started = Instant::now();
        match with_timeout(
            COMMAND_TIMEOUT,
            self.dev.set_tx(arbitrary_int::u24::new(ticks)),
        )
        .await
        {
            Err(_) => return Err(ModemError::Timeout("transmit: SetTx")),
            Ok(Err(e)) => return Err(ModemError::Radio(e)),
            Ok(Ok(_)) => {}
        }

        // Four airtimes plus the oscillator startup, so a *late* TxDone is
        // still caught and reported as late rather than as missing.
        let deadline = Duration::from_micros(airtime_us as u64 * 4 + 20_000);
        if self.irq.wait_asserted(deadline).await.is_err() {
            let _ = self.clear_interrupts().await;
            return Err(ModemError::NoInterrupt);
        }
        let elapsed_us = started.elapsed().as_micros() as u32;

        let pending = self.pending().await?;
        let _ = self.clear_interrupts().await;
        if pending & irq_bits::bit::TX_DONE == 0 {
            return Err(ModemError::WrongInterrupt(pending));
        }
        Ok(TxReport {
            elapsed_us,
            airtime_us,
            pending,
        })
    }

    /// Enter continuous receive.
    ///
    /// `0xffffff` is the chip's "stay in RX until told otherwise" timeout, and
    /// it keeps receiving rather than stopping on the first packet — which is
    /// what a modem wants and what a single-shot receive would get wrong on the
    /// second packet rather than the first.
    pub async fn start_rx(&mut self, config: &ValidConfig) -> Result<(), ModemError> {
        let sequence = async {
            // Back to the receive ceiling, in case a transmit narrowed it.
            self.dev
                .set_lora_packet(packet(config, MAX_PAYLOAD))
                .await?;
            self.dev
                .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            self.dev
                .set_rx(arbitrary_int::u24::new(0x00ff_ffff))
                .await?;
            Ok::<(), lr11xx::Error>(())
        };
        match with_timeout(CONFIGURE_TIMEOUT, sequence).await {
            Err(_) => Err(ModemError::Timeout("start_rx")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// Wait for a packet, up to `timeout`.
    ///
    /// `Ok(None)` means the deadline passed with nothing — the ordinary case
    /// on a quiet channel, and deliberately not an error. An interrupt that is
    /// not `RxDone` also gives `Ok(None)` after being cleared: a header or CRC
    /// error is a fact about the air, not a fault in the modem.
    pub async fn receive(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<Option<RxReport>, ModemError> {
        if self.irq.wait_asserted(timeout).await.is_err() {
            return Ok(None);
        }
        let sequence = async {
            let (_, pending) = self.dev.status().await?;
            let pending = pending.raw_value();
            let mut report = None;
            if pending & irq_bits::bit::RX_DONE != 0 {
                let status = self.dev.rx_buffer_status().await?;
                let len = (status.payload_length() as usize).min(buf.len());
                self.dev
                    .read_buffer8(status.offset(), &mut buf[..len])
                    .await?;
                let packet = self.dev.lora_packet_status().await?;
                report = Some(RxReport {
                    len,
                    // The chip reports RSSI as a positive number of half-dBm
                    // below zero, and SNR as quarter-dB. Both conversions are
                    // easy to get subtly wrong and neither produces a value
                    // that looks obviously incorrect.
                    rssi_dbm: -(packet.rssi() as i16) / 2,
                    snr_db: (packet.snr() as i16 + 2) / 4,
                    snr_quarter_db: packet.snr(),
                    pending,
                });
            }
            self.dev
                .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            Ok::<_, lr11xx::Error>(report)
        };
        match with_timeout(COMMAND_TIMEOUT, sequence).await {
            Err(_) => Err(ModemError::Timeout("receive: read")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(report)) => Ok(report),
        }
    }

    /// Leave whatever mode the chip is in, on the RC oscillator.
    pub async fn standby(&mut self) -> Result<(), ModemError> {
        match with_timeout(COMMAND_TIMEOUT, self.dev.standby(false)).await {
            Err(_) => Err(ModemError::Timeout("standby")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(_)) => Ok(()),
        }
    }

    /// The pending interrupt word.
    pub async fn pending(&mut self) -> Result<u32, ModemError> {
        match with_timeout(COMMAND_TIMEOUT, self.dev.status()).await {
            Err(_) => Err(ModemError::Timeout("status")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok((_, pending))) => Ok(pending.raw_value()),
        }
    }

    /// Clear every interrupt this firmware names.
    pub async fn clear_interrupts(&mut self) -> Result<(), ModemError> {
        let clear = self
            .dev
            .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED));
        match with_timeout(COMMAND_TIMEOUT, clear).await {
            Err(_) => Err(ModemError::Timeout("clear_irq")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(_)) => Ok(()),
        }
    }
}

/// The modulation word for a configuration.
///
/// The codes come from `oxinode-core`, where they are tested; the matches here
/// only turn them into the crate's enums. Both are total over the range
/// `ValidConfig` guarantees, and the fallbacks below are unreachable rather
/// than merely unlikely — see the assertions in
/// `oxinode_core::lr1121::config`.
fn modulation(config: &ValidConfig) -> LoRaModulation {
    LoRaModulation::builder()
        .with_sf(spreading_factor(config.spreading_factor))
        .with_bwl(bandwidth(config.bandwidth_code()))
        .with_cr(coding_rate(config.coding_rate_code()))
        .with_low_data_rate_optimize(config.low_data_rate_optimize())
        .build()
}

/// The packet word for a configuration at a given payload length.
///
/// The length means two different things: on transmit it is the number of bytes
/// to send, and on receive it is the largest packet that will be accepted, with
/// anything longer raising a header error. So it is a parameter here rather
/// than a field of the configuration.
fn packet(config: &ValidConfig, payload_len: u8) -> LoRaPacket {
    LoRaPacket::builder()
        .with_preamble_length(config.preamble_symbols)
        .with_header_implicit(config.implicit_header)
        .with_payload_length(payload_len)
        .with_crc(config.crc)
        .with_invert_iq(config.invert_iq)
        .build()
}

fn spreading_factor(sf: u8) -> SpreadingFactor {
    match sf {
        5 => SpreadingFactor::SF5,
        6 => SpreadingFactor::SF6,
        7 => SpreadingFactor::SF7,
        8 => SpreadingFactor::SF8,
        9 => SpreadingFactor::SF9,
        10 => SpreadingFactor::SF10,
        11 => SpreadingFactor::SF11,
        _ => SpreadingFactor::SF12,
    }
}

fn bandwidth(code: u8) -> LoRaBandwidth {
    match code {
        0x03 => LoRaBandwidth::KHz62,
        0x04 => LoRaBandwidth::KHz125,
        0x05 => LoRaBandwidth::KHz250,
        _ => LoRaBandwidth::KHz500,
    }
}

/// Short interleaver only.
///
/// The crate also defines codes 5–7 for the long interleaver at the same rates,
/// which is why `oxinode-core` translates 4/5–4/8 into 1–4 instead of passing
/// the host's number through: `CodingRate::Long45` is a perfectly valid setting
/// that no ordinary LoRa receiver is using.
fn coding_rate(code: u8) -> CodingRate {
    match code {
        0x01 => CodingRate::Short45,
        0x02 => CodingRate::Short46,
        0x03 => CodingRate::Short47,
        _ => CodingRate::Short48,
    }
}
