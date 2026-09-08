//! What goes on the air: one header byte in front of every LoRa frame, and
//! a packet longer than a frame split across two of them.
//!
//! A LoRa frame carries at most 255 bytes and Reticulum's MTU is 500, so a
//! full-size packet cannot be one frame. The stock RNode firmware puts a
//! single header byte in front of every frame -- a sequence number in the
//! high nibble and a split flag in the low bit -- and sends a long packet as
//! two frames with the same header, which the receiver joins by sequence.
//! Reticulum's `RNodeInterface` assumes the firmware does this: its `HW_MTU`
//! of 508 is exactly two frames of 254 bytes, and that is where
//! [`PACKET_MAX`] comes from.
//!
//! Until this module oxinode sent raw packets, which meant two things phase
//! 15 recorded: it could not carry a full-MTU packet at all, and a stock
//! RNode read the first byte of every oxinode packet as a header and
//! oxinode read every stock RNode header as the first byte of a packet, so
//! the two did not interoperate over the air.
//!
//! # Provenance
//!
//! The frame layout is interface data -- the header byte's fields, the flag
//! value, the 254-byte split point, the same header on both halves, the
//! receiver's rule for joining and discarding -- and is what a receiver has
//! to know to read a stock RNode's frames. It was taken from the RNode
//! firmware's published constants and descriptions of its behaviour, and
//! from what a stock RNode does on the air. The firmware's code is GPL and
//! none of it has been read, ported or transliterated; everything below is
//! oxinode's own.
//!
//! # Two departures, both on purpose
//!
//! **The sequence counts.** The stock firmware draws its sequence nibble at
//! random, so two consecutive split packets from one board share a nibble
//! one time in sixteen -- and a receiver that lost the second half of the
//! first then joins it to the first half of the second. [`Sequence`] steps
//! instead, from a seed the caller draws once, so consecutive packets from
//! one board never share a nibble. A receiver cannot tell the difference,
//! and the collision is left to two boards on the same nibble at the same
//! moment.
//!
//! **A first half goes stale.** The stock receiver holds a first half until
//! something replaces it, however long that takes. [`Reassembler`] holds it
//! for [`max_age_us`]: the second half of a split follows the first with no
//! carrier sense between them, so one that has not arrived after two
//! airtimes and a second is not coming, and a later split with the same
//! nibble is a new packet rather than its other half.

use crate::lr1121::config::{ValidConfig, MAX_PAYLOAD};

/// Bytes of header in front of every frame.
pub const HEADER_LEN: usize = 1;
/// The header's split flag: this frame is one half of a packet.
pub const FLAG_SPLIT: u8 = 0x01;
/// The most a frame can carry, header included. A LoRa packet's limit.
pub const FRAME_MAX: usize = MAX_PAYLOAD as usize;
/// The most packet a frame can carry.
pub const FRAME_PAYLOAD: usize = FRAME_MAX - HEADER_LEN;
/// The longest packet: two frames' worth. `RNodeInterface.HW_MTU`.
pub const PACKET_MAX: usize = 2 * FRAME_PAYLOAD;
/// The number of frames a packet may be split into.
pub const FRAMES_MAX: usize = 2;

const _: () = assert!(PACKET_MAX == super::kiss::HW_MTU);
const _: () = assert!(FRAME_MAX == 255 && FRAME_PAYLOAD == 254 && PACKET_MAX == 508);

/// The header byte for a sequence nibble and a split flag.
///
/// The sequence goes in the high nibble; the split flag is the low bit and
/// the other three low bits are zero.
pub const fn header(sequence: u8, split: bool) -> u8 {
    (sequence << 4) | if split { FLAG_SPLIT } else { 0 }
}

/// The sequence nibble of a header.
pub const fn sequence_of(header: u8) -> u8 {
    header >> 4
}

/// Whether a header marks one half of a split packet.
pub const fn is_split(header: u8) -> bool {
    header & FLAG_SPLIT != 0
}

/// How long a first half is worth holding, for a configuration.
///
/// The second half follows the first with nothing between them but the
/// turnaround, so two airtimes of the longest frame plus a second is
/// generous; at SF12 and 62.5 kHz that is still under a minute.
pub const fn max_age_us(config: &ValidConfig) -> u64 {
    2 * config.get().airtime_us(MAX_PAYLOAD) as u64 + 1_000_000
}

/// The sequence nibble for the next packet.
///
/// Steps by one per packet from a seed, so consecutive packets from one
/// board never share a nibble; see the module documentation for why that
/// is better than the random draw it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sequence(u8);

impl Sequence {
    /// Start from a seed. Any byte will do; only its low nibble is used.
    pub const fn new(seed: u8) -> Self {
        Self(seed & 0x0F)
    }

    /// The nibble for the next packet.
    pub fn take(&mut self) -> u8 {
        let n = self.0;
        self.0 = (n + 1) & 0x0F;
        n
    }
}

/// A packet, cut into the frames that carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split<'a> {
    header: u8,
    packet: &'a [u8],
}

impl<'a> Split<'a> {
    /// Cut `packet` into frames under sequence nibble `sequence`.
    ///
    /// `None` if the packet is longer than [`PACKET_MAX`]: a third frame is
    /// not something a receiver would join.
    pub const fn new(packet: &'a [u8], sequence: u8) -> Option<Self> {
        if packet.len() > PACKET_MAX {
            return None;
        }
        Some(Self {
            header: header(sequence, packet.len() > FRAME_PAYLOAD),
            packet,
        })
    }

    /// The header every frame of this packet carries.
    pub const fn header(&self) -> u8 {
        self.header
    }

    /// How many frames: one, or two for a packet longer than a frame holds.
    pub const fn frames(&self) -> usize {
        if self.packet.len() > FRAME_PAYLOAD {
            2
        } else {
            1
        }
    }

    /// Write frame `index` into `out` and say how long it is.
    ///
    /// `None` for an index past the last frame, or an `out` shorter than
    /// [`FRAME_MAX`]. The first frame takes the first [`FRAME_PAYLOAD`] bytes
    /// of the packet and the second takes the rest, which is where a stock
    /// RNode cuts too.
    pub fn frame(&self, index: usize, out: &mut [u8]) -> Option<usize> {
        if index >= self.frames() || out.len() < FRAME_MAX {
            return None;
        }
        let start = index * FRAME_PAYLOAD;
        let end = (start + FRAME_PAYLOAD).min(self.packet.len());
        let body = &self.packet[start..end];
        out[0] = self.header;
        out[HEADER_LEN..HEADER_LEN + body.len()].copy_from_slice(body);
        Some(HEADER_LEN + body.len())
    }
}

/// What the radio measured about a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    /// RSSI in dBm.
    pub rssi_dbm: i16,
    /// SNR in quarter-dB, as the chip reports it.
    pub snr_quarter_db: i8,
}

impl Signal {
    /// The mean of two, for a packet that arrived as two frames.
    const fn mean(self, other: Signal) -> Signal {
        Signal {
            rssi_dbm: (self.rssi_dbm + other.rssi_dbm) / 2,
            snr_quarter_db: ((self.snr_quarter_db as i16 + other.snr_quarter_db as i16) / 2) as i8,
        }
    }
}

/// A whole packet, out of a reassembler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet<'a> {
    /// The packet, header stripped and halves joined.
    pub payload: &'a [u8],
    /// Its signal: one frame's, or the mean of two.
    pub signal: Signal,
}

/// The first half of a split, waiting for its other half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    sequence: u8,
    signal: Signal,
    since_us: u64,
}

/// Frames in, packets out.
///
/// Holds at most one first half. Fed every frame the radio receives, in the
/// order it received them, with the time each arrived.
pub struct Reassembler {
    buf: [u8; PACKET_MAX],
    len: usize,
    pending: Option<Pending>,
    max_age_us: u64,
}

impl Reassembler {
    /// A reassembler that holds a first half for `max_age_us`; see
    /// [`max_age_us`].
    pub const fn new(max_age_us: u64) -> Self {
        Self {
            buf: [0; PACKET_MAX],
            len: 0,
            pending: None,
            max_age_us,
        }
    }

    /// Change how long a first half is held, for a new configuration.
    pub fn set_max_age_us(&mut self, max_age_us: u64) {
        self.max_age_us = max_age_us;
    }

    /// Whether a first half is being held.
    pub const fn is_holding(&self) -> bool {
        self.pending.is_some()
    }

    /// Drop whatever is held. For a reconfiguration: the other half of a
    /// packet from the old channel is not coming on the new one.
    pub fn clear(&mut self) {
        self.pending = None;
        self.len = 0;
    }

    /// Take one frame, and hand back the packet it completes, if any.
    ///
    /// The rules, which are a stock RNode receiver's:
    ///
    /// * a frame without the split flag is a packet by itself, and throws
    ///   away any first half being held;
    /// * a split frame with nothing held, or with a different sequence
    ///   nibble held, is a new first half and replaces what was held;
    /// * a split frame with the same nibble held is the second half, and
    ///   the joined packet is returned with the mean of the two signals.
    ///
    /// Plus one that is oxinode's own: a first half older than the maximum
    /// age counts as not held. An empty frame is not a frame -- there is no
    /// header to read -- and a frame whose header says it is a packet and
    /// whose body is empty is nothing to hand up. Both give `None`.
    pub fn feed(&mut self, frame: &[u8], signal: Signal, now_us: u64) -> Option<Packet<'_>> {
        let (&header, body) = frame.split_first()?;
        if let Some(p) = self.pending {
            if now_us.wrapping_sub(p.since_us) > self.max_age_us {
                self.clear();
            }
        }
        if !is_split(header) {
            self.clear();
            if body.is_empty() {
                return None;
            }
            self.buf[..body.len()].copy_from_slice(body);
            self.len = body.len();
            return Some(Packet {
                payload: &self.buf[..self.len],
                signal,
            });
        }
        let sequence = sequence_of(header);
        match self.pending {
            Some(p) if p.sequence == sequence => {
                let end = (self.len + body.len()).min(PACKET_MAX);
                self.buf[self.len..end].copy_from_slice(&body[..end - self.len]);
                self.len = end;
                self.pending = None;
                Some(Packet {
                    payload: &self.buf[..self.len],
                    signal: p.signal.mean(signal),
                })
            }
            _ => {
                self.buf[..body.len()].copy_from_slice(body);
                self.len = body.len();
                self.pending = Some(Pending {
                    sequence,
                    signal,
                    since_us: now_us,
                });
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::{RadioConfig, DEFAULT};

    const SIGNAL: Signal = Signal {
        rssi_dbm: -60,
        snr_quarter_db: 40,
    };

    fn packet_of(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + 3) as u8).collect()
    }

    fn frames_of(packet: &[u8], sequence: u8) -> Vec<Vec<u8>> {
        let split = Split::new(packet, sequence).expect("fits");
        let mut out = [0u8; FRAME_MAX];
        (0..split.frames())
            .map(|i| {
                let n = split.frame(i, &mut out).expect("a frame");
                out[..n].to_vec()
            })
            .collect()
    }

    /// The header byte's layout: the sequence nibble in the high bits, the
    /// split flag in bit 0, and nothing else set.
    #[test]
    fn the_header_is_a_sequence_nibble_and_a_split_flag() {
        for s in 0..16u8 {
            assert_eq!(header(s, false), s << 4);
            assert_eq!(header(s, true), (s << 4) | FLAG_SPLIT);
            assert_eq!(sequence_of(header(s, true)), s);
            assert_eq!(sequence_of(header(s, false)), s);
            assert!(is_split(header(s, true)));
            assert!(!is_split(header(s, false)));
            assert_eq!(header(s, true) & 0x0E, 0, "the other low bits are zero");
        }
    }

    /// A packet that fits is one frame: the header and then the packet,
    /// with the flag clear -- right up to 254 bytes, which is a full frame.
    #[test]
    fn a_packet_that_fits_is_one_frame() {
        for len in [0usize, 1, 100, FRAME_PAYLOAD] {
            let packet = packet_of(len);
            let split = Split::new(&packet, 5).unwrap();
            assert_eq!(split.frames(), 1, "{len}");
            assert!(!is_split(split.header()), "{len}");
            let frames = frames_of(&packet, 5);
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].len(), len + 1);
            assert_eq!(frames[0][0], header(5, false));
            assert_eq!(&frames[0][1..], &packet[..]);
        }
    }

    /// One byte more than a frame holds is two frames, both carrying the
    /// same header with the flag set; the first is full, the second has the
    /// rest. A 508-byte packet is two full frames; 509 does not go.
    #[test]
    fn a_longer_packet_is_two_frames_under_one_header() {
        for len in [FRAME_PAYLOAD + 1, 400, PACKET_MAX] {
            let packet = packet_of(len);
            let frames = frames_of(&packet, 9);
            assert_eq!(frames.len(), 2, "{len}");
            assert_eq!(frames[0].len(), FRAME_MAX, "{len}");
            assert_eq!(frames[1].len(), len - FRAME_PAYLOAD + 1, "{len}");
            assert_eq!(frames[0][0], header(9, true));
            assert_eq!(frames[1][0], header(9, true));
            assert_eq!(&frames[0][1..], &packet[..FRAME_PAYLOAD]);
            assert_eq!(&frames[1][1..], &packet[FRAME_PAYLOAD..]);
        }
        assert!(Split::new(&packet_of(PACKET_MAX + 1), 0).is_none());
        let packet = packet_of(400);
        let split = Split::new(&packet, 0).unwrap();
        assert_eq!(split.frame(2, &mut [0u8; FRAME_MAX]), None);
        assert_eq!(split.frame(0, &mut [0u8; FRAME_MAX - 1]), None);
    }

    /// The whole of the split, at every length there is: what goes in is
    /// what comes out, through the frames.
    #[test]
    fn every_length_survives_the_round_trip() {
        let mut r = Reassembler::new(1_000_000);
        for len in 1..=PACKET_MAX {
            let packet = packet_of(len);
            let frames = frames_of(&packet, (len % 16) as u8);
            let mut got = None;
            for (i, f) in frames.iter().enumerate() {
                let out = r
                    .feed(f, SIGNAL, i as u64 * 1000)
                    .map(|p| p.payload.to_vec());
                assert!(got.is_none(), "{len}: delivered before the last frame");
                got = out;
            }
            assert_eq!(got.as_deref(), Some(&packet[..]), "{len}");
            assert!(!r.is_holding(), "{len}");
        }
    }

    /// A two-frame packet comes back whole, with the signal averaged over
    /// its two frames, and nothing is handed up after the first.
    #[test]
    fn two_frames_are_joined_and_their_signal_averaged() {
        let packet = packet_of(400);
        let frames = frames_of(&packet, 3);
        let mut r = Reassembler::new(1_000_000);
        let first = Signal {
            rssi_dbm: -70,
            snr_quarter_db: 20,
        };
        let second = Signal {
            rssi_dbm: -60,
            snr_quarter_db: 30,
        };
        assert!(r.feed(&frames[0], first, 0).is_none());
        assert!(r.is_holding());
        let got = r.feed(&frames[1], second, 500_000).expect("the packet");
        assert_eq!(got.payload, &packet[..]);
        assert_eq!(
            got.signal,
            Signal {
                rssi_dbm: -65,
                snr_quarter_db: 25
            }
        );
    }

    /// A stock RNode's own cut, written out by hand rather than through
    /// `Split`, so the split point is pinned by the test and not by the code
    /// under it: 254 bytes of packet in the first frame, the rest in the
    /// second, both behind the same header.
    #[test]
    fn a_stock_rnodes_split_is_reassembled() {
        let packet = packet_of(300);
        let hdr = (0xA << 4) | 0x01;
        let mut f1 = vec![hdr];
        f1.extend_from_slice(&packet[..254]);
        let mut f2 = vec![hdr];
        f2.extend_from_slice(&packet[254..]);
        assert_eq!(f1.len(), 255);
        let mut r = Reassembler::new(1_000_000);
        assert!(r.feed(&f1, SIGNAL, 0).is_none());
        let got = r.feed(&f2, SIGNAL, 1000).expect("the packet");
        assert_eq!(got.payload, &packet[..]);
        // And a stock single frame: a header with the flag clear.
        let mut f = vec![0x70u8];
        f.extend_from_slice(&packet[..50]);
        assert_eq!(
            r.feed(&f, SIGNAL, 2000).map(|p| p.payload.to_vec()),
            Some(packet[..50].to_vec())
        );
    }

    /// The second half never comes. Nothing is handed up for the first, and
    /// the next packet -- a whole one, or a split with a different nibble --
    /// is delivered on its own with the stale half thrown away, not joined
    /// to it.
    #[test]
    fn a_missing_second_frame_delivers_nothing_and_does_not_taint_the_next() {
        let lost = frames_of(&packet_of(400), 1);
        let whole = frames_of(&packet_of(100), 2);
        let mut r = Reassembler::new(1_000_000);
        assert!(r.feed(&lost[0], SIGNAL, 0).is_none());
        assert!(r.is_holding());
        let got = r.feed(&whole[0], SIGNAL, 1000).expect("the whole packet");
        assert_eq!(got.payload, &packet_of(100)[..]);
        assert!(!r.is_holding());

        // A split with a different nibble replaces the held half.
        let other = frames_of(&packet_of(300), 7);
        assert!(r.feed(&lost[0], SIGNAL, 2000).is_none());
        assert!(r.feed(&other[0], SIGNAL, 3000).is_none());
        let got = r.feed(&other[1], SIGNAL, 4000).expect("the other packet");
        assert_eq!(got.payload, &packet_of(300)[..]);
    }

    /// The first half never came. The second is held as if it were a first
    /// half, which is all a receiver can do with it, and is never delivered
    /// by itself.
    #[test]
    fn a_missing_first_frame_is_held_and_never_delivered_alone() {
        let frames = frames_of(&packet_of(400), 4);
        let mut r = Reassembler::new(1_000_000);
        assert!(r.feed(&frames[1], SIGNAL, 0).is_none());
        assert!(r.is_holding());
        let whole = frames_of(&packet_of(10), 5);
        let got = r.feed(&whole[0], SIGNAL, 1000).expect("the whole packet");
        assert_eq!(got.payload.len(), 10);
    }

    /// Out of order is the same as a missing first: the frames of one packet
    /// are indistinguishable by header, so a reassembler joins them in the
    /// order they arrive. The point of the test is that it does not panic
    /// and does not deliver something of the wrong length.
    #[test]
    fn out_of_order_frames_join_in_arrival_order() {
        let packet = packet_of(400);
        let frames = frames_of(&packet, 4);
        let mut r = Reassembler::new(1_000_000);
        assert!(r.feed(&frames[1], SIGNAL, 0).is_none());
        let got = r.feed(&frames[0], SIGNAL, 1000).expect("joined");
        assert_eq!(got.payload.len(), 400);
        assert_ne!(got.payload, &packet[..]);
        assert_eq!(&got.payload[..146], &packet[254..]);
    }

    /// A first half that has waited longer than the maximum age is not
    /// completed by a later split with the same nibble; that one is a new
    /// first half.
    #[test]
    fn a_stale_first_half_is_not_completed() {
        let a = frames_of(&packet_of(400), 6);
        let b = frames_of(&packet_of(300), 6);
        let mut r = Reassembler::new(2_000_000);
        assert!(r.feed(&a[0], SIGNAL, 0).is_none());
        // Within the age: joined.
        let got = r.feed(&a[1], SIGNAL, 2_000_000).expect("in time");
        assert_eq!(got.payload, &packet_of(400)[..]);
        // Past it: a new first half, and then its own second.
        assert!(r.feed(&a[0], SIGNAL, 10_000_000).is_none());
        assert!(r.feed(&b[0], SIGNAL, 12_000_001).is_none());
        assert!(r.is_holding());
        let got = r.feed(&b[1], SIGNAL, 12_100_000).expect("b");
        assert_eq!(got.payload, &packet_of(300)[..]);
        // A reconfiguration clears it outright.
        assert!(r.feed(&a[0], SIGNAL, 20_000_000).is_none());
        r.clear();
        assert!(!r.is_holding());
        assert!(r.feed(&a[1], SIGNAL, 20_000_100).is_none());
    }

    /// The sequence steps through all sixteen nibbles and wraps, so two
    /// consecutive packets never share one -- and the header of each packet
    /// carries the nibble it was given.
    #[test]
    fn the_sequence_nibble_steps_and_wraps() {
        let mut s = Sequence::new(0xF3);
        let nibbles: Vec<u8> = (0..32).map(|_| s.take()).collect();
        assert_eq!(nibbles[0], 3, "seeded from the low nibble");
        assert!(nibbles.iter().all(|&n| n < 16));
        for w in nibbles.windows(2) {
            assert_ne!(w[0], w[1]);
        }
        let mut seen = nibbles[..16].to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..16).collect::<Vec<u8>>());
        assert_eq!(&nibbles[..16], &nibbles[16..]);

        let mut s = Sequence::new(0);
        let first = Split::new(b"one", s.take()).unwrap().header();
        let second = Split::new(b"two", s.take()).unwrap().header();
        assert_eq!(sequence_of(first), 0);
        assert_eq!(sequence_of(second), 1);
        assert_ne!(first, second);
    }

    /// Nothing in a frame is not a frame, and a header with nothing behind
    /// it is not a packet.
    #[test]
    fn empty_frames_and_empty_packets_are_nothing() {
        let mut r = Reassembler::new(1_000_000);
        assert!(r.feed(&[], SIGNAL, 0).is_none());
        assert!(r.feed(&[header(1, false)], SIGNAL, 0).is_none());
        // A held half is discarded by the empty whole packet all the same.
        let frames = frames_of(&packet_of(400), 2);
        assert!(r.feed(&frames[0], SIGNAL, 0).is_none());
        assert!(r.feed(&[header(1, false)], SIGNAL, 1).is_none());
        assert!(!r.is_holding());
    }

    /// A second half longer than there is room for cannot overrun: the
    /// packet is cut at the maximum.
    #[test]
    fn an_overlong_join_is_cut_at_the_maximum() {
        let mut r = Reassembler::new(1_000_000);
        let mut f = vec![header(2, true)];
        f.extend_from_slice(&[1u8; 254]);
        assert!(r.feed(&f, SIGNAL, 0).is_none());
        let mut g = vec![header(2, true)];
        g.extend_from_slice(&[2u8; 254]);
        g.push(3);
        let got = r.feed(&g, SIGNAL, 1).expect("joined");
        assert_eq!(got.payload.len(), PACKET_MAX);
    }

    /// The age a first half is held for is two airtimes of a full frame and
    /// a second, at the configuration in force.
    #[test]
    fn the_maximum_age_follows_the_airtime() {
        let spike = ValidConfig::new(DEFAULT).unwrap();
        assert_eq!(
            max_age_us(&spike),
            2 * spike.airtime_us(255) as u64 + 1_000_000
        );
        let slow = ValidConfig::new(RadioConfig {
            spreading_factor: 12,
            bandwidth_hz: 62_500,
            ..DEFAULT
        })
        .unwrap();
        assert!(max_age_us(&slow) > max_age_us(&spike));
        assert!(max_age_us(&slow) < 60_000_000, "under a minute");
    }
}
