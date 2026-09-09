//! The GPS receiver, from the bytes it sends.
//!
//! The Super IO carries a GNSS module with a ceramic antenna, on a UART and
//! behind a load switch. Everything about it that can be decided without the
//! UART is here: framing the byte stream into sentences, checking their
//! checksums, reading the three sentence types the screen needs, keeping
//! track of what the receiver currently knows, and deciding whether it
//! should be powered at all. The firmware's part is a load switch, a UART
//! and a task that feeds this module and copies its answer out for the
//! screen; see `src/gps.rs`.
//!
//! # NMEA, as much of it as is needed
//!
//! A receiver talks NMEA 0183: lines of ASCII, `$` to `*`, two hex digits of
//! checksum, `\r\n`, never more than 82 characters. The checksum is the XOR
//! of everything between the `$` and the `*`, and a line that fails it is
//! dropped whole: a corrupted field would otherwise be a coordinate a few
//! hundred kilometres out, which is worse than no coordinate. The rest of
//! the framing is equally unforgiving, because a UART with nothing on the
//! other end -- the wrong pin, the wrong baud, a module still powering up
//! -- produces bytes that look like nothing, and every one of them has to be
//! discarded without upsetting the sentence that follows.
//!
//! Three sentences are read. `GGA` carries the fix itself: quality,
//! satellites used, latitude, longitude, altitude. `RMC` carries the date,
//! which `GGA` does not, and a second opinion on whether there is a fix.
//! `GSV` carries how many satellites are in view, which is the number worth
//! watching while there is no fix yet. The talker -- `GP` for GPS alone,
//! `GN` for a mix of constellations, `GL`, `GB`, `GA` for the others -- is
//! ignored for the first two and kept for the third, because each
//! constellation reports its own view and the total is their sum.
//!
//! # Integers only
//!
//! Coordinates are micro-degrees in an `i32`, which places a point to about
//! eleven centimetres and is more than the receiver knows. Altitude is
//! decimetres. Nothing here uses floating point: the board has an FPU, but
//! a value that is going to be printed to six places is better held to
//! exactly six places than rounded twice on the way.
//!
//! # The rule about empty state, again
//!
//! [`Position`] is what the screen draws, and every value on it is an
//! `Option`. A receiver that has never had a fix reports `None` for the
//! fix, not the equator; one that has lost its fix keeps the last one and
//! reports how old it is, because a position that is a minute stale is
//! still a position and the age is the honest part.

use crate::pad::{Millis, Mode};

/// The longest a sentence may be, `$` to `\n` inclusive, per NMEA 0183.
pub const MAX_SENTENCE: usize = 82;

/// How long after the last sentence a powered receiver counts as silent.
///
/// A receiver sends at least once a second; five seconds of nothing means
/// the module is not talking to us, whatever the reason.
pub const SILENT_AFTER_MS: Millis = 5_000;

/// How old a fix may be before the receiver is called searching again.
///
/// Between sentences a fix is a second old and still current; a receiver
/// that has stopped saying it has one is not one that has one.
pub const FIX_STALE_AFTER_MS: Millis = 5_000;

/// The baud rates to try, most likely first.
///
/// Every module this board is likely to carry -- Quectel's L76 series,
/// Zhongke's ATGM336H, u-blox's M8 and M10 -- wakes up at 9600, and the
/// reference firmware probes in this order.
pub const PROBE_BAUDS: [u32; 3] = [9_600, 115_200, 38_400];

/// How long to listen at one baud rate on one pin order before trying the
/// next. A receiver sends every second; three of them is two missed.
pub const PROBE_WINDOW_MS: Millis = 3_000;

/// A time of day, UTC, as the receiver reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// A calendar date, as the receiver reports it. The year is complete: the
/// receiver sends two digits and this century is assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

/// Where the receiver last said it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fix {
    /// Micro-degrees, north positive.
    pub lat_udeg: i32,
    /// Micro-degrees, east positive.
    pub lon_udeg: i32,
    /// Decimetres above mean sea level, if the receiver gave one.
    pub alt_dm: Option<i32>,
}

/// What the receiver is doing, for the screen's first row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// The load switch is off.
    #[default]
    Off,
    /// Powered, and nothing readable has arrived recently.
    Silent,
    /// Talking, and looking for satellites.
    Searching,
    /// Has a fix, and said so within the last few seconds.
    Fix,
}

impl Status {
    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Status::Off => "off",
            Status::Silent => "no data",
            Status::Searching => "searching",
            Status::Fix => "fix",
        }
    }
}

/// What the Position screen knows: plain values, copied out of the receiver
/// once per redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub status: Status,
    /// Satellites in the current solution, from `GGA`.
    pub sats_used: Option<u8>,
    /// Satellites the receiver can see, summed over every constellation
    /// that has reported.
    pub sats_in_view: Option<u8>,
    /// The last fix, kept after it is lost.
    pub fix: Option<Fix>,
    /// Seconds since the receiver last confirmed the fix.
    pub fix_age_s: Option<u32>,
    pub time: Option<Time>,
    pub date: Option<Date>,
}

impl Position {
    /// A receiver that is off and has nothing.
    pub const OFF: Position = Position {
        status: Status::Off,
        sats_used: None,
        sats_in_view: None,
        fix: None,
        fix_age_s: None,
        time: None,
        date: None,
    };
}

// ---- framing -------------------------------------------------------------

/// Why a line was thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// The two hex digits after `*` did not match the XOR of the body.
    Checksum,
    /// A line ended without `*hh` on it.
    NoChecksum,
    /// Longer than NMEA allows, which means the framing has been lost.
    TooLong,
    /// A byte that cannot appear in a sentence: not printable ASCII.
    Garbage,
    /// A sentence that framed correctly but whose fields do not parse.
    Field,
}

impl Malformed {
    /// A short name, for a log.
    pub const fn name(self) -> &'static str {
        match self {
            Malformed::Checksum => "checksum",
            Malformed::NoChecksum => "no checksum",
            Malformed::TooLong => "too long",
            Malformed::Garbage => "garbage",
            Malformed::Field => "bad field",
        }
    }
}

/// What one byte did to the framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing complete yet.
    Pending,
    /// A sentence with a good checksum is ready: see [`Lexer::sentence`].
    Sentence,
    /// A line was dropped, and this is why.
    Bad(Malformed),
}

/// Frames a byte stream into checked sentences.
///
/// One byte at a time, like the KISS decoder, because that is how they
/// arrive from a UART ring buffer. Anything before a `$` is discarded;
/// anything after one is collected until `\r` or `\n`, checked, and either
/// offered as a sentence or counted as a bad line.
pub struct Lexer {
    buf: [u8; MAX_SENTENCE],
    len: usize,
    /// A `$` has been seen and the line is being collected.
    open: bool,
    /// Where the body ends: the index of `*` in a completed sentence.
    body: usize,
}

impl Default for Lexer {
    fn default() -> Self {
        Self::new()
    }
}

impl Lexer {
    pub const fn new() -> Self {
        Lexer {
            buf: [0; MAX_SENTENCE],
            len: 0,
            open: false,
            body: 0,
        }
    }

    /// Feed one byte.
    pub fn push(&mut self, byte: u8) -> Step {
        match byte {
            b'$' => {
                // A new start mid-sentence is the previous one lost: no
                // checksum, and no point keeping it.
                let lost = self.open && self.len > 0;
                self.open = true;
                self.len = 0;
                if lost {
                    Step::Bad(Malformed::NoChecksum)
                } else {
                    Step::Pending
                }
            }
            b'\r' | b'\n' => {
                if !self.open {
                    return Step::Pending;
                }
                self.open = false;
                if self.len == 0 {
                    // `\r` then `\n`: the second one closes nothing.
                    return Step::Pending;
                }
                self.check()
            }
            0x20..=0x7e => {
                if !self.open {
                    return Step::Pending;
                }
                if self.len == MAX_SENTENCE {
                    self.open = false;
                    self.len = 0;
                    return Step::Bad(Malformed::TooLong);
                }
                self.buf[self.len] = byte;
                self.len += 1;
                Step::Pending
            }
            _ => {
                // Line noise. Whatever was being collected is suspect, and
                // a byte like this is what the wrong baud rate produces.
                let lost = self.open && self.len > 0;
                self.open = false;
                self.len = 0;
                if lost {
                    Step::Bad(Malformed::Garbage)
                } else {
                    Step::Pending
                }
            }
        }
    }

    /// Verify the collected line. Leaves `len` at zero on failure so a
    /// stale body cannot be read back.
    fn check(&mut self) -> Step {
        let line = &self.buf[..self.len];
        let Some(star) = line.iter().rposition(|&b| b == b'*') else {
            self.len = 0;
            return Step::Bad(Malformed::NoChecksum);
        };
        let Some(given) = hex_byte(&line[star + 1..]) else {
            self.len = 0;
            return Step::Bad(Malformed::NoChecksum);
        };
        let computed = line[..star].iter().fold(0u8, |acc, &b| acc ^ b);
        if given != computed {
            self.len = 0;
            return Step::Bad(Malformed::Checksum);
        }
        self.body = star;
        Step::Sentence
    }

    /// The last complete sentence's body: between `$` and `*`, exclusive.
    /// Valid until the next [`push`](Self::push).
    pub fn sentence(&self) -> &str {
        // Only printable ASCII is ever stored, so this cannot fail.
        core::str::from_utf8(&self.buf[..self.body]).unwrap_or("")
    }
}

/// Exactly two hex digits.
fn hex_byte(s: &[u8]) -> Option<u8> {
    if s.len() != 2 {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    Some((digit(s[0])? << 4) | digit(s[1])?)
}

// ---- sentences -----------------------------------------------------------

/// A `GGA` sentence: the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gga {
    pub time: Option<Time>,
    /// The quality field: 0 is no fix; 1 is a fix; 2 is a differential fix;
    /// higher values are RTK and estimated modes.
    pub quality: u8,
    pub sats_used: u8,
    /// Present when the quality is not zero and the coordinates are given.
    pub fix: Option<Fix>,
}

/// An `RMC` sentence: the date, and whether the receiver calls its data
/// valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rmc {
    pub time: Option<Time>,
    pub date: Option<Date>,
    /// `A` for active; `V` for void.
    pub valid: bool,
    pub fix: Option<Fix>,
}

/// A `GSV` sentence: how many satellites one constellation can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gsv {
    /// The talker: `GP`, `GL`, `GB`, `GA`, `GQ`, or `GN`.
    pub talker: [u8; 2],
    pub in_view: u8,
}

/// What a sentence turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Gga(Gga),
    Rmc(Rmc),
    Gsv(Gsv),
    /// Framed correctly and not one of the three above. `GSA`, `GLL`,
    /// `VTG`, `TXT`, proprietary `P...` lines: all fine, all ignored.
    Other,
}

/// Read one sentence body: the text between `$` and `*`.
pub fn parse(body: &str) -> Result<Report, Malformed> {
    let mut fields = body.split(',');
    let head = fields.next().unwrap_or("");
    // A talker and a type: `GNGGA`. Proprietary sentences start with `P`
    // and have no fixed shape; they are somebody else's business.
    if head.len() != 5 || !head.is_ascii() {
        return Ok(Report::Other);
    }
    let (talker, kind) = head.split_at(2);
    if talker.starts_with('P') {
        return Ok(Report::Other);
    }
    match kind {
        "GGA" => gga(fields).map(Report::Gga),
        "RMC" => rmc(fields).map(Report::Rmc),
        "GSV" => gsv(talker, fields).map(Report::Gsv),
        _ => Ok(Report::Other),
    }
}

/// `GGA,time,lat,N,lon,E,quality,sats,hdop,alt,M,geoid,M,age,station`.
fn gga<'a>(mut f: impl Iterator<Item = &'a str>) -> Result<Gga, Malformed> {
    let time = time(f.next().ok_or(Malformed::Field)?)?;
    let lat = f.next().ok_or(Malformed::Field)?;
    let ns = f.next().ok_or(Malformed::Field)?;
    let lon = f.next().ok_or(Malformed::Field)?;
    let ew = f.next().ok_or(Malformed::Field)?;
    let quality = small(f.next().ok_or(Malformed::Field)?)?.unwrap_or(0);
    let sats_used = small(f.next().ok_or(Malformed::Field)?)?.unwrap_or(0);
    let _hdop = f.next().ok_or(Malformed::Field)?;
    let alt = f.next().ok_or(Malformed::Field)?;
    let alt_unit = f.next().ok_or(Malformed::Field)?;
    let alt_dm = match (alt, alt_unit) {
        ("", _) => None,
        (alt, "M") => Some(i32::try_from(fixed(alt, 1)?).map_err(|_| Malformed::Field)?),
        _ => return Err(Malformed::Field),
    };
    let fix = if quality == 0 {
        None
    } else {
        coordinates(lat, ns, lon, ew)?.map(|(lat_udeg, lon_udeg)| Fix {
            lat_udeg,
            lon_udeg,
            alt_dm,
        })
    };
    Ok(Gga {
        time,
        quality,
        sats_used,
        fix,
    })
}

/// `RMC,time,status,lat,N,lon,E,speed,course,date,...`.
fn rmc<'a>(mut f: impl Iterator<Item = &'a str>) -> Result<Rmc, Malformed> {
    let time = time(f.next().ok_or(Malformed::Field)?)?;
    let valid = match f.next().ok_or(Malformed::Field)? {
        "A" => true,
        "V" | "" => false,
        _ => return Err(Malformed::Field),
    };
    let lat = f.next().ok_or(Malformed::Field)?;
    let ns = f.next().ok_or(Malformed::Field)?;
    let lon = f.next().ok_or(Malformed::Field)?;
    let ew = f.next().ok_or(Malformed::Field)?;
    let _speed = f.next().ok_or(Malformed::Field)?;
    let _course = f.next().ok_or(Malformed::Field)?;
    let date = date(f.next().ok_or(Malformed::Field)?)?;
    let fix = if valid {
        coordinates(lat, ns, lon, ew)?.map(|(lat_udeg, lon_udeg)| Fix {
            lat_udeg,
            lon_udeg,
            alt_dm: None,
        })
    } else {
        None
    };
    Ok(Rmc {
        time,
        date,
        valid,
        fix,
    })
}

/// `GSV,total,number,in_view,...`. Every message of a set carries the
/// total in view, so only the count is read and the rest is ignored.
fn gsv<'a>(talker: &str, mut f: impl Iterator<Item = &'a str>) -> Result<Gsv, Malformed> {
    let _total = f.next().ok_or(Malformed::Field)?;
    let _number = f.next().ok_or(Malformed::Field)?;
    let in_view = small(f.next().ok_or(Malformed::Field)?)?.unwrap_or(0);
    let t = talker.as_bytes();
    Ok(Gsv {
        talker: [t[0], t[1]],
        in_view,
    })
}

/// `hhmmss` or `hhmmss.sss`; empty when the receiver has no time yet.
fn time(s: &str) -> Result<Option<Time>, Malformed> {
    if s.is_empty() {
        return Ok(None);
    }
    let whole = s.split('.').next().unwrap_or("");
    if whole.len() != 6 || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Malformed::Field);
    }
    let two = |at: usize| (whole.as_bytes()[at] - b'0') * 10 + (whole.as_bytes()[at + 1] - b'0');
    let (hour, minute, second) = (two(0), two(2), two(4));
    if hour > 23 || minute > 59 || second > 60 {
        return Err(Malformed::Field);
    }
    Ok(Some(Time {
        hour,
        minute,
        second,
    }))
}

/// `ddmmyy`; empty when the receiver has no date yet.
fn date(s: &str) -> Result<Option<Date>, Malformed> {
    if s.is_empty() {
        return Ok(None);
    }
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Malformed::Field);
    }
    let two = |at: usize| (s.as_bytes()[at] - b'0') * 10 + (s.as_bytes()[at + 1] - b'0');
    let (day, month, year) = (two(0), two(2), two(4));
    if day == 0 || day > 31 || month == 0 || month > 12 {
        return Err(Malformed::Field);
    }
    Ok(Some(Date {
        year: 2000 + year as u16,
        month,
        day,
    }))
}

/// A small unsigned count, or `None` for an empty field.
fn small(s: &str) -> Result<Option<u8>, Malformed> {
    if s.is_empty() {
        return Ok(None);
    }
    s.parse::<u8>().map(Some).map_err(|_| Malformed::Field)
}

/// A decimal number scaled to `places` fractional digits, so `408.0` at one
/// place is 4080 and `-2.4` is -24. Extra fractional digits are dropped
/// rather than rounded; a missing point is a whole number.
fn fixed(s: &str, places: u32) -> Result<i64, Malformed> {
    let (negative, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    if s.is_empty() {
        return Err(Malformed::Field);
    }
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Malformed::Field);
    }
    if whole.is_empty() && frac.is_empty() {
        return Err(Malformed::Field);
    }
    let mut value: i64 = whole.parse().unwrap_or(0);
    for n in 0..places as usize {
        value = value.checked_mul(10).ok_or(Malformed::Field)?;
        value += frac.as_bytes().get(n).map_or(0, |&b| (b - b'0') as i64);
    }
    Ok(if negative { -value } else { value })
}

/// Latitude and longitude as `ddmm.mmmm,N,dddmm.mmmm,E` into micro-degrees.
/// Empty fields -- a receiver without a fix sends them -- are `None`.
fn coordinates(lat: &str, ns: &str, lon: &str, ew: &str) -> Result<Option<(i32, i32)>, Malformed> {
    if lat.is_empty() && lon.is_empty() {
        return Ok(None);
    }
    let lat = angle(lat, 2)?;
    let lon = angle(lon, 3)?;
    let lat = match ns {
        "N" => lat,
        "S" => -lat,
        _ => return Err(Malformed::Field),
    };
    let lon = match ew {
        "E" => lon,
        "W" => -lon,
        _ => return Err(Malformed::Field),
    };
    if lat.abs() > 90_000_000 || lon.abs() > 180_000_000 {
        return Err(Malformed::Field);
    }
    Ok(Some((lat, lon)))
}

/// `ddmm.mmmm` with `degree_digits` digits of degrees, into micro-degrees.
fn angle(s: &str, degree_digits: usize) -> Result<i32, Malformed> {
    // The degrees are everything before the last two digits of the whole
    // part, and every receiver pads them: two digits of latitude, three of
    // longitude, always.
    let whole_len = s.split_once('.').map_or(s.len(), |(w, _)| w.len());
    if whole_len != degree_digits + 2 {
        return Err(Malformed::Field);
    }
    let (deg, min) = s.split_at(whole_len - 2);
    let deg: i64 = deg.parse().map_err(|_| Malformed::Field)?;
    // Minutes to a millionth of a minute, then to micro-degrees, rounded.
    let micro_min = fixed(min, 6)?;
    if !(0..60_000_000).contains(&micro_min) {
        return Err(Malformed::Field);
    }
    let udeg = deg * 1_000_000 + (micro_min + 30) / 60;
    i32::try_from(udeg).map_err(|_| Malformed::Field)
}

// ---- the receiver --------------------------------------------------------

/// How many constellations' views are remembered. GPS, GLONASS, BeiDou,
/// Galileo and QZSS is five; a sixth talker would be dropped from the sum,
/// not crash anything.
const TALKERS: usize = 5;

/// What the receiver currently knows, assembled from its sentences.
pub struct Receiver {
    lexer: Lexer,
    on: bool,
    heard_at: Option<Millis>,
    fix: Option<Fix>,
    /// When the receiver last said the fix was current.
    fix_at: Option<Millis>,
    sats_used: Option<u8>,
    in_view: [Option<([u8; 2], u8)>; TALKERS],
    time: Option<Time>,
    date: Option<Date>,
    sentences: u32,
    malformed: u32,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    /// A receiver that is off.
    pub const fn new() -> Self {
        Receiver {
            lexer: Lexer::new(),
            on: false,
            heard_at: None,
            fix: None,
            fix_at: None,
            sats_used: None,
            in_view: [None; TALKERS],
            time: None,
            date: None,
            sentences: 0,
            malformed: 0,
        }
    }

    /// The load switch was driven. Powering off forgets everything: the
    /// module's own memory is what makes the next fix quick, not ours, and
    /// a position shown after the receiver was deliberately turned off is a
    /// claim nobody is standing behind.
    pub fn power(&mut self, on: bool) {
        *self = Receiver::new();
        self.on = on;
    }

    /// Whether the load switch is on.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Whether any well-formed sentence has arrived since power-up. This is
    /// the probe's success: a checksum that matches at this baud rate on
    /// this pin is not a coincidence.
    pub fn heard(&self) -> bool {
        self.heard_at.is_some()
    }

    /// Sentences accepted and lines dropped since power-up, for the log.
    pub fn counts(&self) -> (u32, u32) {
        (self.sentences, self.malformed)
    }

    /// Feed bytes from the UART, as of `now`.
    pub fn feed(&mut self, bytes: &[u8], now: Millis) {
        for &b in bytes {
            match self.lexer.push(b) {
                Step::Pending => {}
                Step::Bad(_) => self.malformed += 1,
                Step::Sentence => match parse(self.lexer.sentence()) {
                    Ok(report) => {
                        self.sentences += 1;
                        self.heard_at = Some(now);
                        self.take(report, now);
                    }
                    Err(_) => self.malformed += 1,
                },
            }
        }
    }

    fn take(&mut self, report: Report, now: Millis) {
        match report {
            Report::Gga(gga) => {
                if let Some(t) = gga.time {
                    self.time = Some(t);
                }
                self.sats_used = Some(gga.sats_used);
                if let Some(fix) = gga.fix {
                    self.fix = Some(fix);
                    self.fix_at = Some(now);
                }
            }
            Report::Rmc(rmc) => {
                if let Some(t) = rmc.time {
                    self.time = Some(t);
                }
                if let Some(d) = rmc.date {
                    self.date = Some(d);
                }
                if let Some(fix) = rmc.fix {
                    // RMC has no altitude; keep GGA's if it is for the same
                    // place, which within a second it is.
                    let alt_dm = self.fix.and_then(|f| f.alt_dm);
                    self.fix = Some(Fix { alt_dm, ..fix });
                    self.fix_at = Some(now);
                }
            }
            Report::Gsv(gsv) => {
                let slot = self
                    .in_view
                    .iter()
                    .position(|s| matches!(s, Some((t, _)) if *t == gsv.talker))
                    .or_else(|| self.in_view.iter().position(Option::is_none));
                if let Some(slot) = slot {
                    self.in_view[slot] = Some((gsv.talker, gsv.in_view));
                }
            }
            Report::Other => {}
        }
    }

    /// What the receiver knows as of `now`, for the screen.
    pub fn position(&self, now: Millis) -> Position {
        if !self.on {
            return Position::OFF;
        }
        let heard = self
            .heard_at
            .is_some_and(|at| now.saturating_sub(at) < SILENT_AFTER_MS);
        let fix_age_s = self
            .fix_at
            .map(|at| (now.saturating_sub(at) / 1_000).min(u32::MAX as Millis) as u32);
        let status = if !heard {
            Status::Silent
        } else if self
            .fix_at
            .is_some_and(|at| now.saturating_sub(at) < FIX_STALE_AFTER_MS)
        {
            Status::Fix
        } else {
            Status::Searching
        };
        let sats_in_view = self
            .in_view
            .iter()
            .flatten()
            .map(|(_, n)| *n)
            .reduce(|a, b| a.saturating_add(b));
        Position {
            status,
            sats_used: self.sats_used,
            sats_in_view,
            fix: self.fix,
            fix_age_s,
            time: self.time,
            date: self.date,
        }
    }
}

// ---- power ---------------------------------------------------------------

/// Whether the receiver should be powered: the switch, and the menu.
///
/// The three-way switch has a position labelled GPS ON, and the reference
/// firmware turns its GPS on and off from it. So does this: the receiver is
/// on while the switch is there and off while it is at Power ON, and the
/// menu's `GPS On/Off` overrides that until the switch is next moved. A
/// board with no Super IO, where both lines float, has no GPS to power and
/// stays off. The default therefore *is* the switch, which is a considered
/// answer to the power question: the largest continuous draw on the board
/// is under a physical control whose position can be seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    on: bool,
    switch: Option<Mode>,
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

impl Control {
    /// Off, and the switch not yet read.
    pub const fn new() -> Self {
        Control {
            on: false,
            switch: None,
        }
    }

    /// Whether the receiver should be powered.
    pub const fn is_on(self) -> bool {
        self.on
    }

    /// The switch as read now. The receiver follows it when it *moves*;
    /// between moves the menu's choice stands. Returns whether the wanted
    /// state changed.
    pub fn switch(&mut self, mode: Mode) -> bool {
        if self.switch == Some(mode) {
            return false;
        }
        self.switch = Some(mode);
        let want = match mode {
            Mode::GpsOn => true,
            Mode::PowerOn => false,
            // Mid-travel, or no Super IO: says nothing.
            Mode::Neither | Mode::Invalid => return false,
        };
        if want == self.on {
            return false;
        }
        self.on = want;
        true
    }

    /// `GPS On/Off` from the menu. Returns the new state.
    pub fn toggle(&mut self) -> bool {
        self.on = !self.on;
        self.on
    }
}

// ---- the probe -----------------------------------------------------------

/// One way to listen: which pin, at what rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    /// Listen on P0.19 instead of P0.20. The schematic and the variant
    /// agree, read carefully, that the module transmits on P0.20 -- see
    /// `docs/hardware/gps.md` -- so that is tried first, and the other
    /// order is tried before giving up rather than trusted never to be
    /// needed.
    pub swapped: bool,
    pub baud: u32,
}

/// Every attempt in order: each baud rate on the expected pin, then each
/// on the other. The firmware runs one [`PROBE_WINDOW_MS`] per attempt and
/// stops at the first that [`Receiver::heard`].
pub fn attempts() -> impl Iterator<Item = Attempt> {
    [false, true].into_iter().flat_map(|swapped| {
        PROBE_BAUDS
            .into_iter()
            .map(move |baud| Attempt { swapped, baud })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from a Quectel L76K-class module at 9600 baud, in order,
    // through the first fix; a u-blox M8 from an older log; and the lines a
    // receiver sends while it has nothing.
    const FIX_GGA: &str =
        "$GNGGA,013407.000,4722.61322,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,*71\r\n";
    const FIX_RMC: &str =
        "$GNRMC,013407.000,A,4722.61322,N,00832.50164,E,0.12,0.00,080926,,,A*7F\r\n";
    const GSV_GP_1: &str =
        "$GPGSV,3,1,11,01,45,120,32,03,20,210,28,07,70,030,40,08,15,300,22*79\r\n";
    const GSV_GP_2: &str =
        "$GPGSV,3,2,11,11,50,250,30,14,10,090,18,17,35,330,27,22,25,060,20*7B\r\n";
    const GSV_GP_3: &str = "$GPGSV,3,3,11,27,05,140,,30,60,180,38,32,15,270,*4B\r\n";
    const GSV_GL: &str = "$GLGSV,1,1,03,65,30,100,25,72,60,200,35,81,10,020,*5D\r\n";
    const NOFIX_GGA: &str = "$GNGGA,013300.000,,,,,0,00,25.5,,,,,,*7B\r\n";
    const NOFIX_RMC: &str = "$GNRMC,013300.000,V,,,,,,,080926,,,N*57\r\n";
    const TXT: &str = "$GPTXT,01,01,02,ANTSTATUS=OK*3B\r\n";
    const SOUTH_WEST_GGA: &str =
        "$GNGGA,235959.500,3352.07830,S,15112.60370,E,2,12,0.80,-2.4,M,20.3,M,,*46\r\n";
    const UBLOX_GGA: &str =
        "$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76\r\n";
    const GLL: &str = "$GNGLL,4722.61322,N,00832.50164,E,013407.000,A,A*4E\r\n";
    const VTG: &str = "$GNVTG,0.00,T,,M,0.12,N,0.22,K,A*20\r\n";
    const GSA: &str = "$GNGSA,A,3,01,03,07,08,11,30,,,,,,,1.50,1.20,0.90*1C\r\n";

    const ZURICH: Fix = Fix {
        lat_udeg: 47_376_887,
        lon_udeg: 8_541_694,
        alt_dm: Some(4_080),
    };

    /// Run a whole capture through a lexer and collect what came out.
    fn lex(bytes: &[u8]) -> (Vec<String>, Vec<Malformed>) {
        let mut lexer = Lexer::new();
        let mut good = Vec::new();
        let mut bad = Vec::new();
        for &b in bytes {
            match lexer.push(b) {
                Step::Pending => {}
                Step::Sentence => good.push(lexer.sentence().to_string()),
                Step::Bad(why) => bad.push(why),
            }
        }
        (good, bad)
    }

    fn body(line: &str) -> &str {
        let line = line.trim_end();
        &line[1..line.rfind('*').unwrap()]
    }

    // ---- framing ----

    /// Every captured line frames, and its body is what was between `$`
    /// and `*`.
    #[test]
    fn captured_sentences_frame_with_their_checksums() {
        for line in [
            FIX_GGA,
            FIX_RMC,
            GSV_GP_1,
            GSV_GP_2,
            GSV_GP_3,
            GSV_GL,
            NOFIX_GGA,
            NOFIX_RMC,
            TXT,
            SOUTH_WEST_GGA,
            UBLOX_GGA,
            GLL,
            VTG,
            GSA,
        ] {
            let (good, bad) = lex(line.as_bytes());
            assert_eq!(good, [body(line)], "{line}");
            assert!(bad.is_empty(), "{line}: {bad:?}");
        }
    }

    /// One corrupted character fails the checksum, and the line is dropped
    /// whole rather than read with a wrong field.
    #[test]
    fn a_corrupted_character_fails_the_checksum() {
        let mut line = FIX_GGA.to_string();
        // 4722 -> 4122: a different latitude, one digit away.
        line.replace_range(19..20, "1");
        let (good, bad) = lex(line.as_bytes());
        assert!(good.is_empty());
        assert_eq!(bad, [Malformed::Checksum]);
        // Lower-case hex in the checksum is accepted; NMEA says upper but
        // some receivers do not.
        let (good, _) = lex(FIX_RMC.replace("*7F", "*7f").as_bytes());
        assert_eq!(good.len(), 1);
    }

    /// A line with no `*hh` on it, a bare `\r\n`, and a `$` with nothing
    /// after it are all dropped without disturbing the next sentence.
    #[test]
    fn lines_without_a_checksum_are_dropped() {
        let stream = format!("$GNGGA,013407.000\r\n\r\n$\r\n{FIX_RMC}");
        let (good, bad) = lex(stream.as_bytes());
        assert_eq!(good, [body(FIX_RMC)]);
        assert_eq!(bad, [Malformed::NoChecksum]);
        // A `*` with one digit, or with letters that are not hex.
        for tail in ["*7", "*7G", "*", "*717"] {
            let (good, bad) = lex(format!("$GPTXT,01,01,02,ANTSTATUS=OK{tail}\r\n").as_bytes());
            assert!(good.is_empty(), "{tail}");
            assert_eq!(bad, [Malformed::NoChecksum], "{tail}");
        }
    }

    /// The wrong baud rate produces bytes with the high bit set and control
    /// characters; every one is discarded, and the first real sentence after
    /// them is read.
    #[test]
    fn garbage_between_sentences_is_discarded() {
        let mut stream: Vec<u8> = vec![0xff, 0x00, 0xfe, 0x80, 0x07];
        stream.extend_from_slice(b"$GNG\xffGA,junk*00\r\n");
        stream.extend_from_slice(FIX_GGA.as_bytes());
        stream.extend_from_slice(&[0xaa, 0x55]);
        let (good, bad) = lex(&stream);
        assert_eq!(good, [body(FIX_GGA)]);
        // The noise before any `$` is not a lost line; the noise inside
        // one is, and what follows it up to the line end is not a line.
        assert_eq!(bad, [Malformed::Garbage]);
    }

    /// A `$` arriving mid-sentence -- the tail of one line lost -- starts
    /// the next one cleanly.
    #[test]
    fn a_new_start_abandons_the_old_line() {
        let stream = format!("$GNGGA,013407.000,4722.{FIX_RMC}");
        let (good, bad) = lex(stream.as_bytes());
        assert_eq!(good, [body(FIX_RMC)]);
        assert_eq!(bad, [Malformed::NoChecksum]);
    }

    /// A line longer than NMEA allows is framing lost, not a long sentence.
    #[test]
    fn an_overlong_line_is_dropped() {
        let mut stream = "$".to_string();
        stream.push_str(&"A".repeat(MAX_SENTENCE + 10));
        stream.push_str("*00\r\n");
        stream.push_str(FIX_GGA);
        let (good, bad) = lex(stream.as_bytes());
        assert_eq!(good, [body(FIX_GGA)]);
        assert_eq!(bad, [Malformed::TooLong]);
        // Exactly the limit fits: 82 characters including `$` and `\r\n`
        // is 79 in the buffer.
        let mut at_limit = "$GPTXT,".to_string();
        at_limit.push_str(&"x".repeat(MAX_SENTENCE - "$GPTXT,".len() - 3 - 2));
        let sum = at_limit.as_bytes()[1..].iter().fold(0u8, |a, &b| a ^ b);
        at_limit.push_str(&format!("*{sum:02X}\r\n"));
        assert_eq!(at_limit.len(), MAX_SENTENCE);
        let (good, bad) = lex(at_limit.as_bytes());
        assert_eq!(good.len(), 1, "{bad:?}");
    }

    /// A sentence read in fragments -- which is how a ring buffer hands it
    /// over -- is the same sentence.
    #[test]
    fn fragments_reassemble() {
        let mut lexer = Lexer::new();
        let bytes = FIX_GGA.as_bytes();
        let mut got = None;
        for chunk in bytes.chunks(7) {
            for &b in chunk {
                if lexer.push(b) == Step::Sentence {
                    got = Some(lexer.sentence().to_string());
                }
            }
        }
        assert_eq!(got.as_deref(), Some(body(FIX_GGA)));
    }

    // ---- parsing ----

    /// The fix sentence, field by field, to the micro-degree.
    #[test]
    fn gga_reads_the_fix() {
        let Report::Gga(gga) = parse(body(FIX_GGA)).unwrap() else {
            panic!("not GGA");
        };
        assert_eq!(
            gga.time,
            Some(Time {
                hour: 1,
                minute: 34,
                second: 7
            })
        );
        assert_eq!(gga.quality, 1);
        assert_eq!(gga.sats_used, 7);
        assert_eq!(gga.fix, Some(ZURICH));
    }

    /// Southern and western hemispheres are negative; a differential fix is
    /// a fix; a negative altitude is below sea level, not an error; an
    /// unpadded satellite count and a u-blox's four-place minutes read.
    #[test]
    fn the_other_hemispheres_and_the_other_receivers() {
        let Report::Gga(sydney) = parse(body(SOUTH_WEST_GGA)).unwrap() else {
            panic!()
        };
        assert_eq!(sydney.quality, 2);
        assert_eq!(sydney.sats_used, 12);
        // 33 52.07830 S: 33 + 52.0783 / 60 = 33.867972
        // 151 12.60370 E: 151 + 12.6037 / 60 = 151.210062
        assert_eq!(
            sydney.fix,
            Some(Fix {
                lat_udeg: -33_867_972,
                lon_udeg: 151_210_062,
                alt_dm: Some(-24),
            })
        );
        let Report::Gga(dublin) = parse(body(UBLOX_GGA)).unwrap() else {
            panic!()
        };
        assert_eq!(dublin.sats_used, 8);
        // 53 21.6802 N = 53.361337; 6 30.3372 W = -6.505620
        assert_eq!(
            dublin.fix,
            Some(Fix {
                lat_udeg: 53_361_337,
                lon_udeg: -6_505_620,
                alt_dm: Some(617),
            })
        );
    }

    /// No fix is `None`, not zero: the empty fields stay empty and the
    /// quality says why.
    #[test]
    fn gga_without_a_fix_has_no_coordinates() {
        let Report::Gga(gga) = parse(body(NOFIX_GGA)).unwrap() else {
            panic!()
        };
        assert_eq!(gga.quality, 0);
        assert_eq!(gga.sats_used, 0);
        assert_eq!(gga.fix, None);
        assert!(gga.time.is_some(), "the time is known before the fix is");
        // Some receivers send zeros with quality 0 rather than empty
        // fields. Quality 0 is still no fix.
        let zeros = "GNGGA,000000.000,0000.00000,N,00000.00000,E,0,00,99.9,0.0,M,0.0,M,,";
        let Report::Gga(gga) = parse(zeros).unwrap() else {
            panic!()
        };
        assert_eq!(gga.fix, None);
    }

    /// RMC carries the date, the validity flag, and the position without
    /// altitude.
    #[test]
    fn rmc_reads_the_date_and_the_flag() {
        let Report::Rmc(rmc) = parse(body(FIX_RMC)).unwrap() else {
            panic!()
        };
        assert!(rmc.valid);
        assert_eq!(
            rmc.date,
            Some(Date {
                year: 2026,
                month: 9,
                day: 8
            })
        );
        assert_eq!(
            rmc.fix,
            Some(Fix {
                alt_dm: None,
                ..ZURICH
            })
        );
        let Report::Rmc(void) = parse(body(NOFIX_RMC)).unwrap() else {
            panic!()
        };
        assert!(!void.valid);
        assert_eq!(void.fix, None);
        assert!(void.date.is_some());
    }

    /// GSV gives the count in view per talker, from any message of a set.
    #[test]
    fn gsv_reads_the_count_in_view() {
        for line in [GSV_GP_1, GSV_GP_2, GSV_GP_3] {
            assert_eq!(
                parse(body(line)).unwrap(),
                Report::Gsv(Gsv {
                    talker: *b"GP",
                    in_view: 11
                })
            );
        }
        assert_eq!(
            parse(body(GSV_GL)).unwrap(),
            Report::Gsv(Gsv {
                talker: *b"GL",
                in_view: 3
            })
        );
    }

    /// Sentences that are not wanted are `Other`, not errors: the receiver
    /// sends plenty of them and every one is fine.
    #[test]
    fn other_sentences_are_ignored_not_rejected() {
        for line in [TXT, GLL, VTG, GSA] {
            assert_eq!(parse(body(line)).unwrap(), Report::Other, "{line}");
        }
        assert_eq!(parse("PMTK001,314,3").unwrap(), Report::Other);
        assert_eq!(parse("PCAS06,0").unwrap(), Report::Other);
        assert_eq!(parse("").unwrap(), Report::Other);
        assert_eq!(parse("GN").unwrap(), Report::Other);
    }

    /// Framed correctly and wrong inside: every one is a `Field` error,
    /// and none is a coordinate.
    #[test]
    fn malformed_fields_are_refused() {
        let bad = [
            // Too few fields.
            "GNGGA,013407.000,4722.61322,N",
            // A hemisphere that is not one.
            "GNGGA,013407.000,4722.61322,X,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            // Minutes past sixty.
            "GNGGA,013407.000,4761.00000,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            // Latitude past ninety.
            "GNGGA,013407.000,9100.00000,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            // Longitude past 180.
            "GNGGA,013407.000,4722.61322,N,18100.00000,E,1,07,1.20,408.0,M,47.1,M,,",
            // Letters where digits go.
            "GNGGA,013407.000,47ab.61322,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            "GNGGA,0134O7.000,4722.61322,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            "GNGGA,013407.000,4722.61322,N,00832.50164,E,x,07,1.20,408.0,M,47.1,M,,",
            "GNGGA,013407.000,4722.61322,N,00832.50164,E,1,07,1.20,4O8.0,M,47.1,M,,",
            // Altitude in something other than metres.
            "GNGGA,013407.000,4722.61322,N,00832.50164,E,1,07,1.20,408.0,F,47.1,M,,",
            // A time of day that does not exist.
            "GNGGA,250000.000,4722.61322,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            // A latitude with only one digit before the minutes.
            "GNGGA,013407.000,722.61322,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            // A fix with one coordinate missing.
            "GNGGA,013407.000,,N,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,",
            // RMC with a status that is neither A nor V, and a bad date.
            "GNRMC,013407.000,Q,4722.61322,N,00832.50164,E,0.12,0.00,080926,,,A",
            "GNRMC,013407.000,A,4722.61322,N,00832.50164,E,0.12,0.00,320926,,,A",
            "GNRMC,013407.000,A,4722.61322,N,00832.50164,E,0.12,0.00,08092,,,A",
            // GSV with a count that is not a number.
            "GPGSV,3,1,eleven,01,45,120,32",
            "GPGSV,3",
        ];
        for line in bad {
            assert_eq!(parse(line), Err(Malformed::Field), "{line}");
        }
    }

    /// Angles convert exactly at the boundaries: a whole degree, sixty
    /// minutes short of one, and a rounding that lands on the micro-degree.
    #[test]
    fn angles_to_micro_degrees() {
        assert_eq!(angle("4700.00000", 2), Ok(47_000_000));
        assert_eq!(angle("4730.00000", 2), Ok(47_500_000));
        assert_eq!(angle("4759.99999", 2), Ok(48_000_000));
        assert_eq!(angle("00000.0000", 3), Ok(0));
        assert_eq!(angle("18000.0000", 3), Ok(180_000_000));
        // No fraction at all, and a fraction longer than six digits.
        assert_eq!(angle("4730", 2), Ok(47_500_000));
        assert_eq!(angle("4730.000000999", 2), Ok(47_500_000));
        assert_eq!(angle("47.5", 2), Err(Malformed::Field));
    }

    /// Fixed-point parsing keeps exactly the places asked for.
    #[test]
    fn fixed_point_reads_as_written() {
        assert_eq!(fixed("408.0", 1), Ok(4080));
        assert_eq!(fixed("408", 1), Ok(4080));
        assert_eq!(fixed("-2.4", 1), Ok(-24));
        assert_eq!(fixed("-0.05", 1), Ok(0));
        assert_eq!(fixed("1.23456789", 6), Ok(1_234_567));
        assert_eq!(fixed(".5", 1), Ok(5));
        assert_eq!(fixed("", 1), Err(Malformed::Field));
        assert_eq!(fixed("-", 1), Err(Malformed::Field));
        assert_eq!(fixed(".", 1), Err(Malformed::Field));
        assert_eq!(fixed("1e3", 1), Err(Malformed::Field));
    }

    // ---- the receiver ----

    fn feed(rx: &mut Receiver, lines: &[&str], now: Millis) {
        for line in lines {
            rx.feed(line.as_bytes(), now);
        }
    }

    /// Off knows nothing; on and silent knows it is silent; a fix is a fix.
    #[test]
    fn the_receiver_follows_the_stream() {
        let mut rx = Receiver::new();
        assert_eq!(rx.position(0), Position::OFF);
        assert!(!rx.heard());

        rx.power(true);
        assert_eq!(rx.position(100).status, Status::Silent);
        assert!(!rx.heard());

        feed(&mut rx, &[NOFIX_GGA, NOFIX_RMC, GSV_GP_1, GSV_GL], 1_000);
        assert!(rx.heard());
        let p = rx.position(1_500);
        assert_eq!(p.status, Status::Searching);
        assert_eq!(p.sats_used, Some(0));
        assert_eq!(p.sats_in_view, Some(14), "GPS 11 plus GLONASS 3");
        assert_eq!(p.fix, None);
        assert_eq!(p.fix_age_s, None);
        assert_eq!(p.time.map(|t| t.minute), Some(33));
        assert_eq!(p.date.map(|d| d.day), Some(8));

        feed(&mut rx, &[FIX_GGA, FIX_RMC, GLL, VTG, GSA], 2_000);
        let p = rx.position(2_400);
        assert_eq!(p.status, Status::Fix);
        assert_eq!(p.sats_used, Some(7));
        assert_eq!(p.fix, Some(ZURICH), "altitude from GGA survives RMC");
        assert_eq!(p.fix_age_s, Some(0));
        assert_eq!(p.time.map(|t| t.second), Some(7));
        assert_eq!(rx.counts(), (9, 0));
    }

    /// A fix that stops being confirmed is kept, aged, and the status says
    /// searching; silence says silent; power off forgets it all.
    #[test]
    fn a_lost_fix_is_kept_and_aged() {
        let mut rx = Receiver::new();
        rx.power(true);
        feed(&mut rx, &[FIX_GGA, FIX_RMC], 10_000);
        assert_eq!(rx.position(12_000).status, Status::Fix);
        assert_eq!(rx.position(12_000).fix_age_s, Some(2));

        // Still talking, no longer fixed.
        feed(&mut rx, &[NOFIX_GGA, NOFIX_RMC], 14_000);
        feed(&mut rx, &[NOFIX_GGA, NOFIX_RMC], 16_000);
        let p = rx.position(16_500);
        assert_eq!(p.status, Status::Searching);
        assert_eq!(p.fix, Some(ZURICH), "the last fix stays");
        assert_eq!(p.fix_age_s, Some(6));
        assert_eq!(p.sats_used, Some(0));

        // Then nothing at all.
        let p = rx.position(16_000 + SILENT_AFTER_MS);
        assert_eq!(p.status, Status::Silent);
        assert_eq!(p.fix, Some(ZURICH));

        rx.power(false);
        assert_eq!(rx.position(20_000), Position::OFF);
        rx.power(true);
        let p = rx.position(20_000);
        assert_eq!(p.status, Status::Silent);
        assert_eq!(p.fix, None, "power off forgot the fix");
        assert_eq!(rx.counts(), (0, 0));
    }

    /// A stream with corruption in it: the good lines count, the bad ones
    /// are counted, and the fix comes from the good ones.
    #[test]
    fn corruption_is_counted_and_survived() {
        let mut rx = Receiver::new();
        rx.power(true);
        let mut stream = Vec::new();
        stream.extend_from_slice(&[0xff, 0xfe]);
        stream.extend_from_slice(FIX_GGA.replace("408.0", "409.0").as_bytes()); // checksum
        stream.extend_from_slice(b"$GNGGA,garbage,without,a,star\r\n");
        stream.extend_from_slice(
            b"$GNGGA,013407.000,4722.61322,X,00832.50164,E,1,07,1.20,408.0,M,47.1,M,,*67\r\n",
        );
        stream.extend_from_slice(FIX_GGA.as_bytes());
        rx.feed(&stream, 1_000);
        assert_eq!(rx.counts(), (1, 3));
        assert_eq!(rx.position(1_000).fix, Some(ZURICH));
    }

    /// The count in view is the sum over talkers, updated in place as each
    /// constellation reports again, and bounded by the slots.
    #[test]
    fn satellites_in_view_are_summed_over_talkers() {
        let mut rx = Receiver::new();
        rx.power(true);
        feed(&mut rx, &[GSV_GP_1, GSV_GP_2, GSV_GP_3, GSV_GL], 0);
        assert_eq!(rx.position(0).sats_in_view, Some(14));
        // GPS reports fewer next cycle: replaced, not added.
        rx.feed(
            b"$GPGSV,1,1,04,01,45,120,32,03,20,210,28,07,70,030,40,08,15,300,22*7F\r\n",
            1_000,
        );
        assert_eq!(rx.position(1_000).sats_in_view, Some(7));
        // Three more constellations fill the table; a sixth is dropped.
        for (talker, n) in [("GB", 5), ("GA", 6), ("GQ", 1), ("GI", 9)] {
            let body = format!("{talker}GSV,1,1,{n:02},01,45,120,32");
            let sum = body.bytes().fold(0u8, |a, b| a ^ b);
            rx.feed(format!("${body}*{sum:02X}\r\n").as_bytes(), 2_000);
        }
        assert_eq!(rx.position(2_000).sats_in_view, Some(7 + 5 + 6 + 1));
    }

    /// The age does not wrap when the clock runs long, and does not go
    /// negative if it runs short.
    #[test]
    fn the_age_is_bounded() {
        let mut rx = Receiver::new();
        rx.power(true);
        feed(&mut rx, &[FIX_GGA], 5_000);
        assert_eq!(rx.position(4_000).fix_age_s, Some(0));
        assert_eq!(rx.position(u64::MAX).fix_age_s, Some(u32::MAX));
    }

    // ---- power ----

    /// The switch decides at boot and whenever it moves; the menu decides
    /// in between.
    #[test]
    fn the_switch_and_the_menu_share_the_decision() {
        let mut c = Control::new();
        assert!(!c.is_on());
        assert!(c.switch(Mode::GpsOn), "GPS ON at boot turns it on");
        assert!(c.is_on());
        assert!(
            !c.switch(Mode::GpsOn),
            "reading the same position again is not a move"
        );
        assert!(!c.toggle(), "the menu turns it off");
        assert!(
            !c.switch(Mode::GpsOn),
            "and the switch, unmoved, does not turn it back on"
        );
        assert!(
            !c.switch(Mode::PowerOn),
            "moving to Power ON while already off changes nothing"
        );
        assert!(c.switch(Mode::GpsOn), "moving back to GPS ON turns it on");
        assert!(c.switch(Mode::PowerOn), "and to Power ON turns it off");
        assert!(c.toggle(), "the menu turns it on at Power ON");
        assert!(!c.switch(Mode::PowerOn), "unmoved, the switch leaves it");
    }

    /// Boot at Power ON, a board with no Super IO, and a switch caught
    /// mid-travel all leave the receiver off.
    #[test]
    fn nothing_but_gps_on_turns_it_on() {
        for mode in [Mode::PowerOn, Mode::Neither, Mode::Invalid] {
            let mut c = Control::new();
            assert!(!c.switch(mode), "{mode:?}");
            assert!(!c.is_on(), "{mode:?}");
        }
        // Mid-travel between the two positions does not turn a running
        // receiver off.
        let mut c = Control::new();
        c.switch(Mode::GpsOn);
        assert!(!c.switch(Mode::Neither));
        assert!(c.is_on());
        assert!(!c.switch(Mode::GpsOn));
        assert!(c.is_on());
    }

    // ---- the probe ----

    /// Every baud on the expected pin before any on the other, most likely
    /// first, and nothing twice.
    #[test]
    fn the_probe_tries_the_expected_pin_first() {
        let all: Vec<Attempt> = attempts().collect();
        assert_eq!(all.len(), 2 * PROBE_BAUDS.len());
        assert_eq!(
            all[0],
            Attempt {
                swapped: false,
                baud: 9_600
            }
        );
        assert!(all[..PROBE_BAUDS.len()].iter().all(|a| !a.swapped));
        assert!(all[PROBE_BAUDS.len()..].iter().all(|a| a.swapped));
        for (n, a) in all.iter().enumerate() {
            assert!(!all[..n].contains(a), "{a:?} repeats");
        }
        // A window is longer than the receiver's cycle, or a probe could
        // miss a healthy module.
        const { assert!(PROBE_WINDOW_MS >= 2_000) };
    }

    /// The words on the screen are the receiver's four states.
    #[test]
    fn every_status_has_a_word() {
        let words: Vec<&str> = [Status::Off, Status::Silent, Status::Searching, Status::Fix]
            .iter()
            .map(|s| s.word())
            .collect();
        assert_eq!(words, ["off", "no data", "searching", "fix"]);
        assert_eq!(Position::default(), Position::OFF);
    }
}
