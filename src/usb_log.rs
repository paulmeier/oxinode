//! Ships defmt output to the host over a CDC-ACM serial port.
//!
//! Shared by every firmware image, because with no debug probe this is the only
//! way anything says anything. See `src/logger.rs` for the buffer it drains.

use crate::logger;
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{ControlChanged, Receiver, Sender};
use embassy_usb::driver::Driver;

/// Full-speed bulk endpoints are 64 bytes; anything larger is silently clamped.
pub const MAX_PACKET_SIZE: u16 = 64;

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
            if tx.write_packet(&buf[..n]).await.is_err() {
                break; // host went away
            }
            // A full-size packet needs a zero-length packet behind it, or the
            // host keeps waiting for the rest of the transfer.
            if n == MAX_PACKET_SIZE as usize && tx.write_packet(&[]).await.is_err() {
                break;
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
