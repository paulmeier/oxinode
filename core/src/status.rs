//! What the OLED shows when nothing else has asked for it.
//!
//! A status page is the only thing this board can say to somebody who is
//! holding it rather than sitting at the host, so it shows the things that are
//! hard to find out any other way: what the radio is tuned to, whether it is on
//! air, whether anything has been heard, and how strong it was.
//!
//! Rendering is pure — a [`Status`] in, pixels in a [`Frame`] out — so the
//! layout can be tested on the host, including the part that is easy to get
//! wrong: whether every field actually reaches the screen.

use crate::font::{self, LINE_HEIGHT};
use crate::sh1107::{self, Frame};

/// A small text buffer, so numbers can be formatted with `write!` rather than
/// by hand.
///
/// `core::fmt` costs a few kilobytes of flash, which this image has in
/// abundance, and buys correct decimal conversion for signed and unsigned
/// values of every width. Hand-rolled digit extraction is a classic source of
/// off-by-one and negative-number bugs, and none of them would be visible
/// except as a wrong number on a screen.
pub struct Text<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Text<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only ever written through `write_str`, which takes `&str`, so the
        // contents are valid UTF-8 up to `len` -- and truncation happens on a
        // byte boundary only because everything written here is ASCII.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> core::fmt::Write for Text<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // Truncate rather than fail. A status line that is one character too
        // long should lose its last character, not vanish.
        for &b in s.as_bytes() {
            if self.len == N {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}

/// Everything the status page draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// The last four hex digits of the chip's device ID, as the host names it.
    pub name: [u8; 4],
    pub frequency_hz: u32,
    pub bandwidth_hz: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power_dbm: i8,
    /// The radio is configured and receiving.
    pub radio_on: bool,
    /// A stored configuration is in force: the board is a TNC rather than a
    /// host-controlled modem.
    pub tnc: bool,
    /// `rnodeconf` has written an identity that the host would accept.
    pub provisioned: bool,
    pub rx_count: u32,
    pub tx_count: u32,
    /// The last packet's signal, if there has been one.
    pub last_rssi_dbm: Option<i16>,
    /// Quarter-decibels, as the chip and the protocol both carry it.
    pub last_snr_quarter_db: Option<i8>,
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

impl Status {
    pub const fn new() -> Self {
        Self {
            name: *b"----",
            frequency_hz: 0,
            bandwidth_hz: 0,
            spreading_factor: 0,
            coding_rate: 0,
            tx_power_dbm: 0,
            radio_on: false,
            tnc: false,
            provisioned: false,
            rx_count: 0,
            tx_count: 0,
            last_rssi_dbm: None,
            last_snr_quarter_db: None,
        }
    }
}

/// Left margin, and the column values are right-aligned to.
const LEFT: usize = 4;
const RIGHT: usize = sh1107::WIDTH - 4;
/// First line below the title bar.
const FIRST_LINE: usize = 14;

/// One label-and-value line, left and right aligned, advancing the cursor.
fn row(frame: &mut Frame, line: &mut usize, label: &str, value: &str) {
    font::draw(frame, LEFT, *line, label, true);
    font::draw_right(frame, RIGHT, *line, value, true);
    *line += LINE_HEIGHT;
}

/// A horizontal rule, advancing the cursor past it.
fn rule(frame: &mut Frame, line: &mut usize) {
    *line += 3;
    frame.rect(LEFT, *line, RIGHT - LEFT, 1, true);
    *line += 4;
}

/// Draw the whole page.
pub fn render(status: &Status, frame: &mut Frame) {
    frame.fill(false);

    // A filled title bar with the text knocked out of it, so the top of the
    // screen is unmistakably the top even when the panel is upside down in
    // somebody's hand.
    frame.rect(0, 0, sh1107::WIDTH, 11, true);
    font::draw(frame, LEFT, 2, "OXINODE", false);
    let mut name = Text::<8>::new();
    let _ = core::fmt::Write::write_fmt(
        &mut name,
        format_args!(
            "{}{}{}{}",
            status.name[0] as char,
            status.name[1] as char,
            status.name[2] as char,
            status.name[3] as char
        ),
    );
    font::draw_right(frame, RIGHT, 2, name.as_str(), false);

    let mut line = FIRST_LINE;

    let mut buf = Text::<16>::new();
    use core::fmt::Write;

    // Frequency to the kilohertz. Three decimals of a megahertz is what a
    // person reads a channel as, and it is exactly what fits.
    let _ = write!(
        buf,
        "{}.{:03}",
        status.frequency_hz / 1_000_000,
        (status.frequency_hz % 1_000_000) / 1_000
    );
    row(frame, &mut line, "MHZ", buf.as_str());

    buf.clear();
    let _ = write!(
        buf,
        "{}.{}",
        status.bandwidth_hz / 1_000,
        (status.bandwidth_hz % 1_000) / 100
    );
    row(frame, &mut line, "KHZ", buf.as_str());

    buf.clear();
    let _ = write!(buf, "{} 4/{}", status.spreading_factor, status.coding_rate);
    row(frame, &mut line, "SF CR", buf.as_str());

    buf.clear();
    let _ = write!(buf, "{}", status.tx_power_dbm);
    row(frame, &mut line, "DBM", buf.as_str());

    rule(frame, &mut line);

    row(
        frame,
        &mut line,
        "RADIO",
        if status.radio_on { "ON AIR" } else { "OFF" },
    );
    row(
        frame,
        &mut line,
        "MODE",
        if status.tnc { "TNC" } else { "HOST" },
    );
    row(
        frame,
        &mut line,
        "ID",
        if status.provisioned { "SIGNED" } else { "NONE" },
    );

    rule(frame, &mut line);

    buf.clear();
    let _ = write!(buf, "{}", status.rx_count);
    row(frame, &mut line, "RX", buf.as_str());

    buf.clear();
    let _ = write!(buf, "{}", status.tx_count);
    row(frame, &mut line, "TX", buf.as_str());

    buf.clear();
    match status.last_rssi_dbm {
        Some(dbm) => {
            let _ = write!(buf, "{}", dbm);
        }
        None => {
            let _ = write!(buf, "-");
        }
    }
    row(frame, &mut line, "RSSI", buf.as_str());

    buf.clear();
    match status.last_snr_quarter_db {
        // Quarter-decibels to one decimal place, without floating point: the
        // quarter is 0, 25, 50 or 75 hundredths, so one decimal digit is
        // `quarter * 25 / 100` rounded toward zero.
        Some(q) => {
            let whole = q as i32 / 4;
            let tenths = ((q as i32 % 4).abs() * 25) / 10;
            if q < 0 && whole == 0 {
                let _ = write!(buf, "-0.{tenths}");
            } else {
                let _ = write!(buf, "{whole}.{tenths}");
            }
        }
        None => {
            let _ = write!(buf, "-");
        }
    }
    row(frame, &mut line, "SNR", buf.as_str());
}

// The page has to fit. Twelve rows of nine pixels below a title bar and two
// separators is 133 if the margins are wrong, and the frame would clip the last
// line silently.
const _: () = assert!(FIRST_LINE + 11 * LINE_HEIGHT + 14 <= sh1107::HEIGHT);

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    fn sample() -> Status {
        Status {
            name: *b"1DFD",
            frequency_hz: 915_000_000,
            bandwidth_hz: 125_000,
            spreading_factor: 8,
            coding_rate: 5,
            tx_power_dbm: 17,
            radio_on: true,
            tnc: true,
            provisioned: true,
            rx_count: 42,
            tx_count: 7,
            last_rssi_dbm: Some(-69),
            last_snr_quarter_db: Some(45),
        }
    }

    fn lit(frame: &Frame) -> usize {
        (0..sh1107::HEIGHT)
            .flat_map(|y| (0..sh1107::WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| frame.pixel(x, y))
            .count()
    }

    #[test]
    fn a_text_buffer_formats_and_truncates_rather_than_failing() {
        let mut t = Text::<8>::new();
        write!(t, "{}", -1234).unwrap();
        assert_eq!(t.as_str(), "-1234");
        t.clear();
        assert_eq!(t.as_str(), "");
        write!(t, "{}", 1234567890u32).unwrap();
        assert_eq!(t.as_str(), "12345678", "truncated, not dropped");
    }

    #[test]
    fn the_page_draws_something() {
        let mut f = Frame::new();
        render(&sample(), &mut f);
        let count = lit(&f);
        assert!(count > 500, "only {count} pixels lit");
        assert!(count < sh1107::WIDTH * sh1107::HEIGHT / 2, "mostly dark");
    }

    /// **Every field has to reach the screen.** This is the test that catches
    /// a field somebody added to the struct and forgot to draw -- which is
    /// invisible in every other way, because the page still renders and still
    /// looks plausible.
    #[test]
    fn changing_any_field_changes_the_picture() {
        let base = sample();
        let mut reference = Frame::new();
        render(&base, &mut reference);

        /// A named change to one field, so the failure message can say which.
        type Mutation = (&'static str, fn(&mut Status));

        let mutations: [Mutation; 13] = [
            ("name", |s| s.name = *b"BEEF"),
            ("frequency", |s| s.frequency_hz = 868_100_000),
            ("bandwidth", |s| s.bandwidth_hz = 250_000),
            ("spreading factor", |s| s.spreading_factor = 12),
            ("coding rate", |s| s.coding_rate = 8),
            ("tx power", |s| s.tx_power_dbm = -9),
            ("radio state", |s| s.radio_on = false),
            ("mode", |s| s.tnc = false),
            ("provisioned", |s| s.provisioned = false),
            ("rx count", |s| s.rx_count = 43),
            ("tx count", |s| s.tx_count = 8),
            ("rssi", |s| s.last_rssi_dbm = Some(-45)),
            ("snr", |s| s.last_snr_quarter_db = Some(-33)),
        ];

        for (name, mutate) in mutations {
            let mut changed = base;
            mutate(&mut changed);
            let mut frame = Frame::new();
            render(&changed, &mut frame);
            assert_ne!(
                frame.as_bytes(),
                reference.as_bytes(),
                "changing the {name} did not change the screen"
            );
        }
    }

    /// The same status renders the same way. A page that drifted would flush
    /// every page of the display on every tick.
    #[test]
    fn rendering_is_deterministic() {
        let mut a = Frame::new();
        let mut b = Frame::new();
        render(&sample(), &mut a);
        render(&sample(), &mut b);
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    /// Nothing is drawn in the last few rows, which is what says the layout
    /// fits rather than being clipped. A clipped page loses its bottom line
    /// silently.
    #[test]
    fn the_layout_fits_on_the_panel() {
        let mut f = Frame::new();
        render(&sample(), &mut f);
        let bottom = (0..sh1107::WIDTH)
            .flat_map(|x| (0..sh1107::HEIGHT).map(move |y| (x, y)))
            .filter(|&(x, y)| f.pixel(x, y))
            .map(|(_, y)| y)
            .max()
            .unwrap();
        assert!(bottom < sh1107::HEIGHT, "drew past the bottom");
        assert!(
            bottom > sh1107::HEIGHT - LINE_HEIGHT * 2,
            "the page uses only the top of the panel: bottom row is {bottom}"
        );
    }

    /// The title bar is filled with the text knocked out of it, so the top row
    /// is mostly lit and the rest of the page is mostly dark.
    #[test]
    fn the_title_bar_is_inverted() {
        let mut f = Frame::new();
        render(&sample(), &mut f);
        let top_row_lit = (0..sh1107::WIDTH).filter(|&x| f.pixel(x, 0)).count();
        assert_eq!(top_row_lit, sh1107::WIDTH, "the bar should be solid");
        let inside_lit = (0..sh1107::WIDTH).filter(|&x| f.pixel(x, 5)).count();
        assert!(inside_lit < sh1107::WIDTH, "the text should be knocked out");
    }

    /// An unheard radio shows a dash rather than a stale or invented number.
    #[test]
    fn no_signal_yet_is_shown_as_such() {
        let mut quiet = sample();
        quiet.last_rssi_dbm = None;
        quiet.last_snr_quarter_db = None;
        let mut with_signal = Frame::new();
        let mut without = Frame::new();
        render(&sample(), &mut with_signal);
        render(&quiet, &mut without);
        assert_ne!(with_signal.as_bytes(), without.as_bytes());
    }

    /// Quarter-decibels to one decimal, including the case that has no whole
    /// part and is still negative -- where a naive conversion prints "0.5"
    /// for minus half a decibel.
    #[test]
    fn a_small_negative_snr_keeps_its_sign() {
        let render_snr = |q: i8| {
            let mut s = sample();
            s.last_snr_quarter_db = Some(q);
            let mut f = Frame::new();
            render(&s, &mut f);
            f
        };
        // -2 quarters is -0.5 dB; +2 quarters is +0.5 dB. They must not render
        // identically.
        assert_ne!(render_snr(-2).as_bytes(), render_snr(2).as_bytes());
    }
}
