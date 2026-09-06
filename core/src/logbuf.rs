//! A byte ring buffer sitting between the defmt logger and the USB link.
//!
//! Logging has to work from anywhere, including interrupt context, and must
//! never block: the transport is USB, which cannot be driven from inside a
//! critical section. So the logger only ever writes into this buffer, and a
//! separate task drains it to the host whenever the host is listening.
//!
//! When the buffer fills, the **oldest** bytes are discarded rather than the
//! newest. That is the right way round for a diagnostic log: with no host
//! attached the buffer fills within seconds of boot, and dropping new writes
//! would leave you staring at boot messages while the thing you actually want
//! to watch scrolls into the void. Keeping the newest means attaching a
//! terminal always shows recent activity.
//!
//! The cost is that an overwrite can truncate a defmt frame mid-flight. defmt's
//! wire format is frame-delimited, so the decoder resynchronises at the next
//! frame boundary; the damage is bounded to one message. [`RingBuffer::dropped`]
//! keeps the loss visible so it is never silent.

/// A fixed-capacity byte ring buffer with oldest-first eviction.
pub struct RingBuffer<const N: usize> {
    buf: [u8; N],
    read: usize,
    len: usize,
    dropped: u32,
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RingBuffer<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            read: 0,
            len: 0,
            dropped: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append bytes, evicting the oldest when full.
    ///
    /// Returns how many bytes were evicted by this call. Those bytes are also
    /// added to the running [`RingBuffer::dropped`] total.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let mut evicted = 0;
        for &byte in data {
            if self.len == N {
                self.read = (self.read + 1) % N;
                self.len -= 1;
                evicted += 1;
            }
            self.buf[(self.read + self.len) % N] = byte;
            self.len += 1;
        }
        self.dropped = self.dropped.saturating_add(evicted as u32);
        evicted
    }

    /// Copy out up to `out.len()` bytes, oldest first. Returns how many.
    pub fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for (i, slot) in out.iter_mut().take(n).enumerate() {
            *slot = self.buf[(self.read + i) % N];
        }
        self.read = (self.read + n) % N;
        self.len -= n;
        n
    }

    /// Total bytes lost to eviction since the last [`RingBuffer::take_dropped`].
    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Read and clear the dropped-byte counter.
    pub fn take_dropped(&mut self) -> u32 {
        core::mem::replace(&mut self.dropped, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain<const N: usize>(rb: &mut RingBuffer<N>) -> Vec<u8> {
        let mut out = vec![0; rb.len()];
        let n = rb.pop(&mut out);
        out.truncate(n);
        out
    }

    #[test]
    fn starts_empty() {
        let rb = RingBuffer::<8>::new();
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.capacity(), 8);
        assert_eq!(rb.dropped(), 0);
    }

    #[test]
    fn round_trips_bytes_in_order() {
        let mut rb = RingBuffer::<8>::new();
        assert_eq!(rb.push(b"abc"), 0);
        assert_eq!(rb.len(), 3);
        assert_eq!(drain(&mut rb), b"abc");
        assert!(rb.is_empty());
    }

    #[test]
    fn pop_from_empty_yields_nothing() {
        let mut rb = RingBuffer::<8>::new();
        let mut out = [0u8; 4];
        assert_eq!(rb.pop(&mut out), 0);
    }

    #[test]
    fn partial_pop_leaves_the_rest_in_order() {
        let mut rb = RingBuffer::<8>::new();
        rb.push(b"abcdef");
        let mut out = [0u8; 2];
        assert_eq!(rb.pop(&mut out), 2);
        assert_eq!(&out, b"ab");
        assert_eq!(drain(&mut rb), b"cdef");
    }

    #[test]
    fn wraps_around_the_end_of_the_backing_array() {
        // Push and drain most of the buffer so the next write straddles the
        // wrap point, which is where an off-by-one would show up.
        let mut rb = RingBuffer::<8>::new();
        rb.push(b"123456");
        assert_eq!(drain(&mut rb), b"123456");
        rb.push(b"abcde");
        assert_eq!(rb.len(), 5);
        assert_eq!(drain(&mut rb), b"abcde");
        assert_eq!(rb.dropped(), 0, "nothing should have been evicted");
    }

    #[test]
    fn fills_exactly_to_capacity_without_evicting() {
        let mut rb = RingBuffer::<4>::new();
        assert_eq!(rb.push(b"abcd"), 0);
        assert_eq!(rb.len(), 4);
        assert_eq!(rb.dropped(), 0);
        assert_eq!(drain(&mut rb), b"abcd");
    }

    #[test]
    fn evicts_the_oldest_not_the_newest() {
        // The whole point: a log with no reader must keep recent activity, not
        // preserve boot messages forever.
        let mut rb = RingBuffer::<4>::new();
        rb.push(b"abcd");
        assert_eq!(rb.push(b"ef"), 2);
        assert_eq!(drain(&mut rb), b"cdef");
    }

    #[test]
    fn a_write_larger_than_the_buffer_keeps_its_tail() {
        let mut rb = RingBuffer::<4>::new();
        assert_eq!(rb.push(b"abcdefgh"), 4);
        assert_eq!(rb.len(), 4);
        assert_eq!(drain(&mut rb), b"efgh");
    }

    #[test]
    fn dropped_counter_accumulates_and_resets() {
        let mut rb = RingBuffer::<4>::new();
        rb.push(b"abcd");
        rb.push(b"e");
        rb.push(b"f");
        assert_eq!(rb.dropped(), 2);
        assert_eq!(rb.take_dropped(), 2);
        assert_eq!(rb.dropped(), 0, "taking the count must clear it");
    }

    #[test]
    fn a_drain_that_keeps_up_loses_nothing() {
        // The normal case: a host is attached and reading. The logger writes
        // small bursts, the drain task consumes at least as fast, and the
        // stream crosses the wrap point many times.
        let mut rb = RingBuffer::<16>::new();
        let mut expected = Vec::new();
        let mut got = Vec::new();
        for i in 0..200u32 {
            let chunk = [i as u8, (i >> 8) as u8, 0xAA];
            rb.push(&chunk);
            expected.extend_from_slice(&chunk);
            let mut out = [0u8; 4];
            let n = rb.pop(&mut out);
            got.extend_from_slice(&out[..n]);
        }
        got.extend(drain(&mut rb));
        assert_eq!(
            rb.dropped(),
            0,
            "a drain reading 4 must outpace 3-byte writes"
        );
        assert_eq!(got, expected);
    }

    #[test]
    fn a_drain_that_falls_behind_accounts_for_every_byte() {
        // The abnormal case, and the one that matters: nothing is reading fast
        // enough. Bytes may be evicted, but none may be invented or duplicated,
        // and the loss must be counted rather than silent.
        let mut rb = RingBuffer::<16>::new();
        let mut pushed = 0usize;
        let mut received = 0usize;
        for i in 0..200u32 {
            let chunk = [i as u8, (i >> 8) as u8, 0xAA];
            rb.push(&chunk);
            pushed += chunk.len();
            let mut out = [0u8; 2];
            received += rb.pop(&mut out);
        }
        let dropped = rb.dropped() as usize;
        assert!(
            dropped > 0,
            "this drain cannot keep up; eviction is expected"
        );
        assert_eq!(
            received + dropped + rb.len(),
            pushed,
            "every byte written must be delivered, evicted, or still buffered"
        );
    }
}
