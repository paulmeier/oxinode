//! The part of LXMF the board speaks: a message in one packet, either way.
//!
//! `rns-core` has no LXMF, so this is written here, from LXMF 1.1.1's
//! `LXMessage.unpack_from_bytes` and `LXMessage.pack`, and tested against
//! messages LXMF itself packed (`tools/lxmf_vectors.py`). It is the
//! opportunistic subset: a message small enough for one packet, encrypted to
//! an `lxmf.delivery` destination, with no link. Links, resources and
//! propagation nodes are later work (#67), and a message that needs them
//! never reaches this, nor leaves it: [`pack`] refuses one.
//!
//! # The wire format
//!
//! LXMF packs a message as the destination hash, the source hash, the
//! signature and a msgpack payload. Sent opportunistically, **the destination
//! hash is not sent**: the packet is addressed to it already, so LXMF strips
//! those sixteen bytes before encrypting, and the receiver puts them back
//! before reading. What the board decrypts is therefore
//!
//! ```text
//! source (16) | signature (64) | payload (msgpack)
//! ```
//!
//! and the payload is `[timestamp, title, content, fields]`, with a stamp as a
//! fifth element when the sender made one. The message hash is SHA-256 over
//! the destination, the source and the payload *without* the stamp, and the
//! signature is the sender's Ed25519 over those bytes followed by the hash.
//!
//! LXMF finds the stampless payload by unpacking and packing again. That is
//! not done here: msgpack writes an array as its header and then each element
//! in turn, so the four-element payload is the five-element one's bytes with
//! the header changed and the stamp cut off, and taking it that way needs no
//! encoder that agrees with Python's to the byte.
//!
//! # Untrusted bytes, read in place
//!
//! Anyone who has heard the board's announce can encrypt to it, so what this
//! reads is a stranger's. It is walked in place rather than decoded into
//! values: `rns-core`'s msgpack decoder reserves room for every array and map
//! before reading it, which a few hundred bytes of nested headers can turn
//! into more heap than the board has, and an allocation that fails is a
//! reboot. Here nothing is allocated but the two strings kept, and nesting
//! past [`MAX_DEPTH`] is refused.
//!
//! # What is kept
//!
//! The title and the content, as text; when the sender says it was sent, in
//! whole seconds; whether it carried a stamp; and whether it carried anything
//! the board does not show ([`Message::attachment`]). The fields themselves
//! are not kept. The board cannot show an image, a file or a voice message,
//! and saying one came is more honest than saying nothing.
//!
//! # What the board sends
//!
//! What LXMF packs, as LXMF packs it: the timestamp as a double, the title
//! and the content as bytes, and an empty map of fields. The board writes no
//! title, as the phones write none. It is signed once, by [`pack`], and
//! encrypted afresh for every attempt, as LXMF encrypts a retry.
//!
//! One packet carries [`ENCRYPTED_MDU`] bytes of what is encrypted, which is
//! [`MAX_CONTENT`] bytes of text with no title. That is a little less than
//! LXMF's own limit for one packet, 295 bytes: LXMF sizes its limit for a
//! packet with one address, and one sent through a transport node carries a
//! second, which past 287 bytes makes the packet longer than Reticulum
//! sends. The board's limit is the one that always fits.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use rns_crypto::identity::Identity;
use rns_crypto::sha256::sha256;

/// The address and the source, each a truncated hash.
pub const HASH_LEN: usize = 16;
/// An Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;
/// The least an opportunistic message can be: a source, a signature, and a
/// payload of at least an array header.
pub const MIN_LEN: usize = HASH_LEN + SIGNATURE_LEN + 1;

/// How deep the fields may nest before the message is refused. LXMF's own
/// fields nest three deep at most (an attachment list of name and bytes
/// pairs); this is room for that and for what comes after.
pub const MAX_DEPTH: usize = 8;

/// The fields LXMF defines that describe a message rather than carry
/// something with it: how to draw the sender's icon, the thread it is in, a
/// stamp ticket for replies, how to render the content, and what it replies
/// to. Every other field, LXMF's or anybody's, is something the board does
/// not show, and makes the message one with an attachment.
pub const DESCRIPTIVE_FIELDS: [u64; 5] = [
    0x04, // FIELD_ICON_APPEARANCE
    0x08, // FIELD_THREAD
    0x0c, // FIELD_TICKET
    0x0f, // FIELD_RENDERER
    0x30, // FIELD_REPLY_TO
];

/// The most one packet to a single destination carries encrypted, with two
/// addresses in its header: Reticulum's `Packet.ENCRYPTED_MDU`.
pub const ENCRYPTED_MDU: usize = rns_core::constants::ENCRYPTED_MDU;

/// The most text a message the board sends can carry, in bytes of UTF-8,
/// with no title: what makes [`ENCRYPTED_MDU`] exactly. A source, a
/// signature, then the payload: an array header, a double, an empty title,
/// the content as bin16 and an empty map.
pub const MAX_CONTENT: usize = ENCRYPTED_MDU - (HASH_LEN + SIGNATURE_LEN + 1 + 9 + 2 + 3 + 1);

/// A message the board would send that does not fit in one packet, by this
/// many bytes. It is refused, never cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong {
    pub over: usize,
}

/// A message the board has packed and signed: what is encrypted for every
/// attempt, and the hash its proofs and its retries are known by.
#[derive(Clone, PartialEq, Eq)]
pub struct Outbound {
    /// The message hash: the same for every attempt.
    pub hash: [u8; 32],
    /// The recipient's `lxmf.delivery` address.
    pub destination: [u8; HASH_LEN],
    /// The source, the signature and the payload: what LXMF encrypts, with
    /// the destination stripped as it is on the air.
    plaintext: Vec<u8>,
}

impl core::fmt::Debug for Outbound {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Outbound")
            .field("hash", &self.hash)
            .field("destination", &self.destination)
            .field("len", &self.plaintext.len())
            .finish()
    }
}

impl Outbound {
    /// What is encrypted to the recipient.
    pub fn plaintext(&self) -> &[u8] {
        &self.plaintext
    }
}

/// A message from `identity`, whose `lxmf.delivery` address is `source`, to
/// `destination`: `content`, no title, no fields, stamped `timestamp_s` Unix
/// seconds. Packed and signed as LXMF's `LXMessage.pack` does.
pub fn pack(
    identity: &Identity,
    source: &[u8; HASH_LEN],
    destination: &[u8; HASH_LEN],
    timestamp_s: u64,
    content: &str,
) -> Result<Outbound, TooLong> {
    if content.len() > MAX_CONTENT {
        return Err(TooLong {
            over: content.len() - MAX_CONTENT,
        });
    }
    let mut payload = Vec::with_capacity(content.len() + 16);
    payload.push(0x94);
    // The one float the board writes, made here from whole seconds and
    // written straight out: LXMF stamps every message with `time.time()`.
    payload.push(0xcb);
    payload.extend_from_slice(&(timestamp_s as f64).to_be_bytes());
    payload.extend_from_slice(&[0xc4, 0x00]);
    let len = content.len();
    if len <= usize::from(u8::MAX) {
        payload.extend_from_slice(&[0xc4, len as u8]);
    } else {
        payload.push(0xc5);
        payload.extend_from_slice(&(len as u16).to_be_bytes());
    }
    payload.extend_from_slice(content.as_bytes());
    payload.push(0x80);

    let mut signed = Vec::with_capacity(2 * HASH_LEN + payload.len() + 32);
    signed.extend_from_slice(destination);
    signed.extend_from_slice(source);
    signed.extend_from_slice(&payload);
    let hash = sha256(&signed);
    signed.extend_from_slice(&hash);
    // A key that cannot sign is a board with no private key, which a node
    // never is; an empty signature would be refused by the recipient.
    let signature = identity.sign(&signed).unwrap_or([0; SIGNATURE_LEN]);

    let mut plaintext = Vec::with_capacity(HASH_LEN + SIGNATURE_LEN + payload.len());
    plaintext.extend_from_slice(source);
    plaintext.extend_from_slice(&signature);
    plaintext.extend_from_slice(&payload);
    debug_assert!(plaintext.len() <= ENCRYPTED_MDU);
    Ok(Outbound {
        hash,
        destination: *destination,
        plaintext,
    })
}

/// Why a message could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// Shorter than a source and a signature.
    Short,
    /// The payload is not an array of four or five elements.
    Payload,
    /// The timestamp is not a number.
    Timestamp,
    /// The title or the content is not bytes or a string.
    Text,
    /// The fields are not a map, or are not msgpack.
    Fields,
    /// Something after the payload, or a payload cut short.
    Length,
}

/// One message, as it arrived. See the module docs.
#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    /// The message hash: what LXMF calls the message ID, and what a retry
    /// of the same message has again.
    pub hash: [u8; 32],
    /// The sender's `lxmf.delivery` address.
    pub source: [u8; HASH_LEN],
    /// When the sender says it sent it, in whole Unix seconds. `None` for a
    /// timestamp that is negative, not finite, or too large to be a time.
    pub sent_s: Option<u64>,
    pub title: String,
    pub content: String,
    /// It carried a field the board does not show: an image, a file, a voice
    /// message, telemetry. See [`DESCRIPTIVE_FIELDS`].
    pub attachment: bool,
    /// It carried a stamp. The board asks for none and does not check one.
    pub stamped: bool,
    signature: [u8; SIGNATURE_LEN],
    /// What the signature covers: the destination, the source and the
    /// stampless payload, then the hash. Kept until the message is verified,
    /// because the sender's key may not have been heard yet.
    signed: Vec<u8>,
}

impl core::fmt::Debug for Message {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Message")
            .field("hash", &self.hash)
            .field("source", &self.source)
            .field("sent_s", &self.sent_s)
            .field("title", &self.title)
            .field("content", &self.content)
            .field("attachment", &self.attachment)
            .field("stamped", &self.stamped)
            .finish_non_exhaustive()
    }
}

impl Message {
    /// A message to `destination`, from what the packet decrypted to: the
    /// source, the signature and the payload, with the destination stripped
    /// as it is on the air.
    pub fn unpack(destination: &[u8; HASH_LEN], plaintext: &[u8]) -> Result<Self, Malformed> {
        if plaintext.len() < MIN_LEN {
            return Err(Malformed::Short);
        }
        let mut source = [0u8; HASH_LEN];
        source.copy_from_slice(&plaintext[..HASH_LEN]);
        let mut signature = [0u8; SIGNATURE_LEN];
        signature.copy_from_slice(&plaintext[HASH_LEN..HASH_LEN + SIGNATURE_LEN]);
        let payload = &plaintext[HASH_LEN + SIGNATURE_LEN..];

        let (count, mut at) = match payload[0] {
            0x94 => (4, 1),
            0x95 => (5, 1),
            // A sixteen-bit array header with a count of four or five: legal
            // msgpack, though no packer writes one for so few.
            0xdc => match payload.get(1..3) {
                Some([0, n @ (4 | 5)]) => (usize::from(*n), 3),
                _ => return Err(Malformed::Payload),
            },
            _ => return Err(Malformed::Payload),
        };
        let sent_s = timestamp(payload, &mut at)?;
        let title = text(payload, &mut at)?;
        let content = text(payload, &mut at)?;
        let attachment = fields(payload, &mut at)?;
        let stampless_end = at;
        if count == 5 {
            at = skip(payload, at, 0).ok_or(Malformed::Length)?;
        }
        if at != payload.len() {
            return Err(Malformed::Length);
        }

        // The stampless payload, as LXMF packs it again: a four-element
        // array header and the first four elements as they came.
        let mut signed = Vec::with_capacity(2 * HASH_LEN + payload.len() + 32);
        signed.extend_from_slice(destination);
        signed.extend_from_slice(&source);
        signed.push(0x94);
        let first = if payload[0] == 0xdc { 3 } else { 1 };
        signed.extend_from_slice(&payload[first..stampless_end]);
        let hash = sha256(&signed);
        signed.extend_from_slice(&hash);

        Ok(Self {
            hash,
            source,
            sent_s,
            title,
            content,
            attachment,
            stamped: count == 5,
            signature,
            signed,
        })
    }

    /// Whether `public_key` -- announced for the source -- is the source's,
    /// and signed this message.
    pub fn verify(&self, public_key: &[u8; 64]) -> bool {
        let Ok(identity) = Identity::from_public_key(public_key) else {
            return false;
        };
        let address =
            rns_core::destination::destination_hash("lxmf", &["delivery"], Some(identity.hash()));
        address == self.source && identity.verify(&self.signature, &self.signed)
    }

    /// The signed bytes are no longer needed: the message has been checked,
    /// one way or the other, and it keeps only what it shows.
    pub fn settle(&mut self) {
        self.signed = Vec::new();
    }

    /// Whether [`Message::settle`] has been called.
    pub fn settled(&self) -> bool {
        self.signed.is_empty()
    }
}

/// A msgpack length that follows a header byte: one, two or four bytes, big
/// endian.
fn length(data: &[u8], at: usize, bytes: usize) -> Option<usize> {
    let raw = data.get(at..at + bytes)?;
    Some(raw.iter().fold(0usize, |n, &b| (n << 8) | usize::from(b)))
}

/// The timestamp: LXMF writes `time.time()`, a float, and an integer is as
/// good. Read at the boundary into whole seconds, and no float kept.
fn timestamp(data: &[u8], at: &mut usize) -> Result<Option<u64>, Malformed> {
    let bad = Malformed::Timestamp;
    let head = *data.get(*at).ok_or(bad)?;
    let (seconds, len) = match head {
        0x00..=0x7f => (Some(u64::from(head)), 1),
        0xe0..=0xff => (None, 1),
        0xcc..=0xcf => {
            let n = 1 << (head - 0xcc);
            (Some(length(data, *at + 1, n).ok_or(bad)? as u64), 1 + n)
        }
        0xd0..=0xd3 => {
            let n = 1usize << (head - 0xd0);
            let raw = data.get(*at + 1..*at + 1 + n).ok_or(bad)?;
            // Sign-extended from its top bit; only the non-negative are times.
            let negative = raw[0] & 0x80 != 0;
            let value = raw.iter().fold(0u64, |v, &b| (v << 8) | u64::from(b));
            ((!negative).then_some(value), 1 + n)
        }
        0xca => {
            let raw = data.get(*at + 1..*at + 5).ok_or(bad)?;
            let f = f32::from_be_bytes(raw.try_into().map_err(|_| bad)?);
            (whole_seconds(f64::from(f)), 5)
        }
        0xcb => {
            let raw = data.get(*at + 1..*at + 9).ok_or(bad)?;
            let f = f64::from_be_bytes(raw.try_into().map_err(|_| bad)?);
            (whole_seconds(f), 9)
        }
        _ => return Err(bad),
    };
    *at += len;
    Ok(seconds)
}

/// A float's whole seconds, if it is a time at all.
fn whole_seconds(f: f64) -> Option<u64> {
    // 2^63: comfortably past any time, and below where `as` saturates.
    (f.is_finite() && (0.0..9.2e18).contains(&f)).then_some(f as u64)
}

/// A title or the content: bytes, as LXMF writes them, or a string, as some
/// senders do. Text that is not UTF-8 is kept with what does not decode
/// replaced, and draws as the hollow box.
fn text(data: &[u8], at: &mut usize) -> Result<String, Malformed> {
    let bad = Malformed::Text;
    let head = *data.get(*at).ok_or(bad)?;
    let (len, start) = match head {
        0xa0..=0xbf => (usize::from(head & 0x1f), 1),
        0xc4 | 0xd9 => (length(data, *at + 1, 1).ok_or(bad)?, 2),
        0xc5 | 0xda => (length(data, *at + 1, 2).ok_or(bad)?, 3),
        0xc6 | 0xdb => (length(data, *at + 1, 4).ok_or(bad)?, 5),
        _ => return Err(bad),
    };
    let body = data
        .get(*at + start..)
        .and_then(|rest| rest.get(..len))
        .ok_or(bad)?;
    *at += start + len;
    Ok(String::from_utf8_lossy(body).into_owned())
}

/// The fields: a map, walked to its end. Says whether any key is one the
/// board does not show.
fn fields(data: &[u8], at: &mut usize) -> Result<bool, Malformed> {
    let bad = Malformed::Fields;
    let head = *data.get(*at).ok_or(bad)?;
    let (count, mut i) = match head {
        0x80..=0x8f => (usize::from(head & 0x0f), *at + 1),
        0xde => (length(data, *at + 1, 2).ok_or(bad)?, *at + 3),
        0xdf => (length(data, *at + 1, 4).ok_or(bad)?, *at + 5),
        // No fields at all: LXMF writes an empty map, but nil means the same.
        0xc0 => {
            *at += 1;
            return Ok(false);
        }
        _ => return Err(bad),
    };
    let mut attachment = false;
    for _ in 0..count {
        let key = match *data.get(i).ok_or(bad)? {
            k @ 0x00..=0x7f => Some(u64::from(k)),
            0xcc => data.get(i + 1).map(|&k| u64::from(k)),
            _ => None,
        };
        attachment |= !key.is_some_and(|k| DESCRIPTIVE_FIELDS.contains(&k));
        i = skip(data, i, 1).ok_or(bad)?;
        i = skip(data, i, 1).ok_or(bad)?;
    }
    *at = i;
    Ok(attachment)
}

/// Where the msgpack value at `at` ends, without decoding it. `None` for
/// bytes that are not msgpack, a value cut short, or nesting past
/// [`MAX_DEPTH`].
fn skip(data: &[u8], at: usize, depth: usize) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }
    let head = *data.get(at)?;
    let (items, start) = match head {
        0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => return Some(at + 1),
        0xa0..=0xbf => return end(data, at + 1 + usize::from(head & 0x1f)),
        0xc4 | 0xd9 => return end(data, at + 2 + length(data, at + 1, 1)?),
        0xc5 | 0xda => return end(data, at + 3 + length(data, at + 1, 2)?),
        0xc6 | 0xdb => return end(data, at + 5 + length(data, at + 1, 4)?),
        0xcc | 0xd0 => return end(data, at + 2),
        0xcd | 0xd1 => return end(data, at + 3),
        0xce | 0xd2 | 0xca => return end(data, at + 5),
        0xcf | 0xd3 | 0xcb => return end(data, at + 9),
        // fixext 1, 2, 4, 8, 16: a type byte and the data.
        0xd4 => return end(data, at + 3),
        0xd5 => return end(data, at + 4),
        0xd6 => return end(data, at + 6),
        0xd7 => return end(data, at + 10),
        0xd8 => return end(data, at + 18),
        0xc7 => return end(data, at + 3 + length(data, at + 1, 1)?),
        0xc8 => return end(data, at + 4 + length(data, at + 1, 2)?),
        0xc9 => return end(data, at + 6 + length(data, at + 1, 4)?),
        0x90..=0x9f => (usize::from(head & 0x0f), at + 1),
        0xdc => (length(data, at + 1, 2)?, at + 3),
        0xdd => (length(data, at + 1, 4)?, at + 5),
        0x80..=0x8f => (2 * usize::from(head & 0x0f), at + 1),
        0xde => (2 * length(data, at + 1, 2)?, at + 3),
        0xdf => (2usize.checked_mul(length(data, at + 1, 4)?)?, at + 5),
        // 0xc1 is never used.
        0xc1 => return None,
    };
    // A count larger than the bytes left needs no check of its own: the
    // walk stops at the first element that runs off the end.
    let mut i = start;
    for _ in 0..items {
        i = skip(data, i, depth + 1)?;
    }
    Some(i)
}

/// `at`, if it is within the data.
fn end(data: &[u8], at: usize) -> Option<usize> {
    (at <= data.len()).then_some(at)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::node::key::NodeKey;
    use alloc::vec;

    const VECTORS: &str = include_str!("vectors/lxmf.txt");
    const ANNOUNCES: &str = include_str!("vectors/vectors.txt");

    pub(crate) fn vector(name: &str) -> Vec<u8> {
        let line = VECTORS
            .lines()
            .chain(ANNOUNCES.lines())
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("{name} is not in the vectors"));
        (0..line.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hash16(name: &str) -> [u8; 16] {
        vector(name).try_into().unwrap()
    }

    /// The board in the vectors: `bytes(range(64, 128))`.
    pub(crate) fn board() -> NodeKey {
        NodeKey::from_private(core::array::from_fn(|i| 64 + i as u8))
    }

    /// The sender's public key, as its announce carried it.
    fn sender_key() -> [u8; 64] {
        vector("public_key").try_into().unwrap()
    }

    /// What a packet in the vectors decrypts to, by the board's key.
    pub(crate) fn opened(packet: &[u8]) -> Vec<u8> {
        let raw = rns_core::packet::RawPacket::unpack(packet).unwrap();
        board().identity().decrypt(raw.data()).unwrap()
    }

    fn read(file: &[u8]) -> Message {
        Message::unpack(&hash16("board"), &opened(file)).unwrap()
    }

    #[test]
    fn the_board_in_the_vectors_is_the_one_python_sent_to() {
        assert_eq!(board().hashes().lxmf, hash16("board"));
    }

    #[test]
    fn a_message_from_python_reads_as_lxmf_reads_it() {
        let message = read(include_bytes!("vectors/message.bin"));
        assert_eq!(message.hash.as_slice(), vector("message_hash"));
        assert_eq!(message.source, hash16("sender"));
        assert_eq!(
            message.sent_s,
            Some(1_792_800_000),
            "the quarter second goes"
        );
        assert_eq!(message.title, "");
        assert_eq!(message.content.as_bytes(), vector("content"));
        assert!(!message.attachment);
        assert!(!message.stamped);
        assert!(message.verify(&sender_key()));
    }

    #[test]
    fn the_destination_is_not_on_the_air_and_goes_back_on_for_the_hash() {
        let plaintext = opened(include_bytes!("vectors/message.bin"));
        // What decrypted starts with the source, not the board's address.
        assert_eq!(plaintext[..16], hash16("sender"));
        assert!(!plaintext
            .windows(16)
            .any(|w| w == hash16("board").as_slice()));
        // And read as if it were addressed elsewhere, the hash is another.
        let elsewhere = Message::unpack(&[0; 16], &plaintext).unwrap();
        assert_ne!(elsewhere.hash.as_slice(), vector("message_hash"));
        assert!(!elsewhere.verify(&sender_key()));
    }

    #[test]
    fn a_retry_is_another_packet_and_the_same_message() {
        let first = include_bytes!("vectors/message.bin");
        let again = include_bytes!("vectors/message_retry.bin");
        assert_ne!(first[..], again[..]);
        assert_eq!(read(first).hash, read(again).hash);
    }

    #[test]
    fn a_title_and_fields_are_read_and_an_image_is_an_attachment() {
        let message = read(include_bytes!("vectors/message_fields.bin"));
        assert_eq!(message.hash.as_slice(), vector("fields_hash"));
        assert_eq!(message.title, "Photo");
        assert_eq!(message.content, "see attached");
        assert!(message.attachment);
        assert!(message.verify(&sender_key()));
    }

    #[test]
    fn a_stamp_is_left_out_of_the_hash_as_lxmf_leaves_it() {
        let plaintext = opened(include_bytes!("vectors/message_stamped.bin"));
        assert!(plaintext.ends_with(&vector("stamp")));
        let message = Message::unpack(&hash16("board"), &plaintext).unwrap();
        assert!(message.stamped);
        assert_eq!(message.hash.as_slice(), vector("stamped_hash"));
        assert_eq!(message.content, "stamped");
        assert!(message.verify(&sender_key()));
    }

    #[test]
    fn a_message_does_not_verify_against_another_key() {
        let stranger = read(include_bytes!("vectors/stranger_message.bin"));
        assert_eq!(stranger.source, hash16("stranger"));
        assert_eq!(stranger.hash.as_slice(), vector("stranger_hash"));
        assert!(
            !stranger.verify(&sender_key()),
            "the sender's key, not the stranger's"
        );
        // A key that signed nothing here, whose own address is the board's.
        let board = board().identity().get_public_key().unwrap();
        assert!(!read(include_bytes!("vectors/message.bin")).verify(&board));
    }

    #[test]
    fn the_key_must_be_the_sources_own_as_well_as_the_signers() {
        // The signature holds for the sender's key whatever the message is
        // said to be from afterwards; the key's own address is what ties
        // the two together.
        let mut message = read(include_bytes!("vectors/message.bin"));
        message.source = hash16("stranger");
        assert!(!message.verify(&sender_key()));
    }

    #[test]
    fn any_changed_byte_of_what_is_signed_fails_verification() {
        let plaintext = opened(include_bytes!("vectors/message.bin"));
        for i in 0..plaintext.len() {
            let mut damaged = plaintext.clone();
            damaged[i] ^= 0x01;
            if let Ok(message) = Message::unpack(&hash16("board"), &damaged) {
                assert!(
                    !message.verify(&sender_key()),
                    "byte {i} changed and it still verified"
                );
            }
        }
    }

    #[test]
    fn settling_drops_what_was_kept_to_verify() {
        let mut message = read(include_bytes!("vectors/message.bin"));
        assert!(!message.settled());
        message.settle();
        assert!(message.settled());
        assert!(!message.verify(&sender_key()), "nothing left to check");
        assert_eq!(message.content.as_bytes(), vector("content"));
    }

    /// A plaintext with the vectors' source and signature and this payload.
    fn with_payload(payload: &[u8]) -> Vec<u8> {
        let mut out = opened(include_bytes!("vectors/message.bin"))[..80].to_vec();
        out.extend_from_slice(payload);
        out
    }

    fn unpacked(payload: &[u8]) -> Result<Message, Malformed> {
        Message::unpack(&[0; 16], &with_payload(payload))
    }

    #[test]
    fn what_is_not_a_message_is_refused_and_not_a_panic() {
        let plaintext = opened(include_bytes!("vectors/message.bin"));
        for len in 0..plaintext.len() {
            assert!(
                Message::unpack(&[0; 16], &plaintext[..len]).is_err(),
                "{len} bytes"
            );
        }
        let mut longer = plaintext.clone();
        longer.push(0xc0);
        assert_eq!(
            Message::unpack(&[0; 16], &longer).err(),
            Some(Malformed::Length)
        );
        assert_eq!(
            unpacked(&[0x93, 0, 0xc4, 0, 0xc4, 0]).err(),
            Some(Malformed::Payload)
        );
        assert_eq!(unpacked(&[0x96]).err(), Some(Malformed::Payload));
        assert_eq!(
            unpacked(&[0x94, 0xc4, 0, 0xc4, 0, 0xc4, 0, 0x80]).err(),
            Some(Malformed::Timestamp)
        );
        assert_eq!(
            unpacked(&[0x94, 0, 0x01, 0xc4, 0, 0x80]).err(),
            Some(Malformed::Text)
        );
        assert_eq!(
            unpacked(&[0x94, 0, 0xc4, 0, 0xc4, 0, 0x90]).err(),
            Some(Malformed::Fields)
        );
        assert_eq!(
            unpacked(&[0x94, 0, 0xc4, 9, b'a', 0xc4, 0, 0x80]).err(),
            Some(Malformed::Text),
            "a title longer than what is left"
        );
    }

    #[test]
    fn timestamps_of_every_shape_and_none_that_is_not_a_time() {
        let sent = |ts: &[u8]| {
            let mut payload = vec![0x94];
            payload.extend_from_slice(ts);
            payload.extend_from_slice(&[0xc4, 0, 0xc4, 0, 0x80]);
            unpacked(&payload).map(|m| m.sent_s)
        };
        assert_eq!(sent(&[0x07]), Ok(Some(7)));
        assert_eq!(
            sent(&[0xce, 0x6a, 0xdb, 0x7e, 0x80]),
            Ok(Some(1_792_769_664))
        );
        assert_eq!(
            sent(&[0xd3, 0, 0, 0, 0, 0x6a, 0xdb, 0x7e, 0x80]),
            Ok(Some(1_792_769_664))
        );
        assert_eq!(sent(&[0xd0, 0xff]), Ok(None), "negative");
        assert_eq!(sent(&[0xff]), Ok(None), "negative fixint");
        let mut f = vec![0xcb];
        f.extend_from_slice(&1_792_800_000.9f64.to_be_bytes());
        assert_eq!(sent(&f), Ok(Some(1_792_800_000)));
        let mut nan = vec![0xcb];
        nan.extend_from_slice(&f64::NAN.to_be_bytes());
        assert_eq!(sent(&nan), Ok(None));
        let mut small = vec![0xca];
        small.extend_from_slice(&1024.5f32.to_be_bytes());
        assert_eq!(sent(&small), Ok(Some(1024)));
        let mut huge = vec![0xcb];
        huge.extend_from_slice(&1e300f64.to_be_bytes());
        assert_eq!(sent(&huge), Ok(None));
    }

    #[test]
    fn text_may_be_a_string_and_need_not_be_utf8() {
        let message = unpacked(&[0x94, 0, 0xa2, b'h', b'i', 0xc4, 2, 0xc3, 0x28, 0x80]).unwrap();
        assert_eq!(message.title, "hi");
        assert_eq!(message.content, "\u{fffd}(");
    }

    #[test]
    fn only_fields_the_board_does_not_show_are_attachments() {
        let attachment = |map: &[u8]| {
            let mut payload = vec![0x94, 0, 0xc4, 0, 0xc4, 0];
            payload.extend_from_slice(map);
            unpacked(&payload).map(|m| m.attachment)
        };
        assert_eq!(attachment(&[0x80]), Ok(false));
        assert_eq!(attachment(&[0xc0]), Ok(false), "nil is no fields");
        // A renderer and an icon describe the message.
        assert_eq!(attachment(&[0x82, 0x0f, 0x00, 0x04, 0x90]), Ok(false));
        // Telemetry, a file list, audio, an unknown key, a string key.
        for map in [
            &[0x81, 0x02, 0xc4, 0][..],
            &[0x81, 0x05, 0x91, 0x92, 0xa1, b'f', 0xc4, 0],
            &[0x81, 0x07, 0x92, 0x10, 0xc4, 0],
            &[0x81, 0xcc, 0xfb, 0xc0],
            &[0x81, 0xa1, b'x', 0xc0],
        ] {
            assert_eq!(attachment(map), Ok(true), "{map:02x?}");
        }
    }

    #[test]
    fn nesting_past_the_limit_is_refused_and_lies_about_length_too() {
        let deep = |n: usize| {
            let mut payload = vec![0x94, 0, 0xc4, 0, 0xc4, 0, 0x81, 0x02];
            payload.extend(core::iter::repeat_n(0x91, n));
            payload.push(0xc0);
            unpacked(&payload).map(|m| m.attachment)
        };
        assert_eq!(deep(MAX_DEPTH - 1), Ok(true));
        assert_eq!(deep(MAX_DEPTH), Err(Malformed::Fields));
        assert_eq!(deep(200), Err(Malformed::Fields));
        // An array that claims four billion elements, in a few bytes.
        assert_eq!(
            unpacked(&[0x94, 0, 0xc4, 0, 0xc4, 0, 0x81, 0x02, 0xdd, 0xff, 0xff, 0xff, 0xff]).err(),
            Some(Malformed::Fields)
        );
        assert_eq!(
            unpacked(&[0x94, 0, 0xc4, 0, 0xc4, 0, 0xdf, 0xff, 0xff, 0xff, 0xff]).err(),
            Some(Malformed::Fields)
        );
    }

    #[test]
    fn skip_knows_every_msgpack_type() {
        // One of each, packed, and where each ends.
        let values: &[&[u8]] = &[
            &[0x05],
            &[0xe5],
            &[0xc0],
            &[0xc2],
            &[0xc3],
            &[0xa2, b'a', b'b'],
            &[0xd9, 1, b'a'],
            &[0xda, 0, 1, b'a'],
            &[0xdb, 0, 0, 0, 1, b'a'],
            &[0xc4, 1, 0],
            &[0xc5, 0, 1, 0],
            &[0xc6, 0, 0, 0, 1, 0],
            &[0xcc, 1],
            &[0xcd, 1, 2],
            &[0xce, 1, 2, 3, 4],
            &[0xcf, 1, 2, 3, 4, 5, 6, 7, 8],
            &[0xd0, 1],
            &[0xd1, 1, 2],
            &[0xd2, 1, 2, 3, 4],
            &[0xd3, 1, 2, 3, 4, 5, 6, 7, 8],
            &[0xca, 1, 2, 3, 4],
            &[0xcb, 1, 2, 3, 4, 5, 6, 7, 8],
            &[0xd4, 1, 0],
            &[0xd5, 1, 0, 0],
            &[0xd6, 1, 0, 0, 0, 0],
            &[0xd7, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0xd8, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0xc7, 1, 1, 0],
            &[0xc8, 0, 1, 1, 0],
            &[0xc9, 0, 0, 0, 1, 1, 0],
            &[0x92, 0x01, 0xc0],
            &[0xdc, 0, 1, 0x01],
            &[0xdd, 0, 0, 0, 1, 0x01],
            &[0x81, 0x01, 0x02],
            &[0xde, 0, 1, 0x01, 0x02],
            &[0xdf, 0, 0, 0, 1, 0x01, 0x02],
        ];
        for value in values {
            assert_eq!(skip(value, 0, 0), Some(value.len()), "{value:02x?}");
            assert_eq!(
                skip(&value[..value.len() - 1], 0, 0),
                None,
                "{value:02x?} cut"
            );
        }
        assert_eq!(skip(&[0xc1], 0, 0), None);
    }

    /// The board's message to the bench peer, read back by the code that
    /// reads the peer's messages to the board, with the addresses swapped.
    fn packed(content: &str) -> Outbound {
        let key = board();
        pack(
            &key.identity(),
            &key.hashes().lxmf,
            &hash16("sender"),
            1_792_800_000,
            content,
        )
        .unwrap()
    }

    #[test]
    fn what_the_board_packs_reads_and_verifies_as_what_it_receives() {
        let out = packed("Hello from the board");
        let back = Message::unpack(&hash16("sender"), out.plaintext()).unwrap();
        assert_eq!(back.hash, out.hash);
        assert_eq!(back.source, board().hashes().lxmf);
        assert_eq!(back.sent_s, Some(1_792_800_000));
        assert_eq!(
            (back.title.as_str(), back.content.as_str()),
            ("", "Hello from the board")
        );
        assert!(!back.attachment && !back.stamped);
        let key = board().identity().get_public_key().unwrap();
        assert!(back.verify(&key), "signed by the board's key");
        assert!(!back.verify(&sender_key()));
        // Addressed to somebody else, the hash and the signature are another.
        let elsewhere = Message::unpack(&hash16("stranger"), out.plaintext()).unwrap();
        assert!(!elsewhere.verify(&key));
    }

    #[test]
    fn the_payload_is_lxmfs_layout() {
        let out = packed("hi");
        let payload = &out.plaintext()[HASH_LEN + SIGNATURE_LEN..];
        let mut want = alloc::vec![0x94, 0xcb];
        want.extend_from_slice(&1_792_800_000f64.to_be_bytes());
        want.extend_from_slice(&[0xc4, 0, 0xc4, 2, b'h', b'i', 0x80]);
        assert_eq!(payload, want);
        // Signing is deterministic: the same message is the same bytes.
        assert_eq!(packed("hi"), out);
    }

    #[test]
    fn the_longest_message_fills_a_packet_and_one_byte_more_is_refused() {
        assert_eq!(MAX_CONTENT, 287);
        let longest = "x".repeat(MAX_CONTENT);
        assert_eq!(packed(&longest).plaintext().len(), ENCRYPTED_MDU);
        // Past 255 bytes the content is bin16, and still read back.
        let back = Message::unpack(&hash16("sender"), packed(&longest).plaintext()).unwrap();
        assert_eq!(back.content, longest);
        let key = board();
        let over = pack(
            &key.identity(),
            &key.hashes().lxmf,
            &hash16("sender"),
            0,
            &"x".repeat(MAX_CONTENT + 3),
        );
        assert_eq!(over.err(), Some(TooLong { over: 3 }));
        // A byte is a byte: a character the font lacks counts its UTF-8.
        let wide = "é".repeat(MAX_CONTENT / 2 + 1);
        assert!(matches!(
            pack(
                &key.identity(),
                &key.hashes().lxmf,
                &hash16("sender"),
                0,
                &wide
            ),
            Err(TooLong { over: 1 })
        ));
    }

    #[test]
    fn debug_shows_the_message_not_the_signature() {
        let shown = format!("{:?}", read(include_bytes!("vectors/message.bin")));
        assert!(shown.contains("content"));
        assert!(!shown.contains("signature") && !shown.contains("signed"));
    }
}
