//! The RNode command set: the bytes, their payload shapes, and — the part that
//! is easy to get wrong — which of them the host un-escapes.
//!
//! Every constant here is pinned by an observable behaviour of Reticulum's
//! `RNodeInterface` (RNS 1.5.0), and the doc comment says which. See
//! [`super`] for why the host and not the firmware is the source.

use super::eeprom;

/// Command bytes, as `RNodeInterface.KISS` and `rnodeconf.KISS` define them.
pub mod cmd {
    /// A packet: outbound from the host, or inbound from the radio.
    pub const DATA: u8 = 0x00;
    /// Centre frequency, four bytes big-endian.
    pub const FREQUENCY: u8 = 0x01;
    /// Bandwidth in hertz, four bytes big-endian.
    pub const BANDWIDTH: u8 = 0x02;
    /// Transmit power in dBm, one byte.
    pub const TXPOWER: u8 = 0x03;
    /// Spreading factor, one byte.
    pub const SF: u8 = 0x04;
    /// Coding rate as the denominator of 4/n, one byte.
    pub const CR: u8 = 0x05;
    /// Radio on or off, one byte. See [`super::RadioState`].
    pub const RADIO_STATE: u8 = 0x06;
    /// Configuration lock, one byte.
    pub const RADIO_LOCK: u8 = 0x07;
    /// Hardware detection. See [`super::DETECT_REQ`].
    pub const DETECT: u8 = 0x08;
    /// The host is closing the interface.
    pub const LEAVE: u8 = 0x0A;
    /// Short-term airtime limit, two bytes, in hundredths of a percent.
    pub const ST_ALOCK: u8 = 0x0B;
    /// Long-term airtime limit, two bytes, in hundredths of a percent.
    pub const LT_ALOCK: u8 = 0x0C;
    /// Flow control: the device is ready for another packet.
    pub const READY: u8 = 0x0F;
    /// Packets received since boot, four bytes.
    pub const STAT_RX: u8 = 0x21;
    /// Packets transmitted since boot, four bytes.
    pub const STAT_TX: u8 = 0x22;
    /// RSSI of the last packet, one byte, offset by [`super::RSSI_OFFSET`].
    pub const STAT_RSSI: u8 = 0x23;
    /// SNR of the last packet, one signed byte in quarter-dB.
    pub const STAT_SNR: u8 = 0x24;
    /// Channel and airtime statistics, eleven bytes.
    pub const STAT_CHTM: u8 = 0x25;
    /// Physical-layer parameters, twelve bytes.
    pub const STAT_PHYPRM: u8 = 0x26;
    /// Battery state.
    pub const STAT_BAT: u8 = 0x27;
    /// Blink the indicator.
    pub const BLINK: u8 = 0x30;
    /// Display intensity. Phase 7.
    pub const DISP_INT: u8 = 0x45;
    /// Random byte.
    pub const RANDOM: u8 = 0x40;
    /// Framebuffer extents. Phase 7.
    pub const FB_EXT: u8 = 0x41;
    /// Read the framebuffer. Phase 7.
    pub const FB_READ: u8 = 0x42;
    /// Write the framebuffer. Phase 7.
    pub const FB_WRITE: u8 = 0x43;
    /// Bluetooth control. Phase 8.
    pub const BT_CTRL: u8 = 0x46;
    /// Which board this is. See [`super::super::eeprom::BOARD_HMBRW`].
    pub const BOARD: u8 = 0x47;
    /// Which platform this is. See [`super::PLATFORM_NRF52`].
    pub const PLATFORM: u8 = 0x48;
    /// Which microcontroller this is. See [`super::MCU_NRF52`].
    pub const MCU: u8 = 0x49;
    /// Firmware version, two bytes: major then minor.
    pub const FW_VERSION: u8 = 0x50;
    /// Read the device's EEPROM, all of it, in one frame.
    pub const ROM_READ: u8 = 0x51;
    /// Write one EEPROM byte: `[address, value]`.
    pub const ROM_WRITE: u8 = 0x52;
    /// Store the current radio configuration and enter TNC mode.
    pub const CONF_SAVE: u8 = 0x53;
    /// Forget the stored radio configuration.
    pub const CONF_DELETE: u8 = 0x54;
    /// Reset the device. Guarded by [`super::CONFIRM_BYTE`].
    pub const RESET: u8 = 0x55;
    /// Report the device hash. See [`super::super::eeprom::device_hash`].
    pub const DEV_HASH: u8 = 0x56;
    /// Store a device signature over that hash, 64 bytes.
    pub const DEV_SIG: u8 = 0x57;
    /// Store the expected firmware hash, 32 bytes.
    pub const FW_HASH: u8 = 0x58;
    /// Erase the EEPROM. Guarded by [`super::CONFIRM_BYTE`].
    pub const ROM_WIPE: u8 = 0x59;
    /// Report a stored hash. See [`super::hash_kind`].
    pub const HASHES: u8 = 0x60;
    /// A firmware update is about to happen.
    pub const FW_UPD: u8 = 0x61;
    /// Bluetooth pairing PIN, four bytes. Phase 8.
    pub const BT_PIN: u8 = 0x62;
    /// Display address, blanking, rotation, reconditioning. Phase 7.
    pub const DISP_ADR: u8 = 0x63;
    /// See [`DISP_ADR`].
    pub const DISP_BLNK: u8 = 0x64;
    /// See [`DISP_ADR`].
    pub const DISP_ROT: u8 = 0x67;
    /// See [`DISP_ADR`].
    pub const DISP_RCND: u8 = 0x68;
    /// Read the display. Phase 7.
    pub const DISP_READ: u8 = 0x66;
    /// Neopixel intensity. This board has no neopixel.
    pub const NP_INT: u8 = 0x65;
    /// Disable interference avoidance.
    pub const DIS_IA: u8 = 0x69;
    /// WiFi mode, SSID, key, channel, address, netmask, and the sector they
    /// are stored in. This board has no WiFi.
    pub const WIFI_MODE: u8 = 0x6A;
    /// See [`WIFI_MODE`].
    pub const WIFI_SSID: u8 = 0x6B;
    /// See [`WIFI_MODE`].
    pub const WIFI_PSK: u8 = 0x6C;
    /// See [`WIFI_MODE`].
    pub const CFG_READ: u8 = 0x6D;
    /// See [`WIFI_MODE`].
    pub const WIFI_CHN: u8 = 0x6E;
    /// See [`WIFI_MODE`].
    pub const WIFI_IP: u8 = 0x84;
    /// See [`WIFI_MODE`].
    pub const WIFI_NM: u8 = 0x85;
    /// A hardware error. See [`super::error`].
    pub const ERROR: u8 = 0x90;
}

/// What the host sends in a [`cmd::DETECT`] frame to ask "are you an RNode?".
pub const DETECT_REQ: u8 = 0x73;
/// What it must get back. Any other answer, or none, and `configure_device`
/// closes the port.
pub const DETECT_RESP: u8 = 0x46;

/// The argument `rnodeconf` sends with the two commands that destroy
/// something: [`cmd::ROM_WIPE`] and [`cmd::RESET`].
///
/// Checked rather than assumed, for the same reason [`DETECT_REQ`] is. These
/// are the only two commands on the link that cannot be undone, and the port
/// is open to whatever anyone sends down it — a terminal, a probe from an
/// unrelated tool, a stray byte after a lost frame. One guard byte does not
/// make that safe, but it does mean a wipe has to be asked for.
pub const CONFIRM_BYTE: u8 = 0xF8;

/// Which stored hash a [`cmd::HASHES`] request is asking for.
pub mod hash_kind {
    /// The hash the firmware is *expected* to have, as set by [`super::cmd::FW_HASH`].
    pub const TARGET_FIRMWARE: u8 = 0x01;
    /// The hash of the firmware actually running.
    pub const FIRMWARE: u8 = 0x02;
}

/// `KISS.PLATFORM_NRF52`. The host uses this to decide the device has a
/// display, which is why phase 7 is a consequence of answering honestly here.
pub const PLATFORM_NRF52: u8 = 0x70;
/// `ROM.MCU_NRF52`, from `rnodeconf`.
pub const MCU_NRF52: u8 = 0x71;

/// Added to a reported RSSI byte to recover dBm: the host computes
/// `byte - RSSI_OFFSET`, so the device sends `dBm + RSSI_OFFSET`.
pub const RSSI_OFFSET: i16 = 157;

/// The firmware version oxinode reports.
///
/// **This is a gate, not a label.** `validate_firmware` calls `RNS.panic()` —
/// it terminates the host process — unless the reported version is at least
/// `REQUIRED_FW_VER_MAJ`.`REQUIRED_FW_VER_MIN`, which in RNS 1.5.0 is 1.52.
/// Reporting oxinode's own version number here would take down any host that
/// connected to it.
///
/// So this is a statement about protocol compatibility rather than about
/// oxinode's release: it says "I speak what a 1.52 RNode speaks". oxinode's own
/// version is reported through the board and ROM commands in phase 6.
pub const FW_VERSION_MAJOR: u8 = 1;
/// See [`FW_VERSION_MAJOR`].
pub const FW_VERSION_MINOR: u8 = 52;

/// The minimum the host accepts, recorded so the assertion below can check it.
pub const REQUIRED_FW_VERSION: (u8, u8) = (1, 52);

/// Error codes the host understands in a [`cmd::ERROR`] frame.
///
/// The first two make the host raise `IOError` and drop the interface; the
/// others it records and carries on. Choosing the wrong one turns a recoverable
/// condition into a dropped link.
pub mod error {
    /// The radio would not initialise. Host drops the interface.
    pub const INITRADIO: u8 = 0x01;
    /// A transmission failed. Host drops the interface.
    pub const TXFAILED: u8 = 0x02;
    /// The EEPROM is locked. Phase 6.
    pub const EEPROM_LOCKED: u8 = 0x03;
    /// The outbound queue is full. Host records it and continues.
    pub const QUEUE_FULL: u8 = 0x04;
    /// Out of memory. Host records it and continues.
    pub const MEMORY_LOW: u8 = 0x05;
    /// The modem stopped answering. Host records it and continues.
    pub const MODEM_TIMEOUT: u8 = 0x06;
}

/// Whether the **host** un-escapes this command's payload when it reads one.
///
/// This is the sharpest edge in the protocol. Reticulum's parser has a branch
/// per command: the multi-byte fields accumulate through an unescaping step,
/// and the single-byte fields are read straight out of the stream. So a device
/// that escapes uniformly corrupts every single-byte field whose value happens
/// to be `0xC0` or `0xDB` — and since the host's parser lets *every* byte of
/// such a frame overwrite the value, what it ends up with is the second byte of
/// the escape sequence.
///
/// There are **two** hosts, and they read different command sets: `rnsd` runs
/// `RNodeInterface`, and `rnodeconf` has a parser of its own. Both are
/// transcribed here, branch by branch, and the table below is their union. The
/// two agree on every command they both parse — which is a fact worth having a
/// test for rather than an assumption, since nothing forces them to.
///
/// It comes out as a clean rule — multi-byte yes, single-byte no — but it is
/// recorded as a table rather than as a rule, because it is a fact about
/// someone else's code and the next version of it does not have to stay tidy.
pub const fn host_unescapes(command: u8) -> bool {
    matches!(
        command,
        cmd::DATA
            | cmd::FREQUENCY
            | cmd::BANDWIDTH
            | cmd::FW_VERSION
            | cmd::STAT_RX
            | cmd::STAT_TX
            | cmd::ST_ALOCK
            | cmd::LT_ALOCK
            | cmd::STAT_CHTM
            | cmd::STAT_PHYPRM
            | cmd::STAT_BAT
            | cmd::FB_READ
            | cmd::DISP_READ
            // Read by rnodeconf only, and all multi-byte. ROM_READ is the
            // whole EEPROM in one frame, and it certainly contains 0xC0: a
            // 128-byte RSA signature is uniformly distributed bytes.
            | cmd::ROM_READ
            | cmd::CFG_READ
            | cmd::DEV_HASH
            | cmd::HASHES
            | cmd::BT_PIN
    )
}

/// Frame a response, escaping it or not according to [`host_unescapes`].
///
/// The single place that decision is made. Every response goes through here,
/// so "did we escape this one correctly?" is not a question that can be asked
/// per call site, and cannot be answered wrongly at one of them.
pub fn encode_response(command: u8, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    super::kiss::encode(command, payload, host_unescapes(command), out)
}

/// The length [`encode_response`] will produce.
pub fn response_len(command: u8, payload: &[u8]) -> usize {
    super::kiss::encoded_len(payload, host_unescapes(command))
}

/// Radio power state, as [`cmd::RADIO_STATE`] carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioState {
    /// `KISS.RADIO_STATE_OFF`.
    Off,
    /// `KISS.RADIO_STATE_ON`.
    On,
    /// `KISS.RADIO_STATE_ASK` — report the current state, do not change it.
    Ask,
}

impl RadioState {
    /// Decode the byte the host sends.
    pub const fn decode(byte: u8) -> Option<Self> {
        match byte {
            0x00 => Some(Self::Off),
            0x01 => Some(Self::On),
            0xFF => Some(Self::Ask),
            _ => None,
        }
    }

    /// The byte to report. [`RadioState::Ask`] is a question and never an
    /// answer, so it has none.
    pub const fn encode(self) -> Option<u8> {
        match self {
            Self::Off => Some(0x00),
            Self::On => Some(0x01),
            Self::Ask => None,
        }
    }
}

/// A command from the host, decoded.
///
/// Borrowed rather than owned: a data frame is up to [`super::kiss::HW_MTU`] bytes and
/// this is a `no_std` crate with no allocator, so the payload stays in the
/// decoder's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    /// Transmit this.
    Data(&'a [u8]),
    /// Set the centre frequency, in hertz.
    SetFrequency(u32),
    /// Set the bandwidth, in hertz.
    SetBandwidth(u32),
    /// Set the transmit power, in dBm.
    SetTxPower(i8),
    /// Set the spreading factor.
    SetSpreadingFactor(u8),
    /// Set the coding rate, as the denominator of 4/n.
    SetCodingRate(u8),
    /// Turn the radio on or off, or ask what it is doing.
    SetRadioState(RadioState),
    /// Lock or unlock the configuration.
    SetRadioLock(bool),
    /// Set the short-term airtime limit, in hundredths of a percent.
    SetShortAirtimeLimit(u16),
    /// Set the long-term airtime limit, in hundredths of a percent.
    SetLongAirtimeLimit(u16),
    /// "Are you an RNode?"
    Detect,
    /// The host is closing the interface.
    Leave,
    /// Report the firmware version.
    QueryFirmwareVersion,
    /// Report the platform.
    QueryPlatform,
    /// Report the microcontroller.
    QueryMcu,
    /// Report which board this is.
    QueryBoard,

    /// Hand back the whole EEPROM image.
    ReadRom,
    /// Write one EEPROM byte.
    WriteRom { addr: u8, value: u8 },
    /// Erase the EEPROM.
    WipeRom,
    /// Store the current radio configuration and enter TNC mode.
    SaveConfig,
    /// Forget the stored radio configuration and go back to host control.
    DeleteConfig,
    /// Restart the device.
    Reset,

    /// Report the device hash.
    QueryDeviceHash,
    /// Store a signature over that hash, made on the host.
    StoreDeviceSignature(&'a [u8]),
    /// Store the hash the running firmware is expected to have.
    SetFirmwareHash(&'a [u8]),
    /// Report a stored hash. See [`hash_kind`].
    QueryHash(u8),
    /// A firmware update is about to be flashed.
    FirmwareUpdateImminent,

    /// Show the host's picture instead of the device's own page, or stop.
    ShowExternalFramebuffer(bool),
    /// One row of that picture: `[line, 8 bytes]`.
    WriteFramebuffer { line: u8, data: &'a [u8] },
    /// Hand the stored picture back.
    ReadFramebuffer,
    /// Hand back what is actually on the screen.
    ReadDisplay,
    /// Set the display's contrast.
    SetDisplayIntensity(u8),

    /// A command for hardware this board does not have — WiFi, a neopixel.
    /// Distinct from [`Command::NotYetImplemented`] because there is no phase
    /// in which it becomes implemented; the answer is "not on this board".
    NotApplicable(u8),
    /// A command that is understood but not implemented in this phase — the
    /// display and Bluetooth ones, mostly. Kept distinct from [`Command::Unknown`]
    /// so a log can say "phase 7" rather than "no idea".
    NotYetImplemented(u8),
    /// A command byte this firmware does not know.
    Unknown(u8),
    /// A known command whose payload was the wrong length or an impossible
    /// value. Distinct from [`Command::Unknown`] because it means the host and
    /// this firmware disagree about a command they both claim to know, which is
    /// a much more interesting thing to see in a log.
    Malformed(u8),
}

/// Read four bytes, big-endian.
const fn be32(payload: &[u8]) -> Option<u32> {
    if payload.len() < 4 {
        return None;
    }
    Some(
        (payload[0] as u32) << 24
            | (payload[1] as u32) << 16
            | (payload[2] as u32) << 8
            | payload[3] as u32,
    )
}

/// Read two bytes, big-endian.
const fn be16(payload: &[u8]) -> Option<u16> {
    if payload.len() < 2 {
        return None;
    }
    Some((payload[0] as u16) << 8 | payload[1] as u16)
}

/// Decode one frame.
///
/// The length checks are `<` rather than `!=` on purpose. The host's own writer
/// sends exactly the documented widths, but its *reader* acts on a field the
/// moment enough bytes have arrived and ignores the rest — so being strict
/// about trailing bytes would be stricter than the protocol's other end, and
/// would reject frames a conforming host might send.
pub fn decode<'a>(command: u8, payload: &'a [u8]) -> Command<'a> {
    match command {
        cmd::DATA => Command::Data(payload),
        cmd::FREQUENCY => match be32(payload) {
            Some(hz) => Command::SetFrequency(hz),
            None => Command::Malformed(command),
        },
        cmd::BANDWIDTH => match be32(payload) {
            Some(hz) => Command::SetBandwidth(hz),
            None => Command::Malformed(command),
        },
        // The host writes `bytes([self.txpower])`, so the value is a u8 on the
        // wire. Read as signed here because the LR1121's low-power PA goes down
        // to -17 dBm and the config layer speaks dBm, not bit patterns.
        cmd::TXPOWER => match payload.first() {
            Some(&b) => Command::SetTxPower(b as i8),
            None => Command::Malformed(command),
        },
        cmd::SF => match payload.first() {
            Some(&b) => Command::SetSpreadingFactor(b),
            None => Command::Malformed(command),
        },
        cmd::CR => match payload.first() {
            Some(&b) => Command::SetCodingRate(b),
            None => Command::Malformed(command),
        },
        cmd::RADIO_STATE => match payload.first().copied().and_then(RadioState::decode) {
            Some(state) => Command::SetRadioState(state),
            None => Command::Malformed(command),
        },
        cmd::RADIO_LOCK => match payload.first() {
            Some(&b) => Command::SetRadioLock(b != 0),
            None => Command::Malformed(command),
        },
        cmd::ST_ALOCK => match be16(payload) {
            Some(v) => Command::SetShortAirtimeLimit(v),
            None => Command::Malformed(command),
        },
        cmd::LT_ALOCK => match be16(payload) {
            Some(v) => Command::SetLongAirtimeLimit(v),
            None => Command::Malformed(command),
        },
        // The request byte is checked rather than assumed. A DETECT frame
        // carrying anything else is not this handshake, and answering it would
        // be claiming to be an RNode to something that did not ask.
        cmd::DETECT => match payload.first() {
            Some(&DETECT_REQ) => Command::Detect,
            _ => Command::Malformed(command),
        },
        cmd::LEAVE => Command::Leave,

        // The two commands that destroy something. Both carry a guard byte,
        // and both check it: see [`CONFIRM_BYTE`].
        cmd::ROM_WIPE => match payload.first() {
            Some(&CONFIRM_BYTE) => Command::WipeRom,
            _ => Command::Malformed(command),
        },
        cmd::RESET => match payload.first() {
            Some(&CONFIRM_BYTE) => Command::Reset,
            _ => Command::Malformed(command),
        },

        cmd::ROM_READ => Command::ReadRom,
        // `[address, value]`. The address is one byte and the image is 256, so
        // there is no address this cannot reach and no bounds check to get
        // wrong -- see `super::eeprom`.
        cmd::ROM_WRITE => match (payload.first(), payload.get(1)) {
            (Some(&addr), Some(&value)) => Command::WriteRom { addr, value },
            _ => Command::Malformed(command),
        },
        cmd::CONF_SAVE => Command::SaveConfig,
        cmd::CONF_DELETE => Command::DeleteConfig,

        cmd::DEV_HASH => Command::QueryDeviceHash,
        cmd::DEV_SIG => match payload.get(..eeprom::DEVICE_SIGNATURE_LEN) {
            Some(sig) => Command::StoreDeviceSignature(sig),
            None => Command::Malformed(command),
        },
        cmd::FW_HASH => match payload.get(..eeprom::HASH_LEN) {
            Some(hash) => Command::SetFirmwareHash(hash),
            None => Command::Malformed(command),
        },
        cmd::HASHES => match payload.first() {
            Some(&kind @ (hash_kind::TARGET_FIRMWARE | hash_kind::FIRMWARE)) => {
                Command::QueryHash(kind)
            }
            _ => Command::Malformed(command),
        },
        cmd::FW_UPD => Command::FirmwareUpdateImminent,
        // The host sends a 0x00 argument with each of these. It is a
        // placeholder, not a selector -- there is nothing else it can be --
        // so it is not checked.
        cmd::FW_VERSION => Command::QueryFirmwareVersion,
        cmd::PLATFORM => Command::QueryPlatform,
        cmd::MCU => Command::QueryMcu,
        cmd::BOARD => Command::QueryBoard,
        cmd::FB_EXT => match payload.first() {
            Some(&b) => Command::ShowExternalFramebuffer(b != 0),
            None => Command::Malformed(command),
        },
        // `[line, 8 bytes]`, and the eight are checked here rather than in the
        // buffer, so a short frame is reported as a disagreement about a
        // command both ends know rather than silently storing a part-row.
        cmd::FB_WRITE => match (payload.first(), payload.len()) {
            (Some(&line), len) if len > super::display::FB_BYTES_PER_LINE => {
                Command::WriteFramebuffer {
                    line,
                    data: &payload[1..1 + super::display::FB_BYTES_PER_LINE],
                }
            }
            _ => Command::Malformed(command),
        },
        cmd::FB_READ => Command::ReadFramebuffer,
        cmd::DISP_READ => Command::ReadDisplay,
        cmd::DISP_INT => match payload.first() {
            Some(&level) => Command::SetDisplayIntensity(level),
            None => Command::Malformed(command),
        },
        // Real features, not built yet: a blanking timeout, a rotation the
        // host can choose, a reconditioning sweep, and the identify blink.
        cmd::DISP_BLNK | cmd::DISP_ROT | cmd::DISP_RCND | cmd::BLINK => {
            Command::NotYetImplemented(command)
        }
        // The panel's address is *discovered* by scanning the bus at boot, so
        // a host setting it would be replacing a measurement with a guess.
        cmd::DISP_ADR => Command::NotApplicable(command),
        cmd::BT_CTRL | cmd::BT_PIN => Command::NotYetImplemented(command),
        // Hardware this board does not have, and will not grow.
        cmd::NP_INT
        | cmd::WIFI_MODE
        | cmd::WIFI_SSID
        | cmd::WIFI_PSK
        | cmd::WIFI_CHN
        | cmd::WIFI_IP
        | cmd::WIFI_NM
        | cmd::CFG_READ
        | cmd::DIS_IA => Command::NotApplicable(command),
        other => Command::Unknown(other),
    }
}

// The version reported has to clear the version required, or every host that
// connects calls RNS.panic() and dies. Checked when the crate compiles, because
// there is no failure here that a test would catch earlier than a user would.
const _: () = assert!(
    FW_VERSION_MAJOR > REQUIRED_FW_VERSION.0
        || (FW_VERSION_MAJOR == REQUIRED_FW_VERSION.0 && FW_VERSION_MINOR >= REQUIRED_FW_VERSION.1),
    "the reported firmware version is below what Reticulum requires; every host that connects would panic"
);
// The two halves of the detect handshake are different bytes. If they were ever
// made equal, a device echoing its input would pass detection.
const _: () = assert!(DETECT_REQ != DETECT_RESP);
// Single-byte fields must not be escaped and multi-byte ones must be. Spot
// checks on the two that matter most: DATA carries arbitrary bytes and would be
// corrupted without escaping, and STAT_SNR is signed so it reaches 0xDB at an
// ordinary -9.25 dB.
const _: () = assert!(host_unescapes(cmd::DATA));
const _: () = assert!(!host_unescapes(cmd::STAT_SNR));
const _: () = assert!(!host_unescapes(cmd::TXPOWER) && !host_unescapes(cmd::SF));
const _: () = assert!(host_unescapes(cmd::FREQUENCY) && host_unescapes(cmd::BANDWIDTH));

#[cfg(test)]
mod tests {
    use super::super::kiss;
    use super::*;

    /// The gate. Reticulum does not warn about an old firmware version — it
    /// calls `RNS.panic()`, which ends the host process.
    #[test]
    fn the_reported_version_clears_the_version_reticulum_demands() {
        assert_eq!(REQUIRED_FW_VERSION, (1, 52));
        assert!((FW_VERSION_MAJOR, FW_VERSION_MINOR) >= REQUIRED_FW_VERSION);
    }

    /// Transcribed from the host's parser, branch by branch, and checked
    /// against the whole command set rather than the ones we happen to send.
    /// The rule that falls out — multi-byte unescaped, single-byte raw — is
    /// stated here as an assertion so a future edit to the table has to break
    /// it deliberately.
    #[test]
    fn the_unescaping_table_matches_the_hosts_parser() {
        let unescaped = [
            cmd::DATA,
            cmd::FREQUENCY,
            cmd::BANDWIDTH,
            cmd::FW_VERSION,
            cmd::STAT_RX,
            cmd::STAT_TX,
            cmd::ST_ALOCK,
            cmd::LT_ALOCK,
            cmd::STAT_CHTM,
            cmd::STAT_PHYPRM,
            cmd::STAT_BAT,
            cmd::FB_READ,
            cmd::DISP_READ,
        ];
        let raw = [
            cmd::TXPOWER,
            cmd::SF,
            cmd::CR,
            cmd::RADIO_STATE,
            cmd::RADIO_LOCK,
            cmd::STAT_RSSI,
            cmd::STAT_SNR,
            cmd::RANDOM,
            cmd::PLATFORM,
            cmd::MCU,
            cmd::ERROR,
            cmd::RESET,
            cmd::READY,
            cmd::DETECT,
        ];
        for c in unescaped {
            assert!(host_unescapes(c), "{c:#04x} should be unescaped");
        }
        for c in raw {
            assert!(!host_unescapes(c), "{c:#04x} should be read raw");
        }
        // The two sets are disjoint and cover every command this firmware
        // sends, so nothing falls through the gap by being forgotten.
        for c in unescaped {
            assert!(!raw.contains(&c), "{c:#04x} is in both tables");
        }
    }

    /// The whole detect handshake, as bytes, decoded. This is the exact
    /// sequence `RNodeInterface.detect()` writes.
    #[test]
    fn the_detect_handshake_decodes_to_the_four_commands_it_is() {
        let wire = [
            kiss::FEND,
            cmd::DETECT,
            DETECT_REQ,
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
        ];
        // A decoded `Command` borrows the decoder's buffer, so it cannot be
        // held across the next `feed` -- which is the point of the borrow.
        // Collect the frames first, then decode them.
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        let mut frames: Vec<(u8, Vec<u8>)> = Vec::new();
        for &b in &wire {
            if d.feed(b) == kiss::Step::Frame {
                frames.push((d.command(), d.payload().to_vec()));
            }
        }
        let got: Vec<Command> = frames.iter().map(|(c, p)| decode(*c, p)).collect();
        assert_eq!(
            got,
            vec![
                Command::Detect,
                Command::QueryFirmwareVersion,
                Command::QueryPlatform,
                Command::QueryMcu,
            ]
        );
    }

    /// The five setters `initRadio` sends, with realistic values, in order.
    #[test]
    fn the_configuration_setters_decode_to_their_values() {
        assert_eq!(
            decode(cmd::FREQUENCY, &[0x36, 0x89, 0xCA, 0xC0]),
            Command::SetFrequency(915_000_000)
        );
        assert_eq!(
            decode(cmd::BANDWIDTH, &[0x00, 0x01, 0xE8, 0x48]),
            Command::SetBandwidth(125_000)
        );
        assert_eq!(decode(cmd::TXPOWER, &[14]), Command::SetTxPower(14));
        assert_eq!(decode(cmd::SF, &[8]), Command::SetSpreadingFactor(8));
        assert_eq!(decode(cmd::CR, &[5]), Command::SetCodingRate(5));
        assert_eq!(
            decode(cmd::RADIO_STATE, &[0x01]),
            Command::SetRadioState(RadioState::On)
        );
    }

    /// 915 MHz big-endian happens to contain `0xC0` — the frame delimiter — in
    /// its low byte. So the very first thing a US host configures is a frame
    /// that must be escaped, and a firmware that got the escaping wrong would
    /// fail on its first real conversation rather than on some rare packet.
    #[test]
    fn the_default_us_frequency_contains_the_frame_delimiter() {
        let hz: u32 = 915_000_000;
        let bytes = hz.to_be_bytes();
        assert_eq!(bytes, [0x36, 0x89, 0xCA, 0xC0]);
        assert!(bytes.contains(&kiss::FEND));
        // Round-trip it through the framing to be sure.
        let mut buf = [0u8; 16];
        let n = kiss::encode(cmd::FREQUENCY, &bytes, true, &mut buf).unwrap();
        let mut d = kiss::Decoder::<8>::new();
        let mut frame = None;
        for &b in &buf[..n] {
            if d.feed(b) == kiss::Step::Frame {
                frame = Some((d.command(), d.payload().to_vec()));
            }
        }
        let (command, payload) = frame.expect("one frame");
        assert_eq!(decode(command, &payload), Command::SetFrequency(hz));
    }

    /// Transmit power is signed, because the low-power PA reaches -17 dBm and
    /// the host puts the byte on the wire unsigned. Reading it unsigned would
    /// turn -17 dBm into 239 dBm, which the config layer would then refuse for
    /// the wrong reason.
    #[test]
    fn a_negative_transmit_power_survives_the_wire() {
        assert_eq!(decode(cmd::TXPOWER, &[0xEF]), Command::SetTxPower(-17));
        assert_eq!(decode(cmd::TXPOWER, &[0x00]), Command::SetTxPower(0));
        assert_eq!(decode(cmd::TXPOWER, &[0x14]), Command::SetTxPower(20));
    }

    /// A short payload is a disagreement between two implementations that both
    /// claim to know the command, which is worth naming separately from a
    /// command nobody knows.
    #[test]
    fn a_truncated_payload_is_malformed_and_not_unknown() {
        for (command, short) in [
            (cmd::FREQUENCY, &[0x36u8, 0x89, 0xCA][..]),
            (cmd::BANDWIDTH, &[][..]),
            (cmd::TXPOWER, &[][..]),
            (cmd::SF, &[][..]),
            (cmd::CR, &[][..]),
            (cmd::ST_ALOCK, &[0x01][..]),
            (cmd::LT_ALOCK, &[][..]),
        ] {
            assert_eq!(
                decode(command, short),
                Command::Malformed(command),
                "{command:#04x}"
            );
        }
        assert_eq!(decode(0x7E, &[]), Command::Unknown(0x7E));
    }

    /// Trailing bytes are accepted, because the host's own reader acts as soon
    /// as it has enough and ignores the rest. Being stricter than the other end
    /// would reject frames that work everywhere else.
    #[test]
    fn extra_trailing_bytes_do_not_invalidate_a_command() {
        assert_eq!(
            decode(cmd::FREQUENCY, &[0x36, 0x89, 0xCA, 0xC0, 0xFF, 0xFF]),
            Command::SetFrequency(915_000_000)
        );
        assert_eq!(decode(cmd::SF, &[7, 9, 11]), Command::SetSpreadingFactor(7));
    }

    /// A DETECT frame that does not carry the request byte is not the
    /// handshake. Answering it would be announcing "I am an RNode" to something
    /// that never asked.
    #[test]
    fn only_the_real_detect_request_is_answered() {
        assert_eq!(decode(cmd::DETECT, &[DETECT_REQ]), Command::Detect);
        for wrong in [0x00u8, 0x46, 0x72, 0x74, 0xFF] {
            assert_eq!(
                decode(cmd::DETECT, &[wrong]),
                Command::Malformed(cmd::DETECT),
                "{wrong:#04x}"
            );
        }
        assert_eq!(decode(cmd::DETECT, &[]), Command::Malformed(cmd::DETECT));
    }

    /// The three radio states, and the fact that "ask" is a question with no
    /// encoding of its own. A device that echoed 0xFF back would be telling the
    /// host its radio was in state 255.
    #[test]
    fn the_radio_state_round_trips_except_for_the_question() {
        assert_eq!(RadioState::decode(0x00), Some(RadioState::Off));
        assert_eq!(RadioState::decode(0x01), Some(RadioState::On));
        assert_eq!(RadioState::decode(0xFF), Some(RadioState::Ask));
        assert_eq!(RadioState::decode(0x02), None);
        assert_eq!(RadioState::Off.encode(), Some(0x00));
        assert_eq!(RadioState::On.encode(), Some(0x01));
        assert_eq!(RadioState::Ask.encode(), None);
        assert_eq!(
            decode(cmd::RADIO_STATE, &[0x02]),
            Command::Malformed(cmd::RADIO_STATE)
        );
    }

    /// An empty data frame is a data frame with no bytes, not a malformed one.
    /// The host will not send one, but the distinction keeps `Data` total over
    /// its payload.
    #[test]
    fn an_empty_data_frame_is_data() {
        assert_eq!(decode(cmd::DATA, &[]), Command::Data(&[]));
    }

    /// Commands that exist and are somebody else's phase are reported as such,
    /// so a log line can say which phase rather than "unknown command".
    #[test]
    fn the_display_and_bluetooth_commands_are_known_but_not_implemented() {
        for c in [
            cmd::DISP_BLNK,
            cmd::DISP_ROT,
            cmd::DISP_RCND,
            cmd::BLINK,
            cmd::BT_CTRL,
            cmd::BT_PIN,
        ] {
            assert_eq!(
                decode(c, &[0x00]),
                Command::NotYetImplemented(c),
                "{c:#04x}"
            );
        }
    }

    /// Commands for hardware this board does not have are a third thing again.
    /// There is no phase in which a WiFi command becomes implemented on a
    /// board with no WiFi, and a log that said "phase 7" about one would be
    /// telling somebody to wait for something that is not coming.
    #[test]
    fn wifi_and_neopixel_commands_are_not_applicable_rather_than_pending() {
        for c in [
            cmd::WIFI_MODE,
            cmd::WIFI_SSID,
            cmd::WIFI_PSK,
            cmd::WIFI_CHN,
            cmd::WIFI_IP,
            cmd::WIFI_NM,
            cmd::CFG_READ,
            cmd::NP_INT,
            cmd::DIS_IA,
            // The panel's address is discovered by scanning, so setting it
            // would replace a measurement with a guess.
            cmd::DISP_ADR,
        ] {
            assert_eq!(decode(c, &[0x00]), Command::NotApplicable(c), "{c:#04x}");
        }
    }

    /// The display commands, decoded. `CMD_FB_WRITE` is the shape that is easy
    /// to get wrong: one line byte and then exactly eight of picture.
    #[test]
    fn the_display_commands_decode_to_their_arguments() {
        assert_eq!(
            decode(cmd::FB_EXT, &[0x01]),
            Command::ShowExternalFramebuffer(true)
        );
        assert_eq!(
            decode(cmd::FB_EXT, &[0x00]),
            Command::ShowExternalFramebuffer(false)
        );
        assert_eq!(decode(cmd::FB_READ, &[0x01]), Command::ReadFramebuffer);
        assert_eq!(decode(cmd::DISP_READ, &[0x01]), Command::ReadDisplay);
        assert_eq!(
            decode(cmd::DISP_INT, &[0xC0]),
            Command::SetDisplayIntensity(0xC0)
        );

        let row = [7u8, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        assert_eq!(
            decode(cmd::FB_WRITE, &row),
            Command::WriteFramebuffer {
                line: 7,
                data: &row[1..]
            }
        );
        // A row one byte short is a disagreement, not something to pad out.
        assert_eq!(
            decode(cmd::FB_WRITE, &row[..8]),
            Command::Malformed(cmd::FB_WRITE)
        );
        assert_eq!(
            decode(cmd::FB_WRITE, &[]),
            Command::Malformed(cmd::FB_WRITE)
        );
    }

    /// The two commands that cannot be undone both carry a guard byte, and
    /// both check it. The port is open to whatever anyone sends down it, and
    /// an unguarded wipe would be one stray byte away from erasing a board's
    /// provisioning.
    #[test]
    fn a_wipe_or_a_reset_without_its_guard_byte_is_not_one() {
        assert_eq!(decode(cmd::ROM_WIPE, &[CONFIRM_BYTE]), Command::WipeRom);
        assert_eq!(decode(cmd::RESET, &[CONFIRM_BYTE]), Command::Reset);
        for wrong in [0x00u8, 0x01, 0x73, 0xF7, 0xF9, 0xFF] {
            assert_eq!(
                decode(cmd::ROM_WIPE, &[wrong]),
                Command::Malformed(cmd::ROM_WIPE),
                "{wrong:#04x}"
            );
            assert_eq!(
                decode(cmd::RESET, &[wrong]),
                Command::Malformed(cmd::RESET),
                "{wrong:#04x}"
            );
        }
        assert_eq!(
            decode(cmd::ROM_WIPE, &[]),
            Command::Malformed(cmd::ROM_WIPE)
        );
        assert_eq!(decode(cmd::RESET, &[]), Command::Malformed(cmd::RESET));
    }

    /// An EEPROM write is `[address, value]`, and every address the byte can
    /// hold is a real one -- including the two that the framing would
    /// otherwise eat, which arrive escaped and must come out as themselves.
    #[test]
    fn an_eeprom_write_carries_an_address_and_a_value() {
        assert_eq!(
            decode(cmd::ROM_WRITE, &[0x9B, 0x73]),
            Command::WriteRom {
                addr: 0x9B,
                value: 0x73
            }
        );
        assert_eq!(
            decode(cmd::ROM_WRITE, &[kiss::FEND, kiss::FESC]),
            Command::WriteRom {
                addr: kiss::FEND,
                value: kiss::FESC
            }
        );
        assert_eq!(
            decode(cmd::ROM_WRITE, &[0x00]),
            Command::Malformed(cmd::ROM_WRITE)
        );
        assert_eq!(
            decode(cmd::ROM_WRITE, &[]),
            Command::Malformed(cmd::ROM_WRITE)
        );
    }

    /// The signature and hash commands carry fixed-width payloads, and a short
    /// one is a disagreement rather than something to pad out. Storing a
    /// half-length signature would make a device claim to be signed.
    #[test]
    fn a_signature_or_hash_shorter_than_its_field_is_malformed() {
        let sig = [0xA5u8; eeprom::DEVICE_SIGNATURE_LEN];
        assert_eq!(
            decode(cmd::DEV_SIG, &sig),
            Command::StoreDeviceSignature(&sig)
        );
        assert_eq!(
            decode(cmd::DEV_SIG, &sig[..63]),
            Command::Malformed(cmd::DEV_SIG)
        );

        let hash = [0x5Au8; eeprom::HASH_LEN];
        assert_eq!(decode(cmd::FW_HASH, &hash), Command::SetFirmwareHash(&hash));
        assert_eq!(
            decode(cmd::FW_HASH, &hash[..31]),
            Command::Malformed(cmd::FW_HASH)
        );
    }

    /// Only the two hash kinds the host asks for. A third value is a
    /// disagreement about a command both ends claim to know.
    #[test]
    fn only_the_two_defined_hash_kinds_are_accepted() {
        assert_eq!(
            decode(cmd::HASHES, &[hash_kind::TARGET_FIRMWARE]),
            Command::QueryHash(hash_kind::TARGET_FIRMWARE)
        );
        assert_eq!(
            decode(cmd::HASHES, &[hash_kind::FIRMWARE]),
            Command::QueryHash(hash_kind::FIRMWARE)
        );
        for wrong in [0x00u8, 0x03, 0xFF] {
            assert_eq!(
                decode(cmd::HASHES, &[wrong]),
                Command::Malformed(cmd::HASHES),
                "{wrong:#04x}"
            );
        }
    }

    /// The full handshake `rnodeconf` opens with -- twice as long as the one
    /// `rnsd` sends, and written as a single burst in one `write()`, so every
    /// frame's closing delimiter is the next one's opener.
    #[test]
    fn the_rnodeconf_probe_decodes_to_the_eight_commands_it_is() {
        let wire = [
            kiss::FEND,
            cmd::DETECT,
            DETECT_REQ,
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
            cmd::BOARD,
            0x00,
            kiss::FEND,
            cmd::DEV_HASH,
            0x01,
            kiss::FEND,
            cmd::HASHES,
            0x01,
            kiss::FEND,
            cmd::HASHES,
            0x02,
            kiss::FEND,
        ];
        let mut d = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        let mut frames: Vec<(u8, Vec<u8>)> = Vec::new();
        for &b in &wire {
            if d.feed(b) == kiss::Step::Frame {
                frames.push((d.command(), d.payload().to_vec()));
            }
        }
        let got: Vec<Command> = frames.iter().map(|(c, p)| decode(*c, p)).collect();
        assert_eq!(
            got,
            vec![
                Command::Detect,
                Command::QueryFirmwareVersion,
                Command::QueryPlatform,
                Command::QueryMcu,
                Command::QueryBoard,
                Command::QueryDeviceHash,
                Command::QueryHash(hash_kind::TARGET_FIRMWARE),
                Command::QueryHash(hash_kind::FIRMWARE),
            ]
        );
    }

    /// `rnodeconf` is a second host with a second parser, and it reads
    /// commands `rnsd` never sees. Transcribed the same way, branch by branch.
    ///
    /// The two must not disagree about any command they both parse — nothing
    /// in either codebase forces that, and a divergence would mean a frame
    /// that is correct for one host and corrupt for the other, with no way to
    /// be right for both.
    #[test]
    fn the_two_hosts_agree_about_every_command_they_both_read() {
        // rnodeconf accumulates these through an unescaping step.
        let unescaped = [
            cmd::ROM_READ,
            cmd::CFG_READ,
            cmd::DATA,
            cmd::FREQUENCY,
            cmd::BANDWIDTH,
            cmd::BT_PIN,
            cmd::DEV_HASH,
            cmd::HASHES,
            cmd::FW_VERSION,
            cmd::STAT_RX,
            cmd::STAT_TX,
        ];
        // And reads these straight out of the stream.
        let raw = [
            cmd::BOARD,
            cmd::PLATFORM,
            cmd::MCU,
            cmd::TXPOWER,
            cmd::SF,
            cmd::CR,
            cmd::RADIO_STATE,
            cmd::RADIO_LOCK,
            cmd::STAT_RSSI,
            cmd::STAT_SNR,
            cmd::RANDOM,
            cmd::ERROR,
            cmd::DETECT,
        ];
        for c in unescaped {
            assert!(host_unescapes(c), "{c:#04x} should be unescaped");
        }
        for c in raw {
            assert!(!host_unescapes(c), "{c:#04x} should be read raw");
        }
    }

    /// The EEPROM contains a 128-byte RSA signature, which is uniformly
    /// distributed bytes: on any real device it contains `0xC0` and `0xDB`.
    /// An unescaped `CMD_ROM_READ` would therefore end its own frame partway
    /// through, and the host would report a device whose EEPROM is however
    /// many bytes it happened to get before the first delimiter.
    #[test]
    fn an_eeprom_dump_containing_framing_bytes_survives_the_wire() {
        let mut rom = eeprom::Eeprom::new();
        for addr in 0..=u8::MAX {
            rom.write(addr, addr);
        }
        let image = rom.as_bytes();
        assert!(image.contains(&kiss::FEND) && image.contains(&kiss::FESC));

        let mut wire = [0u8; 2 * eeprom::SIZE + 3];
        let n = encode_response(cmd::ROM_READ, image, &mut wire).expect("fits");

        let mut d = kiss::Decoder::<{ eeprom::SIZE }>::new();
        let mut got: Option<Vec<u8>> = None;
        for &b in &wire[..n] {
            if d.feed(b) == kiss::Step::Frame {
                got = Some(d.payload().to_vec());
            }
        }
        assert_eq!(got.as_deref(), Some(&image[..]));
    }

    /// The RSSI offset, in both directions. The host computes `byte - 157`, so
    /// a -157 dBm signal is byte 0 and -45 dBm is byte 112. Getting the sign
    /// wrong would report every signal as impossibly strong.
    #[test]
    fn the_rssi_offset_maps_dbm_onto_a_byte() {
        assert_eq!(RSSI_OFFSET, 157);
        for dbm in [-157i16, -120, -69, -45, -1] {
            let byte = (dbm + RSSI_OFFSET) as u8;
            assert_eq!(byte as i16 - RSSI_OFFSET, dbm, "{dbm} dBm");
        }
        assert_eq!((-45i16 + RSSI_OFFSET) as u8, 112);
    }
}
