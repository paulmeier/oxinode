//! The RNode command set: the bytes, their payload shapes, and — the part that
//! is easy to get wrong — which of them the host un-escapes.
//!
//! Every constant here is pinned by an observable behaviour of Reticulum's
//! `RNodeInterface` (RNS 1.5.0), and the doc comment says which. See
//! [`super`] for why the host and not the firmware is the source.

/// Command bytes, as `RNodeInterface.KISS` defines them.
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
    /// Which platform this is. See [`super::PLATFORM_NRF52`].
    pub const PLATFORM: u8 = 0x48;
    /// Which microcontroller this is. See [`super::MCU_NRF52`].
    pub const MCU: u8 = 0x49;
    /// Firmware version, two bytes: major then minor.
    pub const FW_VERSION: u8 = 0x50;
    /// Read the device's EEPROM. Phase 6.
    pub const ROM_READ: u8 = 0x51;
    /// The device has reset.
    pub const RESET: u8 = 0x55;
    /// Read the display. Phase 7.
    pub const DISP_READ: u8 = 0x66;
    /// A hardware error. See [`super::error`].
    pub const ERROR: u8 = 0x90;
}

/// What the host sends in a [`cmd::DETECT`] frame to ask "are you an RNode?".
pub const DETECT_REQ: u8 = 0x73;
/// What it must get back. Any other answer, or none, and `configure_device`
/// closes the port.
pub const DETECT_RESP: u8 = 0x46;

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
/// The table below is transcribed from that parser, branch by branch. It comes
/// out as a clean rule — multi-byte yes, single-byte no — but it is recorded as
/// a table rather than as a rule, because it is a fact about someone else's
/// code and the next version of it does not have to stay tidy.
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
        // The host sends a 0x00 argument with each of these. It is a
        // placeholder, not a selector -- there is nothing else it can be --
        // so it is not checked.
        cmd::FW_VERSION => Command::QueryFirmwareVersion,
        cmd::PLATFORM => Command::QueryPlatform,
        cmd::MCU => Command::QueryMcu,
        cmd::FB_EXT | cmd::FB_READ | cmd::FB_WRITE | cmd::DISP_READ | cmd::BLINK => {
            Command::NotYetImplemented(command)
        }
        cmd::BT_CTRL => Command::NotYetImplemented(command),
        cmd::ROM_READ => Command::NotYetImplemented(command),
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
            cmd::FB_EXT,
            cmd::FB_READ,
            cmd::FB_WRITE,
            cmd::DISP_READ,
            cmd::BLINK,
            cmd::BT_CTRL,
            cmd::ROM_READ,
        ] {
            assert_eq!(
                decode(c, &[0x00]),
                Command::NotYetImplemented(c),
                "{c:#04x}"
            );
        }
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
