//! The display half of the RNode protocol, on a panel twice the size it
//! assumes.
//!
//! A Reticulum host decides this device has a display purely from its platform
//! byte — `RNodeInterface` sets `self.display = True` for any nRF52 — and then
//! offers three things: an **external framebuffer** an application can push
//! pictures into, a way to read that back, and a way to read what is actually
//! on the screen.
//!
//! # Two buffers, two shapes, and neither is this panel
//!
//! | | size | layout |
//! |---|---|---|
//! | external framebuffer | 64 × 64, 512 bytes | **row-major**, 8 bytes a row |
//! | display readback | 128 × 64, 1024 bytes | **page-major**, a byte is 8 rows |
//! | this panel | 128 × 128, 2048 bytes | page-major |
//!
//! Those are not guesses. The host writes the framebuffer a line at a time as
//! `[line, 8 bytes]` and reads it back by accumulating until it has 512; it
//! reads the display by accumulating until it has 1024. Both counts are exact
//! and both are hard-coded on the host, so a device that sends a different
//! number of bytes is a device whose frame never completes.
//!
//! So the panel has to be presented as something it is not, twice over, and
//! each direction gets the answer that loses least:
//!
//! * **the framebuffer is drawn at double size.** 64 × 64 doubled is exactly
//!   128 × 128, so a picture an application pushes fills the panel with no
//!   cropping and no interpolation — every source pixel becomes a 2 × 2 block.
//!   Showing it at 1:1 in a quarter of the screen would waste three quarters
//!   of a display that somebody is looking at.
//!
//! * **the readback is halved vertically, by OR.** Sending only the top half
//!   would be true about half the screen and silent about the rest; folding
//!   pairs of rows together keeps *everything that is lit* visible in a
//!   recognisable rendition of the whole screen. It is lossy and it is
//!   documented; the alternative was lossy and would have looked complete.

use crate::sh1107::{self, Frame};

/// Width of the external framebuffer, in pixels.
pub const FB_WIDTH: usize = 64;
/// Height of the external framebuffer, in pixels.
pub const FB_HEIGHT: usize = 64;
/// Bytes in one row of it. The host writes exactly this many per line.
pub const FB_BYTES_PER_LINE: usize = FB_WIDTH / 8;
/// The whole external framebuffer. The host reads back exactly this many bytes.
pub const FB_LEN: usize = FB_BYTES_PER_LINE * FB_HEIGHT;

/// Width of the display image the host expects to read back.
pub const DISP_WIDTH: usize = 128;
/// Height of it. Half this panel.
pub const DISP_HEIGHT: usize = 64;
/// The whole display readback. The host accumulates exactly this many bytes.
pub const DISP_LEN: usize = DISP_WIDTH * DISP_HEIGHT / 8;
/// Pages in the readback image.
pub const DISP_PAGES: usize = DISP_HEIGHT / 8;

/// How much bigger the panel is than the framebuffer, in each direction.
pub const FB_SCALE: usize = sh1107::WIDTH / FB_WIDTH;

/// A picture pushed in by the host, and whether it is the one being shown.
#[derive(Clone)]
pub struct External {
    buf: [u8; FB_LEN],
    enabled: bool,
}

impl Default for External {
    fn default() -> Self {
        Self::new()
    }
}

impl External {
    pub const fn new() -> Self {
        Self {
            buf: [0; FB_LEN],
            enabled: false,
        }
    }

    /// Whether the host has asked for this to be shown instead of the device's
    /// own page: `CMD_FB_EXT`.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// The bytes `CMD_FB_READ` answers with.
    pub const fn as_bytes(&self) -> &[u8; FB_LEN] {
        &self.buf
    }

    /// Store one row: `CMD_FB_WRITE`, whose payload is `[line, 8 bytes]`.
    ///
    /// Returns whether it was stored. A line past the end of the buffer is
    /// refused rather than wrapped — a host that miscounts should lose a row,
    /// not overwrite a different one.
    pub fn write_line(&mut self, line: u8, data: &[u8]) -> bool {
        let line = line as usize;
        if line >= FB_HEIGHT || data.len() < FB_BYTES_PER_LINE {
            return false;
        }
        let at = line * FB_BYTES_PER_LINE;
        self.buf[at..at + FB_BYTES_PER_LINE].copy_from_slice(&data[..FB_BYTES_PER_LINE]);
        true
    }

    /// One pixel of the stored picture.
    ///
    /// Row-major, and the **most significant bit is the leftmost pixel**,
    /// which is how every 1-bit-per-pixel image format written a byte at a
    /// time is laid out.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= FB_WIDTH || y >= FB_HEIGHT {
            return false;
        }
        let byte = self.buf[y * FB_BYTES_PER_LINE + x / 8];
        byte & (0x80 >> (x % 8)) != 0
    }

    /// Draw it onto the panel at double size, filling the screen.
    pub fn draw(&self, frame: &mut Frame) {
        for y in 0..FB_HEIGHT {
            for x in 0..FB_WIDTH {
                let on = self.pixel(x, y);
                for dy in 0..FB_SCALE {
                    for dx in 0..FB_SCALE {
                        frame.set_pixel(x * FB_SCALE + dx, y * FB_SCALE + dy, on);
                    }
                }
            }
        }
    }
}

/// Render what is on the panel into the 1024 bytes `CMD_DISP_READ` expects.
///
/// Page-major, a byte being eight rows with the top row in bit 0 — the layout
/// of an SSD1306's display RAM, which is what the hosts that read this were
/// written against.
///
/// Pairs of panel rows are OR-ed together, so a pixel that is lit anywhere
/// survives. See the module docs for why halving beats truncating.
pub fn read_display(frame: &Frame, out: &mut [u8; DISP_LEN]) {
    *out = [0; DISP_LEN];
    for page in 0..DISP_PAGES {
        for column in 0..DISP_WIDTH {
            let mut byte = 0u8;
            for bit in 0..8 {
                let y = (page * 8 + bit) * 2;
                if frame.pixel(column, y) || frame.pixel(column, y + 1) {
                    byte |= 1 << bit;
                }
            }
            out[page * DISP_WIDTH + column] = byte;
        }
    }
}

// The doubling has to be exact in both directions, or a picture is cropped or
// letterboxed and the constant above is a lie.
const _: () = assert!(FB_WIDTH * FB_SCALE == sh1107::WIDTH);
const _: () = assert!(FB_HEIGHT * FB_SCALE == sh1107::HEIGHT);
// And the readback has to be exactly half the panel, or the fold below is not
// a fold.
const _: () = assert!(DISP_WIDTH == sh1107::WIDTH);
const _: () = assert!(DISP_HEIGHT * 2 == sh1107::HEIGHT);
// Both lengths are hard-coded on the host. A device that sends a different
// number of bytes sends a frame that never completes.
const _: () = assert!(FB_LEN == 512);
const _: () = assert!(DISP_LEN == 1024);

#[cfg(test)]
mod tests {
    use super::*;

    /// The two byte counts the host counts to. Everything else here is
    /// negotiable; these are not.
    #[test]
    fn the_lengths_are_the_ones_the_host_waits_for() {
        assert_eq!(FB_LEN, 512);
        assert_eq!(DISP_LEN, 1024);
        assert_eq!(FB_BYTES_PER_LINE, 8);
    }

    #[test]
    fn a_written_line_reads_back_as_pixels() {
        let mut fb = External::new();
        // 0x80 is the leftmost pixel of the row, 0x01 the eighth.
        assert!(fb.write_line(3, &[0x80, 0, 0, 0, 0, 0, 0, 0x01]));
        assert!(fb.pixel(0, 3));
        assert!(!fb.pixel(1, 3));
        assert!(fb.pixel(63, 3));
        assert!(!fb.pixel(62, 3));
        assert!(!fb.pixel(0, 2), "the wrong row was written");
    }

    /// A line past the end is refused rather than wrapped. A host that
    /// miscounts should lose a row, not silently overwrite a different one.
    #[test]
    fn a_line_off_the_end_is_refused() {
        let mut fb = External::new();
        assert!(!fb.write_line(64, &[0xFF; 8]));
        assert!(!fb.write_line(255, &[0xFF; 8]));
        assert!(fb.as_bytes().iter().all(|&b| b == 0));
    }

    /// A short payload is refused too, rather than being padded with whatever
    /// was there.
    #[test]
    fn a_short_line_is_refused() {
        let mut fb = External::new();
        assert!(!fb.write_line(0, &[0xFF; 7]));
        assert!(!fb.write_line(0, &[]));
        assert!(fb.as_bytes().iter().all(|&b| b == 0));
    }

    /// Every one of the 512 bytes is reachable and comes back unchanged: this
    /// is what `CMD_FB_READ` hands the host.
    #[test]
    fn the_whole_framebuffer_round_trips() {
        let mut fb = External::new();
        for line in 0..FB_HEIGHT {
            let row = [line as u8; FB_BYTES_PER_LINE];
            assert!(fb.write_line(line as u8, &row));
        }
        for line in 0..FB_HEIGHT {
            let at = line * FB_BYTES_PER_LINE;
            assert_eq!(
                &fb.as_bytes()[at..at + FB_BYTES_PER_LINE],
                &[line as u8; FB_BYTES_PER_LINE]
            );
        }
    }

    /// Doubling is exact: one source pixel becomes a 2 x 2 block, and 64 x 64
    /// fills 128 x 128 with nothing cropped and nothing left over.
    #[test]
    fn the_framebuffer_is_drawn_at_double_size() {
        let mut fb = External::new();
        fb.write_line(0, &[0x80, 0, 0, 0, 0, 0, 0, 0]);
        fb.write_line(63, &[0, 0, 0, 0, 0, 0, 0, 0x01]);

        let mut frame = Frame::new();
        fb.draw(&mut frame);

        // Top-left source pixel -> the 2x2 block at the panel's top-left.
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert!(frame.pixel(x, y), "({x},{y})");
        }
        assert!(!frame.pixel(2, 0));
        assert!(!frame.pixel(0, 2));

        // Bottom-right source pixel -> the panel's bottom-right corner.
        for (x, y) in [(126, 126), (127, 126), (126, 127), (127, 127)] {
            assert!(frame.pixel(x, y), "({x},{y})");
        }
        assert!(!frame.pixel(125, 127));
    }

    /// Drawing replaces what was there rather than merging with it, so a new
    /// picture does not show the old one through it.
    #[test]
    fn drawing_replaces_rather_than_overlays() {
        let mut frame = Frame::new();
        frame.fill(true);
        let fb = External::new();
        fb.draw(&mut frame);
        assert!(
            frame.as_bytes().iter().all(|&b| b == 0),
            "an empty picture should clear the panel"
        );
    }

    /// A pixel lit anywhere on the panel survives the halving. That is the
    /// property that makes the readback a recognisable rendition rather than
    /// a sampled one -- a one-pixel-tall line would vanish half the time under
    /// sampling.
    #[test]
    fn nothing_lit_disappears_from_the_readback() {
        for y in [0usize, 1, 2, 63, 64, 126, 127] {
            let mut frame = Frame::new();
            frame.set_pixel(37, y, true);
            let mut out = [0u8; DISP_LEN];
            read_display(&frame, &mut out);

            let row = y / 2;
            let page = row / 8;
            let bit = row % 8;
            assert_ne!(
                out[page * DISP_WIDTH + 37] & (1 << bit),
                0,
                "a pixel at y={y} vanished"
            );
            // And nothing else lit up.
            let lit: usize = out.iter().map(|b| b.count_ones() as usize).sum();
            assert_eq!(lit, 1, "y={y} lit {lit} bits");
        }
    }

    /// The readback is page-major with the top row in bit 0, which is the
    /// layout of the SSD1306 RAM the hosts were written against. Getting this
    /// upside down would show the mirror image of the screen.
    #[test]
    fn the_readback_is_in_the_hosts_layout() {
        let mut frame = Frame::new();
        // The panel's top-left pixel.
        frame.set_pixel(0, 0, true);
        let mut out = [0u8; DISP_LEN];
        read_display(&frame, &mut out);
        assert_eq!(out[0], 0x01, "top-left belongs in byte 0, bit 0");

        // And the bottom-right.
        let mut frame = Frame::new();
        frame.set_pixel(127, 127, true);
        read_display(&frame, &mut out);
        assert_eq!(*out.last().unwrap(), 0x80, "bottom-right is the last bit");
    }

    /// An empty screen reads back empty, and a full one reads back full. A
    /// fold that dropped or invented a row would fail one of these.
    #[test]
    fn the_readback_covers_the_whole_panel() {
        let mut out = [0u8; DISP_LEN];
        let mut frame = Frame::new();
        read_display(&frame, &mut out);
        assert!(out.iter().all(|&b| b == 0));

        frame.fill(true);
        read_display(&frame, &mut out);
        assert!(
            out.iter().all(|&b| b == 0xFF),
            "the whole panel should show"
        );
    }

    /// The external framebuffer starts switched off, so a device that has
    /// never been sent a picture shows its own status page.
    #[test]
    fn the_external_framebuffer_starts_off() {
        let fb = External::new();
        assert!(!fb.enabled());
        let mut fb = fb;
        fb.set_enabled(true);
        assert!(fb.enabled());
    }
}
