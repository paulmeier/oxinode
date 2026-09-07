//! Response frames waiting to go to a host.
//!
//! Whole frames or nothing. A byte ring that dropped its oldest bytes — which
//! is right for a log, and is what [`crate::logbuf`] does — would splice two
//! frames together here and hand the host a packet made of two halves. So an
//! overflowing frame is dropped entire and counted.
//!
//! This holds the bytes and knows nothing about how they leave. USB writes
//! them in 64-byte packets and needs a zero-length packet after a full one;
//! Bluetooth writes them in notifications of whatever the negotiated MTU
//! allows. Both ask the same question — "give me the next chunk of at most
//! this many bytes" — and that is the whole interface.

use crate::rnode::command;
use crate::rnode::protocol::Sink;

/// Frames queued for one transport.
pub struct Outbox<const N: usize> {
    buf: [u8; N],
    len: usize,
    dropped: u32,
}

impl<const N: usize> Default for Outbox<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Outbox<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            dropped: 0,
        }
    }

    /// Bytes queued and not yet taken.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Everything queued, in order.
    pub fn pending(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Bytes `sent` so far have left; slide the rest down.
    ///
    /// Taking from the front rather than handing out an iterator, because the
    /// transport's write is asynchronous and may fail partway: what it has
    /// actually delivered is what it tells us, and nothing else moves.
    pub fn consume(&mut self, sent: usize) {
        let sent = sent.min(self.len);
        self.buf.copy_within(sent..self.len, 0);
        self.len -= sent;
    }

    /// Throw everything away: the host this was for has gone.
    ///
    /// Counted as one drop, because from the host's side it is one
    /// conversation that ended, however many frames were in it.
    pub fn abandon(&mut self) {
        if self.len > 0 {
            self.dropped += 1;
        }
        self.len = 0;
    }

    /// Frames lost since this was last asked, and clears the count.
    pub fn take_dropped(&mut self) -> u32 {
        core::mem::replace(&mut self.dropped, 0)
    }
}

impl<const N: usize> Sink for Outbox<N> {
    fn frame(&mut self, cmd: u8, payload: &[u8]) {
        let need = command::response_len(cmd, payload);
        if self.len + need > N {
            self.dropped += 1;
            return;
        }
        match command::encode_response(cmd, payload, &mut self.buf[self.len..]) {
            Some(n) => self.len += n,
            None => self.dropped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rnode::kiss::FEND;

    #[test]
    fn a_frame_is_queued_whole() {
        let mut out = Outbox::<64>::new();
        out.frame(0x01, &[0xAA, 0xBB]);
        assert_eq!(out.pending(), &[FEND, 0x01, 0xAA, 0xBB, FEND]);
        assert_eq!(out.take_dropped(), 0);
    }

    #[test]
    fn a_frame_that_does_not_fit_is_dropped_whole_and_counted() {
        let mut out = Outbox::<8>::new();
        out.frame(0x01, &[1, 2]); // 5 bytes
        out.frame(0x02, &[3, 4]); // would be 10: dropped
        assert_eq!(out.pending(), &[FEND, 0x01, 1, 2, FEND]);
        assert_eq!(out.take_dropped(), 1);
        assert_eq!(out.take_dropped(), 0);
    }

    #[test]
    fn escaped_length_is_what_counts_against_the_capacity() {
        // Two FENDs escape to four bytes; with the delimiters and the command
        // byte the frame is 3 + 4 = 7, which fits in 7 exactly and not in 6.
        let mut fits = Outbox::<7>::new();
        fits.frame(0x00, &[FEND, FEND]);
        assert_eq!(fits.len(), 7);
        let mut not = Outbox::<6>::new();
        not.frame(0x00, &[FEND, FEND]);
        assert!(not.is_empty());
        assert_eq!(not.take_dropped(), 1);
    }

    #[test]
    fn consume_takes_from_the_front_and_keeps_order() {
        let mut out = Outbox::<64>::new();
        out.frame(0x01, &[1]);
        out.frame(0x02, &[2]);
        let all: Vec<u8> = out.pending().to_vec();
        out.consume(3);
        assert_eq!(out.pending(), &all[3..]);
        out.consume(100); // more than is there: everything goes, nothing panics
        assert!(out.is_empty());
    }

    #[test]
    fn abandon_counts_as_one_drop_only_if_something_was_waiting() {
        let mut out = Outbox::<64>::new();
        out.abandon();
        assert_eq!(out.take_dropped(), 0);
        out.frame(0x01, &[1]);
        out.frame(0x02, &[2]);
        out.abandon();
        assert!(out.is_empty());
        assert_eq!(out.take_dropped(), 1);
    }

    #[test]
    fn a_transport_can_drain_in_any_chunk_size() {
        // What both USB (64) and BLE (MTU - 3) do: take the front, send it,
        // consume what went. The reassembly is byte-exact for any chunking.
        let mut out = Outbox::<256>::new();
        for i in 0..10u8 {
            out.frame(0x10 + i, &[i; 7]);
        }
        let expected = out.pending().to_vec();
        for chunk in [1usize, 5, 20, 64, 300] {
            let mut again = Outbox::<256>::new();
            for i in 0..10u8 {
                again.frame(0x10 + i, &[i; 7]);
            }
            let mut got = Vec::new();
            while !again.is_empty() {
                let n = again.pending().len().min(chunk);
                got.extend_from_slice(&again.pending()[..n]);
                again.consume(n);
            }
            assert_eq!(got, expected, "chunk size {chunk}");
        }
    }
}
