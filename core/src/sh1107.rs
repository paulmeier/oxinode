//! The SH1107 OLED controller: its command bytes, and the shape of its RAM.
//!
//! Everything here is from the Sino Wealth SH1107 datasheet (rev. 2018-04) and
//! from the panel the Super IO board actually carries — **128 × 128**, 1.12
//! inches, which the vendor states and which is *not* what the RNode protocol
//! assumes. See `docs/phase-7-display.md`.
//!
//! # The RAM is not laid out the way an SSD1306's is
//!
//! The SH1107 has 16 pages of 128 columns, and one byte covers eight adjacent
//! bits — so far, familiar. What is different is which way those axes point.
//! Figure 10 of the datasheet maps a byte's D0–D7 onto **segment** outputs and
//! the column address onto **common** outputs, and the start-line command
//! (`0xDC`) takes a *column* address to choose which line appears at COM0.
//!
//! On a normal OLED the segments drive columns and the commons drive rows, so
//! that puts the page axis along **x** and the column axis along **y** — the
//! whole thing rotated ninety degrees from an SSD1306. This controller is
//! designed for portrait panels and it shows.
//!
//! The datasheet is not perfectly consistent about this: its reset section says
//! "SEG0 is mapped to the top line of the display", which reads the other way
//! round. That is exactly the kind of disagreement this project settles at the
//! bench rather than by argument, so the mapping is one function —
//! [`ram_position`] — and the test pattern in the `display` image is built to
//! tell the two apart in a single glance.

/// Panel width in pixels.
pub const WIDTH: usize = 128;
/// Panel height in pixels.
pub const HEIGHT: usize = 128;
/// Rows covered by one byte of display RAM.
pub const PAGE_HEIGHT: usize = 8;
/// How many pages the controller has. Fixed by the chip, not by the panel.
pub const PAGES: usize = 16;
/// How many columns the controller has. Also fixed by the chip.
pub const COLUMNS: usize = 128;
/// The whole framebuffer, in bytes.
pub const BUFFER_LEN: usize = PAGES * COLUMNS;

/// Command bytes, as the datasheet numbers them.
pub mod cmd {
    /// `0x00 | (column & 0x0f)` — the low nibble of the column address.
    pub const LOWER_COLUMN: u8 = 0x00;
    /// `0x10 | (column >> 4)` — the high bits. Only `0x10`–`0x17` are legal,
    /// because a column address is seven bits.
    pub const HIGHER_COLUMN: u8 = 0x10;
    /// Page addressing mode: the column address auto-increments. (Power-on.)
    pub const PAGE_ADDRESSING: u8 = 0x20;
    /// Vertical addressing mode: the page address auto-increments instead.
    pub const VERTICAL_ADDRESSING: u8 = 0x21;
    /// Contrast, followed by a byte. Power-on value is `0x80`.
    pub const CONTRAST: u8 = 0x81;
    /// Segment remap normal (`ADC = 0`, power-on).
    pub const SEGMENT_REMAP_NORMAL: u8 = 0xA0;
    /// Segment remap reversed.
    pub const SEGMENT_REMAP_REVERSE: u8 = 0xA1;
    /// Multiplex ratio, followed by `N - 1`. Power-on is 128, i.e. `0x7f`.
    pub const MULTIPLEX: u8 = 0xA8;
    /// Show the RAM contents (power-on).
    pub const ENTIRE_DISPLAY_OFF: u8 = 0xA4;
    /// Light every pixel regardless of RAM. Takes priority over invert.
    pub const ENTIRE_DISPLAY_ON: u8 = 0xA5;
    /// Normal video (power-on).
    pub const NORMAL: u8 = 0xA6;
    /// Inverted video.
    pub const INVERT: u8 = 0xA7;
    /// DC-DC control, followed by a mode byte. See [`super::dcdc`].
    pub const DCDC: u8 = 0xAD;
    /// Display off (power-on). Also enters power-save.
    pub const DISPLAY_OFF: u8 = 0xAE;
    /// Display on.
    pub const DISPLAY_ON: u8 = 0xAF;
    /// `0xB0 | page` — the page address, 0–15.
    pub const PAGE_ADDRESS: u8 = 0xB0;
    /// Common scan from COM0 upwards (power-on).
    pub const COMMON_SCAN_NORMAL: u8 = 0xC0;
    /// Common scan reversed; flips the picture immediately.
    pub const COMMON_SCAN_REVERSE: u8 = 0xC8;
    /// Display offset, followed by a byte. Power-on `0x00`.
    pub const DISPLAY_OFFSET: u8 = 0xD3;
    /// Clock divide and oscillator frequency, followed by a byte. Power-on
    /// `0x50`: nominal oscillator, divide by one.
    pub const CLOCK: u8 = 0xD5;
    /// Pre-charge and discharge periods, followed by a byte. Power-on `0x22`.
    pub const PRECHARGE: u8 = 0xD9;
    /// VCOM deselect level, followed by a byte. Power-on `0x35`.
    pub const VCOM_DESELECT: u8 = 0xDB;
    /// Display start line, followed by a **column** address. Power-on `0x00`.
    ///
    /// Two bytes on this controller, where an SSD1306 packs it into one. A
    /// driver that assumed `0x40 | line` would be setting the column address
    /// instead, which is a very quiet way to scroll the picture.
    pub const START_LINE: u8 = 0xDC;
    /// Begin a read-modify-write. Must be paired with [`RMW_END`].
    pub const RMW_BEGIN: u8 = 0xE0;
    /// End a read-modify-write.
    pub const RMW_END: u8 = 0xEE;
    /// Do nothing.
    pub const NOP: u8 = 0xE3;
}

/// The mode byte that follows [`cmd::DCDC`].
pub mod dcdc {
    /// Built-in converter off: `VPP` must come from outside.
    ///
    /// This is what the Super IO board wants. Its panel supply is a 12 V boost
    /// on the carrier, enabled by P0.23, so the controller's own converter has
    /// nothing to do — and on a module with no inductor fitted, enabling it
    /// would achieve nothing while claiming to be the supply.
    pub const EXTERNAL_VPP: u8 = 0x8A;
    /// Built-in converter on (power-on default).
    pub const INTERNAL: u8 = 0x8B;
}

/// The two bytes an I²C transfer starts with, per the datasheet's Figure 9.
pub mod control {
    /// Continuation bit clear, `C/D` clear: everything after this is commands.
    pub const COMMANDS: u8 = 0x00;
    /// Continuation bit clear, `C/D` set: everything after this is RAM data.
    pub const DATA: u8 = 0x40;
}

/// The 7-bit addresses the SH1107 answers to, chosen by its `SA0` pin.
pub const ADDRESS_SA0_LOW: u8 = 0x3C;
/// See [`ADDRESS_SA0_LOW`]. The Super IO board straps `SA0` high.
pub const ADDRESS_SA0_HIGH: u8 = 0x3D;

/// Whether the page axis runs along x.
///
/// `true` is the reading of Figure 10 — pages along the segments, columns along
/// the commons — and is what the bench is asked to confirm. It is a constant
/// rather than a parameter because the panel is soldered to the board: there is
/// one right answer for this hardware, and carrying both would mean carrying an
/// untested one forever.
pub const PAGE_AXIS_IS_X: bool = true;

/// Where a screen pixel lives in display RAM: `(index, bit)`.
///
/// Returns `None` for a coordinate off the panel, so a caller cannot silently
/// wrap a drawing operation around the edge — which is how a one-pixel overrun
/// turns into a stripe down the opposite side.
pub const fn ram_position(x: usize, y: usize) -> Option<(usize, u8)> {
    if x >= WIDTH || y >= HEIGHT {
        return None;
    }
    let (page, column, bit) = if PAGE_AXIS_IS_X {
        (x / PAGE_HEIGHT, y, (x % PAGE_HEIGHT) as u8)
    } else {
        (y / PAGE_HEIGHT, x, (y % PAGE_HEIGHT) as u8)
    };
    Some((page * COLUMNS + column, bit))
}

/// A framebuffer in the controller's own layout, so a flush is a copy.
#[derive(Clone)]
pub struct Frame {
    buf: [u8; BUFFER_LEN],
    /// Which pages have changed since the last flush. One bit per page.
    dirty: u16,
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    /// An all-dark frame, with every page marked for sending.
    ///
    /// Dirty rather than clean on purpose: the controller's RAM contents after
    /// a reset are undefined, so the first flush has to write all of it.
    pub const fn new() -> Self {
        Self {
            buf: [0; BUFFER_LEN],
            dirty: u16::MAX,
        }
    }

    /// The raw bytes, in controller layout.
    pub const fn as_bytes(&self) -> &[u8; BUFFER_LEN] {
        &self.buf
    }

    /// One page's worth of bytes, ready to be sent after a page-address command.
    pub fn page(&self, page: usize) -> &[u8] {
        &self.buf[page * COLUMNS..(page + 1) * COLUMNS]
    }

    /// Whether a page has changed since it was last marked sent.
    pub const fn is_dirty(&self, page: usize) -> bool {
        self.dirty & (1 << page) != 0
    }

    /// Whether anything at all needs sending.
    pub const fn is_clean(&self) -> bool {
        self.dirty == 0
    }

    /// Record that a page has reached the controller.
    pub const fn mark_sent(&mut self, page: usize) {
        self.dirty &= !(1 << page);
    }

    /// Mark every page for sending, whether or not it changed.
    pub const fn mark_all_dirty(&mut self) {
        self.dirty = u16::MAX;
    }

    /// Set or clear one pixel. Off-panel coordinates are ignored.
    pub fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        let Some((index, bit)) = ram_position(x, y) else {
            return;
        };
        let mask = 1u8 << bit;
        let before = self.buf[index];
        let after = if on { before | mask } else { before & !mask };
        if after != before {
            self.buf[index] = after;
            self.dirty |= 1 << (index / COLUMNS);
        }
    }

    /// Read one pixel back. Off-panel coordinates read as dark.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        match ram_position(x, y) {
            Some((index, bit)) => self.buf[index] & (1 << bit) != 0,
            None => false,
        }
    }

    /// Set every pixel to `on`.
    pub fn fill(&mut self, on: bool) {
        self.buf = [if on { 0xFF } else { 0x00 }; BUFFER_LEN];
        self.dirty = u16::MAX;
    }

    /// Fill a rectangle, clipped to the panel.
    ///
    /// Clipped rather than rejected: drawing is a place where being strict
    /// costs callers a bounds check at every site and buys nothing, because
    /// there is no correct behaviour other than "do not draw off the screen".
    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, on: bool) {
        for py in y..y.saturating_add(h).min(HEIGHT) {
            for px in x..x.saturating_add(w).min(WIDTH) {
                self.set_pixel(px, py, on);
            }
        }
    }

    /// Draw the outline of a rectangle, one pixel thick.
    pub fn frame_rect(&mut self, x: usize, y: usize, w: usize, h: usize, on: bool) {
        if w == 0 || h == 0 {
            return;
        }
        self.rect(x, y, w, 1, on);
        self.rect(x, y + h - 1, w, 1, on);
        self.rect(x, y, 1, h, on);
        self.rect(x + w - 1, y, 1, h, on);
    }
}

/// The commands that take the controller from reset to showing RAM.
///
/// Power-on values are used wherever the datasheet gives one and there is no
/// reason to differ, so that the number of settings this project has invented
/// stays small and each one can be argued for:
///
/// * **`0xAD 0x8A`** — the built-in DC-DC is switched *off*, because this
///   board supplies `VPP` from its own 12 V boost. See [`dcdc`].
/// * **`0xA8 0x7f`** — 128 commons, matching the panel. This is also the
///   power-on value; it is stated because a shorter panel would need it
///   changed and a silent default would hide that.
/// * everything else is the power-on value, written out so that a controller
///   which was *not* freshly reset ends up in the same state as one that was.
///
/// `contrast` is the only parameter, because it is the only one a host can set
/// (`CMD_DISP_INT`).
pub fn init_sequence(contrast: u8, out: &mut [u8; INIT_LEN]) -> &[u8] {
    *out = [
        cmd::DISPLAY_OFF,
        cmd::CLOCK,
        0x50,
        cmd::PAGE_ADDRESSING,
        cmd::CONTRAST,
        contrast,
        cmd::SEGMENT_REMAP_NORMAL,
        cmd::MULTIPLEX,
        0x7F,
        cmd::COMMON_SCAN_NORMAL,
        cmd::DISPLAY_OFFSET,
        0x00,
        cmd::START_LINE,
        0x00,
        cmd::PRECHARGE,
        0x22,
        cmd::VCOM_DESELECT,
        0x35,
        cmd::DCDC,
        dcdc::EXTERNAL_VPP,
        cmd::ENTIRE_DISPLAY_OFF,
        cmd::NORMAL,
        cmd::DISPLAY_ON,
    ];
    out
}

/// How many bytes [`init_sequence`] writes.
pub const INIT_LEN: usize = 23;

/// The three commands that position the write cursor at the start of a page.
pub const fn page_cursor(page: usize) -> [u8; 3] {
    [
        cmd::PAGE_ADDRESS | (page as u8 & 0x0F),
        cmd::LOWER_COLUMN,
        cmd::HIGHER_COLUMN,
    ]
}

// A column address is seven bits, so the high-nibble command only has eight
// legal values. Encoding column 128 or beyond would land on 0x18, which is not
// a command at all.
const _: () = assert!(COLUMNS <= 128);
// The dirty mask is one bit per page.
const _: () = assert!(PAGES <= 16);
// The panel has to be a whole number of pages tall, or the last one is partly
// off-screen and every bounds check below is off by the remainder.
const _: () = assert!(HEIGHT % PAGE_HEIGHT == 0);
// The controller's RAM has to be able to hold the panel.
const _: () = assert!(WIDTH * HEIGHT / 8 <= BUFFER_LEN);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_is_the_one_the_vendor_ships() {
        // 128 x 128, which is *not* the 128 x 64 the RNode protocol's
        // 1024-byte display read assumes. See docs/phase-7-display.md.
        assert_eq!((WIDTH, HEIGHT), (128, 128));
        assert_eq!(BUFFER_LEN, 2048);
        assert_eq!(BUFFER_LEN, WIDTH * HEIGHT / 8);
    }

    #[test]
    fn a_new_frame_is_dark_and_entirely_dirty() {
        let f = Frame::new();
        assert!(f.as_bytes().iter().all(|&b| b == 0));
        assert!(!f.is_clean());
        for page in 0..PAGES {
            assert!(f.is_dirty(page), "page {page}");
        }
    }

    /// The controller's RAM after a reset is undefined, so the first flush has
    /// to send everything. A frame that started clean would leave whatever was
    /// there on screen.
    #[test]
    fn marking_pages_sent_eventually_makes_a_frame_clean() {
        let mut f = Frame::new();
        for page in 0..PAGES {
            f.mark_sent(page);
        }
        assert!(f.is_clean());
        f.mark_all_dirty();
        assert!(!f.is_clean());
    }

    #[test]
    fn a_pixel_round_trips() {
        let mut f = Frame::new();
        for (x, y) in [(0, 0), (127, 127), (0, 127), (127, 0), (63, 64), (8, 7)] {
            assert!(!f.pixel(x, y), "({x},{y}) should start dark");
            f.set_pixel(x, y, true);
            assert!(f.pixel(x, y), "({x},{y}) should be lit");
            f.set_pixel(x, y, false);
            assert!(!f.pixel(x, y), "({x},{y}) should be dark again");
        }
    }

    /// Every pixel on the panel has a distinct home in RAM, and every byte of
    /// RAM is used. An off-by-one in the mapping shows up here as two pixels
    /// sharing a bit — which on screen looks like a faint ghost of the picture
    /// somewhere else, and is very hard to see.
    #[test]
    fn the_mapping_is_a_bijection_onto_the_buffer() {
        let mut seen = vec![false; BUFFER_LEN * 8];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let (index, bit) = ram_position(x, y).expect("on the panel");
                let slot = index * 8 + bit as usize;
                assert!(!seen[slot], "({x},{y}) collides at byte {index} bit {bit}");
                seen[slot] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "some RAM is unreachable");
    }

    /// Off-panel coordinates are refused rather than wrapped. A wrap turns a
    /// one-pixel overrun into a stripe down the far side of the screen.
    #[test]
    fn coordinates_off_the_panel_have_no_home() {
        assert_eq!(ram_position(WIDTH, 0), None);
        assert_eq!(ram_position(0, HEIGHT), None);
        assert_eq!(ram_position(usize::MAX, 0), None);
        let mut f = Frame::new();
        f.set_pixel(WIDTH, 0, true);
        f.set_pixel(0, HEIGHT, true);
        assert!(f.as_bytes().iter().all(|&b| b == 0), "something got drawn");
    }

    /// Only the page a pixel lands in is sent again. Sixteen pages at 100 kHz
    /// is 186 ms of bus time; sending one is 12.
    #[test]
    fn only_the_page_that_changed_is_dirty() {
        let mut f = Frame::new();
        for page in 0..PAGES {
            f.mark_sent(page);
        }
        f.set_pixel(0, 0, true);
        let (index, _) = ram_position(0, 0).unwrap();
        let expected = index / COLUMNS;
        for page in 0..PAGES {
            assert_eq!(f.is_dirty(page), page == expected, "page {page}");
        }
    }

    /// Setting a pixel that is already set changes nothing, and must not mark
    /// a page for a resend it does not need.
    #[test]
    fn a_write_that_changes_nothing_dirties_nothing() {
        let mut f = Frame::new();
        f.set_pixel(10, 10, true);
        for page in 0..PAGES {
            f.mark_sent(page);
        }
        f.set_pixel(10, 10, true);
        assert!(f.is_clean());
        f.set_pixel(10, 10, false);
        assert!(!f.is_clean());
    }

    #[test]
    fn a_rectangle_covers_exactly_its_own_area() {
        let mut f = Frame::new();
        f.rect(10, 20, 5, 7, true);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let inside = (10..15).contains(&x) && (20..27).contains(&y);
                assert_eq!(f.pixel(x, y), inside, "({x},{y})");
            }
        }
    }

    /// Clipped, not wrapped. A rectangle running off the right edge must not
    /// reappear on the left.
    #[test]
    fn a_rectangle_past_the_edge_is_clipped() {
        let mut f = Frame::new();
        f.rect(126, 126, 10, 10, true);
        assert!(f.pixel(127, 127));
        assert!(!f.pixel(0, 0));
        assert!(!f.pixel(0, 127));
        let lit = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| f.pixel(x, y))
            .count();
        assert_eq!(lit, 4, "only the 2x2 corner is on the panel");
    }

    #[test]
    fn an_outline_is_hollow() {
        let mut f = Frame::new();
        f.frame_rect(0, 0, WIDTH, HEIGHT, true);
        assert!(f.pixel(0, 0) && f.pixel(127, 0) && f.pixel(0, 127) && f.pixel(127, 127));
        assert!(f.pixel(64, 0) && f.pixel(64, 127));
        assert!(!f.pixel(64, 64), "the middle should be empty");
    }

    #[test]
    fn filling_sets_or_clears_everything() {
        let mut f = Frame::new();
        f.fill(true);
        assert!(f.as_bytes().iter().all(|&b| b == 0xFF));
        assert!(f.pixel(0, 0) && f.pixel(127, 127));
        f.fill(false);
        assert!(f.as_bytes().iter().all(|&b| b == 0));
    }

    /// The page cursor's three bytes, and the fact that a page address is four
    /// bits. A driver that let page 16 through would write `0xC0`, which is the
    /// common scan direction command and would flip the picture instead.
    #[test]
    fn the_page_cursor_addresses_the_page_and_rewinds_the_column() {
        assert_eq!(page_cursor(0), [0xB0, 0x00, 0x10]);
        assert_eq!(page_cursor(15), [0xBF, 0x00, 0x10]);
        for page in 0..PAGES {
            let c = page_cursor(page);
            assert_eq!(c[0] & 0xF0, cmd::PAGE_ADDRESS, "page {page}");
            assert_eq!(c[0] & 0x0F, page as u8);
            assert_ne!(c[0], cmd::COMMON_SCAN_NORMAL);
        }
    }

    /// The init sequence has to leave the controller showing RAM, right way up,
    /// with the panel's own multiplex ratio and this board's power arrangement.
    #[test]
    fn the_init_sequence_says_what_it_should() {
        let mut buf = [0u8; INIT_LEN];
        let seq = init_sequence(0x80, &mut buf);
        assert_eq!(seq.len(), INIT_LEN);
        assert_eq!(seq[0], cmd::DISPLAY_OFF, "configure before switching on");
        assert_eq!(*seq.last().unwrap(), cmd::DISPLAY_ON);

        let pair = |c: u8| {
            seq.windows(2)
                .find(|w| w[0] == c)
                .map(|w| w[1])
                .unwrap_or_else(|| panic!("{c:#04x} not in the sequence"))
        };
        assert_eq!(
            pair(cmd::MULTIPLEX),
            0x7F,
            "128 commons for a 128-row panel"
        );
        assert_eq!(
            pair(cmd::DCDC),
            dcdc::EXTERNAL_VPP,
            "this board's own boost"
        );
        assert_eq!(pair(cmd::CONTRAST), 0x80);
        assert_eq!(pair(cmd::DISPLAY_OFFSET), 0x00);
        assert_eq!(pair(cmd::START_LINE), 0x00);
        assert!(seq.contains(&cmd::PAGE_ADDRESSING));
        assert!(
            seq.contains(&cmd::ENTIRE_DISPLAY_OFF),
            "show RAM, not all-on"
        );
        assert!(seq.contains(&cmd::NORMAL));
        assert!(!seq.contains(&cmd::INVERT));
    }

    /// The contrast the caller asks for is the contrast that is sent, and it
    /// is the only thing the sequence varies.
    #[test]
    fn contrast_is_the_only_parameter() {
        let mut a = [0u8; INIT_LEN];
        let mut b = [0u8; INIT_LEN];
        let sa = init_sequence(0x10, &mut a).to_vec();
        let sb = init_sequence(0xF0, &mut b).to_vec();
        let differing: Vec<usize> = (0..INIT_LEN).filter(|&i| sa[i] != sb[i]).collect();
        assert_eq!(differing.len(), 1);
        assert_eq!(sa[differing[0] - 1], cmd::CONTRAST);
        assert_eq!((sa[differing[0]], sb[differing[0]]), (0x10, 0xF0));
    }

    /// The two control bytes are distinct and neither is a command byte that
    /// would do something if it were misread as one. `0x00` is "set lower
    /// column address to 0" and `0x40` is not a command at all on this chip.
    #[test]
    fn the_control_bytes_are_the_documented_ones() {
        assert_eq!(control::COMMANDS, 0x00);
        assert_eq!(control::DATA, 0x40);
        assert_ne!(control::COMMANDS, control::DATA);
    }

    /// This board's panel answers at 0x3d. Recorded here as well as in the
    /// scan, because "the driver does not work" and "you are talking to 0x3c"
    /// look identical.
    #[test]
    fn the_two_addresses_are_one_bit_apart() {
        assert_eq!(ADDRESS_SA0_LOW, 0x3C);
        assert_eq!(ADDRESS_SA0_HIGH, 0x3D);
        assert_eq!(ADDRESS_SA0_LOW ^ ADDRESS_SA0_HIGH, 0x01);
    }
}
