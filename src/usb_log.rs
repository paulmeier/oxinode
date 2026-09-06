//! Ships defmt output to the host over a CDC-ACM serial port.
//!
//! Shared by every firmware image, because with no debug probe this is the only
//! way anything says anything. See `src/logger.rs` for the buffer it drains.

use crate::logger;
use embassy_time::{with_timeout, Duration, Timer};
use embassy_usb::class::cdc_acm::{ControlChanged, Receiver, Sender};
use embassy_usb::driver::Driver;

/// Full-speed bulk endpoints are 64 bytes; anything larger is silently clamped.
pub const MAX_PACKET_SIZE: u16 = 64;

/// How long to wait for the host to collect a log packet before giving up on
/// it. Long enough that a busy host is not punished, short enough that a port
/// nobody has open costs one packet rather than the whole log.
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);

/// Drain the log buffer to `tx` forever.
///
/// # The `ready` gate
///
/// [`Sender::wait_connection`] resolves at `SET_CONFIGURATION` — when the host
/// *enumerates* the device — not when someone opens the tty. Draining from that
/// moment writes the whole startup log into a port with no reader, and the host
/// discards it. Whether that matters depends on the image:
///
/// * an image whose interesting output is continuous can pass `|| true` and
///   rely on the next line arriving;
/// * an image whose interesting output is a one-shot startup sequence has to
///   wait, and should pass something like `|| control.dtr()`.
///
/// DTR is only usable on the *first* CDC function in a composite device; on the
/// second it never goes true. That is a constraint on the layout, not a choice.
pub async fn pump<'d, D: Driver<'d>>(tx: &mut Sender<'d, D>, ready: impl Fn() -> bool) -> ! {
    let mut buf = [0u8; MAX_PACKET_SIZE as usize];
    loop {
        tx.wait_connection().await;
        loop {
            if !ready() {
                Timer::after(Duration::from_millis(20)).await;
                continue;
            }

            // Losing bytes is expected whenever nothing is listening, but it
            // must never be silent -- say so on the way back up.
            let lost = logger::take_dropped();
            if lost > 0 {
                defmt::warn!("log buffer overflowed, {=u32} bytes lost", lost);
            }

            let n = logger::drain(&mut buf);
            if n == 0 {
                // The logger runs inside a critical section and cannot wake a
                // task, so this polls. 20 ms is invisible on a log.
                Timer::after(Duration::from_millis(20)).await;
                continue;
            }
            // Bounded, because `write_packet` waits for the host to collect
            // the data and a host with the port closed never does. Unbounded,
            // the pump parks on its first write and the port stays silent even
            // after somebody attaches a terminal -- which is exactly what it
            // did, and it cost a debugging session that had no log to read.
            //
            // Dropping the chunk on a timeout is consistent with the buffer
            // behind it, which already discards the oldest bytes when nothing
            // is listening. A log is lossy by construction; a log that wedges
            // is not a log.
            match with_timeout(WRITE_TIMEOUT, tx.write_packet(&buf[..n])).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => break, // host went away
                Err(_) => continue,  // nobody is collecting; drop it and move on
            }
            // A full-size packet needs a zero-length packet behind it, or the
            // host keeps waiting for the rest of the transfer.
            if n == MAX_PACKET_SIZE as usize {
                match with_timeout(WRITE_TIMEOUT, tx.write_packet(&[])).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => break,
                    Err(_) => continue,
                }
            }
        }
    }
}

/// Read the current line state and ask [`oxinode_core::usb::is_bootloader_touch`]
/// what it means. The rule itself is tested on the host; this is just the part
/// that has to touch the USB stack.
pub fn is_bootloader_touch<'d, D: Driver<'d>>(
    rx: &Receiver<'d, D>,
    control: &ControlChanged<'d>,
) -> bool {
    oxinode_core::usb::is_bootloader_touch(rx.line_coding().data_rate(), control.dtr())
}

/// The same question, asked from the other half of the port.
///
/// Both halves read the same line coding, so this is not a different check --
/// it exists because an image that hands its `Receiver` to a dedicated reader
/// task no longer has one to ask.
pub fn is_bootloader_touch_tx<'d, D: Driver<'d>>(
    tx: &Sender<'d, D>,
    control: &ControlChanged<'d>,
) -> bool {
    oxinode_core::usb::is_bootloader_touch(tx.line_coding().data_rate(), control.dtr())
}
