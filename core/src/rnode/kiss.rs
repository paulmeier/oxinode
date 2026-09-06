//! KISS framing, as Reticulum's `RNodeInterface` reads and writes it.
//!
//! A frame is `FEND`, one command byte, a payload, `FEND`. Inside the payload
//! `FEND` is sent as `FESC TFEND` and `FESC` as `FESC TFESC`, so that the
//! delimiter never appears in the data.
//!
//! # Why the decoder is a byte-at-a-time state machine
//!
//! Because the bytes arrive that way. USB delivers 64-byte packets with no
//! relationship to frame boundaries: a frame can span several, and one packet
//! can hold the tail of one frame and the head of the next. A decoder that
//! wanted whole frames would need to buffer and rescan, and would still have to
//! answer the same questions. Feeding it one byte at a time makes the buffering
//! explicit and the state small enough to reason about.
//!
//! # What a decoder has to survive
//!
//! Not a well-formed stream. The port is open to anything the host sends,
//! including a terminal, a probe from an unrelated tool, or the tail of a
//! previous session, and none of those are hostile so much as simply not KISS.
//! So:
//!
//! * **A frame longer than the buffer is discarded, not truncated.** A
//!   truncated frame is a valid-looking frame with the wrong contents, which is
//!   worse than no frame at all.
//! * **`FEND` always resynchronises.** Whatever state the decoder is in, a
//!   `FEND` starts a new frame. That is what lets it recover from a lost byte
//!   rather than staying wrong forever.
//! * **An escape at the end of a frame is an error**, not a silently dropped
//!   byte.
//! * **Empty frames are ignored.** `FEND FEND` is how a sender pads or idles,
//!   and the host's own writer emits back-to-back `FEND`s when it sends several
//!   commands in one burst — the detect handshake does exactly that.

/// Frame delimiter.
pub const FEND: u8 = 0xC0;
/// Escape.
pub const FESC: u8 = 0xDB;
/// Stands for [`FEND`] after an [`FESC`].
pub const TFEND: u8 = 0xDC;
/// Stands for [`FESC`] after an [`FESC`].
pub const TFESC: u8 = 0xDD;

/// The largest payload Reticulum will accept in a data frame.
///
/// `RNodeInterface.HW_MTU`. The host stops accumulating at this length and
/// silently drops the rest, so a longer frame is not an error it reports — it
/// is a packet that quietly loses its tail.
pub const HW_MTU: usize = 508;

/// Whether a byte has to be escaped inside a payload.
pub const fn needs_escape(byte: u8) -> bool {
    byte == FEND || byte == FESC
}

/// How many bytes `payload` occupies once escaped.
pub fn escaped_len(payload: &[u8]) -> usize {
    let mut n = 0;
    for &b in payload {
        n += if needs_escape(b) { 2 } else { 1 };
    }
    n
}

/// What a decoder produced from one byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing yet; the byte was consumed.
    Pending,
    /// A complete frame is in the decoder's buffer.
    Frame,
    /// Something was wrong and the partial frame was dropped.
    Error(DecodeError),
}

/// Why a frame was dropped.
///
/// These are counted rather than merely returned, because on a link with no
/// second channel the useful question is "is this happening at all", and one
/// occurrence is indistinguishable from a thousand if only the last is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The frame was longer than the buffer.
    TooLong,
    /// An `FESC` was followed by something other than `TFEND` or `TFESC`.
    BadEscape,
    /// The frame ended in the middle of an escape sequence.
    TruncatedEscape,
}

impl DecodeError {
    /// A line fit for a log.
    pub const fn message(self) -> &'static str {
        match self {
            Self::TooLong => "frame longer than the receive buffer",
            Self::BadEscape => "escape followed by neither TFEND nor TFESC",
            Self::TruncatedEscape => "frame ended inside an escape sequence",
        }
    }
}

/// Where the decoder is in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Outside any frame. Everything but `FEND` is discarded.
    Idle,
    /// Inside a frame, next byte is the command.
    Command,
    /// Inside a frame, accumulating payload.
    Payload,
    /// Inside a frame, the previous byte was `FESC`.
    Escaped,
    /// Inside a frame that has already gone wrong. Bytes are discarded until
    /// the next `FEND`, so that a too-long frame does not also produce a
    /// spurious short one from its tail.
    Poisoned,
}

/// A streaming KISS decoder over a fixed buffer.
///
/// `N` is the largest payload it will accept. [`HW_MTU`] is what the host uses.
pub struct Decoder<const N: usize> {
    state: State,
    command: u8,
    buf: [u8; N],
    len: usize,
    dropped: u32,
}

impl<const N: usize> Default for Decoder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Decoder<N> {
    /// A decoder waiting for the first `FEND`.
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            command: 0,
            buf: [0; N],
            len: 0,
            dropped: 0,
        }
    }

    /// Feed one byte.
    ///
    /// When this returns [`Step::Frame`], [`Decoder::command`] and
    /// [`Decoder::payload`] describe it, and stay valid until the next call.
    pub fn feed(&mut self, byte: u8) -> Step {
        // Checked before anything else, in every state. Resynchronising on the
        // delimiter is the entire reason a framing protocol has one.
        if byte == FEND {
            return self.close();
        }
        match self.state {
            State::Idle => Step::Pending,
            State::Poisoned => Step::Pending,
            State::Command => {
                self.command = byte;
                self.len = 0;
                self.state = State::Payload;
                Step::Pending
            }
            State::Payload => {
                if byte == FESC {
                    self.state = State::Escaped;
                    Step::Pending
                } else {
                    self.push(byte)
                }
            }
            State::Escaped => match byte {
                TFEND => {
                    self.state = State::Payload;
                    self.push(FEND)
                }
                TFESC => {
                    self.state = State::Payload;
                    self.push(FESC)
                }
                _ => self.poison(DecodeError::BadEscape),
            },
        }
    }

    /// Handle a `FEND`: finish whatever was in progress and start a new frame.
    fn close(&mut self) -> Step {
        let outcome = match self.state {
            // An empty frame is not an error. The host emits back-to-back
            // FENDs whenever it writes several commands in one burst, which
            // the detect handshake does.
            State::Idle | State::Command | State::Poisoned => Step::Pending,
            State::Payload => Step::Frame,
            State::Escaped => {
                self.dropped += 1;
                Step::Error(DecodeError::TruncatedEscape)
            }
        };
        self.state = State::Command;
        if outcome != Step::Frame {
            self.len = 0;
        }
        outcome
    }

    fn push(&mut self, byte: u8) -> Step {
        if self.len == N {
            return self.poison(DecodeError::TooLong);
        }
        self.buf[self.len] = byte;
        self.len += 1;
        Step::Pending
    }

    /// Abandon the current frame and ignore everything up to the next `FEND`.
    fn poison(&mut self, error: DecodeError) -> Step {
        self.state = State::Poisoned;
        self.len = 0;
        self.dropped += 1;
        Step::Error(error)
    }

    /// The command byte of the frame just completed.
    pub const fn command(&self) -> u8 {
        self.command
    }

    /// The payload of the frame just completed, unescaped.
    pub fn payload(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// How many frames have been dropped since boot.
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Whether a frame is currently being accumulated.
    pub const fn in_frame(&self) -> bool {
        !matches!(self.state, State::Idle)
    }
}

/// Build a frame into a caller-supplied buffer.
///
/// Returns the number of bytes written, or `None` if the buffer is too small.
/// A partial frame is never left behind: on failure nothing has been written
/// that a caller could mistakenly transmit.
///
/// `escape` says whether the payload is escaped, and it is a parameter rather
/// than always-true because **the host does not un-escape every frame**. Its
/// parser has one branch per command; the multi-byte fields go through an
/// unescaping step and the single-byte ones are read raw. Escaping a raw field
/// corrupts it. See [`super::command`], which is where each command's answer
/// to this lives.
pub fn encode(command: u8, payload: &[u8], escape: bool, out: &mut [u8]) -> Option<usize> {
    let body = if escape {
        escaped_len(payload)
    } else {
        payload.len()
    };
    let total = body + 3;
    if out.len() < total {
        return None;
    }
    out[0] = FEND;
    out[1] = command;
    let mut n = 2;
    for &b in payload {
        if escape && needs_escape(b) {
            out[n] = FESC;
            out[n + 1] = if b == FEND { TFEND } else { TFESC };
            n += 2;
        } else {
            out[n] = b;
            n += 1;
        }
    }
    out[n] = FEND;
    Some(n + 1)
}

/// The exact size [`encode`] will produce.
pub fn encoded_len(payload: &[u8], escape: bool) -> usize {
    3 + if escape {
        escaped_len(payload)
    } else {
        payload.len()
    }
}

// The four framing bytes are the protocol's, and every one of them is a value
// that also appears in ordinary binary data -- which is the whole reason the
// escaping exists. A typo here is a modem that works until a packet happens to
// contain 0xC0.
const _: () = assert!(FEND == 0xC0 && FESC == 0xDB && TFEND == 0xDC && TFESC == 0xDD);
const _: () = assert!(needs_escape(FEND) && needs_escape(FESC));
const _: () = assert!(!needs_escape(TFEND) && !needs_escape(TFESC));
// The host stops accumulating a data frame at HW_MTU and drops the remainder
// without reporting anything, so a longer frame loses its tail silently.
const _: () = assert!(HW_MTU == 508);

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole slice and collect every frame it produces.
    fn run<const N: usize>(d: &mut Decoder<N>, bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        for &b in bytes {
            if d.feed(b) == Step::Frame {
                out.push((d.command(), d.payload().to_vec()));
            }
        }
        out
    }

    fn frame(command: u8, payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; encoded_len(payload, true)];
        let n = encode(command, payload, true, &mut buf).expect("sized exactly");
        buf.truncate(n);
        buf
    }

    #[test]
    fn a_frame_round_trips() {
        let mut d = Decoder::<HW_MTU>::new();
        let got = run(&mut d, &frame(0x00, b"hello"));
        assert_eq!(got, vec![(0x00, b"hello".to_vec())]);
    }

    /// The two bytes the escaping exists for. A payload of nothing but
    /// delimiters must survive, because Reticulum's packets are encrypted and
    /// therefore uniformly random — every byte value turns up.
    #[test]
    fn the_delimiter_and_the_escape_survive_a_round_trip() {
        let payload = [FEND, FESC, FEND, FESC, 0x00, 0xFF, FEND];
        let wire = frame(0x00, &payload);
        // Nothing but the two delimiters may appear raw on the wire.
        assert_eq!(wire.iter().filter(|&&b| b == FEND).count(), 2);
        assert_eq!(wire[0], FEND);
        assert_eq!(*wire.last().unwrap(), FEND);
        let mut d = Decoder::<HW_MTU>::new();
        assert_eq!(run(&mut d, &wire), vec![(0x00, payload.to_vec())]);
    }

    /// Every byte value, in one payload, in both directions.
    #[test]
    fn every_byte_value_round_trips() {
        let payload: Vec<u8> = (0..=255u8).collect();
        let mut d = Decoder::<HW_MTU>::new();
        assert_eq!(run(&mut d, &frame(0x00, &payload)), vec![(0x00, payload)]);
    }

    /// The host writes several commands in one burst, which puts `FEND`s back
    /// to back. The detect handshake is exactly this, so a decoder that treats
    /// an empty frame as a frame would answer four commands with five.
    #[test]
    fn the_detect_handshake_decodes_as_four_commands() {
        // FEND CMD_DETECT 0x73 FEND CMD_FW_VERSION 0x00 FEND ... -- the shared
        // FENDs mean each frame's opener is the previous frame's closer.
        let wire = [
            FEND, 0x08, 0x73, FEND, 0x50, 0x00, FEND, 0x48, 0x00, FEND, 0x49, 0x00, FEND,
        ];
        let mut d = Decoder::<HW_MTU>::new();
        assert_eq!(
            run(&mut d, &wire),
            vec![
                (0x08, vec![0x73]),
                (0x50, vec![0x00]),
                (0x48, vec![0x00]),
                (0x49, vec![0x00]),
            ]
        );
    }

    /// Padding, idling, and a stream that opens with delimiters.
    #[test]
    fn empty_frames_produce_nothing() {
        let mut d = Decoder::<HW_MTU>::new();
        assert!(run(&mut d, &[FEND; 16]).is_empty());
        assert_eq!(d.dropped(), 0, "an empty frame is not a dropped one");
        // ...and the decoder still works afterwards.
        assert_eq!(run(&mut d, &frame(0x01, b"x")), vec![(0x01, b"x".to_vec())]);
    }

    /// Bytes before the first delimiter are not a frame. A port that was open
    /// before the board booted has whatever the host last wrote sitting in it.
    #[test]
    fn leading_rubbish_is_discarded_until_the_first_delimiter() {
        let mut d = Decoder::<HW_MTU>::new();
        let mut wire = b"garbage from a terminal\r\n".to_vec();
        wire.extend_from_slice(&frame(0x02, b"ok"));
        assert_eq!(run(&mut d, &wire), vec![(0x02, b"ok".to_vec())]);
    }

    /// USB delivers 64-byte packets with no relation to frame boundaries, so a
    /// frame split at every possible point must decode identically.
    #[test]
    fn a_frame_decodes_the_same_however_it_is_split() {
        let payload = [0x01, FEND, 0x02, FESC, 0x03];
        let wire = frame(0x00, &payload);
        for split in 0..wire.len() {
            let mut d = Decoder::<HW_MTU>::new();
            let mut got = run(&mut d, &wire[..split]);
            got.extend(run(&mut d, &wire[split..]));
            assert_eq!(got, vec![(0x00, payload.to_vec())], "split at {split}");
        }
    }

    /// A frame too long for the buffer is dropped whole. Truncating would
    /// hand up a well-formed frame with the wrong contents, which nothing
    /// downstream could detect.
    #[test]
    fn an_overlong_frame_is_dropped_rather_than_truncated() {
        let mut d = Decoder::<8>::new();
        let payload = [0xAAu8; 32];
        let got = run(&mut d, &frame(0x00, &payload));
        assert!(got.is_empty(), "a truncated frame must not be delivered");
        assert_eq!(d.dropped(), 1);
        // ...and the tail of the overlong frame must not become a frame of its
        // own once the buffer would fit it again.
        assert_eq!(
            run(&mut d, &frame(0x00, b"short")),
            vec![(0x00, b"short".to_vec())]
        );
        assert_eq!(
            d.dropped(),
            1,
            "the recovery frame must not count as a drop"
        );
    }

    /// A payload of exactly the buffer size is not too long. Off-by-one here
    /// would reject the largest legal packet, which is also the one a host
    /// sends least often and notices last.
    #[test]
    fn a_payload_of_exactly_the_buffer_size_is_accepted() {
        let mut d = Decoder::<8>::new();
        let payload = [0x5Au8; 8];
        assert_eq!(
            run(&mut d, &frame(0x00, &payload)),
            vec![(0x00, payload.to_vec())]
        );
        assert_eq!(d.dropped(), 0);
        // One more is one too many.
        let mut d = Decoder::<8>::new();
        assert!(run(&mut d, &frame(0x00, &[0x5Au8; 9])).is_empty());
        assert_eq!(d.dropped(), 1);
    }

    /// An escape has to be followed by one of two bytes. Anything else means
    /// the stream is not what it claims, and guessing would fabricate data.
    #[test]
    fn a_bad_escape_drops_the_frame() {
        let mut d = Decoder::<HW_MTU>::new();
        let mut steps = Vec::new();
        for &b in &[FEND, 0x00, b'a', FESC, 0x41, b'b', FEND] {
            steps.push(d.feed(b));
        }
        assert!(steps.contains(&Step::Error(DecodeError::BadEscape)));
        assert!(!steps.contains(&Step::Frame));
        assert_eq!(d.dropped(), 1);
    }

    /// An escape immediately before the delimiter. The frame is incomplete by
    /// construction; delivering the bytes before it would deliver a payload
    /// that was never sent.
    #[test]
    fn an_escape_at_the_end_of_a_frame_is_an_error() {
        let mut d = Decoder::<HW_MTU>::new();
        let mut steps = Vec::new();
        for &b in &[FEND, 0x00, b'a', FESC, FEND] {
            steps.push(d.feed(b));
        }
        assert_eq!(
            steps.last(),
            Some(&Step::Error(DecodeError::TruncatedEscape))
        );
        assert_eq!(d.dropped(), 1);
    }

    /// After any error, the next well-formed frame must decode. A decoder that
    /// needs a reset after a bad byte is a link that stays down after one.
    #[test]
    fn the_decoder_recovers_from_every_error() {
        for bad in [
            vec![FEND, 0x00, FESC, 0x41, FEND], // bad escape
            vec![FEND, 0x00, b'a', FESC, FEND], // truncated escape
        ] {
            let mut d = Decoder::<HW_MTU>::new();
            run(&mut d, &bad);
            assert_eq!(
                run(&mut d, &frame(0x11, b"after")),
                vec![(0x11, b"after".to_vec())],
                "did not recover from {bad:02x?}"
            );
        }
    }

    /// A command byte with no payload is legal and common: the host's own
    /// setters send one-byte payloads, but a response like `CMD_READY` carries
    /// nothing at all.
    #[test]
    fn a_frame_with_an_empty_payload_is_a_frame() {
        let mut d = Decoder::<HW_MTU>::new();
        assert_eq!(run(&mut d, &[FEND, 0x0F, FEND]), vec![(0x0F, vec![])]);
    }

    /// Unescaped encoding is not a convenience — the host reads single-byte
    /// fields raw, so escaping them would corrupt them. What it costs is that
    /// such a field cannot carry 0xC0, and the encoder must not pretend
    /// otherwise.
    #[test]
    fn an_unescaped_frame_passes_the_payload_through_verbatim() {
        let mut buf = [0u8; 8];
        let n = encode(0x24, &[0xDB], false, &mut buf).unwrap();
        assert_eq!(&buf[..n], &[FEND, 0x24, 0xDB, FEND]);
        // The same value escaped is a different, and for this command wrong,
        // pair of bytes.
        let n = encode(0x24, &[0xDB], true, &mut buf).unwrap();
        assert_eq!(&buf[..n], &[FEND, 0x24, FESC, TFESC, FEND]);
    }

    /// A buffer one byte short must produce nothing, not a partial frame that
    /// a caller might transmit.
    #[test]
    fn encoding_into_too_small_a_buffer_writes_nothing_usable() {
        let payload = [FEND, 0x01];
        let need = encoded_len(&payload, true);
        assert_eq!(need, 3 + 3);
        let mut exact = vec![0u8; need];
        assert_eq!(encode(0x00, &payload, true, &mut exact), Some(need));
        let mut short = vec![0u8; need - 1];
        assert_eq!(encode(0x00, &payload, true, &mut short), None);
        assert!(short.iter().all(|&b| b == 0), "nothing may be written");
    }

    /// The length predicted and the length written have to agree, or every
    /// caller has to over-allocate and hope.
    #[test]
    fn the_predicted_length_is_the_written_length() {
        for payload in [
            vec![],
            vec![0x00],
            vec![FEND],
            vec![FESC],
            vec![FEND, FESC, FEND],
            (0..=255u8).collect(),
        ] {
            for escape in [true, false] {
                let need = encoded_len(&payload, escape);
                let mut buf = vec![0u8; need];
                assert_eq!(
                    encode(0x00, &payload, escape, &mut buf),
                    Some(need),
                    "{payload:02x?} escape={escape}"
                );
            }
        }
    }

    /// Whatever the decoder is fed, it must not deliver a frame that was not
    /// framed, and must never panic. A stand-in for the property a fuzzer
    /// would check, using a cheap deterministic generator so it runs in CI.
    #[test]
    fn arbitrary_input_never_panics_and_never_invents_a_frame() {
        let mut state = 0x12345678u32;
        let mut d = Decoder::<64>::new();
        let mut frames = 0u32;
        for _ in 0..200_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // Bias hard toward the interesting bytes; uniform random almost
            // never produces an escape sequence.
            let byte = match (state >> 16) & 0x7 {
                0 | 1 => FEND,
                2 | 3 => FESC,
                4 => TFEND,
                5 => TFESC,
                _ => (state >> 24) as u8,
            };
            if d.feed(byte) == Step::Frame {
                frames += 1;
                assert!(d.payload().len() <= 64);
            }
        }
        // The generator emits FENDs a quarter of the time, so it must have
        // produced *some* frames -- a test that passes because nothing ever
        // decoded would prove nothing.
        assert!(
            frames > 1_000,
            "only {frames} frames; the generator is wrong"
        );
    }
}
