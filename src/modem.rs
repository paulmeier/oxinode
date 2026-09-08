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
    CadExit, CadParams, CodingRate, Interrupt, LoRaBandwidth, LoRaModulation, LoRaPacket, PaConfig,
    PacketType, RampTime, SpreadingFactor, TxParams,
};
use lr11xx::Lr11xx;
use oxinode_core::lr1121::config::{ValidConfig, MAX_PAYLOAD};
use oxinode_core::lr1121::csma::{self, Backoff, Channel, Step};
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
    /// How the wait for a clear channel went. `Copy` of the core's report,
    /// with the fields spelled out because `oxinode-core` cannot derive
    /// `defmt::Format`.
    pub csma: CsmaReport,
}

/// [`csma::Report`], in a form defmt can print. The default is no wait at
/// all, which is what [`Modem::send`] reports.
#[derive(Debug, Clone, Copy, Default, defmt::Format)]
pub struct CsmaReport {
    /// Senses taken, including the one that cleared the way.
    pub senses: u32,
    /// Senses that heard something.
    pub busy: u32,
    /// Microseconds spent waiting between senses.
    pub waited_us: u32,
    /// Whether the budget ran out and the packet went regardless.
    pub forced: bool,
}

impl From<csma::Report> for CsmaReport {
    fn from(r: csma::Report) -> Self {
        Self {
            senses: r.senses,
            busy: r.busy,
            waited_us: r.waited_us,
            forced: r.forced,
        }
    }
}

/// What one [`Modem::sense`] found.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum Sensed {
    /// Nothing on the air.
    Clear,
    /// Somebody transmitting, and not from the start: nothing to decode.
    Busy,
    /// Somebody transmitting, caught from the preamble: the packet is in
    /// the caller's buffer.
    Heard(RxReport),
}

/// What [`Modem::transmit`] did.
///
/// The wait for a clear channel is spent in receive, so a packet can arrive
/// during it. When one does, the transmission is put aside and the packet
/// handed up first: the caller deals with it and calls `transmit` again with
/// the same [`Backoff`], which remembers how long it has already waited.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub enum TxOutcome {
    /// The packet went.
    Sent(TxReport),
    /// Somebody else's packet arrived while waiting; it is in the caller's
    /// buffer and the transmission has not happened.
    Heard(RxReport),
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
    ///
    /// Standby goes first of all, for a related reason -- see the comment on
    /// the sequence below. Configuration from receive is refused, and refused
    /// quietly.
    pub async fn apply(&mut self, config: &ValidConfig) -> Result<(), ModemError> {
        let sequence = async {
            // Standby first, and this is not tidiness.
            //
            // The LR1121 only accepts its configuration commands in standby. In
            // receive it takes them and then reports `CMD_FAIL` on the next
            // status read -- the same silent refusal phase 3 met with
            // `SetRegMode` and `SetTxCw`. So the first configuration after boot
            // works, because the chip is already in standby, and every
            // *re*-configuration fails: a host that reconnects, or changes one
            // parameter while running, gets a modem that quietly keeps the
            // settings it had.
            //
            // XOSC rather than RC, so the 32 MHz reference is already running.
            // Phase 3 measured the startup as a fixed 5 ms charged to the first
            // operation that needs it, which is a tenth of the airtime at SF7
            // if it is paid once per packet.
            self.dev.standby(true).await?;
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
            // Two settings for the wait before a transmission; see `sense`
            // and `listen`. Once per configuration rather than once per
            // sense, because neither depends on anything that changes
            // between them.
            //
            // A receive with a timeout stops the timer on preamble detection
            // rather than on the header. The listening slots are shorter
            // than a preamble and a header together, so with the default a
            // packet that starts mid-slot is timed out just before it could
            // have been decoded -- which is exactly what happened on the
            // first phase 16 run. Stopped on the preamble, the slot stretches
            // to receive the whole packet.
            self.dev.set_stop_timeout_on_preamble(true).await?;
            // And a positive detection falls into receive rather than back
            // to standby, for a bounded time: a detection made on a preamble
            // is a packet about to arrive, and this is how it is caught.
            let rx_ticks = (csma::cad_rx_timeout_us(config) as u64 * 32_768 / 1_000_000) as u32;
            self.dev
                .set_cad_params(
                    CadParams::builder()
                        .with_symbols(csma::CAD_SYMBOLS)
                        .with_det_peak(csma::CAD_DET_PEAK)
                        .with_det_min(csma::CAD_DET_MIN)
                        .with_cad_exit(CadExit::Rx)
                        .with_timeout(arbitrary_int::u24::new(rx_ticks.min(0x00ff_fffe)))
                        .build(),
                )
                .await?;
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

    /// Listen once, and say whether somebody is transmitting -- and if the
    /// chip caught what they sent, hand it over.
    ///
    /// One LoRa channel activity detection: the chip correlates
    /// [`csma::CAD_SYMBOLS`] symbols against the configured modulation and
    /// raises `CadDone`, with `CadDetected` alongside it if it found one.
    /// On a detection it stays in receive for the time `apply` gave it, so
    /// a detection made on a preamble becomes a received packet in `buf`;
    /// one made mid-packet times out and is reported as busy. Works from
    /// any state; it puts the chip in standby first.
    ///
    /// `CadDetected` is not routed to the interrupt line -- it would wake the
    /// MCU for every packet on a busy band -- but it is in the pending word,
    /// which is where this reads it.
    pub async fn sense(
        &mut self,
        config: &ValidConfig,
        buf: &mut [u8],
    ) -> Result<Sensed, ModemError> {
        let prepare = async {
            // Safe from any state: a finished listen leaves the chip in
            // standby on its own, but a caller need not know that.
            self.dev.standby(true).await?;
            // The receive a detection falls into is a listening one; see
            // `listen` for why it has a symbol timeout. `start_rx` takes it
            // off again.
            self.dev
                .set_lora_synch_timeout(csma::sync_timeout_symbols(config))
                .await?;
            self.dev
                .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            self.dev.set_cad().await?;
            Ok::<(), lr11xx::Error>(())
        };
        match with_timeout(COMMAND_TIMEOUT, prepare).await {
            Err(_) => return Err(ModemError::Timeout("sense: SetCad")),
            Ok(Err(e)) => return Err(ModemError::Radio(e)),
            Ok(Ok(())) => {}
        }
        let deadline = Duration::from_micros(csma::cad_timeout_us(config) as u64);
        if self.irq.wait_asserted(deadline).await.is_err() {
            let _ = self.clear_interrupts().await;
            return Err(ModemError::NoInterrupt);
        }
        let pending = self.pending().await?;
        let _ = self.clear_interrupts().await;
        if pending & irq_bits::bit::CAD_DONE == 0 {
            return Err(ModemError::WrongInterrupt(pending));
        }
        if pending & irq_bits::bit::CAD_DETECTED == 0 {
            return Ok(Sensed::Clear);
        }
        // Detected, and the chip is now receiving. The next interrupt is the
        // packet, its timeout, or a header or CRC error; the receive timer
        // stops on the preamble, so a packet that syncs is received whole.
        let deadline = Duration::from_micros(
            csma::cad_rx_timeout_us(config) as u64 + config.airtime_us(MAX_PAYLOAD) as u64 + 50_000,
        );
        if self.irq.wait_asserted(deadline).await.is_err() {
            // The chip's timers should have ended this long ago. Something
            // was on the air that never became a packet; leave the receive
            // and call it busy rather than failing the transmission.
            self.leave_receive().await?;
            return Ok(Sensed::Busy);
        }
        match with_timeout(COMMAND_TIMEOUT, self.read_packet(buf)).await {
            Err(_) => Err(ModemError::Timeout("sense: read")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(Some(report))) => Ok(Sensed::Heard(report)),
            Ok(Ok(None)) => Ok(Sensed::Busy),
        }
    }

    /// Standby and clear, for a receive that has to be abandoned.
    async fn leave_receive(&mut self) -> Result<(), ModemError> {
        let sequence = async {
            self.dev.standby(true).await?;
            self.dev
                .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            Ok::<(), lr11xx::Error>(())
        };
        match with_timeout(COMMAND_TIMEOUT, sequence).await {
            Err(_) => Err(ModemError::Timeout("leave receive")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// Send a packet once the channel is clear, and wait for the chip to say
    /// it went -- or hand up a packet that arrived while waiting.
    ///
    /// Listens before transmitting: each sense is a channel activity
    /// detection, and the slot between senses is spent in receive, so a
    /// packet that starts during the wait is received rather than missed.
    /// See [`csma`] for the arrangement and [`TxOutcome`] for what to do
    /// with a heard packet. `backoff` is the caller's so that it carries
    /// across a heard packet; the report in [`TxReport`] is its total.
    ///
    /// Returns `Sent` when `TxDone` is pending, and returns an error rather
    /// than a report if some *other* interrupt fired — which is the case a
    /// bare timeout on the interrupt line reports as success.
    pub async fn transmit(
        &mut self,
        config: &ValidConfig,
        payload: &[u8],
        backoff: &mut Backoff,
        rx_buf: &mut [u8],
    ) -> Result<TxOutcome, ModemError> {
        if payload.len() > MAX_PAYLOAD as usize {
            return Err(ModemError::PayloadTooLong(payload.len()));
        }

        // Receiving during the wait needs the receive ceiling on the packet
        // length, which the last transmission may have narrowed.
        match with_timeout(
            COMMAND_TIMEOUT,
            self.dev.set_lora_packet(packet(config, MAX_PAYLOAD)),
        )
        .await
        {
            Err(_) => return Err(ModemError::Timeout("transmit: SetPacketParams")),
            Ok(Err(e)) => return Err(ModemError::Radio(e)),
            Ok(Ok(_)) => {}
        }

        // Wait for the air to be clear, listening in between -- and during.
        loop {
            let heard = match self.sense(config, rx_buf).await? {
                Sensed::Clear => Channel::Clear,
                Sensed::Busy => Channel::Busy,
                Sensed::Heard(report) => {
                    let _ = backoff.next(Channel::Busy);
                    return Ok(TxOutcome::Heard(report));
                }
            };
            match backoff.next(heard) {
                Step::Wait(us) => {
                    if let Some(report) = self.listen(config, us, rx_buf).await? {
                        // The channel was in use, whatever the sense said,
                        // so the next attempt starts its count over.
                        let _ = backoff.next(Channel::Busy);
                        return Ok(TxOutcome::Heard(report));
                    }
                }
                Step::Transmit | Step::Force => break,
            }
        }
        let csma = CsmaReport::from(backoff.report());
        self.send(config, payload, csma).await.map(TxOutcome::Sent)
    }

    /// Send a frame now, with no wait for a clear channel, and wait for the
    /// chip to say it went.
    ///
    /// For the second frame of a split packet, which follows the first with
    /// nothing between them: the receiver holds the first half until the
    /// second arrives, and a stock RNode holds it until something else
    /// does, so a carrier-sense wait here would be a window for another
    /// packet to throw the first half away. The channel was clear a moment
    /// ago and a neighbour that sensed it saw this board's first frame.
    ///
    /// [`Modem::transmit`] ends here too; `csma` is its report of the wait,
    /// and empty for a bare send.
    pub async fn send(
        &mut self,
        config: &ValidConfig,
        payload: &[u8],
        csma: CsmaReport,
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

        // Loaded after the wait rather than before it: the wait receives, and
        // a received packet lands in the same buffer.
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
            csma,
        })
    }

    /// Receive for one slot of the wait before a transmission.
    ///
    /// Single-shot receive with the chip's own timeout, which `apply` set to
    /// stop counting once a preamble is detected -- so a packet that starts
    /// inside the slot is received whole, however long it is, and the wait
    /// stretches to fit it. `Ok(None)` is the slot passing with nothing, or
    /// with something that was not a packet: a header or CRC error is a fact
    /// about the air, and the next sense decides what it means.
    async fn listen(
        &mut self,
        config: &ValidConfig,
        slot_us: u32,
        buf: &mut [u8],
    ) -> Result<Option<RxReport>, ModemError> {
        let ticks = ((slot_us as u64 * 32_768 / 1_000_000) as u32).clamp(1, 0x00ff_fffe);
        let sequence = async {
            // From standby, and back to the receive ceiling: a previous
            // attempt may have narrowed the packet length to its payload.
            self.dev.standby(true).await?;
            self.dev
                .set_lora_packet(packet(config, MAX_PAYLOAD))
                .await?;
            // The receive timer stops on preamble detection, which is what
            // lets a packet that starts mid-slot be received whole -- and
            // what would leave the chip receiving forever after a false
            // preamble, or one whose header a collision corrupted. Phase 16
            // saw exactly that: a transmission failed with no interrupt
            // after a second of silence. So the chip's other timer is set
            // too, in symbols from the start of the receive, long enough for
            // a real packet to validate its header and no longer.
            self.dev
                .set_lora_synch_timeout(csma::sync_timeout_symbols(config))
                .await?;
            self.dev
                .clear_irq(Interrupt::new_with_raw_value(irq_bits::ALL_NAMED))
                .await?;
            self.dev.set_rx(arbitrary_int::u24::new(ticks)).await?;
            Ok::<(), lr11xx::Error>(())
        };
        match with_timeout(CONFIGURE_TIMEOUT, sequence).await {
            Err(_) => return Err(ModemError::Timeout("listen: SetRx")),
            Ok(Err(e)) => return Err(ModemError::Radio(e)),
            Ok(Ok(())) => {}
        }
        // The slot, plus the longest packet that could have started inside
        // it, plus the oscillator.
        let deadline =
            Duration::from_micros(slot_us as u64 + config.airtime_us(MAX_PAYLOAD) as u64 + 50_000);
        if self.irq.wait_asserted(deadline).await.is_err() {
            // Both of the chip's timers should have ended this. Abandon the
            // receive; the next sense decides what the air is doing.
            self.leave_receive().await?;
            return Ok(None);
        }
        match with_timeout(COMMAND_TIMEOUT, self.read_packet(buf)).await {
            Err(_) => Err(ModemError::Timeout("listen: read")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(report)) => Ok(report),
        }
    }

    /// Enter continuous receive.
    ///
    /// `0xffffff` is the chip's "stay in RX until told otherwise" timeout, and
    /// it keeps receiving rather than stopping on the first packet — which is
    /// what a modem wants and what a single-shot receive would get wrong on the
    /// second packet rather than the first.
    pub async fn start_rx(&mut self, config: &ValidConfig) -> Result<(), ModemError> {
        let sequence = async {
            // Standby for the same reason as in `apply`: `SetPacketParams` is a
            // configuration command, and issuing it from receive is refused
            // silently. Insurance rather than a necessity on the paths that
            // exist today -- `apply` always precedes this, and a finished
            // transmission leaves the chip in standby anyway -- but it makes
            // `start_rx` safe to call from any state rather than only from the
            // two it happens to be called from now.
            self.dev.standby(true).await?;
            // Back to the receive ceiling, in case a transmit narrowed it.
            self.dev
                .set_lora_packet(packet(config, MAX_PAYLOAD))
                .await?;
            // And no symbol timeout: that is for the listening windows
            // before a transmission, and continuous receive must not end
            // after a few dozen symbols of nothing.
            self.dev.set_lora_synch_timeout(0).await?;
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
        match with_timeout(COMMAND_TIMEOUT, self.read_packet(buf)).await {
            Err(_) => Err(ModemError::Timeout("receive: read")),
            Ok(Err(e)) => Err(ModemError::Radio(e)),
            Ok(Ok(report)) => Ok(report),
        }
    }

    /// Read what the asserted interrupt line is about, and the packet if it
    /// is one. Clears every interrupt on the way out. Unbounded: the callers
    /// bound it.
    async fn read_packet(&mut self, buf: &mut [u8]) -> Result<Option<RxReport>, lr11xx::Error> {
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
        Ok(report)
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
