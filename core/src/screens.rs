//! What each screen is allowed to know, and how it is drawn from that.
//!
//! Phase 9's shell can navigate five screens and phase 10's pad can drive it;
//! this is what the screens *say*. The division the shell enforces holds:
//! [`crate::ui`] never decides what is true, and neither does this module. It
//! is handed a [`State`] -- plain values, copied out of the modem loop by the
//! caller -- and formats and draws it. The render path takes state by borrow
//! and reaches into nothing: no protocol, no clock, no pin.
//!
//! That is what makes every screen a host test. The same [`State`] rendered on
//! the board is rendered by the simulator, and a golden image of each screen,
//! empty and populated, is what says the layout is right.
//!
//! # The rule about empty state
//!
//! A screen with no data says it has no data. A field that is not known is an
//! `Option`, and `None` draws as a dash or a sentence, never as a plausible
//! zero: a `Position` reading `0.000000` is a claim to be in the Gulf of
//! Guinea, and a battery reading `0%` with no cell fitted is a claim the board
//! is about to die. The same principle makes the radio refuse a power it
//! cannot produce rather than clamping it -- a value the user cannot tell is
//! fake is worse than an honest gap.
//!
//! # Lines, not layouts
//!
//! Each screen is a list of text lines, and [`crate::ui::page`] draws them
//! from the scroll position with a scrollbar when there are more than fit.
//! That keeps every screen scrollable for free and every screen the same
//! shape; a `label value` row is right-aligned by padding, because the font is
//! fixed-width and a column of values is what makes a page of numbers readable
//! at a glance.

use core::fmt::Write;

use crate::battery;
use crate::ble::NAME_LEN;
use crate::edit::{Editor, Field, Lock, FREQ_DIGITS};
use crate::font;
use crate::lr1121::config::{ConfigError, RadioConfig};
use crate::rnode::command::{FW_VERSION_MAJOR, FW_VERSION_MINOR};
use crate::rnode::store::MAX_BONDS;
use crate::sh1107::{self, Frame};
use crate::status::{Bluetooth as Link, Text};
use crate::ui::{self, Nav, Screen};

/// Characters that fit on one content line.
///
/// The content starts at [`ui::CONTENT_LEFT`] and must stop short of the
/// scrollbar, which is the last two columns; the font is six pixels per cell.
pub const LINE_CHARS: usize = (sh1107::WIDTH - ui::CONTENT_LEFT - 2 - 1) / font::ADVANCE;

/// The most lines any screen produces. The radio screen is the longest, and
/// a refusal reason wrapped under it is the longest it gets.
pub const MAX_LINES: usize = 20;

/// Which host has the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Host {
    /// Nobody has the port open and no phone is connected.
    #[default]
    None,
    /// A host has the KISS port open.
    Usb,
    /// A phone is connected; it is the host while it is.
    Bluetooth,
}

/// What the radio is doing, as distinct from what it was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Air {
    /// Configured off, or not yet configured.
    #[default]
    Off,
    /// Configured, and listening.
    Receiving,
    /// The host asked for something this radio cannot do, and it stayed off.
    Refused(ConfigError),
    /// The configuration was valid but the chip would not take it.
    Failed,
    /// The radio never came up. This board is a modem in name only.
    NoRadio,
}

impl Air {
    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Air::Off => "off",
            Air::Receiving => "receiving",
            Air::Refused(_) => "refused",
            Air::Failed => "failed",
            Air::NoRadio => "no radio",
        }
    }
}

impl Host {
    /// Who has the radio, if it is not the panel: phase 12's rule, in one
    /// place. A live session on either transport owns the live configuration.
    pub const fn lock(self) -> Option<Lock> {
        match self {
            Host::None => None,
            Host::Usb => Some(Lock::Usb),
            Host::Bluetooth => Some(Lock::Bluetooth),
        }
    }
}

/// What [`Screen::Home`] knows: what the modem is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Home {
    pub host: Host,
    /// A frame passed between the modem and its host in the last few seconds.
    /// Meaningless with no host, and not drawn then.
    pub talking: bool,
    pub air: Air,
    pub rx_count: u32,
    pub tx_count: u32,
    /// The last packet's signal, if there has been one.
    pub last_rssi_dbm: Option<i16>,
    /// Quarter-decibels, as the chip and the protocol both carry it.
    pub last_snr_quarter_db: Option<i8>,
    /// Seconds since boot. `None` only where there is no clock, which is the
    /// simulator; a board always knows.
    pub uptime_s: Option<u32>,
    /// The cell, if there is one to read.
    pub battery: Option<battery::Reading>,
    /// The charger's status line.
    pub charging: bool,
}

/// What [`Screen::Radio`] knows: the configuration, and whether it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Radio {
    /// The protocol's current configuration -- the same values the host reads
    /// back, which is the point: what the screen says is what `rnsd` sees.
    pub config: RadioConfig,
    pub air: Air,
    /// A stored configuration is in force: TNC mode.
    pub tnc: bool,
}

impl Default for Radio {
    /// The configuration a modem boots with. Not a placeholder: it is what the
    /// radio would be given if the host said "on" without setting anything.
    fn default() -> Self {
        Radio {
            config: crate::lr1121::config::DEFAULT,
            air: Air::Off,
            tnc: false,
        }
    }
}

/// What [`Screen::Bluetooth`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BluetoothScreen {
    pub link: Link,
    /// The advertised name, once the stack is up.
    pub name: Option<[u8; NAME_LEN]>,
    /// A pairing in progress: the six digits the phone has to be told.
    pub passkey: Option<u32>,
    /// Phones remembered, out of [`MAX_BONDS`].
    pub bonded: u8,
}

/// What [`Screen::Position`] knows, which is that it knows nothing.
///
/// The GPS is not driven until phase 14. There is no field here on purpose:
/// a `fix: Option<Fix>` that is always `None` is a promise the screen cannot
/// keep yet, and the sentence it draws is the whole truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position;

/// What the device's identity amounts to, as far as the device can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Identity {
    /// `rnodeconf --rom` has never been run.
    #[default]
    None,
    /// Locked, but the checksum does not match the identity block.
    BadChecksum,
    /// Locked and self-consistent, but no signature has been stored.
    Unsigned,
    /// Locked, self-consistent, and carrying a signature.
    ///
    /// *Carrying*, not *validated*: the signature is RSA, made with the
    /// host's private key, and the device has no public key to check it
    /// against. Only the host can validate it, and does, on every connect.
    /// What the device can verify is the checksum, and that is what
    /// [`Identity::BadChecksum`] reports.
    Signed,
}

impl Identity {
    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Identity::None => "none",
            Identity::BadChecksum => "bad checksum",
            Identity::Unsigned => "unsigned",
            Identity::Signed => "signed",
        }
    }
}

/// What [`Screen::System`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct System {
    /// oxinode's own version, from its manifest.
    pub version: &'static str,
    /// The device serial, as the host names the port: sixteen hex digits of
    /// the chip's factory ID.
    pub serial: Option<[u8; 16]>,
    pub identity: Identity,
    /// Bytes between the top of static data and the stack pointer.
    pub free_ram: Option<u32>,
}

impl Default for System {
    fn default() -> Self {
        System {
            version: "-",
            serial: None,
            identity: Identity::None,
            free_ram: None,
        }
    }
}

/// Everything the five screens know between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    pub home: Home,
    pub radio: Radio,
    pub bluetooth: BluetoothScreen,
    pub position: Position,
    pub system: System,
}

/// The free RAM figure: what lies between the end of static data and the
/// current stack pointer.
///
/// That is the only honest number on a board with no allocator. The stack
/// grows down from the top of RAM and everything else is laid out from the
/// bottom, so the gap is what is left for the stack to grow into -- and a gap
/// that has closed is `None` rather than a wrapped number.
pub const fn free_ram(heap_start: u32, stack_pointer: u32) -> Option<u32> {
    stack_pointer.checked_sub(heap_start)
}

/// A screen's content, one formatted line at a time.
///
/// Fixed buffers, because the board has no allocator and this is built on
/// its stack once per redraw.
pub struct Lines {
    rows: [Text<LINE_CHARS>; MAX_LINES],
    len: usize,
    /// A line was handed in wider than the row and lost its tail. `Text`
    /// truncates silently, which is right on the board -- a line one
    /// character too long should lose that character, not vanish -- and
    /// wrong in a test, where the loss is the bug. So it is recorded.
    overflowed: bool,
}

impl Default for Lines {
    fn default() -> Self {
        Self::new()
    }
}

impl Lines {
    pub fn new() -> Self {
        Lines {
            rows: core::array::from_fn(|_| Text::new()),
            len: 0,
            overflowed: false,
        }
    }

    /// Whether any line handed in was too wide for the row and was cut.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// How many lines there are.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The lines, for [`ui::page`].
    pub fn as_strs(&self) -> [&str; MAX_LINES] {
        core::array::from_fn(|i| self.rows[i].as_str())
    }

    /// The line at `n`.
    pub fn get(&self, n: usize) -> Option<&str> {
        (n < self.len).then(|| self.rows[n].as_str())
    }

    /// Add one line as given. Past [`MAX_LINES`] it is dropped, which is a
    /// bug in the screen that produced it and is asserted against below.
    pub fn line(&mut self, text: &str) {
        if self.len == MAX_LINES {
            return;
        }
        if text.len() > LINE_CHARS {
            self.overflowed = true;
        }
        self.rows[self.len].clear();
        let _ = self.rows[self.len].write_str(text);
        self.len += 1;
    }

    /// Add a `label value` row with the value flush right.
    pub fn row(&mut self, label: &str, value: &str) {
        if self.len == MAX_LINES {
            return;
        }
        let row = &mut self.rows[self.len];
        row.clear();
        let width = LINE_CHARS.saturating_sub(label.len());
        // A value wider than the room it has pushes the label off rather than
        // being cut: `{value:>width$}` never truncates, and the row then
        // loses its tail to `Text`.
        if value.len() > width {
            self.overflowed = true;
        }
        let _ = write!(row, "{label}{value:>width$}");
        self.len += 1;
    }

    /// Add a sentence, wrapped at word boundaries to the line width.
    pub fn wrapped(&mut self, text: &str) {
        for line in wrap(text, LINE_CHARS) {
            self.line(line);
        }
    }
}

/// Break `text` into pieces no longer than `width`, at spaces where it can and
/// mid-word where it must.
pub fn wrap(text: &str, width: usize) -> impl Iterator<Item = &str> {
    let mut rest = text.trim();
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        if rest.len() <= width {
            let line = rest;
            rest = "";
            return Some(line);
        }
        // The last space inside the width, or a hard break if there is none.
        let cut = rest[..=width].rfind(' ').unwrap_or(width);
        let (line, tail) = rest.split_at(cut);
        rest = tail.trim_start();
        Some(line.trim_end())
    })
}

/// A frequency to the kilohertz: `915.000 MHz`.
fn megahertz(out: &mut Text<LINE_CHARS>, hz: u32) {
    let _ = write!(
        out,
        "{}.{:03} MHz",
        hz / 1_000_000,
        (hz % 1_000_000) / 1_000
    );
}

/// Seconds since boot as something readable.
///
/// `h:mm:ss` under a day, `Nd hh:mm` from then on: at a day the seconds stop
/// meaning anything and the width has to stay inside the row.
pub fn uptime(out: &mut Text<LINE_CHARS>, secs: u32) {
    let (d, h, m, s) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        let _ = write!(out, "{d}d {h:02}:{m:02}");
    } else {
        let _ = write!(out, "{h}:{m:02}:{s:02}");
    }
}

/// Quarter-decibels to one decimal without floating point, keeping the sign
/// on a value between zero and minus one -- see the status page's test for
/// the case that loses it.
fn snr(out: &mut Text<LINE_CHARS>, quarter_db: i8) {
    let whole = quarter_db as i32 / 4;
    let tenths = ((quarter_db as i32 % 4).abs() * 25) / 10;
    if quarter_db < 0 && whole == 0 {
        let _ = write!(out, "-0.{tenths} dB");
    } else {
        let _ = write!(out, "{whole}.{tenths} dB");
    }
}

impl State {
    /// The lines one screen shows.
    pub fn lines(&self, screen: Screen, out: &mut Lines) {
        match screen {
            Screen::Home => self.home.lines(out),
            Screen::Radio => self.radio.lines(out),
            Screen::Bluetooth => self.bluetooth.lines(out),
            Screen::Position => self.position.lines(out),
            Screen::System => self.system.lines(out),
        }
    }
}

impl Home {
    pub fn lines(&self, out: &mut Lines) {
        let mut v = Text::<LINE_CHARS>::new();
        out.row(
            "Host",
            match self.host {
                Host::None => "none",
                Host::Usb => "USB",
                Host::Bluetooth => "Bluetooth",
            },
        );
        out.row(
            "Link",
            match (self.host, self.talking) {
                (Host::None, _) => "-",
                (_, true) => "talking",
                (_, false) => "quiet",
            },
        );
        out.row("Radio", self.air.word());
        let _ = write!(v, "{}", self.rx_count);
        out.row("RX", v.as_str());
        v.clear();
        let _ = write!(v, "{}", self.tx_count);
        out.row("TX", v.as_str());
        v.clear();
        match self.last_rssi_dbm {
            Some(dbm) => {
                let _ = write!(v, "{dbm} dBm");
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("RSSI", v.as_str());
        v.clear();
        match self.last_snr_quarter_db {
            Some(q) => snr(&mut v, q),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("SNR", v.as_str());
        v.clear();
        match self.uptime_s {
            Some(secs) => uptime(&mut v, secs),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Uptime", v.as_str());
        v.clear();
        match self.battery {
            Some(cell) => {
                let _ = write!(
                    v,
                    "{}.{:02} V {}%",
                    cell.millivolts / 1000,
                    (cell.millivolts % 1000) / 10,
                    cell.percent
                );
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Battery", v.as_str());
        out.row(
            "Charger",
            if self.charging {
                "charging"
            } else {
                "not charging"
            },
        );
    }
}

impl Radio {
    pub fn lines(&self, out: &mut Lines) {
        let c = &self.config;
        let mut v = Text::<LINE_CHARS>::new();
        megahertz(&mut v, c.frequency_hz);
        out.row("Freq", v.as_str());
        // What the chip is actually told, which differs by the reference
        // correction. Shown always rather than only when it differs, so the
        // line does not come and go with a configuration flag.
        v.clear();
        megahertz(&mut v, c.commanded_frequency_hz());
        out.row("Tuned", v.as_str());
        v.clear();
        let _ = write!(
            v,
            "{}.{} kHz",
            c.bandwidth_hz / 1_000,
            (c.bandwidth_hz % 1_000) / 100
        );
        out.row("BW", v.as_str());
        v.clear();
        let _ = write!(v, "{}", c.spreading_factor);
        out.row("SF", v.as_str());
        v.clear();
        let _ = write!(v, "4/{}", c.coding_rate);
        out.row("CR", v.as_str());
        v.clear();
        let _ = write!(v, "{} dBm", c.tx_power_dbm);
        out.row("Power", v.as_str());
        v.clear();
        match c.bitrate_bps() {
            0 => {
                let _ = v.write_str("-");
            }
            bps => {
                let _ = write!(v, "{bps} bps");
            }
        }
        out.row("Rate", v.as_str());
        out.row("Radio", self.air.word());
        if let Air::Refused(reason) = self.air {
            out.wrapped(reason.message());
        }
        out.row("Mode", if self.tnc { "TNC" } else { "host" });
        v.clear();
        let _ = write!(v, "{} sym", c.preamble_symbols);
        out.row("Preamble", v.as_str());
        v.clear();
        let _ = write!(v, "0x{:02x}", c.sync_word);
        out.row("Sync", v.as_str());
        out.row("CRC", if c.crc { "on" } else { "off" });
        out.row(
            "Header",
            if c.implicit_header {
                "implicit"
            } else {
                "explicit"
            },
        );
        out.row("IQ", if c.invert_iq { "inverted" } else { "normal" });
    }
}

impl BluetoothScreen {
    pub fn lines(&self, out: &mut Lines) {
        out.row(
            "State",
            match self.link {
                Link::Absent => "absent",
                Link::Advertising => "advertising",
                Link::Connected => "connected",
            },
        );
        match &self.name {
            Some(name) => out.row("Name", core::str::from_utf8(name).unwrap_or("?")),
            None => out.row("Name", "-"),
        }
        let mut v = Text::<LINE_CHARS>::new();
        match self.passkey {
            Some(key) => {
                let _ = write!(v, "{:06}", key % 1_000_000);
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Passkey", v.as_str());
        v.clear();
        let _ = write!(v, "{} of {}", self.bonded, MAX_BONDS);
        out.row("Bonded", v.as_str());
    }
}

impl Position {
    pub fn lines(&self, out: &mut Lines) {
        out.line("GPS not driven yet.");
        out.line("");
        out.wrapped("No fix, no time, and no position to show.");
        out.line("");
        out.line("Phase 14.");
    }
}

impl System {
    pub fn lines(&self, out: &mut Lines) {
        out.row("oxinode", self.version);
        let mut v = Text::<LINE_CHARS>::new();
        let _ = write!(v, "{FW_VERSION_MAJOR}.{FW_VERSION_MINOR}");
        out.row("RNode", v.as_str());
        match &self.serial {
            // Sixteen digits and a label do not share a row.
            Some(serial) => {
                out.line("Serial");
                out.line(core::str::from_utf8(serial).unwrap_or("?"));
            }
            None => out.row("Serial", "-"),
        }
        out.row("Identity", self.identity.word());
        v.clear();
        match self.free_ram {
            Some(bytes) => {
                let _ = write!(v, "{}.{} KB", bytes / 1024, (bytes % 1024) * 10 / 1024);
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("RAM free", v.as_str());
    }
}

/// One field's value in the words the Radio screen uses for it, so the
/// editor and the screen never disagree about how a number reads.
fn field_value(out: &mut Text<LINE_CHARS>, field: Field, value: i32) {
    match field {
        Field::Frequency => megahertz(out, value as u32),
        Field::Bandwidth => {
            let _ = write!(out, "{}.{} kHz", value / 1_000, (value % 1_000) / 100);
        }
        Field::SpreadingFactor => {
            let _ = write!(out, "{value}");
        }
        Field::CodingRate => {
            let _ = write!(out, "4/{value}");
        }
        Field::TxPower => {
            let _ = write!(out, "{value} dBm");
        }
    }
}

/// Where the editor's value starts on its line, in characters: after the
/// label and its gap. The cursor line is built to the same column.
const EDIT_VALUE_COL: usize = 6;

impl Editor {
    /// The editor as lines: what it was, what it is now, how to work it,
    /// and why the last confirmation was refused, if it was.
    ///
    /// Left-aligned rather than in `label value` rows, because the frequency
    /// editor puts a cursor under one digit and the cursor line has to land
    /// on the same column as the digit whatever the value's width.
    pub fn lines(&self, out: &mut Lines) {
        let mut v = Text::<LINE_CHARS>::new();
        let mut line = Text::<LINE_CHARS>::new();

        field_value(&mut v, self.field(), self.original());
        let _ = write!(
            line,
            "{:<width$}{}",
            "Was",
            v.as_str(),
            width = EDIT_VALUE_COL
        );
        out.line(line.as_str());

        v.clear();
        line.clear();
        field_value(&mut v, self.field(), self.candidate());
        let _ = write!(
            line,
            "{:<width$}{}",
            "Now",
            v.as_str(),
            width = EDIT_VALUE_COL
        );
        out.line(line.as_str());

        if self.is_stepper() {
            out.line("");
            out.line("Up/Down: change");
        } else {
            // `MMM.kkk`: the cursor's column skips the point. An underscore
            // rather than a caret, because the font has one and not the
            // other, and an underline under a digit reads as a cursor.
            let at = self.cursor();
            let col = EDIT_VALUE_COL + at + usize::from(at >= FREQ_DIGITS / 2);
            line.clear();
            let _ = write!(line, "{:>width$}", "_", width = col + 1);
            out.line(line.as_str());
            out.line("Up/Down: digit");
            out.line("Left/Right: move");
        }
        out.line("OK: set Back: cancel");
        if let Some(reason) = self.refused() {
            out.line("");
            out.line("Refused:");
            out.wrapped(reason.message());
        }
    }
}

impl Lock {
    /// The notice as lines: who has the radio, and what to do about it.
    pub fn lines(self, out: &mut Lines) {
        out.line(self.headline());
        out.line("");
        out.wrapped("It set the radio up and believes what it set, so the panel will not change it underneath.");
        out.line("");
        out.wrapped(match self {
            Lock::Usb => "Close the port to change settings here.",
            Lock::Bluetooth => "Disconnect the phone to change settings here.",
        });
    }
}

/// What goes in the title bar's right corner: nothing without a stack, `BT`
/// while advertising, `BT*` with a phone on the line -- as the phase 7 page
/// had it.
pub const fn badge(link: Link) -> &'static str {
    match link {
        Link::Absent => "",
        Link::Advertising => "BT",
        Link::Connected => "BT*",
    }
}

/// Draw the pairing box over the content: a filled panel with the six digits
/// knocked out of it, big enough to read across a table.
///
/// Over every screen, not only the Bluetooth one. Pairing is started from the
/// phone, and the person holding the phone has to read the digits off the
/// board whatever it happened to be showing.
pub fn passkey_box(frame: &mut Frame, key: u32) {
    let (left, right) = (ui::CONTENT_LEFT, sh1107::WIDTH - ui::CONTENT_LEFT);
    let height = 40;
    let top = ui::CONTENT_TOP + (ui::CONTENT_H - height) / 2;
    frame.rect(left, top, right - left, height, true);
    font::draw(frame, left + 4, top + 5, "PAIR WITH", false);
    let mut digits = Text::<8>::new();
    let _ = write!(digits, "{:06}", key % 1_000_000);
    // Spaced out: each digit gets two cells, so it reads as a code and not
    // as a number.
    let mut x = left + 4;
    for c in digits.as_str().bytes() {
        font::draw_char(frame, x, top + 22, c, false);
        x += font::ADVANCE * 2 + 1;
    }
}

/// Draw the current screen: chrome, its lines, the scrollbar, a menu if one
/// is open, and the pairing box if a phone is waiting for its digits.
///
/// This is the one composition the firmware and the simulator share, on top
/// of [`ui::page`]. The title bar's corners are the battery and the
/// Bluetooth badge, which are the two things worth seeing on every screen.
pub fn render(frame: &mut Frame, nav: &mut Nav, state: &State) {
    let mut lines = Lines::new();
    // An editor or a notice replaces the screen's lines; the chrome stays.
    if let Some(editor) = nav.editor() {
        editor.lines(&mut lines);
    } else if let Some(lock) = nav.notice_shown() {
        lock.lines(&mut lines);
    } else {
        state.lines(nav.screen(), &mut lines);
    }
    let strs = lines.as_strs();

    let mut left = Text::<8>::new();
    if let Some(cell) = state.home.battery {
        let _ = write!(left, "{}%", cell.percent);
    }
    ui::page(
        frame,
        nav,
        left.as_str(),
        badge(state.bluetooth.link),
        &strs[..lines.len()],
    );
    if let Some(key) = state.bluetooth.passkey {
        passkey_box(frame, key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{Input, Screen};

    /// A board mid-session: every field known, every option `Some`.
    fn populated() -> State {
        State {
            home: Home {
                host: Host::Usb,
                talking: true,
                air: Air::Receiving,
                rx_count: 42,
                tx_count: 7,
                last_rssi_dbm: Some(-69),
                last_snr_quarter_db: Some(45),
                uptime_s: Some(3_723),
                battery: Some(battery::Reading {
                    millivolts: 4_020,
                    percent: 91,
                }),
                charging: true,
            },
            radio: Radio {
                config: RadioConfig {
                    frequency_hz: 915_000_000,
                    bandwidth_hz: 125_000,
                    spreading_factor: 8,
                    coding_rate: 5,
                    tx_power_dbm: 17,
                    ..crate::lr1121::config::DEFAULT
                },
                air: Air::Receiving,
                tnc: true,
            },
            bluetooth: BluetoothScreen {
                link: Link::Advertising,
                name: Some(*b"RNode 7F23"),
                passkey: None,
                bonded: 2,
            },
            position: Position,
            system: System {
                version: "0.0.0",
                serial: Some(*b"0123456789ABCDEF"),
                identity: Identity::Signed,
                free_ram: Some(123_456),
            },
        }
    }

    fn lines_of(state: &State, screen: Screen) -> Vec<String> {
        let mut out = Lines::new();
        state.lines(screen, &mut out);
        (0..out.len())
            .map(|n| out.get(n).unwrap().to_string())
            .collect()
    }

    fn frame_of(state: &State, screen: Screen) -> Frame {
        let mut nav = Nav::new();
        for _ in 0..screen.index() {
            nav.handle(Input::Right);
        }
        let mut frame = Frame::new();
        render(&mut frame, &mut nav, state);
        frame
    }

    /// Every line of every screen, empty and populated, fits the row.
    ///
    /// The row format never truncates a value, so a value that is too wide
    /// pushes its label off the left edge -- invisible in any test but this.
    #[test]
    fn every_line_fits_the_panel() {
        let width = sh1107::WIDTH - ui::CONTENT_LEFT - 2;
        for state in [State::default(), populated()] {
            for screen in Screen::ALL {
                for line in lines_of(&state, screen) {
                    assert!(
                        line.len() <= LINE_CHARS && font::width_of(&line) <= width,
                        "{screen:?}: `{line}` is {} chars",
                        line.len()
                    );
                }
                // The rows are the right width by construction; what the
                // check above cannot see is a line that was *cut* to fit.
                let mut out = Lines::new();
                state.lines(screen, &mut out);
                assert!(!out.overflowed(), "{screen:?} lost the tail of a line");
            }
        }
    }

    /// A line too wide for the row is recorded as such, which is the only
    /// way a test can tell a cut line from one that fitted.
    #[test]
    fn a_line_that_is_cut_is_reported() {
        let mut out = Lines::new();
        out.line("exactly twenty chars");
        assert!(!out.overflowed());
        out.line("twenty-one characters");
        assert!(out.overflowed());
        let mut out = Lines::new();
        out.row("Label", "a value that is far too wide");
        assert!(out.overflowed());
    }

    /// No screen produces more lines than the buffer holds.
    #[test]
    fn no_screen_overflows_the_line_buffer() {
        let mut refused = populated();
        refused.radio.air = Air::Refused(ConfigError::UnsupportedBandwidth);
        for state in [State::default(), populated(), refused] {
            for screen in Screen::ALL {
                let mut out = Lines::new();
                state.lines(screen, &mut out);
                assert!(out.len() < MAX_LINES, "{screen:?} has {} lines", out.len());
            }
        }
    }

    /// **The rule about empty state.** Nothing unknown draws as a number.
    #[test]
    fn an_empty_state_shows_dashes_rather_than_zeroes() {
        let home = lines_of(&State::default(), Screen::Home);
        assert!(home
            .iter()
            .any(|l| l.starts_with("RSSI") && l.ends_with('-')));
        assert!(home
            .iter()
            .any(|l| l.starts_with("SNR") && l.ends_with('-')));
        assert!(home
            .iter()
            .any(|l| l.starts_with("Battery") && l.ends_with('-')));
        assert!(home
            .iter()
            .any(|l| l.starts_with("Uptime") && l.ends_with('-')));
        assert!(!home.iter().any(|l| l.contains("0%")), "{home:?}");
        let system = lines_of(&State::default(), Screen::System);
        assert!(system
            .iter()
            .any(|l| l.starts_with("Serial") && l.ends_with('-')));
        assert!(system
            .iter()
            .any(|l| l.starts_with("RAM") && l.ends_with('-')));
        assert!(system.iter().any(|l| l.ends_with("none")));
        let bt = lines_of(&State::default(), Screen::Bluetooth);
        assert!(bt.iter().any(|l| l.starts_with("Name") && l.ends_with('-')));
        assert!(bt.iter().any(|l| l.ends_with("absent")));
    }

    /// The position screen says why it is empty, and shows no coordinate.
    #[test]
    fn the_position_screen_says_it_has_nothing() {
        for state in [State::default(), populated()] {
            let lines = lines_of(&state, Screen::Position);
            let text = lines.join(" ");
            assert!(text.contains("not driven"), "{text}");
            assert!(text.contains("Phase 14"), "{text}");
            assert!(!text.contains('0'), "a coordinate crept in: {text}");
        }
    }

    /// The whole populated state reaches the screen: every field, changed on
    /// its own, changes some line of some screen.
    #[test]
    fn changing_any_field_changes_a_line() {
        type Mutation = (&'static str, fn(&mut State));
        let mutations: [Mutation; 25] = [
            ("host", |s| s.home.host = Host::Bluetooth),
            ("talking", |s| s.home.talking = false),
            ("home air", |s| s.home.air = Air::Off),
            ("rx", |s| s.home.rx_count += 1),
            ("tx", |s| s.home.tx_count += 1),
            ("rssi", |s| s.home.last_rssi_dbm = Some(-45)),
            ("snr", |s| s.home.last_snr_quarter_db = Some(-2)),
            ("uptime", |s| s.home.uptime_s = Some(90_000)),
            ("battery", |s| s.home.battery = None),
            ("charging", |s| s.home.charging = false),
            ("frequency", |s| s.radio.config.frequency_hz = 916_000_000),
            ("bandwidth", |s| s.radio.config.bandwidth_hz = 250_000),
            ("sf", |s| s.radio.config.spreading_factor = 9),
            ("cr", |s| s.radio.config.coding_rate = 8),
            ("power", |s| s.radio.config.tx_power_dbm = 2),
            ("preamble", |s| s.radio.config.preamble_symbols = 12),
            ("sync", |s| s.radio.config.sync_word = 0x2b),
            ("crc", |s| s.radio.config.crc = false),
            ("header", |s| s.radio.config.implicit_header = true),
            ("iq", |s| s.radio.config.invert_iq = true),
            ("correction", |s| s.radio.config.correct_reference = false),
            ("radio air", |s| {
                s.radio.air = Air::Refused(ConfigError::PowerAboveModuleRating)
            }),
            ("tnc", |s| s.radio.tnc = false),
            ("link", |s| s.bluetooth.link = Link::Connected),
            ("bonded", |s| s.bluetooth.bonded = 3),
        ];
        let base = populated();
        let all = |s: &State| -> Vec<Vec<String>> {
            Screen::ALL.iter().map(|&sc| lines_of(s, sc)).collect()
        };
        let reference = all(&base);
        for (name, mutate) in mutations {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(all(&changed), reference, "changing {name} changed no line");
        }
        // And the rest, which change the picture rather than a line.
        let system: [Mutation; 4] = [
            ("version", |s| s.system.version = "9.9.9"),
            ("serial", |s| s.system.serial = None),
            ("identity", |s| s.system.identity = Identity::Unsigned),
            ("ram", |s| s.system.free_ram = Some(1)),
        ];
        for (name, mutate) in system {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(
                lines_of(&changed, Screen::System),
                lines_of(&base, Screen::System),
                "{name}"
            );
        }
        for (name, mutate) in [
            ("name", (|s| s.bluetooth.name = None) as fn(&mut State)),
            ("passkey", |s| s.bluetooth.passkey = Some(29_717)),
        ] {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(
                frame_of(&changed, Screen::Bluetooth).as_bytes(),
                frame_of(&base, Screen::Bluetooth).as_bytes(),
                "{name}"
            );
        }
    }

    /// A refused configuration says why, in words that fit.
    #[test]
    fn a_refusal_carries_its_reason() {
        let mut state = populated();
        state.radio.air = Air::Refused(ConfigError::PowerAboveModuleRating);
        let lines = lines_of(&state, Screen::Radio);
        let text = lines.join(" ");
        assert!(text.contains("refused"));
        assert!(
            text.contains("power is above the module's 20 dBm rating"),
            "{text}"
        );
        assert!(lines.len() > ui::visible_lines(), "long enough to scroll");
    }

    /// The radio screen is longer than the panel, so it scrolls and the
    /// scrollbar shows.
    #[test]
    fn the_radio_screen_scrolls() {
        let state = populated();
        let mut nav = Nav::new();
        nav.handle(Input::Right);
        let mut top = Frame::new();
        render(&mut top, &mut nav, &state);
        assert!(lines_of(&state, Screen::Radio).len() > ui::visible_lines());
        let bar = |f: &Frame| {
            (ui::CONTENT_TOP..ui::CONTENT_BOTTOM).any(|y| f.pixel(sh1107::WIDTH - 1, y))
        };
        assert!(bar(&top), "a scrollbar");
        nav.handle(Input::Down);
        let mut down = Frame::new();
        render(&mut down, &mut nav, &state);
        assert_eq!(nav.scroll(), 1);
        assert_ne!(top.as_bytes(), down.as_bytes());
    }

    /// Uptime formats as read, and stays inside the row past a day.
    #[test]
    fn uptime_is_readable_at_every_scale() {
        let fmt = |secs| {
            let mut t = Text::<LINE_CHARS>::new();
            uptime(&mut t, secs);
            t.as_str().to_string()
        };
        assert_eq!(fmt(0), "0:00:00");
        assert_eq!(fmt(59), "0:00:59");
        assert_eq!(fmt(3_723), "1:02:03");
        assert_eq!(fmt(86_399), "23:59:59");
        assert_eq!(fmt(86_400), "1d 00:00");
        assert_eq!(fmt(90_061), "1d 01:01");
        assert_eq!(fmt(u32::MAX), "49710d 06:28");
        assert!("Uptime".len() + fmt(u32::MAX).len() <= LINE_CHARS);
    }

    /// Wrapping breaks at spaces, never leaves a line too long, and loses no
    /// words.
    #[test]
    fn wrapping_keeps_every_word_and_every_line_short() {
        let text = "power is above the module's 20 dBm rating";
        let lines: Vec<&str> = wrap(text, LINE_CHARS).collect();
        assert!(lines.iter().all(|l| l.len() <= LINE_CHARS), "{lines:?}");
        assert!(lines
            .iter()
            .all(|l| !l.starts_with(' ') && !l.ends_with(' ')));
        assert_eq!(lines.join(" "), text);
        assert_eq!(wrap("", 10).count(), 0);
        assert_eq!(wrap("short", 10).collect::<Vec<_>>(), ["short"]);
        // A word longer than the width is cut rather than looping forever.
        let long: Vec<&str> = wrap("abcdefghijklmnopqrstuvwxyz", 10).collect();
        assert_eq!(long, ["abcdefghij", "klmnopqrst", "uvwxyz"]);
    }

    /// A row's value sits flush right.
    #[test]
    fn rows_are_right_aligned() {
        let mut out = Lines::new();
        out.row("RX", "42");
        assert_eq!(out.get(0).unwrap().len(), LINE_CHARS);
        assert!(out.get(0).unwrap().ends_with("42"));
        assert!(out.get(0).unwrap().starts_with("RX "));
    }

    /// The free-RAM arithmetic does not wrap when the stack has eaten the gap.
    #[test]
    fn free_ram_is_the_gap_or_nothing() {
        assert_eq!(free_ram(0x2000_1000, 0x2003_F000), Some(0x3E000));
        assert_eq!(free_ram(0x2000_1000, 0x2000_1000), Some(0));
        assert_eq!(free_ram(0x2000_1000, 0x2000_0FFF), None);
    }

    /// The pairing box appears over any screen while a passkey is set.
    #[test]
    fn the_passkey_is_shown_over_every_screen() {
        let mut pairing = populated();
        pairing.bluetooth.passkey = Some(29_717);
        for screen in Screen::ALL {
            let with = frame_of(&pairing, screen);
            let without = frame_of(&populated(), screen);
            assert_ne!(with.as_bytes(), without.as_bytes(), "{screen:?}");
            // The box is a filled block inside the content area.
            let mid = ui::CONTENT_TOP + ui::CONTENT_H / 2;
            let lit = (ui::CONTENT_LEFT..sh1107::WIDTH - ui::CONTENT_LEFT)
                .filter(|&x| with.pixel(x, mid))
                .count();
            assert!(lit > 80, "{screen:?}: box row has {lit} lit");
        }
    }

    /// The title bar carries the battery and the badge, and only when known.
    #[test]
    fn the_title_bar_corners_are_the_battery_and_the_badge() {
        let mut bare = State::default();
        let mut with = populated();
        with.bluetooth.link = Link::Connected;
        let mut chrome_bare = Frame::new();
        chrome_bare.fill(false);
        ui::title_bar(&mut chrome_bare, "", Screen::Home.title(), "");
        let mut chrome_with = Frame::new();
        chrome_with.fill(false);
        ui::title_bar(&mut chrome_with, "91%", Screen::Home.title(), "BT*");
        let row = |f: &Frame| {
            (0..sh1107::WIDTH)
                .map(|x| f.pixel(x, 4))
                .collect::<Vec<_>>()
        };
        assert_eq!(row(&frame_of(&bare, Screen::Home)), row(&chrome_bare));
        assert_eq!(row(&frame_of(&with, Screen::Home)), row(&chrome_with));
        bare.bluetooth.link = Link::Advertising;
        assert_ne!(row(&frame_of(&bare, Screen::Home)), row(&chrome_bare));
    }

    /// **Redraw stays incremental.** The same state redrawn dirties nothing;
    /// a screen change dirties the pages that changed and not the ones that
    /// did not.
    #[test]
    fn a_redraw_costs_only_the_pages_that_changed() {
        let state = populated();
        let mut nav = Nav::new();
        let mut live = Frame::new();
        let mut scratch = Frame::new();
        render(&mut scratch, &mut nav, &state);
        live.copy_from(&scratch);
        for page in 0..sh1107::PAGES {
            live.mark_sent(page);
        }
        assert!(live.is_clean());

        // A tick with nothing new.
        render(&mut scratch, &mut nav, &state);
        live.copy_from(&scratch);
        assert!(live.is_clean(), "an unchanged screen dirtied a page");

        // One counter ticks: the line it is on, and nothing else.
        let mut ticked = state;
        ticked.home.rx_count += 1;
        render(&mut scratch, &mut nav, &ticked);
        live.copy_from(&scratch);
        let dirty = (0..sh1107::PAGES).filter(|&p| live.is_dirty(p)).count();
        assert!((1..=2).contains(&dirty), "one line touched {dirty} pages");
        for page in 0..sh1107::PAGES {
            live.mark_sent(page);
        }

        // A screen change between two short screens: the chrome and the
        // content, but the blank rows between the last line and the icon
        // strip stay as they were. (Home to Radio does redraw every page,
        // because the radio screen fills every content row; what this
        // guards is that nothing marks the whole panel dirty on principle.)
        nav.handle(Input::Right);
        nav.handle(Input::Right);
        render(&mut scratch, &mut nav, &ticked);
        live.copy_from(&scratch);
        for page in 0..sh1107::PAGES {
            live.mark_sent(page);
        }
        assert_eq!(nav.screen(), Screen::Bluetooth);
        nav.handle(Input::Right);
        render(&mut scratch, &mut nav, &ticked);
        live.copy_from(&scratch);
        let dirty = (0..sh1107::PAGES).filter(|&p| live.is_dirty(p)).count();
        assert!(dirty < sh1107::PAGES, "a screen change flushed every page");
        assert!(dirty > 2, "a screen change touched only {dirty} pages");
    }

    // ---- phase 12: the editor ---------------------------------------------

    use crate::edit::{Editor, Field, Lock};

    fn editor_lines(editor: &Editor) -> Vec<String> {
        let mut out = Lines::new();
        editor.lines(&mut out);
        (0..out.len())
            .map(|n| out.get(n).unwrap().to_string())
            .collect()
    }

    /// Every editor, fresh and refused, fits the panel in width and height.
    /// It cannot scroll, so a line past the eleventh would be invisible.
    #[test]
    fn every_editor_fits_without_scrolling() {
        let width = sh1107::WIDTH - ui::CONTENT_LEFT - 2;
        for field in Field::ALL {
            let mut e = Editor::open(field, populated().radio.config);
            for pass in ["fresh", "refused"] {
                if pass == "refused" {
                    // Drive every field to a refusal it can reach; two of
                    // them cannot, and stay legal.
                    for _ in 0..12 {
                        e.up();
                    }
                    e.confirm();
                }
                let lines = editor_lines(&e);
                assert!(
                    lines.len() <= ui::visible_lines(),
                    "{field:?} {pass}: {} lines",
                    lines.len()
                );
                let mut out = Lines::new();
                e.lines(&mut out);
                assert!(!out.overflowed(), "{field:?} {pass}: a line was cut");
                for line in &lines {
                    assert!(
                        line.len() <= LINE_CHARS && font::width_of(line) <= width,
                        "{field:?} {pass}: `{line}`"
                    );
                }
            }
        }
        // And the notices.
        for lock in [Lock::Usb, Lock::Bluetooth] {
            let mut out = Lines::new();
            lock.lines(&mut out);
            assert!(!out.overflowed(), "{lock:?}: a line was cut");
            assert!(out.len() <= ui::visible_lines());
            for n in 0..out.len() {
                assert!(out.get(n).unwrap().len() <= LINE_CHARS);
            }
        }
    }

    /// The editor says what the value was and what it is now, in the same
    /// words the Radio screen uses for it.
    #[test]
    fn the_editor_shows_was_and_now_in_the_screens_words() {
        let mut e = Editor::open(Field::TxPower, populated().radio.config);
        e.up();
        let lines = editor_lines(&e);
        assert_eq!(lines[0], "Was   17 dBm");
        assert_eq!(lines[1], "Now   18 dBm");
        let radio = lines_of(&populated(), Screen::Radio);
        assert!(radio.iter().any(|l| l.ends_with("17 dBm")));

        let mut e = Editor::open(Field::Bandwidth, populated().radio.config);
        e.up();
        let lines = editor_lines(&e);
        assert_eq!(lines[0], "Was   125.0 kHz");
        assert_eq!(lines[1], "Now   250.0 kHz");
        let e = Editor::open(Field::CodingRate, populated().radio.config);
        assert_eq!(editor_lines(&e)[1], "Now   4/5");
        let e = Editor::open(Field::SpreadingFactor, populated().radio.config);
        assert_eq!(editor_lines(&e)[1], "Now   8");
    }

    /// The frequency editor's cursor sits under the digit it edits, on both
    /// sides of the decimal point.
    #[test]
    fn the_cursor_is_under_the_digit_it_edits() {
        let mut e = Editor::open(Field::Frequency, populated().radio.config);
        for expect in 0..crate::edit::FREQ_DIGITS {
            let lines = editor_lines(&e);
            assert_eq!(lines[1], "Now   915.000 MHz");
            let caret = lines[2].find('_').expect("a cursor line");
            let digit = lines[1].as_bytes()[caret];
            assert!(
                digit.is_ascii_digit(),
                "cursor {expect} is under `{}`",
                digit as char
            );
            // The k-th digit of the value, skipping the point.
            let digits: Vec<usize> = lines[1]
                .char_indices()
                .filter(|(_, c)| c.is_ascii_digit())
                .map(|(i, _)| i)
                .collect();
            assert_eq!(caret, digits[expect], "cursor {expect}");
            e.right();
        }
    }

    /// A refused value says so, with the same reason the host path gives.
    #[test]
    fn a_refused_editor_says_why() {
        let mut e = Editor::open(Field::TxPower, populated().radio.config);
        for _ in 0..5 {
            e.up(); // 17 -> 22
        }
        assert_eq!(
            e.confirm(),
            crate::edit::Confirm::Refused(ConfigError::PowerAboveModuleRating)
        );
        let text = editor_lines(&e).join(" ");
        assert!(text.contains("Now   22 dBm"), "{text}");
        assert!(text.contains("Refused:"), "{text}");
        assert!(
            text.contains(ConfigError::PowerAboveModuleRating.message()),
            "{text}"
        );
        e.down();
        assert!(!editor_lines(&e).join(" ").contains("Refused"));
    }

    /// The notice names the host that has the radio.
    #[test]
    fn the_notice_names_the_host() {
        let mut out = Lines::new();
        Lock::Usb.lines(&mut out);
        assert!(out.get(0).unwrap().starts_with("USB"));
        let mut out = Lines::new();
        Lock::Bluetooth.lines(&mut out);
        assert!(out.get(0).unwrap().starts_with("Phone"));
        assert_eq!(Host::None.lock(), None);
        assert_eq!(Host::Usb.lock(), Some(Lock::Usb));
        assert_eq!(Host::Bluetooth.lock(), Some(Lock::Bluetooth));
    }

    /// Rendering with an editor open draws the editor, the field's title,
    /// and the same chrome; closing it puts the screen back exactly.
    #[test]
    fn rendering_an_editor_replaces_the_content_and_nothing_else() {
        let state = populated();
        let mut nav = Nav::new();
        nav.handle(Input::Right);
        let mut before = Frame::new();
        render(&mut before, &mut nav, &state);
        nav.edit(Editor::open(Field::Frequency, state.radio.config));
        let mut editing = Frame::new();
        render(&mut editing, &mut nav, &state);
        assert_ne!(before.as_bytes(), editing.as_bytes());
        // The icon strip is the same.
        for y in ui::CONTENT_BOTTOM..sh1107::HEIGHT {
            for x in 0..sh1107::WIDTH {
                assert_eq!(
                    before.pixel(x, y),
                    editing.pixel(x, y),
                    "strip at ({x}, {y})"
                );
            }
        }
        nav.handle(Input::Back);
        let mut after = Frame::new();
        render(&mut after, &mut nav, &state);
        assert_eq!(before.as_bytes(), after.as_bytes());

        nav.notice(Lock::Usb);
        let mut notice = Frame::new();
        render(&mut notice, &mut nav, &state);
        assert_ne!(notice.as_bytes(), before.as_bytes());
        assert_ne!(notice.as_bytes(), editing.as_bytes());
    }

    /// What the simulator and the board draw is the same composition.
    #[test]
    fn render_is_page_plus_the_pairing_box() {
        let state = populated();
        let mut nav = Nav::new();
        let mut got = Frame::new();
        render(&mut got, &mut nav, &state);

        let mut lines = Lines::new();
        state.lines(Screen::Home, &mut lines);
        let strs = lines.as_strs();
        let mut want_nav = Nav::new();
        let mut want = Frame::new();
        ui::page(&mut want, &mut want_nav, "91%", "BT", &strs[..lines.len()]);
        assert_eq!(got.as_bytes(), want.as_bytes());
    }
}
