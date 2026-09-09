//! The RNode conversation, with no hardware anywhere near it.
//!
//! Commands go in, responses and at most one radio action come out. That is
//! the whole of it, and the reason it is shaped that way is that it makes the
//! entire `rnsd` startup — detect, five setters, power on, validate — a unit
//! test rather than a session at the bench.
//!
//! # What the host is actually checking
//!
//! `configure_device` will not bring an interface online unless the device
//! **echoes its configuration back and the echo matches**: frequency to within
//! 100 Hz, then bandwidth, TX power, spreading factor and radio state exactly.
//! So every setter here answers, and answers with what it was told rather than
//! with what it will do about it.
//!
//! That distinction is the whole of [`Protocol::set_frequency`]'s subtlety. The
//! frequency the modem *commands* is 73 ppm above the one the host asked for
//! — 67 kHz at 915 MHz, against a 100 Hz tolerance, or 670 times too far. The
//! host must be told the frequency it asked for, and the config keeping the
//! wanted and commanded frequencies as separate fields is what makes that one
//! line instead of a bug hunt.
//!
//! # Invalid configurations
//!
//! A host is free to ask for a configuration this board cannot do — 868 MHz on
//! a US antenna, or 22 dBm on a module rated for 20. Nothing here clamps: a
//! clamp is a lie the host cannot detect.
//!
//! Instead the setter stores and echoes what was asked, and the radio simply
//! does not come on. The host then finds a radio-state mismatch and reports
//! *"the reported radio parameters did not match your configuration. Make sure
//! that your hardware actually supports the parameters specified"* — which is
//! exactly what happened. Sending `CMD_ERROR` instead would make it say
//! "hardware initialisation error", which is both harsher and less true.
//!
//! The specific limit that was hit is kept in [`Protocol::last_error`] for the
//! log port, because the serial link has no way to carry it and a person
//! debugging this deserves better than "mismatch".

use super::command::RSSI_OFFSET;
use super::command::{cmd, hash_kind, Command, RadioState};
use super::command::{DETECT_RESP, FW_VERSION_MAJOR, FW_VERSION_MINOR, MCU_NRF52, PLATFORM_NRF52};
use super::display::External;
use super::eeprom::{self, Eeprom, BOARD_HMBRW};
use super::store::DeviceStore;
use crate::lr1121::config::{ConfigError, RadioConfig, Setting, ValidConfig, DEFAULT};

/// Somewhere for response frames to go.
///
/// The implementation decides framing and transport; this module decides
/// content. Escaping is not the implementor's problem — it is settled by
/// [`super::command::encode_response`], which every sink should use.
pub trait Sink {
    /// Emit one response frame.
    fn frame(&mut self, command: u8, payload: &[u8]);
}

/// What the firmware should do to the radio as a result of a command.
///
/// At most one per command, which is true of the protocol rather than a
/// simplification: the host changes one thing at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action<'a> {
    /// Nothing to do.
    None,
    /// Program the current configuration and start receiving.
    Reconfigure,
    /// Stop. The radio is off, or the host has left.
    Standby,
    /// Send this.
    Transmit(&'a [u8]),
    /// The persistent device data changed. Write it out.
    ///
    /// Deliberately not "write it out *now*". `rnodeconf` provisions a board
    /// by sending 155 single-byte writes six milliseconds apart, and a device
    /// that erased a flash page for each of them would need thirteen seconds
    /// to absorb one second of commands. The caller is expected to hold the
    /// change in memory and commit once the writes stop — see
    /// `docs/architecture/provisioning.md`.
    Persist,
    /// The host asked the device to restart.
    Reset,
    /// What the panel should show has changed. Redraw it.
    Redraw,
    /// Answer `CMD_DISP_READ`, which needs the pixels the panel is actually
    /// showing — and those live with the display driver, not here.
    ReportDisplay,
}

/// What a change made from the panel amounts to.
///
/// Two things rather than one [`Action`], because a panel edit can need both:
/// the radio reprogrammed *and* the record written. A host command never
/// does -- its setters do not touch the radio and its saves do not touch the
/// setters -- which is why `Action` is one thing per command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelChange {
    /// What to do to the radio.
    pub radio: Action<'static>,
    /// The persistent record changed: write it out, on the usual timer.
    pub persist: bool,
}

impl PanelChange {
    /// Nothing happened.
    pub const NONE: PanelChange = PanelChange {
        radio: Action::None,
        persist: false,
    };
}

/// The protocol state.
pub struct Protocol {
    config: RadioConfig,
    state: RadioState,
    locked: bool,
    short_airtime_limit: u16,
    long_airtime_limit: u16,
    detected: bool,
    rx_count: u32,
    tx_count: u32,
    last_error: Option<ConfigError>,
    /// Everything that survives a power cycle.
    store: DeviceStore,
    /// The MCU's factory device ID, which the device hash is bound to. Zero on
    /// a `Protocol::new`, because a pure state machine has no chip to ask.
    mcu_id: u64,
    /// The picture the host can push, and whether it is being shown.
    external: External,
    /// Display contrast, as `CMD_DISP_INT` sets it. The SH1107's power-on
    /// value, so a host that never sets it gets the panel's own default.
    display_intensity: u8,
}

impl Default for Protocol {
    fn default() -> Self {
        Self::new()
    }
}

impl Protocol {
    /// A modem that has just booted: radio off, the default configuration
    /// loaded, nothing detected yet.
    ///
    /// Off is the right starting state and not merely the cautious one. The
    /// host explicitly turns the radio on as the last step of `initRadio`, and
    /// a device that was already transmitting before being configured would be
    /// on whatever channel the previous host left it on.
    pub const fn new() -> Self {
        Self {
            config: DEFAULT,
            state: RadioState::Off,
            locked: false,
            short_airtime_limit: 0,
            long_airtime_limit: 0,
            detected: false,
            rx_count: 0,
            tx_count: 0,
            last_error: None,
            store: DeviceStore::new(),
            mcu_id: 0,
            external: External::new(),
            display_intensity: 0x80,
        }
    }

    /// A modem that has just booted on a real board: whatever was in flash,
    /// and the chip's own device ID.
    pub const fn with_storage(store: DeviceStore, mcu_id: u64) -> Self {
        let mut protocol = Self::new();
        protocol.store = store;
        protocol.mcu_id = mcu_id;
        protocol
    }

    /// The persistent device data, for the storage layer to write out.
    /// The persistent state, for the parts of it the protocol does not
    /// manage: the firmware keeps the Bluetooth bonds here so that one record
    /// and one flash write cover everything.
    pub fn store_mut(&mut self) -> &mut DeviceStore {
        &mut self.store
    }

    pub const fn store(&self) -> &DeviceStore {
        &self.store
    }

    /// The EEPROM image, as the host reads it.
    pub const fn rom(&self) -> &Eeprom {
        &self.store.rom
    }

    /// The picture the host has pushed, and whether it is the one to show.
    pub const fn external(&self) -> &External {
        &self.external
    }

    /// Contrast, as `CMD_DISP_INT` last set it.
    pub const fn display_intensity(&self) -> u8 {
        self.display_intensity
    }

    /// Adopt the stored configuration, if there is one: TNC mode.
    ///
    /// A provisioned device with a saved configuration is supposed to come up
    /// on air by itself, with no host attached — that is what `rnodeconf --tnc`
    /// asks for and the only reason to store a configuration at all.
    ///
    /// The stored values are put through the same validation as anything a
    /// host sends, and for the same reason: they were written by a tool that
    /// does not know what this radio can do. A configuration that does not
    /// validate leaves the radio off, with [`Protocol::last_error`] saying why,
    /// exactly as an impossible request from a host would.
    ///
    /// A host that connects afterwards and configures the device overrides
    /// this, which is right: it is the same set of fields.
    pub fn resume_stored_config(&mut self) -> Action<'static> {
        let Some(stored) = self.store.rom.stored_config() else {
            return Action::None;
        };
        self.config.frequency_hz = stored.frequency_hz;
        self.config.bandwidth_hz = stored.bandwidth_hz;
        self.config.spreading_factor = stored.spreading_factor;
        self.config.coding_rate = stored.coding_rate;
        self.config.tx_power_dbm = stored.tx_power_dbm;
        match ValidConfig::new(self.config) {
            Ok(_) => {
                self.last_error = None;
                self.state = RadioState::On;
                Action::Reconfigure
            }
            Err(e) => {
                self.last_error = Some(e);
                self.state = RadioState::Off;
                Action::None
            }
        }
    }

    /// Whether a stored configuration is in force: the device is a TNC rather
    /// than a host-controlled modem.
    pub const fn is_tnc(&self) -> bool {
        self.store.rom.stored_config().is_some()
    }

    /// The configuration as the host has set it — the *wanted* values.
    pub const fn config(&self) -> &RadioConfig {
        &self.config
    }

    /// The configuration, if it is one the radio can be given.
    pub const fn valid_config(&self) -> Option<ValidConfig> {
        match ValidConfig::new(self.config) {
            Ok(valid) => Some(valid),
            Err(_) => None,
        }
    }

    /// Whether the radio should be on.
    pub const fn radio_is_on(&self) -> bool {
        matches!(self.state, RadioState::On)
    }

    /// Whether a host has completed the detect handshake.
    pub const fn detected(&self) -> bool {
        self.detected
    }

    /// Why the radio last refused to come on, if it did.
    ///
    /// The host cannot be told this — the protocol has no frame for it — so it
    /// exists for the log port.
    pub const fn last_error(&self) -> Option<ConfigError> {
        self.last_error
    }

    /// Packets received and transmitted since boot.
    pub const fn counters(&self) -> (u32, u32) {
        (self.rx_count, self.tx_count)
    }

    /// Handle one command from the host.
    pub fn handle<'a, S: Sink>(&mut self, command: Command<'a>, out: &mut S) -> Action<'a> {
        match command {
            Command::Detect => {
                self.detected = true;
                out.frame(cmd::DETECT, &[DETECT_RESP]);
                Action::None
            }
            Command::QueryFirmwareVersion => {
                out.frame(cmd::FW_VERSION, &[FW_VERSION_MAJOR, FW_VERSION_MINOR]);
                Action::None
            }
            Command::QueryPlatform => {
                out.frame(cmd::PLATFORM, &[PLATFORM_NRF52]);
                Action::None
            }
            Command::QueryMcu => {
                out.frame(cmd::MCU, &[MCU_NRF52]);
                Action::None
            }
            Command::QueryBoard => {
                out.frame(cmd::BOARD, &[BOARD_HMBRW]);
                Action::None
            }

            // The whole image in one frame, escaped -- it contains a 128-byte
            // RSA signature, which is uniformly distributed bytes and so
            // certainly contains the frame delimiter.
            Command::ReadRom => {
                out.frame(cmd::ROM_READ, self.store.rom.as_bytes());
                Action::None
            }
            // No reply. The host does not wait for one; it writes the next
            // byte six milliseconds later and reads the whole image back at
            // the end, which is a better check than 155 acknowledgements.
            Command::WriteRom { addr, value } => {
                self.store.rom.write(addr, value);
                Action::Persist
            }
            Command::WipeRom => {
                self.store.wipe();
                Action::Persist
            }
            Command::SaveConfig => {
                self.store.rom.save_config(&self.config);
                Action::Persist
            }
            Command::DeleteConfig => {
                self.store.rom.delete_config();
                Action::Persist
            }
            Command::Reset => Action::Reset,

            // Answered only when there is an identity to hash. An
            // unprovisioned device would otherwise hand back a hash of erased
            // bytes, and let a signature be made over a device that does not
            // exist yet -- which the next provisioning would inherit.
            Command::QueryDeviceHash => {
                if self.store.rom.is_provisioned() {
                    let hash = eeprom::device_hash(&self.store.rom, self.mcu_id);
                    out.frame(cmd::DEV_HASH, &hash);
                }
                Action::None
            }
            Command::StoreDeviceSignature(sig) => {
                let mut stored = [0u8; eeprom::DEVICE_SIGNATURE_LEN];
                stored.copy_from_slice(sig);
                self.store.device_signature = Some(stored);
                Action::Persist
            }
            Command::SetFirmwareHash(hash) => {
                let mut stored = [0u8; eeprom::HASH_LEN];
                stored.copy_from_slice(hash);
                self.store.target_firmware_hash = Some(stored);
                Action::Persist
            }
            Command::QueryHash(kind) => {
                self.report_hash(kind, out);
                Action::None
            }
            // Nothing to prepare. On an ESP32 this is what stops the device
            // serving its bootstrap console from flash that is about to be
            // overwritten; there is no such thing here.
            Command::FirmwareUpdateImminent => Action::None,

            Command::ShowExternalFramebuffer(on) => {
                self.external.set_enabled(on);
                Action::Redraw
            }
            // No reply, and no redraw per line. The host writes sixty-four of
            // these in a burst, and repainting the panel after each would cost
            // fourteen seconds of bus time to show one picture.
            Command::WriteFramebuffer { line, data } => {
                self.external.write_line(line, data);
                Action::None
            }
            Command::ReadFramebuffer => {
                out.frame(cmd::FB_READ, self.external.as_bytes());
                Action::None
            }
            Command::ReadDisplay => Action::ReportDisplay,
            Command::SetDisplayIntensity(level) => {
                self.display_intensity = level;
                Action::Redraw
            }

            Command::SetFrequency(hz) => {
                self.set(|c| c.frequency_hz = hz);
                self.report_frequency(out);
                Action::None
            }
            Command::SetBandwidth(hz) => {
                self.set(|c| c.bandwidth_hz = hz);
                self.report_bandwidth(out);
                Action::None
            }
            Command::SetTxPower(dbm) => {
                self.set(|c| c.tx_power_dbm = dbm);
                self.report_tx_power(out);
                Action::None
            }
            Command::SetSpreadingFactor(sf) => {
                self.set(|c| c.spreading_factor = sf);
                self.report_spreading_factor(out);
                Action::None
            }
            Command::SetCodingRate(cr) => {
                self.set(|c| c.coding_rate = cr);
                self.report_coding_rate(out);
                Action::None
            }

            Command::SetRadioLock(locked) => {
                self.locked = locked;
                out.frame(cmd::RADIO_LOCK, &[u8::from(self.locked)]);
                Action::None
            }
            Command::SetShortAirtimeLimit(v) => {
                self.short_airtime_limit = v;
                out.frame(cmd::ST_ALOCK, &v.to_be_bytes());
                Action::None
            }
            Command::SetLongAirtimeLimit(v) => {
                self.long_airtime_limit = v;
                out.frame(cmd::LT_ALOCK, &v.to_be_bytes());
                Action::None
            }

            Command::SetRadioState(RadioState::Ask) => {
                self.report_state(out);
                Action::None
            }
            Command::SetRadioState(RadioState::Off) => {
                self.state = RadioState::Off;
                self.report_state(out);
                Action::Standby
            }
            Command::SetRadioState(RadioState::On) => match ValidConfig::new(self.config) {
                Ok(_) => {
                    self.last_error = None;
                    self.state = RadioState::On;
                    self.report_state(out);
                    Action::Reconfigure
                }
                Err(e) => {
                    // Stay off, and say so. The host compares the reported
                    // state against the one it asked for, finds the mismatch,
                    // and tells the operator their hardware does not support
                    // what they configured -- which is true, and is a better
                    // message than any this end could send.
                    self.last_error = Some(e);
                    self.state = RadioState::Off;
                    self.report_state(out);
                    Action::Standby
                }
            },

            // The host is closing the interface. Stop transmitting; do not
            // answer, because there may be nothing listening any more.
            Command::Leave => {
                self.state = RadioState::Off;
                Action::Standby
            }

            Command::Data(payload) => {
                if self.radio_is_on() {
                    Action::Transmit(payload)
                } else {
                    // Dropped rather than queued. A packet handed to a radio
                    // that is off has nowhere to wait that the host would not
                    // consider stale by the time it could go.
                    Action::None
                }
            }

            // Silence, deliberately. A response to a command we do not
            // implement would be a response the host has to interpret, and the
            // one thing worse than not answering is answering wrongly.
            Command::NotYetImplemented(_)
            | Command::NotApplicable(_)
            | Command::Unknown(_)
            | Command::Malformed(_) => Action::None,
        }
    }

    /// Apply a change, unless the configuration is locked.
    ///
    /// A locked device keeps its configuration and keeps echoing the value it
    /// actually holds, so a host that tries to change something under a lock
    /// sees its own setting come back unchanged rather than being told nothing.
    fn set(&mut self, f: impl FnOnce(&mut RadioConfig)) {
        if !self.locked {
            f(&mut self.config);
        }
    }

    // ---- changes from the panel --------------------------------------------
    //
    // The panel is a second controller, and the rule for when it may act is
    // the caller's -- see `docs/architecture/interface.md`. What is here is
    // what acting *means*, and it means the same as it does for a host: a
    // setting lands unvalidated, the radio is reprogrammed only through
    // `ValidConfig`, and a configuration the radio cannot do leaves it off
    // with the reason in `last_error`. None of it answers on the wire, because
    // by the rule nobody is on the wire when it happens.

    /// Bring the radio up on the current configuration, or refuse: the
    /// same steps as `CMD_RADIO_STATE` on, without the reply.
    fn switch_on(&mut self) -> Action<'static> {
        match ValidConfig::new(self.config) {
            Ok(_) => {
                self.last_error = None;
                self.state = RadioState::On;
                Action::Reconfigure
            }
            Err(e) => {
                self.last_error = Some(e);
                self.state = RadioState::Off;
                Action::Standby
            }
        }
    }

    /// One parameter, changed from the panel.
    ///
    /// The setting lands as a host's would, and if the radio is on it is
    /// reprogrammed -- or refused, identically. In TNC mode the stored
    /// configuration is updated too, because it is the one the board boots
    /// with and a panel edit that vanished at the next power cycle would be
    /// a setting that only looked changed. Under host control nothing is
    /// stored: the host owns that configuration and will set it again.
    ///
    /// A host's radio lock is respected, as it is for the host's own setters.
    pub fn set_from_panel(&mut self, setting: Setting) -> PanelChange {
        if self.locked {
            return PanelChange::NONE;
        }
        self.config.apply(setting);
        let persist = self.is_tnc();
        if persist {
            self.store.rom.save_config(&self.config);
        }
        let radio = if self.radio_is_on() {
            self.switch_on()
        } else {
            Action::None
        };
        PanelChange { radio, persist }
    }

    /// `Radio On/Off` from the panel.
    pub fn toggle_from_panel(&mut self) -> Action<'static> {
        if self.radio_is_on() {
            self.state = RadioState::Off;
            Action::Standby
        } else {
            self.switch_on()
        }
    }

    /// `Save Config` from the panel: the live configuration becomes the one
    /// the board boots with, exactly as `CMD_CONF_SAVE` does it.
    pub fn save_from_panel(&mut self) -> PanelChange {
        self.store.rom.save_config(&self.config);
        PanelChange {
            radio: Action::None,
            persist: true,
        }
    }

    /// `Reset Config` from the panel: back to what the board boots with.
    ///
    /// With a stored configuration that is TNC mode, resumed exactly as at
    /// boot -- radio on. Without one it is the default, with the radio left
    /// in whatever state it was: on stays on, reprogrammed.
    pub fn reset_from_panel(&mut self) -> Action<'static> {
        if self.is_tnc() {
            return self.resume_stored_config();
        }
        self.config = DEFAULT;
        if self.radio_is_on() {
            self.switch_on()
        } else {
            Action::None
        }
    }

    /// Report every value `validateRadioState` compares.
    ///
    /// Not used by the command path — each setter answers for itself — but it
    /// is what a device should send unprompted after a reset, and it is the
    /// convenient way to assert the whole reported state in one place.
    pub fn report_all<S: Sink>(&self, out: &mut S) {
        self.report_frequency(out);
        self.report_bandwidth(out);
        self.report_tx_power(out);
        self.report_spreading_factor(out);
        self.report_coding_rate(out);
        self.report_state(out);
    }

    /// The **wanted** frequency, not the commanded one.
    ///
    /// The modem commands 73 ppm high to cancel the module's reference error.
    /// At 915 MHz that is 67 kHz, and the host rejects a mismatch beyond
    /// 100 Hz — so reporting the commanded frequency would make every
    /// interface fail to come online, with a message pointing at the frequency
    /// the operator had configured correctly.
    fn report_frequency<S: Sink>(&self, out: &mut S) {
        out.frame(cmd::FREQUENCY, &self.config.frequency_hz.to_be_bytes());
    }

    fn report_bandwidth<S: Sink>(&self, out: &mut S) {
        out.frame(cmd::BANDWIDTH, &self.config.bandwidth_hz.to_be_bytes());
    }

    fn report_tx_power<S: Sink>(&self, out: &mut S) {
        out.frame(cmd::TXPOWER, &[self.config.tx_power_dbm as u8]);
    }

    fn report_spreading_factor<S: Sink>(&self, out: &mut S) {
        out.frame(cmd::SF, &[self.config.spreading_factor]);
    }

    fn report_coding_rate<S: Sink>(&self, out: &mut S) {
        out.frame(cmd::CR, &[self.config.coding_rate]);
    }

    fn report_state<S: Sink>(&self, out: &mut S) {
        // `RadioState::Ask` has no encoding, and can never be the state we are
        // in: it is a question the host asks, never an answer.
        if let Some(byte) = self.state.encode() {
            out.frame(cmd::RADIO_STATE, &[byte]);
        }
    }

    /// A packet arrived: tell the host about the signal, then the packet.
    ///
    /// The order matters. Reticulum clears its stored RSSI and SNR as soon as
    /// it has handed a data frame upwards, so anything sent afterwards belongs
    /// to no packet at all.
    pub fn received<S: Sink>(
        &mut self,
        rssi_dbm: i16,
        snr_quarter_db: i8,
        payload: &[u8],
        out: &mut S,
    ) {
        self.rx_count = self.rx_count.saturating_add(1);
        out.frame(cmd::STAT_RSSI, &[encode_rssi(rssi_dbm)]);
        out.frame(cmd::STAT_SNR, &[encode_snr(snr_quarter_db)]);
        out.frame(cmd::DATA, payload);
    }

    /// A packet went out. Counts it and releases the host's flow control.
    ///
    /// `CMD_READY` is what lets a host with `flow_control` enabled send the
    /// next packet. Reticulum defaults that off, so this is not required — but
    /// a device that never sends it simply stops working the moment somebody
    /// turns the option on, which is the kind of failure that gets blamed on
    /// the radio.
    pub fn transmitted<S: Sink>(&mut self, out: &mut S) {
        self.tx_count = self.tx_count.saturating_add(1);
        out.frame(cmd::READY, &[]);
    }

    /// Report the packet counters.
    pub fn report_counters<S: Sink>(&self, out: &mut S) {
        out.frame(cmd::STAT_RX, &self.rx_count.to_be_bytes());
        out.frame(cmd::STAT_TX, &self.tx_count.to_be_bytes());
    }

    /// Answer a stored-hash request.
    ///
    /// Only the target hash exists here, and that is a real limit rather than
    /// an omission. `hash_kind::FIRMWARE` asks the device for the hash of the
    /// firmware it is *running*, and an image cannot contain a hash of itself;
    /// producing one means hashing the flash at runtime over an extent the
    /// linker does not hand us in any form worth trusting. Answering with
    /// something else would be worse than not answering: the host compares the
    /// two, and a wrong match reads as a verified firmware.
    fn report_hash<S: Sink>(&self, kind: u8, out: &mut S) {
        if kind != hash_kind::TARGET_FIRMWARE {
            return;
        }
        let Some(hash) = self.store.target_firmware_hash else {
            return;
        };
        let mut payload = [0u8; 1 + eeprom::HASH_LEN];
        payload[0] = hash_kind::TARGET_FIRMWARE;
        payload[1..].copy_from_slice(&hash);
        out.frame(cmd::HASHES, &payload);
    }

    /// Tell the host something went wrong. See [`super::command::error`].
    pub fn report_error<S: Sink>(&self, code: u8, out: &mut S) {
        out.frame(cmd::ERROR, &[code]);
    }
}

/// dBm to the byte the host expects: it computes `byte - RSSI_OFFSET`.
///
/// Saturating, because the byte cannot represent below −157 dBm or above
/// +98 dBm and wrapping either end would report a strong signal as a weak one
/// or the reverse.
pub const fn encode_rssi(dbm: i16) -> u8 {
    let v = dbm + RSSI_OFFSET;
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

/// SNR in quarter-dB to the byte the host expects, which reads it signed.
///
/// One value has to be avoided. The host does **not** un-escape this frame, so
/// the byte travels raw — and a raw `0xC0` is the frame delimiter, which would
/// end the frame early and leave the host with no SNR at all. `0xC0` is −64
/// quarter-dB, or −16 dB: below the demodulation floor at every spreading
/// factor the LR1121 offers, so a packet reporting it did not arrive. It is
/// nudged by a quarter of a decibel rather than escaped.
///
/// `0xDB`, the escape byte, needs no such treatment: unescaped it arrives
/// intact, and it is *escaping* it that would corrupt it.
pub const fn encode_snr(quarter_db: i8) -> u8 {
    let byte = quarter_db as u8;
    if byte == super::kiss::FEND {
        byte.wrapping_add(1)
    } else {
        byte
    }
}

// The RSSI mapping has to be exact at the ends, because those are where a sign
// error stops looking like a plausible reading.
const _: () = assert!(encode_rssi(-157) == 0);
const _: () = assert!(encode_rssi(-45) == 112);
const _: () = assert!(encode_rssi(-1000) == 0 && encode_rssi(1000) == 255);
// The one byte that cannot travel raw must not be produced.
const _: () = assert!(encode_snr(-64) != super::kiss::FEND);
// ...and the escape byte must travel unchanged, because this frame is not
// un-escaped at the other end.
const _: () = assert!(encode_snr(-37) == super::kiss::FESC);

#[cfg(test)]
mod tests {
    use super::super::command::{self, decode, error};
    use super::super::kiss;
    use super::*;

    /// Records frames as the protocol emits them.
    #[derive(Default)]
    struct Frames(Vec<(u8, Vec<u8>)>);

    impl Sink for Frames {
        fn frame(&mut self, command: u8, payload: &[u8]) {
            self.0.push((command, payload.to_vec()));
        }
    }

    impl Frames {
        fn commands(&self) -> Vec<u8> {
            self.0.iter().map(|(c, _)| *c).collect()
        }
        fn payload(&self, command: u8) -> Option<&[u8]> {
            self.0
                .iter()
                .rev()
                .find(|(c, _)| *c == command)
                .map(|(_, p)| p.as_slice())
        }
    }

    /// Writes real KISS bytes, so a test can check the wire and not just the
    /// intent.
    #[derive(Default)]
    struct Wire(Vec<u8>);

    impl Sink for Wire {
        fn frame(&mut self, command: u8, payload: &[u8]) {
            let mut buf = vec![0u8; command::response_len(command, payload)];
            let n = command::encode_response(command, payload, &mut buf).expect("sized exactly");
            buf.truncate(n);
            self.0.extend_from_slice(&buf);
        }
    }

    /// Drive the protocol with raw host bytes, exactly as Reticulum writes
    /// them, and return everything the device says back.
    fn converse(p: &mut Protocol, host_bytes: &[u8]) -> Wire {
        let mut wire = Wire::default();
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        for &b in host_bytes {
            if d.feed(b) == kiss::Step::Frame {
                let (c, payload) = (d.command(), d.payload().to_vec());
                p.handle(decode(c, &payload), &mut wire);
            }
        }
        wire
    }

    /// Decode a device-to-host byte stream the way the host's reader would.
    fn read_back(wire: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        for &b in wire {
            if d.feed(b) == kiss::Step::Frame {
                out.push((d.command(), d.payload().to_vec()));
            }
        }
        out
    }

    /// A frame written the way `rnodeconf` writes one: escaped payload.
    fn host_frame(command: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![kiss::FEND, command];
        for &b in payload {
            match b {
                kiss::FEND => out.extend_from_slice(&[kiss::FESC, kiss::TFEND]),
                kiss::FESC => out.extend_from_slice(&[kiss::FESC, kiss::TFESC]),
                other => out.push(other),
            }
        }
        out.push(kiss::FEND);
        out
    }

    /// The identity block and its MD5, as in `eeprom`'s own tests: computed by
    /// Python's `hashlib` rather than by the code under test.
    const GOLDEN_INFO: [u8; eeprom::INFO_LEN] = [
        0xF0, 0xFF, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68, 0xB9, 0xB1, 0x40,
    ];
    const GOLDEN_CHECKSUM: [u8; 16] = [
        0x41, 0x9E, 0x21, 0xE3, 0x9C, 0x5B, 0x75, 0x3F, 0x1F, 0x34, 0x41, 0x8C, 0xEE, 0xB5, 0x92,
        0x5E,
    ];

    /// Every byte `rnodeconf --rom` writes, in the order it writes them: the
    /// identity, the checksum, the 128-byte signature, and the lock byte last.
    ///
    /// The signature bytes are deliberately every value from 0 to 127 and then
    /// some — a real RSA signature is uniformly distributed, so it contains
    /// `0xC0` and `0xDB`, and this must too or the escaping is not exercised.
    fn provisioning_writes() -> Vec<u8> {
        let mut bytes = Vec::new();
        for (i, b) in GOLDEN_INFO.iter().enumerate() {
            bytes.extend(host_frame(
                cmd::ROM_WRITE,
                &[eeprom::addr::PRODUCT + i as u8, *b],
            ));
        }
        for (i, b) in GOLDEN_CHECKSUM.iter().enumerate() {
            bytes.extend(host_frame(
                cmd::ROM_WRITE,
                &[eeprom::addr::CHECKSUM + i as u8, *b],
            ));
        }
        for i in 0..eeprom::SIGNATURE_LEN {
            bytes.extend(host_frame(
                cmd::ROM_WRITE,
                &[eeprom::addr::SIGNATURE + i as u8, (i as u8).wrapping_mul(3)],
            ));
        }
        bytes.extend(host_frame(
            cmd::ROM_WRITE,
            &[eeprom::addr::INFO_LOCK, eeprom::INFO_LOCK_BYTE],
        ));
        bytes
    }

    /// The bytes `RNodeInterface.detect()` writes, verbatim.
    fn detect_bytes() -> Vec<u8> {
        vec![
            kiss::FEND,
            cmd::DETECT,
            command::DETECT_REQ,
            kiss::FEND,
            cmd::FW_VERSION,
            0x00,
            kiss::FEND,
            cmd::PLATFORM,
            0x00,
            kiss::FEND,
            cmd::MCU,
            0x00,
            kiss::FEND,
        ]
    }

    /// The bytes `initRadio()` writes for a given configuration.
    fn init_radio_bytes(hz: u32, bw: u32, dbm: u8, sf: u8, cr: u8) -> Vec<u8> {
        let mut v = Vec::new();
        let mut push = |command: u8, payload: &[u8]| {
            let mut buf = vec![0u8; kiss::encoded_len(payload, true)];
            let n = kiss::encode(command, payload, true, &mut buf).unwrap();
            v.extend_from_slice(&buf[..n]);
        };
        push(cmd::FREQUENCY, &hz.to_be_bytes());
        push(cmd::BANDWIDTH, &bw.to_be_bytes());
        push(cmd::TXPOWER, &[dbm]);
        push(cmd::SF, &[sf]);
        push(cmd::CR, &[cr]);
        push(cmd::RADIO_STATE, &[0x01]);
        v
    }

    /// The whole of `configure_device`, as bytes, against a fresh device — and
    /// then the check `validateRadioState` performs. This is the test that says
    /// whether `rnsd` would come online, and it runs on the host in
    /// microseconds.
    #[test]
    fn a_reticulum_host_would_bring_this_interface_online() {
        let mut p = Protocol::new();

        let detect = read_back(&converse(&mut p, &detect_bytes()).0);
        assert_eq!(
            detect,
            vec![
                (cmd::DETECT, vec![command::DETECT_RESP]),
                (cmd::FW_VERSION, vec![1, 52]),
                (cmd::PLATFORM, vec![command::PLATFORM_NRF52]),
                (cmd::MCU, vec![command::MCU_NRF52]),
            ]
        );
        assert!(p.detected());

        let (hz, bw, dbm, sf, cr) = (915_000_000u32, 125_000u32, 14u8, 8u8, 5u8);
        let replies = read_back(&converse(&mut p, &init_radio_bytes(hz, bw, dbm, sf, cr)).0);

        // Everything validateRadioState compares, read back the way the host
        // reads it.
        let find = |c: u8| {
            replies
                .iter()
                .rev()
                .find(|(k, _)| *k == c)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("no {c:#04x} reply"))
        };
        let r_frequency = u32::from_be_bytes(find(cmd::FREQUENCY).try_into().unwrap());
        let r_bandwidth = u32::from_be_bytes(find(cmd::BANDWIDTH).try_into().unwrap());
        assert!(hz.abs_diff(r_frequency) <= 100, "frequency mismatch");
        assert_eq!(bw, r_bandwidth, "bandwidth mismatch");
        assert_eq!(find(cmd::TXPOWER), vec![dbm], "TX power mismatch");
        assert_eq!(find(cmd::SF), vec![sf], "spreading factor mismatch");
        assert_eq!(find(cmd::RADIO_STATE), vec![0x01], "radio state mismatch");
        assert!(p.radio_is_on());
    }

    /// The 73 ppm correction must not reach the host. It is 67 kHz at 915 MHz
    /// against a 100 Hz tolerance — 670 times too far — so a device that
    /// reported the commanded frequency would fail to come online *every time*,
    /// with a message pointing at a frequency the operator had set correctly.
    #[test]
    fn the_reference_correction_is_invisible_to_the_host() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetFrequency(915_000_000), &mut out);

        let reported = u32::from_be_bytes(out.payload(cmd::FREQUENCY).unwrap().try_into().unwrap());
        assert_eq!(reported, 915_000_000, "the host must see what it asked for");

        // ...while the radio is still commanded 67 kHz higher.
        let commanded = p.config().commanded_frequency_hz();
        assert!(p.config().correct_reference);
        assert!(commanded - reported > 60_000);
        assert!(
            reported.abs_diff(915_000_000) <= 100,
            "within the host's tolerance"
        );
    }

    /// A configuration this board cannot do is echoed faithfully and then
    /// refused. Nothing clamps: the host asked for 22 dBm and is told 22 dBm,
    /// so it cannot conclude the request was honoured when it was not.
    #[test]
    fn an_impossible_configuration_is_echoed_but_not_switched_on() {
        for (command, expect) in [
            (Command::SetTxPower(22), ConfigError::PowerAboveModuleRating),
            (
                Command::SetFrequency(868_000_000),
                ConfigError::FrequencyOutOfBand,
            ),
            (
                Command::SetBandwidth(100_000),
                ConfigError::UnsupportedBandwidth,
            ),
            (
                Command::SetSpreadingFactor(13),
                ConfigError::SpreadingFactorOutOfRange,
            ),
        ] {
            let mut p = Protocol::new();
            let mut out = Frames::default();
            p.handle(command, &mut out);
            let action = p.handle(Command::SetRadioState(RadioState::On), &mut out);

            assert_eq!(action, Action::Standby, "{command:?}");
            assert!(!p.radio_is_on(), "{command:?}");
            // The host reads state 0 against the 1 it asked for, and reports a
            // mismatch -- which is the accurate message.
            assert_eq!(
                out.payload(cmd::RADIO_STATE),
                Some(&[0x00u8][..]),
                "{command:?}"
            );
            assert_eq!(p.last_error(), Some(expect), "{command:?}");
        }
    }

    /// ...and a valid one afterwards clears the refusal, so a host that fixes
    /// its configuration and retries succeeds without a power cycle.
    #[test]
    fn fixing_the_configuration_lets_the_radio_come_on() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetTxPower(22), &mut out);
        p.handle(Command::SetRadioState(RadioState::On), &mut out);
        assert!(!p.radio_is_on());

        p.handle(Command::SetTxPower(14), &mut out);
        let action = p.handle(Command::SetRadioState(RadioState::On), &mut out);
        assert_eq!(action, Action::Reconfigure);
        assert!(p.radio_is_on());
        assert_eq!(p.last_error(), None);
    }

    /// A packet only goes out when the radio is on. Before `initRadio`
    /// finishes there is no channel to send it on, and the configuration is
    /// whatever the last host left behind.
    #[test]
    fn data_is_only_transmitted_once_the_radio_is_on() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        assert_eq!(p.handle(Command::Data(b"hello"), &mut out), Action::None);

        p.handle(Command::SetRadioState(RadioState::On), &mut out);
        assert_eq!(
            p.handle(Command::Data(b"hello"), &mut out),
            Action::Transmit(b"hello")
        );

        p.handle(Command::SetRadioState(RadioState::Off), &mut out);
        assert_eq!(p.handle(Command::Data(b"hello"), &mut out), Action::None);
    }

    /// A received packet is announced signal-first. Reticulum clears its stored
    /// RSSI and SNR the moment it hands a data frame upwards, so a device that
    /// sent them afterwards would attach them to no packet at all.
    #[test]
    fn signal_quality_is_reported_before_the_packet_it_belongs_to() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.received(-69, 44, b"payload", &mut out);
        assert_eq!(
            out.commands(),
            vec![cmd::STAT_RSSI, cmd::STAT_SNR, cmd::DATA]
        );
        assert_eq!(out.payload(cmd::STAT_RSSI), Some(&[88u8][..]));
        assert_eq!(out.payload(cmd::DATA), Some(&b"payload"[..]));
        assert_eq!(p.counters(), (1, 0));
    }

    /// A packet full of frame delimiters has to survive, because Reticulum's
    /// traffic is encrypted and therefore looks like random bytes. This is the
    /// end-to-end version: through the protocol, onto the wire, and back out
    /// the way the host reads it.
    #[test]
    fn a_packet_of_nothing_but_delimiters_survives_the_round_trip() {
        // 256 bytes covers every value; 512 would not fit, because HW_MTU is
        // 508 and the limit is real rather than notional.
        let payload: Vec<u8> = (0..=255u8).collect();
        let mut p = Protocol::new();
        let mut wire = Wire::default();
        p.received(-100, 0, &payload, &mut wire);
        let frames = read_back(&wire.0);
        assert_eq!(frames.last(), Some(&(cmd::DATA, payload)));
    }

    /// The single-byte frames must reach the host unescaped, because its parser
    /// does not un-escape them. This is the failure that would look like a
    /// working modem reporting a nonsense SNR.
    #[test]
    fn single_byte_frames_travel_raw_even_when_the_value_is_an_escape() {
        let mut p = Protocol::new();
        let mut wire = Wire::default();
        // -37 quarter-dB is -9.25 dB, an ordinary SNR, and 0xDB on the wire --
        // which is the escape byte.
        p.received(-100, -37, b"x", &mut wire);
        assert!(
            wire.0
                .windows(3)
                .any(|w| w == [kiss::FEND, cmd::STAT_SNR, kiss::FESC]),
            "the SNR byte must follow its command verbatim: {:02x?}",
            wire.0
        );
        // `read_back` cannot be used here, and that is the point rather than a
        // limitation of the helper: it un-escapes every frame, which is exactly
        // what the host does *not* do for this command. Modelling the host
        // correctly means reading the byte straight off the wire, which is what
        // its parser does.
        let i = wire
            .0
            .windows(2)
            .position(|w| w == [kiss::FEND, cmd::STAT_SNR])
            .expect("an SNR frame");
        let byte = wire.0[i + 2];
        assert_eq!(byte, 0xDB);
        assert_eq!(byte as i8, -37, "-9.25 dB");
        assert_eq!(wire.0[i + 3], kiss::FEND, "one byte, then the delimiter");
    }

    /// The one SNR the wire cannot carry. -16 dB is below the demodulation
    /// floor at every spreading factor, so nudging it is invisible in practice
    /// — but sending 0xC0 raw would end the frame and leave the host with no
    /// SNR at all, which is not.
    #[test]
    fn the_one_snr_that_would_end_its_own_frame_is_nudged() {
        assert_eq!(encode_snr(-64), 0xC1);
        assert_ne!(encode_snr(-64), kiss::FEND);
        // A quarter of a decibel, and only for that one value.
        for q in -128i8..=127 {
            let b = encode_snr(q);
            assert_ne!(b, kiss::FEND, "{q} encodes to the frame delimiter");
            if q != -64 {
                assert_eq!(b, q as u8, "{q} must be untouched");
            }
        }
    }

    /// The asymmetry runs both ways, and in the receive direction it is
    /// harmless — but only because of what the host can actually send.
    ///
    /// Reticulum escapes the multi-byte commands it writes and sends the
    /// single-byte ones raw, exactly as it reads them. This firmware's decoder
    /// un-escapes uniformly, which is wrong in principle for `CMD_TXPOWER`,
    /// `CMD_SF`, `CMD_CR` and `CMD_RADIO_STATE`.
    ///
    /// It cannot bite, because a raw byte is only misread when it is `0xC0` or
    /// `0xDB`, and neither is a value those four fields can carry: spreading
    /// factor is 5–12, coding rate 5–8, radio state 0, 1 or 0xFF, and TX power
    /// would have to be −64 or −37 dBm. This test walks every legal value of
    /// each and shows the round trip is exact — so the simplification is
    /// deliberate and bounded rather than unnoticed.
    #[test]
    fn every_value_the_host_can_send_raw_survives_a_uniform_decoder() {
        let mut cases: Vec<(u8, u8)> = Vec::new();
        for sf in 5..=12u8 {
            cases.push((cmd::SF, sf));
        }
        for cr in 5..=8u8 {
            cases.push((cmd::CR, cr));
        }
        for state in [0x00u8, 0x01, 0xFF] {
            cases.push((cmd::RADIO_STATE, state));
        }
        for dbm in -17i8..=22 {
            cases.push((cmd::TXPOWER, dbm as u8));
        }

        for (command, value) in cases {
            // The host writes these raw: FEND, command, value, FEND.
            let wire = [kiss::FEND, command, value, kiss::FEND];
            let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
            let mut got = None;
            for &b in &wire {
                if d.feed(b) == kiss::Step::Frame {
                    got = Some((d.command(), d.payload().to_vec()));
                }
            }
            let (c, payload) = got.unwrap_or_else(|| {
                panic!("{command:#04x} with {value:#04x} did not decode at all")
            });
            assert_eq!(c, command);
            assert_eq!(payload, vec![value], "{command:#04x} = {value:#04x}");
            assert_eq!(d.dropped(), 0);
        }

        // And the two bytes that would break it are ones no legal value
        // reaches, which is the whole reason this is safe.
        assert!(!(5..=12).contains(&(kiss::FEND as i32)));
        for forbidden in [kiss::FEND, kiss::FESC] {
            assert!(!(5..=12u8).contains(&forbidden), "spreading factor");
            assert!(!(5..=8u8).contains(&forbidden), "coding rate");
            assert!(![0x00u8, 0x01, 0xFF].contains(&forbidden), "radio state");
            let as_dbm = forbidden as i8;
            assert!(!(-17..=22).contains(&as_dbm), "TX power {as_dbm}");
        }
    }

    /// A locked device keeps its configuration and says so. The host sees its
    /// own setting come back unchanged, which is a mismatch it reports, rather
    /// than silence it cannot interpret.
    #[test]
    fn a_locked_configuration_does_not_change_and_reports_what_it_has() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetSpreadingFactor(7), &mut out);
        p.handle(Command::SetRadioLock(true), &mut out);
        p.handle(Command::SetSpreadingFactor(12), &mut out);
        assert_eq!(p.config().spreading_factor, 7);
        assert_eq!(out.payload(cmd::SF), Some(&[7u8][..]));

        p.handle(Command::SetRadioLock(false), &mut out);
        p.handle(Command::SetSpreadingFactor(12), &mut out);
        assert_eq!(p.config().spreading_factor, 12);
        assert_eq!(out.payload(cmd::SF), Some(&[12u8][..]));
    }

    /// Asking does not change anything. A device that treated the question as
    /// a command would switch its radio off every time a host enquired.
    #[test]
    fn asking_for_the_radio_state_reports_it_without_changing_it() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetRadioState(RadioState::On), &mut out);
        assert_eq!(
            p.handle(Command::SetRadioState(RadioState::Ask), &mut out),
            Action::None
        );
        assert!(p.radio_is_on());
        assert_eq!(out.payload(cmd::RADIO_STATE), Some(&[0x01u8][..]));
    }

    /// Commands we do not implement get silence. A response the host has to
    /// interpret is worse than none, and `CMD_UNKNOWN` is the host's own
    /// sentinel rather than something to send.
    #[test]
    fn unimplemented_and_unknown_commands_produce_no_response() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        for c in [
            Command::NotYetImplemented(cmd::FB_READ),
            Command::Unknown(0x7E),
            Command::Malformed(cmd::FREQUENCY),
        ] {
            assert_eq!(p.handle(c, &mut out), Action::None, "{c:?}");
        }
        assert!(out.0.is_empty(), "{:?}", out.0);
    }

    /// Leaving stops the radio and says nothing, because the host that asked
    /// is on its way out and may already have closed the port.
    #[test]
    fn leaving_stops_the_radio_silently() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetRadioState(RadioState::On), &mut out);
        out.0.clear();
        assert_eq!(p.handle(Command::Leave, &mut out), Action::Standby);
        assert!(!p.radio_is_on());
        assert!(out.0.is_empty());
    }

    /// Counters count, and saturate rather than wrapping. A modem that ran for
    /// a long time should report an implausible number rather than start again
    /// at zero, which reads as a device that just rebooted.
    #[test]
    fn the_counters_count_and_do_not_wrap() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetRadioState(RadioState::On), &mut out);
        for _ in 0..3 {
            p.received(-50, 20, b"x", &mut out);
        }
        for _ in 0..2 {
            p.transmitted(&mut out);
        }
        assert_eq!(p.counters(), (3, 2));

        p.rx_count = u32::MAX;
        p.received(-50, 20, b"x", &mut out);
        assert_eq!(p.counters().0, u32::MAX);
    }

    /// Flow control: a transmitted packet releases the host to send the next.
    #[test]
    fn a_transmitted_packet_releases_the_hosts_flow_control() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.transmitted(&mut out);
        assert_eq!(out.commands(), vec![cmd::READY]);
        assert_eq!(out.payload(cmd::READY), Some(&[][..]));
    }

    /// Every error code the host understands must survive the wire. The first
    /// two make it drop the interface, so sending the wrong one turns a
    /// recoverable condition into a dead link.
    #[test]
    fn error_codes_reach_the_host_intact() {
        for code in [
            error::INITRADIO,
            error::TXFAILED,
            error::QUEUE_FULL,
            error::MEMORY_LOW,
            error::MODEM_TIMEOUT,
        ] {
            let p = Protocol::new();
            let mut wire = Wire::default();
            p.report_error(code, &mut wire);
            assert_eq!(read_back(&wire.0), vec![(cmd::ERROR, vec![code])]);
        }
    }

    /// The whole reported state at once, which is what a host validates
    /// against. Nothing may be missing: an absent frame leaves the host's
    /// corresponding field at `None`, and `validateRadioState` compares it
    /// anyway.
    #[test]
    fn reporting_everything_covers_every_field_the_host_validates() {
        let p = Protocol::new();
        let mut out = Frames::default();
        p.report_all(&mut out);
        for c in [
            cmd::FREQUENCY,
            cmd::BANDWIDTH,
            cmd::TXPOWER,
            cmd::SF,
            cmd::CR,
            cmd::RADIO_STATE,
        ] {
            assert!(out.payload(c).is_some(), "{c:#04x} was not reported");
        }
    }

    /// Every setter answers. A silent setter is an interface that never comes
    /// online, and the host gives no clue which one it was still waiting for.
    #[test]
    fn every_setter_answers_with_its_own_command() {
        let cases: [(Command, u8); 8] = [
            (Command::SetFrequency(915_000_000), cmd::FREQUENCY),
            (Command::SetBandwidth(125_000), cmd::BANDWIDTH),
            (Command::SetTxPower(14), cmd::TXPOWER),
            (Command::SetSpreadingFactor(8), cmd::SF),
            (Command::SetCodingRate(5), cmd::CR),
            (Command::SetRadioLock(true), cmd::RADIO_LOCK),
            (Command::SetShortAirtimeLimit(500), cmd::ST_ALOCK),
            (Command::SetLongAirtimeLimit(1500), cmd::LT_ALOCK),
        ];
        for (command, expected) in cases {
            let mut p = Protocol::new();
            let mut out = Frames::default();
            p.handle(command, &mut out);
            assert_eq!(out.commands(), vec![expected], "{command:?}");
        }
    }

    /// The airtime limits are echoed in the units they arrived in — hundredths
    /// of a percent — because the host divides by 100 to get back to a
    /// percentage and would otherwise show a limit a hundred times off.
    #[test]
    fn the_airtime_limits_round_trip_in_hundredths_of_a_percent() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        // 2.5% arrives as 250.
        p.handle(Command::SetShortAirtimeLimit(250), &mut out);
        assert_eq!(out.payload(cmd::ST_ALOCK), Some(&[0x00, 0xFA][..]));
        let echoed = u16::from_be_bytes([0x00, 0xFA]);
        assert_eq!(echoed as f32 / 100.0, 2.5);
    }

    // ---- provisioning ---------------------------------------------------

    /// `rnodeconf`'s probe is twice as long as `rnsd`'s, and an unprovisioned
    /// board answers five of the eight. The three it does not answer are
    /// silences with reasons: no identity to hash, and no stored hashes.
    #[test]
    fn the_rnodeconf_probe_is_answered_as_far_as_an_unprovisioned_board_can() {
        let mut p = Protocol::new();
        let mut wire = Wire::default();
        let bytes = [
            host_frame(cmd::DETECT, &[command::DETECT_REQ]),
            host_frame(cmd::FW_VERSION, &[0x00]),
            host_frame(cmd::PLATFORM, &[0x00]),
            host_frame(cmd::MCU, &[0x00]),
            host_frame(cmd::BOARD, &[0x00]),
            host_frame(cmd::DEV_HASH, &[0x01]),
            host_frame(cmd::HASHES, &[0x01]),
            host_frame(cmd::HASHES, &[0x02]),
        ]
        .concat();
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        for &b in &bytes {
            if d.feed(b) == kiss::Step::Frame {
                let (c, payload) = (d.command(), d.payload().to_vec());
                p.handle(decode(c, &payload), &mut wire);
            }
        }
        let replies = read_back(&wire.0);
        assert_eq!(
            replies.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            vec![
                cmd::DETECT,
                cmd::FW_VERSION,
                cmd::PLATFORM,
                cmd::MCU,
                cmd::BOARD
            ]
        );
        assert_eq!(
            replies.last().unwrap().1,
            vec![eeprom::BOARD_HMBRW],
            "the board byte is what rnodeconf prints after the model"
        );
    }

    /// The whole `rnodeconf --rom` bootstrap, as bytes, followed by the
    /// `download_eeprom` it validates with. This is provisioning in one test:
    /// if it passes, a real host provisioning a real board gets a device it
    /// calls provisioned.
    #[test]
    fn provisioning_over_the_wire_produces_an_eeprom_the_host_accepts() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        let mut persists = 0;
        for &b in &provisioning_writes() {
            if d.feed(b) == kiss::Step::Frame {
                let (c, payload) = (d.command(), d.payload().to_vec());
                if p.handle(decode(c, &payload), &mut sink) == Action::Persist {
                    persists += 1;
                }
            }
        }
        assert_eq!(
            persists,
            eeprom::INFO_LEN + 16 + eeprom::SIGNATURE_LEN + 1,
            "every write has to ask to be persisted"
        );
        assert!(
            sink.0.is_empty(),
            "an EEPROM write is not acknowledged; the host reads the image back instead"
        );

        // Now the readback, and parse it the way parse_eeprom does.
        let mut wire = Wire::default();
        p.handle(Command::ReadRom, &mut wire);
        let dump = read_back(&wire.0);
        assert_eq!(dump.len(), 1);
        let (command, image) = &dump[0];
        assert_eq!(*command, cmd::ROM_READ);
        assert_eq!(image.len(), eeprom::SIZE);

        assert_eq!(
            image[eeprom::addr::INFO_LOCK as usize],
            eeprom::INFO_LOCK_BYTE
        );
        assert_eq!(&image[..eeprom::INFO_LEN], &GOLDEN_INFO);
        assert_eq!(
            &image[eeprom::addr::CHECKSUM as usize..][..16],
            &GOLDEN_CHECKSUM
        );
        assert!(p.rom().is_provisioned());
    }

    /// A device hash is only offered once there is an identity to hash. An
    /// unprovisioned board that answered would let `rnodeconf --sign` create a
    /// signature over erased bytes, which the next provisioning would inherit.
    #[test]
    fn the_device_hash_is_answered_only_once_there_is_an_identity() {
        let mut p = Protocol::with_storage(DeviceStore::new(), 0x0011_2233_4455_6677);
        let mut before = Frames::default();
        p.handle(Command::QueryDeviceHash, &mut before);
        assert!(before.0.is_empty());

        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        let mut sink = Frames::default();
        for &b in &provisioning_writes() {
            if d.feed(b) == kiss::Step::Frame {
                let (c, payload) = (d.command(), d.payload().to_vec());
                p.handle(decode(c, &payload), &mut sink);
            }
        }

        let mut after = Frames::default();
        p.handle(Command::QueryDeviceHash, &mut after);
        let hash = after.payload(cmd::DEV_HASH).expect("a hash");
        assert_eq!(hash.len(), eeprom::HASH_LEN);
        assert_eq!(
            hash,
            eeprom::device_hash(p.rom(), 0x0011_2233_4455_6677),
            "the hash on the wire is the one the module computes"
        );
    }

    /// The signature `rnodeconf --sign` sends back is stored whole, and asked
    /// to be persisted.
    #[test]
    fn a_device_signature_is_stored_and_persisted() {
        let mut p = Protocol::new();
        let sig = [0xA5u8; eeprom::DEVICE_SIGNATURE_LEN];
        let mut sink = Frames::default();
        assert_eq!(
            p.handle(Command::StoreDeviceSignature(&sig), &mut sink),
            Action::Persist
        );
        assert_eq!(p.store().device_signature, Some(sig));
        assert!(sink.0.is_empty());
    }

    /// A wipe takes the provisioning and the signature with it, and the device
    /// is then exactly what it was before it was ever provisioned.
    #[test]
    fn a_wipe_returns_the_device_to_the_state_it_shipped_in() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        for &b in &provisioning_writes() {
            if d.feed(b) == kiss::Step::Frame {
                let (c, payload) = (d.command(), d.payload().to_vec());
                p.handle(decode(c, &payload), &mut sink);
            }
        }
        p.handle(Command::StoreDeviceSignature(&[1; 64]), &mut sink);
        assert!(p.rom().is_provisioned());

        assert_eq!(p.handle(Command::WipeRom, &mut sink), Action::Persist);
        assert!(!p.rom().is_provisioned());
        assert_eq!(p.store(), &DeviceStore::new());
    }

    /// TNC mode, the round trip that gives storing a configuration a point:
    /// the host configures the device exactly as `rnsd` would, saves, and the
    /// same values come back on the next boot with the radio coming up on
    /// its own.
    #[test]
    fn a_saved_configuration_comes_back_at_the_next_boot() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        p.handle(Command::SetFrequency(915_200_000), &mut sink);
        p.handle(Command::SetBandwidth(250_000), &mut sink);
        p.handle(Command::SetTxPower(17), &mut sink);
        p.handle(Command::SetSpreadingFactor(9), &mut sink);
        p.handle(Command::SetCodingRate(6), &mut sink);
        assert_eq!(p.handle(Command::SaveConfig, &mut sink), Action::Persist);
        assert!(p.is_tnc());

        // Reboot: the record is what survives, nothing else.
        let record = p.store().encode();
        let restored = DeviceStore::decode(&record).expect("a valid record");
        let mut next = Protocol::with_storage(restored, 0);
        assert!(!next.radio_is_on(), "nothing is on until it is resumed");

        assert_eq!(next.resume_stored_config(), Action::Reconfigure);
        assert!(next.radio_is_on());
        assert_eq!(next.config().frequency_hz, 915_200_000);
        assert_eq!(next.config().bandwidth_hz, 250_000);
        assert_eq!(next.config().tx_power_dbm, 17);
        assert_eq!(next.config().spreading_factor, 9);
        assert_eq!(next.config().coding_rate, 6);
    }

    /// A device with no stored configuration comes up host-controlled, which
    /// is what every board does until somebody asks for TNC mode.
    #[test]
    fn without_a_stored_configuration_the_radio_stays_off_at_boot() {
        let mut p = Protocol::new();
        assert!(!p.is_tnc());
        assert_eq!(p.resume_stored_config(), Action::None);
        assert!(!p.radio_is_on());
    }

    /// A stored configuration is validated on the way out, not just on the way
    /// in. It was written by a tool that does not know what this radio can do,
    /// and the alternative to checking is a board that boots with nobody
    /// watching and programs the chip with nonsense.
    #[test]
    fn a_stored_configuration_the_radio_cannot_do_leaves_it_off() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        p.handle(Command::SetSpreadingFactor(13), &mut sink);
        p.handle(Command::SaveConfig, &mut sink);

        let restored = DeviceStore::decode(&p.store().encode()).unwrap();
        let mut next = Protocol::with_storage(restored, 0);
        assert_eq!(next.resume_stored_config(), Action::None);
        assert!(!next.radio_is_on());
        assert_eq!(
            next.last_error(),
            Some(ConfigError::SpreadingFactorOutOfRange)
        );
    }

    /// Deleting the configuration puts the device back under host control.
    #[test]
    fn deleting_the_configuration_ends_tnc_mode() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        p.handle(Command::SaveConfig, &mut sink);
        assert!(p.is_tnc());
        assert_eq!(p.handle(Command::DeleteConfig, &mut sink), Action::Persist);
        assert!(!p.is_tnc());
    }

    /// The target firmware hash round trips. The *running* firmware's hash
    /// does not, and that is a limit rather than an oversight: an image cannot
    /// contain a hash of itself, and answering with something else would let
    /// the host's comparison succeed against a value that means nothing.
    #[test]
    fn the_target_firmware_hash_round_trips_and_the_running_one_is_not_invented() {
        let mut p = Protocol::new();
        let hash = [0x5Au8; eeprom::HASH_LEN];
        let mut sink = Frames::default();
        assert_eq!(
            p.handle(Command::SetFirmwareHash(&hash), &mut sink),
            Action::Persist
        );

        let mut target = Frames::default();
        p.handle(Command::QueryHash(hash_kind::TARGET_FIRMWARE), &mut target);
        let payload = target.payload(cmd::HASHES).expect("a hash frame");
        assert_eq!(payload[0], hash_kind::TARGET_FIRMWARE);
        assert_eq!(&payload[1..], &hash);
        assert_eq!(payload.len(), 33, "the host reads exactly 33 bytes");

        let mut running = Frames::default();
        p.handle(Command::QueryHash(hash_kind::FIRMWARE), &mut running);
        assert!(running.0.is_empty());
    }

    /// Before one is set, there is nothing to report, and reporting zeroes
    /// would be reporting a hash.
    #[test]
    fn an_unset_firmware_hash_is_not_reported_as_zero() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        p.handle(Command::QueryHash(hash_kind::TARGET_FIRMWARE), &mut sink);
        assert!(sink.0.is_empty());
    }

    /// A reset is an action for the caller, not something the protocol does,
    /// and it is asked for rather than assumed — the decoder has already
    /// checked the guard byte, and a frame without it never gets here.
    #[test]
    fn a_reset_is_handed_to_the_caller() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        assert_eq!(p.handle(Command::Reset, &mut sink), Action::Reset);
        assert!(sink.0.is_empty());
        assert_eq!(
            p.handle(Command::Malformed(cmd::RESET), &mut sink),
            Action::None
        );
    }

    /// Commands for hardware this board does not have are silent, like every
    /// other command it cannot answer. A device that replied to a WiFi query
    /// would be claiming to have WiFi.
    #[test]
    fn commands_for_absent_hardware_are_silent() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        for c in [cmd::WIFI_MODE, cmd::NP_INT, cmd::CFG_READ] {
            assert_eq!(p.handle(Command::NotApplicable(c), &mut sink), Action::None);
        }
        assert!(sink.0.is_empty());
    }

    /// Provisioning does not disturb the radio. `rnodeconf` writes 155 bytes
    /// into a device that a host may still have on air, and an EEPROM write
    /// that reconfigured the radio would take the link down mid-provisioning.
    #[test]
    fn writing_the_eeprom_does_not_touch_the_radio() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        p.handle(Command::SetRadioState(RadioState::On), &mut sink);
        assert!(p.radio_is_on());
        let before = *p.config();

        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        for &b in &provisioning_writes() {
            if d.feed(b) == kiss::Step::Frame {
                let (c, payload) = (d.command(), d.payload().to_vec());
                let action = p.handle(decode(c, &payload), &mut sink);
                assert_eq!(action, Action::Persist);
            }
        }
        assert!(p.radio_is_on());
        assert_eq!(p.config(), &before);
    }

    // ---- changes from the panel --------------------------------------------

    /// A panel edit with the radio on reprograms it, and what a host would
    /// read back afterwards is the new value.
    #[test]
    fn a_panel_edit_reaches_the_radio_and_the_readback() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetRadioState(RadioState::On), &mut out);
        let change = p.set_from_panel(Setting::SpreadingFactor(10));
        assert_eq!(change.radio, Action::Reconfigure);
        assert!(!change.persist, "host mode: nothing stored");
        assert!(p.radio_is_on());
        assert_eq!(p.config().spreading_factor, 10);
        let mut after = Frames::default();
        p.report_all(&mut after);
        assert_eq!(after.payload(cmd::SF), Some(&[10u8][..]));
    }

    /// With the radio off a panel edit lands and does nothing else, exactly
    /// as a host's setter does before `initRadio` finishes.
    #[test]
    fn a_panel_edit_with_the_radio_off_only_lands() {
        let mut p = Protocol::new();
        let change = p.set_from_panel(Setting::Frequency(916_000_000));
        assert_eq!(change, PanelChange::NONE);
        assert_eq!(p.config().frequency_hz, 916_000_000);
        assert!(!p.radio_is_on());
    }

    /// **Validated identically to the host path, refused not clamped.** A
    /// panel edit the radio cannot do leaves it off with the same reason a
    /// host would have left in `last_error`, and the value is kept as asked.
    #[test]
    fn a_panel_edit_is_refused_the_way_a_hosts_is() {
        let mut host = Protocol::new();
        let mut out = Frames::default();
        host.handle(Command::SetRadioState(RadioState::On), &mut out);
        host.handle(Command::SetTxPower(22), &mut out);
        let host_action = host.handle(Command::SetRadioState(RadioState::On), &mut out);

        let mut panel = Protocol::new();
        panel.handle(Command::SetRadioState(RadioState::On), &mut out);
        let change = panel.set_from_panel(Setting::TxPower(22));

        assert_eq!(change.radio, host_action);
        assert_eq!(change.radio, Action::Standby);
        assert_eq!(panel.last_error(), host.last_error());
        assert_eq!(
            panel.last_error(),
            Some(ConfigError::PowerAboveModuleRating)
        );
        assert_eq!(panel.radio_is_on(), host.radio_is_on());
        assert!(!panel.radio_is_on());
        assert_eq!(panel.config().tx_power_dbm, 22, "kept, not clamped");
    }

    /// A host's radio lock holds against the panel too.
    #[test]
    fn a_panel_edit_respects_the_hosts_lock() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetRadioLock(true), &mut out);
        let before = *p.config();
        assert_eq!(p.set_from_panel(Setting::CodingRate(8)), PanelChange::NONE);
        assert_eq!(p.config(), &before);
    }

    /// **Settings that should persist do.** In TNC mode a panel edit goes
    /// into the stored configuration, and the next boot comes up on it.
    #[test]
    fn in_tnc_mode_a_panel_edit_survives_a_reboot() {
        let mut p = Protocol::new();
        let mut out = Frames::default();
        p.handle(Command::SetFrequency(915_000_000), &mut out);
        p.handle(Command::SaveConfig, &mut out);
        assert!(p.is_tnc());
        p.resume_stored_config();
        assert!(p.radio_is_on());

        let change = p.set_from_panel(Setting::Frequency(916_250_000));
        assert!(change.persist, "TNC mode: the boot configuration changed");
        assert_eq!(change.radio, Action::Reconfigure);

        // The record is what survives.
        let restored = DeviceStore::decode(&p.store().encode()).unwrap();
        let mut next = Protocol::with_storage(restored, 0);
        assert_eq!(next.resume_stored_config(), Action::Reconfigure);
        assert_eq!(next.config().frequency_hz, 916_250_000);
    }

    /// ...and under host control it does not, because the host owns that
    /// configuration and sets it again on every connect.
    #[test]
    fn under_host_control_a_panel_edit_is_not_stored() {
        let mut p = Protocol::new();
        let change = p.set_from_panel(Setting::Frequency(916_250_000));
        assert!(!change.persist);
        assert!(!p.is_tnc());
        assert_eq!(p.rom().stored_config(), None);
        let restored = DeviceStore::decode(&p.store().encode()).unwrap();
        let next = Protocol::with_storage(restored, 0);
        assert_eq!(next.config().frequency_hz, DEFAULT.frequency_hz);
    }

    /// `Save Config` from the panel is `CMD_CONF_SAVE`: the board is a TNC
    /// from then on, on the live configuration.
    #[test]
    fn saving_from_the_panel_enters_tnc_mode() {
        let mut p = Protocol::new();
        p.set_from_panel(Setting::SpreadingFactor(11));
        let change = p.save_from_panel();
        assert!(change.persist);
        assert_eq!(change.radio, Action::None);
        assert!(p.is_tnc());
        assert_eq!(p.rom().stored_config().unwrap().spreading_factor, 11);
    }

    /// The toggle goes both ways, and refuses the way a host's `on` does.
    #[test]
    fn the_panel_toggle_switches_the_radio_and_refuses_what_it_cannot_do() {
        let mut p = Protocol::new();
        assert_eq!(p.toggle_from_panel(), Action::Reconfigure);
        assert!(p.radio_is_on());
        assert_eq!(p.toggle_from_panel(), Action::Standby);
        assert!(!p.radio_is_on());

        p.set_from_panel(Setting::Bandwidth(100_000));
        assert_eq!(p.toggle_from_panel(), Action::Standby);
        assert!(!p.radio_is_on());
        assert_eq!(p.last_error(), Some(ConfigError::UnsupportedBandwidth));
    }

    /// `Reset Config` is what the board boots with: the stored configuration
    /// in TNC mode, on; the default otherwise, in whatever state it was.
    #[test]
    fn resetting_from_the_panel_returns_to_the_boot_configuration() {
        // TNC mode.
        let mut tnc = Protocol::new();
        let mut out = Frames::default();
        tnc.handle(Command::SetSpreadingFactor(9), &mut out);
        tnc.handle(Command::SaveConfig, &mut out);
        tnc.handle(Command::SetSpreadingFactor(12), &mut out);
        assert_eq!(tnc.reset_from_panel(), Action::Reconfigure);
        assert_eq!(tnc.config().spreading_factor, 9);
        assert!(tnc.radio_is_on());

        // Host mode, radio off: the default, still off.
        let mut off = Protocol::new();
        off.set_from_panel(Setting::TxPower(2));
        assert_eq!(off.reset_from_panel(), Action::None);
        assert_eq!(off.config(), &DEFAULT);
        assert!(!off.radio_is_on());

        // Host mode, radio on: the default, reprogrammed.
        let mut on = Protocol::new();
        on.toggle_from_panel();
        on.set_from_panel(Setting::TxPower(2));
        assert_eq!(on.reset_from_panel(), Action::Reconfigure);
        assert_eq!(on.config(), &DEFAULT);
    }

    // ---- the display -----------------------------------------------------

    /// The whole `display_image` sequence the host writes: enable, then
    /// sixty-four rows. It must not repaint per row -- the host sends them in
    /// a burst, and a full panel flush each time would be fourteen seconds of
    /// bus traffic to show one picture.
    #[test]
    fn pushing_a_picture_does_not_repaint_per_row() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();

        assert_eq!(
            p.handle(Command::ShowExternalFramebuffer(true), &mut sink),
            Action::Redraw,
            "switching the source is worth a repaint"
        );
        assert!(p.external().enabled());

        for line in 0..64u8 {
            let row = [line; 8];
            assert_eq!(
                p.handle(Command::WriteFramebuffer { line, data: &row }, &mut sink),
                Action::None,
                "row {line} asked for a repaint"
            );
        }
        assert!(sink.0.is_empty(), "rows are not acknowledged");

        // And the picture is all there.
        let mut read = Frames::default();
        p.handle(Command::ReadFramebuffer, &mut read);
        let bytes = read.payload(cmd::FB_READ).expect("a framebuffer");
        assert_eq!(bytes.len(), 512);
        for line in 0..64usize {
            assert_eq!(&bytes[line * 8..line * 8 + 8], &[line as u8; 8]);
        }
    }

    /// `CMD_DISP_READ` is handed back to the caller, because the pixels it
    /// wants are the ones on the panel and those live with the display driver.
    /// Answering it from here would mean answering with the protocol's idea of
    /// the screen rather than the screen.
    #[test]
    fn reading_the_display_is_handed_to_the_caller() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        assert_eq!(
            p.handle(Command::ReadDisplay, &mut sink),
            Action::ReportDisplay
        );
        assert!(sink.0.is_empty());
    }

    /// A 512-byte framebuffer read has to survive the framing, and it will
    /// contain `0xC0` on any real picture.
    #[test]
    fn a_framebuffer_read_survives_the_wire() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        for line in 0..64u8 {
            // Every byte value appears across the picture, framing bytes and
            // all.
            let row = [
                line.wrapping_mul(4),
                kiss::FEND,
                kiss::FESC,
                line,
                0x00,
                0xFF,
                line ^ 0x5A,
                line.wrapping_add(1),
            ];
            p.handle(Command::WriteFramebuffer { line, data: &row }, &mut sink);
        }

        let mut wire = Wire::default();
        p.handle(Command::ReadFramebuffer, &mut wire);

        // A 1024-byte decoder, not the HW_MTU one `read_back` uses: this frame
        // is 512 bytes of payload, which is larger than any *packet* and so
        // larger than the buffer the firmware decodes inbound frames with.
        // The host's own reader allows 1024, which is the number that matters.
        let mut d = kiss::Decoder::<1024>::new();
        let mut frames: Vec<(u8, Vec<u8>)> = Vec::new();
        for &b in &wire.0 {
            if d.feed(b) == kiss::Step::Frame {
                frames.push((d.command(), d.payload().to_vec()));
            }
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, cmd::FB_READ);
        assert_eq!(frames[0].1.len(), 512, "the host counts to exactly 512");
        assert_eq!(&frames[0].1, p.external().as_bytes());
    }

    /// Contrast is stored and asks for a repaint. It is the one display
    /// setting a host can change on this board.
    #[test]
    fn the_display_intensity_is_settable() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        assert_eq!(p.display_intensity(), 0x80, "the panel's own default");
        assert_eq!(
            p.handle(Command::SetDisplayIntensity(0x20), &mut sink),
            Action::Redraw
        );
        assert_eq!(p.display_intensity(), 0x20);
        assert!(sink.0.is_empty());
    }

    /// Turning the external framebuffer off puts the device's own page back.
    #[test]
    fn the_external_framebuffer_can_be_switched_off_again() {
        let mut p = Protocol::new();
        let mut sink = Frames::default();
        p.handle(Command::ShowExternalFramebuffer(true), &mut sink);
        assert!(p.external().enabled());
        assert_eq!(
            p.handle(Command::ShowExternalFramebuffer(false), &mut sink),
            Action::Redraw
        );
        assert!(!p.external().enabled());
    }
}
