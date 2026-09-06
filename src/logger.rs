//! `defmt` global logger, buffered for delivery over USB.
//!
//! A logger is required here rather than optional: the `lr11xx` driver calls
//! `defmt::debug!` internally and `defmt` is a mandatory dependency of it with
//! no feature to switch off, so the radio firmware will not *link* without a
//! `#[defmt::global_logger]`. There is no debug probe on this board, so
//! `defmt-rtt` is no use and the log has to leave over USB.
//!
//! USB cannot be driven from inside a critical section, and logging must work
//! from interrupt context, so the two are decoupled: this logger only appends
//! encoded bytes to a ring buffer, and [`drain`] hands them to a task that owns
//! the USB endpoint. See `oxinode_core::logbuf` for what happens when nothing
//! is reading.

use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::RestoreState;
use oxinode_core::logbuf::RingBuffer;

/// Roughly a second of chatty bring-up logging at 115200-equivalent rates, and
/// small enough to be unremarkable against 256 KB of RAM.
const CAPACITY: usize = 4096;

static mut BUFFER: RingBuffer<CAPACITY> = RingBuffer::new();
static mut ENCODER: defmt::Encoder = defmt::Encoder::new();
static mut RESTORE: RestoreState = RestoreState::invalid();

/// Guards against a log call made from inside another log call, which would
/// interleave two frames into one and corrupt both.
static TAKEN: AtomicBool = AtomicBool::new(false);

// Every frame carries the time since boot, in microseconds.
//
// defmt requires *a* timestamp definition -- the link fails with an undefined
// `_defmt_timestamp` without one -- but microsecond uptime is worth having for
// its own sake. Phase 3 spends its time waiting on BUSY and on TX airtime, and
// both are far easier to reason about when the log says how long they took.
defmt::timestamp!("{=u64:us}", embassy_time::Instant::now().as_micros());

#[defmt::global_logger]
struct Logger;

unsafe impl defmt::Logger for Logger {
    fn acquire() {
        // SAFETY: paired with the release() below, which restores this token.
        let restore = unsafe { critical_section::acquire() };

        if TAKEN.load(Ordering::Relaxed) {
            // Re-entered from inside a log call. Nothing sensible can be
            // written, and panicking inside the logger would be worse than a
            // lost message.
            unsafe { critical_section::release(restore) };
            return;
        }
        TAKEN.store(true, Ordering::Relaxed);

        // SAFETY: the critical section makes this the only accessor, and the
        // re-entrancy check above rules out the one case it does not cover.
        unsafe {
            RESTORE = restore;
            (*core::ptr::addr_of_mut!(ENCODER)).start_frame(append);
        }
    }

    unsafe fn write(bytes: &[u8]) {
        if !TAKEN.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: only reachable between acquire() and release(), which hold
        // the critical section.
        unsafe { (*core::ptr::addr_of_mut!(ENCODER)).write(bytes, append) };
    }

    unsafe fn release() {
        if !TAKEN.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: as above; still inside the critical section taken by acquire.
        unsafe { (*core::ptr::addr_of_mut!(ENCODER)).end_frame(append) };
        TAKEN.store(false, Ordering::Relaxed);
        // SAFETY: restores exactly the token acquire() stored.
        unsafe { critical_section::release(RESTORE) };
    }

    unsafe fn flush() {
        // Deliberately a no-op. Flushing would mean waiting for the USB task to
        // drain the buffer, and this can be called from a critical section with
        // interrupts masked -- the task could never run, so it would hang.
    }
}

/// Append encoded bytes to the buffer. Only called with the critical section held.
fn append(bytes: &[u8]) {
    // SAFETY: reachable only from the Logger impl, inside its critical section.
    unsafe {
        (*core::ptr::addr_of_mut!(BUFFER)).push(bytes);
    }
}

/// Move buffered log bytes into `out`, returning how many were copied.
///
/// Called from the USB task. Takes its own brief critical section so a log call
/// from an interrupt cannot tear a read.
pub fn drain(out: &mut [u8]) -> usize {
    critical_section::with(|_| {
        // SAFETY: the critical section makes this the only accessor.
        unsafe { (*core::ptr::addr_of_mut!(BUFFER)).pop(out) }
    })
}

/// How many bytes are waiting to be drained. Diagnostic.
pub fn pending() -> usize {
    critical_section::with(|_| {
        // SAFETY: the critical section makes this the only accessor.
        unsafe { (*core::ptr::addr_of_mut!(BUFFER)).len() }
    })
}

/// Bytes lost to a full buffer since this was last called, and clears the count.
pub fn take_dropped() -> u32 {
    critical_section::with(|_| {
        // SAFETY: as above.
        unsafe { (*core::ptr::addr_of_mut!(BUFFER)).take_dropped() }
    })
}
