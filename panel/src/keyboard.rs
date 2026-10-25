//! Typing on a six-way pad: a grid of keys under the text it types.
//!
//! The directions move a highlight over the grid, select types the key
//! under it, and back takes the last character away. There is no holding a
//! key for a capital and no pressing twice for a symbol: one gesture, one
//! meaning, as everywhere else in this crate. A capital is the *Shift* key
//! and then the letter, and *Shift* also turns the number row into its
//! symbols, so what the grid shows is always what select types.
//!
//! # The grid
//!
//! Four rows of ten characters in the order of a QWERTY keyboard, with the
//! digits on top, and a wider column on the right of the four keys that are
//! not characters:
//!
//! ```text
//! 1 2 3 4 5 6 7 8 9 0  Quit
//! q w e r t y u i o p  Send
//! a s d f g h j k l '  Space
//! z x c v b n m . , ?  Shift
//! ```
//!
//! The highlight wraps at every edge, so no key is more than two rows and
//! five columns from any other, and it starts on `h`, in the middle. On short
//! messages that is about four and a half presses a character, moves and
//! the press together -- [`tests::presses_per_character`] measures it --
//! against about ten for a wheel of letters at each position. The only
//! characters are ones the font has, so nothing typed draws as a box.
//!
//! # Leaving and sending
//!
//! Back on an empty message leaves, since there is nothing to lose. With
//! text there, back deletes, and leaving is the *Quit* key, which asks
//! first: a stray press does not throw a message away.
//!
//! *Send* hands the text to the application ([`Typed::Send`]) and waits:
//! keys are ignored until the application says it was refused
//! ([`Keyboard::refuse`]), and why, or closes the keyboard because it was
//! sent. A press that was already queued behind *Send* cannot change what
//! is being sent.
//!
//! The text is held to `N` bytes, the most the application can send. A
//! character past that is refused with a note, never cut from somewhere
//! else.

use crate::font;
use crate::layout::Layout;
use crate::nav::Input;
use crate::Canvas;

/// Columns in the grid: ten characters and the wide column.
pub const COLS: usize = 11;
/// Rows in the grid.
pub const ROWS: usize = 4;

/// Where the highlight starts: on `h`.
pub const START: (usize, usize) = (2, 5);

/// One key in the grid.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Key {
    /// A character, and the one it types with *Shift* on.
    Char(u8, u8),
    /// Leave, asking first if there is text.
    Quit,
    /// Hand the text to the application.
    Send,
    /// A space.
    Space,
    /// The next character from the other half of its key.
    Shift,
}

impl Key {
    /// The word on a key that is not a character.
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Key::Char(..) => None,
            Key::Quit => Some("Quit"),
            Key::Send => Some("Send"),
            Key::Space => Some("Space"),
            Key::Shift => Some("Shift"),
        }
    }
}

/// A letter, which shifts to its capital.
const fn letter(c: u8) -> Key {
    Key::Char(c, c.to_ascii_uppercase())
}

/// A character that is the same either way.
const fn same(c: u8) -> Key {
    Key::Char(c, c)
}

/// The grid, row by row.
pub const KEYS: [[Key; COLS]; ROWS] = [
    [
        Key::Char(b'1', b'!'),
        Key::Char(b'2', b'#'),
        Key::Char(b'3', b'%'),
        Key::Char(b'4', b'('),
        Key::Char(b'5', b')'),
        Key::Char(b'6', b'*'),
        Key::Char(b'7', b'+'),
        Key::Char(b'8', b'-'),
        Key::Char(b'9', b'='),
        Key::Char(b'0', b'/'),
        Key::Quit,
    ],
    [
        letter(b'q'),
        letter(b'w'),
        letter(b'e'),
        letter(b'r'),
        letter(b't'),
        letter(b'y'),
        letter(b'u'),
        letter(b'i'),
        letter(b'o'),
        letter(b'p'),
        Key::Send,
    ],
    [
        letter(b'a'),
        letter(b's'),
        letter(b'd'),
        letter(b'f'),
        letter(b'g'),
        letter(b'h'),
        letter(b'j'),
        letter(b'k'),
        letter(b'l'),
        Key::Char(b'\'', b':'),
        Key::Space,
    ],
    [
        letter(b'z'),
        letter(b'x'),
        letter(b'c'),
        letter(b'v'),
        letter(b'b'),
        letter(b'n'),
        letter(b'm'),
        same(b'.'),
        same(b','),
        same(b'?'),
        Key::Shift,
    ],
];

/// Where `c` is in the grid, and whether it needs *Shift*. `None` for a
/// character the grid cannot type.
pub fn find(c: u8) -> Option<(usize, usize, bool)> {
    if c == b' ' {
        return Some((2, COLS - 1, false));
    }
    for (r, row) in KEYS.iter().enumerate() {
        for (k, key) in row.iter().enumerate() {
            if let Key::Char(plain, shifted) = *key {
                if plain == c {
                    return Some((r, k, false));
                }
                if shifted == c {
                    return Some((r, k, true));
                }
            }
        }
    }
    None
}

/// What a key did.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Typed {
    /// The keyboard stays open, possibly changed.
    Stay,
    /// The user left, and the text with it.
    Leave,
    /// The user sent the text: [`Keyboard::text`] is what to send. The
    /// keyboard waits for the answer.
    Send,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Mode {
    Typing,
    /// *Quit* with text there: asking whether to throw it away.
    Quitting,
    /// Sent, and waiting to hear whether it went.
    Sending,
    /// It did not go: a headline and the reason. Any key goes back to the
    /// text, which is still there.
    Refused(&'static str, &'static str),
}

/// Why the last press did nothing, said on the status line until the next.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Note {
    Full,
    Empty,
}

impl Note {
    const fn text(self) -> &'static str {
        match self {
            Note::Full => "No room",
            Note::Empty => "Nothing yet",
        }
    }
}

/// The lines the *Quit* question is, each one a line on the panel.
pub const QUIT_LINES: [&str; 3] = [
    "Throw this away?",
    "OK throws it away.",
    "Back keeps typing.",
];

/// A message being typed, of at most `N` bytes, and where the highlight is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Keyboard<const N: usize> {
    text: [u8; N],
    len: usize,
    row: u8,
    col: u8,
    shift: bool,
    mode: Mode,
    note: Option<Note>,
}

impl<const N: usize> Default for Keyboard<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Keyboard<N> {
    /// An empty message, the highlight on `h`.
    pub const fn new() -> Self {
        Keyboard {
            text: [0; N],
            len: 0,
            row: START.0 as u8,
            col: START.1 as u8,
            shift: false,
            mode: Mode::Typing,
            note: None,
        }
    }

    /// What has been typed.
    pub fn text(&self) -> &str {
        // Only the grid's characters are ever stored, and they are ASCII.
        core::str::from_utf8(&self.text[..self.len]).unwrap_or("")
    }

    /// Bytes typed.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing has been typed.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Row and column of the highlight.
    pub const fn at(&self) -> (usize, usize) {
        (self.row as usize, self.col as usize)
    }

    /// The highlighted key.
    pub const fn key(&self) -> Key {
        KEYS[self.row as usize][self.col as usize]
    }

    /// Whether the next character comes from the other half of its key.
    pub const fn shifted(&self) -> bool {
        self.shift
    }

    /// Whether the *Quit* question is up.
    pub const fn quitting(&self) -> bool {
        matches!(self.mode, Mode::Quitting)
    }

    /// Whether the text has been sent and the answer is awaited.
    pub const fn sending(&self) -> bool {
        matches!(self.mode, Mode::Sending)
    }

    /// The refusal being shown, if one is: its headline.
    pub const fn refused(&self) -> Option<&'static str> {
        match self.mode {
            Mode::Refused(headline, _) => Some(headline),
            _ => None,
        }
    }

    /// The application's answer to [`Typed::Send`] when it could not send:
    /// why, as a headline and a reason. The text stays, for another go.
    pub fn refuse(&mut self, headline: &'static str, reason: &'static str) {
        self.mode = Mode::Refused(headline, reason);
    }

    /// One key.
    pub fn handle(&mut self, input: Input) -> Typed {
        match self.mode {
            Mode::Sending => Typed::Stay,
            // Any key acknowledges, and does nothing else: it was pressed
            // to read on, not to type.
            Mode::Refused(..) => {
                self.mode = Mode::Typing;
                Typed::Stay
            }
            Mode::Quitting => match input {
                Input::Select => Typed::Leave,
                Input::Back => {
                    self.mode = Mode::Typing;
                    Typed::Stay
                }
                _ => Typed::Stay,
            },
            Mode::Typing => {
                self.note = None;
                self.typing(input)
            }
        }
    }

    fn typing(&mut self, input: Input) -> Typed {
        match input {
            Input::Up => self.step(ROWS - 1, 0),
            Input::Down => self.step(1, 0),
            Input::Left => self.step(0, COLS - 1),
            Input::Right => self.step(0, 1),
            Input::Back if self.len == 0 => return Typed::Leave,
            Input::Back => self.len -= 1,
            Input::Select => return self.press(),
        }
        Typed::Stay
    }

    /// Move the highlight down `rows` and right `cols`, wrapping.
    fn step(&mut self, rows: usize, cols: usize) {
        self.row = ((self.row as usize + rows) % ROWS) as u8;
        self.col = ((self.col as usize + cols) % COLS) as u8;
    }

    fn press(&mut self) -> Typed {
        match self.key() {
            Key::Char(plain, shifted) => {
                self.push(if self.shift { shifted } else { plain });
            }
            Key::Space => self.push(b' '),
            Key::Shift => self.shift = !self.shift,
            Key::Send if self.len == 0 => self.note = Some(Note::Empty),
            Key::Send => {
                self.mode = Mode::Sending;
                return Typed::Send;
            }
            Key::Quit if self.len == 0 => return Typed::Leave,
            Key::Quit => self.mode = Mode::Quitting,
        }
        Typed::Stay
    }

    /// Type one character, or say there is no room for it. *Shift* is spent
    /// either way: it was for this press.
    fn push(&mut self, c: u8) {
        self.shift = false;
        if self.len == N {
            self.note = Some(Note::Full);
            return;
        }
        self.text[self.len] = c;
        self.len += 1;
    }
}

/// How the grid and the text above it divide the content area.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Geometry {
    /// Height of one row of keys.
    pub row_h: usize,
    /// First pixel row of the grid.
    pub grid_top: usize,
    /// Where the row of the rule over the grid is, if there is room for one.
    pub rule: Option<usize>,
    /// Lines of text that fit above the grid.
    pub lines: usize,
    /// Width of one character key.
    pub cell_w: usize,
    /// Width of the wide column.
    pub wide_w: usize,
    /// Left edge of the grid.
    pub left: usize,
}

/// Lines that fit in `h` pixels: the last one needs no gap under it.
const fn lines_in(h: usize) -> usize {
    if h < font::HEIGHT {
        0
    } else {
        (h - font::HEIGHT) / font::LINE_HEIGHT + 1
    }
}

impl Geometry {
    /// Where things go on `layout`. Keys with a pixel of air above and
    /// below a glyph and two lines of text above them if the panel has the
    /// room; tight keys and one line if it has not.
    pub const fn of(layout: &Layout) -> Geometry {
        let roomy = font::HEIGHT + 4;
        let tight = font::HEIGHT + 1;
        let row_h = if layout.content_h() >= ROWS * roomy + 2 + font::HEIGHT + font::LINE_HEIGHT {
            roomy
        } else {
            tight
        };
        let grid_top = layout.content_bottom.saturating_sub(ROWS * row_h);
        let above = grid_top.saturating_sub(layout.content_top);
        let (rule, lines) = if lines_in(above.saturating_sub(2)) >= 2 {
            (Some(grid_top - 2), lines_in(above - 2))
        } else {
            (None, lines_in(above))
        };
        // Wide enough for the longest word on a key, and a pixel either side.
        let wide_w = font::width_of("Shift") + 2;
        let cell_w = layout.width.saturating_sub(wide_w) / (COLS - 1);
        let left = layout.width.saturating_sub(cell_w * (COLS - 1) + wide_w) / 2;
        Geometry {
            row_h,
            grid_top,
            rule,
            lines,
            cell_w,
            wide_w,
            left,
        }
    }

    /// Left edge and width of the keys in column `col`.
    pub const fn column(&self, col: usize) -> (usize, usize) {
        if col < COLS - 1 {
            (self.left + col * self.cell_w, self.cell_w)
        } else {
            (self.left + (COLS - 1) * self.cell_w, self.wide_w)
        }
    }
}

/// `n` as decimal digits into `out`, returning them.
fn decimal(n: usize, out: &mut [u8; 20]) -> &str {
    let mut i = out.len();
    let mut n = n;
    loop {
        i -= 1;
        out[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&out[i..]).unwrap_or("")
}

/// Draw the keyboard over the content area: the text, a status line, the
/// grid. The chrome is the caller's ([`crate::page`] with no lines, the
/// keyboard as its modal).
pub fn draw<const N: usize>(canvas: &mut impl Canvas, keyboard: &Keyboard<N>) {
    let layout = Layout::for_canvas(canvas);
    let g = Geometry::of(&layout);
    draw_text(canvas, keyboard, &layout, &g);
    if let Some(y) = g.rule {
        canvas.rect(0, y, layout.width, 1, true);
    }
    for (r, row) in KEYS.iter().enumerate() {
        let y = g.grid_top + r * g.row_h;
        for (c, key) in row.iter().enumerate() {
            let (x, w) = g.column(c);
            let lit = keyboard.at() == (r, c);
            if lit {
                canvas.rect(x, y, w, g.row_h, true);
            }
            let ty = y + (g.row_h - font::HEIGHT) / 2;
            match key {
                Key::Char(plain, shifted) => {
                    let ch = if keyboard.shifted() { *shifted } else { *plain };
                    font::draw_char(canvas, x + (w - font::WIDTH) / 2, ty, ch, !lit);
                }
                other => {
                    let label = other.label().unwrap_or("");
                    font::draw(canvas, x + 2, ty, label, !lit);
                    // Shift on is framed, so it shows when the highlight is
                    // elsewhere; the grid's capitals say it too.
                    if *other == Key::Shift && keyboard.shifted() && !lit {
                        canvas.frame_rect(x, y, w, g.row_h, true);
                    }
                }
            }
        }
    }
}

/// The text, or the question, or the refusal: whichever is up.
fn draw_text<const N: usize>(
    canvas: &mut impl Canvas,
    keyboard: &Keyboard<N>,
    layout: &Layout,
    g: &Geometry,
) {
    let x = layout.content_left;
    let line_y = |n: usize| layout.content_top + n * font::LINE_HEIGHT;
    match keyboard.mode {
        Mode::Quitting => {
            for (n, line) in QUIT_LINES.iter().take(g.lines).enumerate() {
                font::draw(canvas, x, line_y(n), line, true);
            }
            return;
        }
        Mode::Refused(headline, reason) => {
            if g.lines > 0 {
                font::draw(canvas, x, line_y(0), headline, true);
            }
            let reason = wrap(reason, layout.line_chars());
            for (n, line) in (1..g.lines).zip(reason) {
                font::draw(canvas, x, line_y(n), line, true);
            }
            return;
        }
        Mode::Typing | Mode::Sending => {}
    }
    if g.lines == 0 {
        return;
    }
    // The status line: what the last press did, or what is going on, and
    // the count against the limit.
    let mut digits = [0u8; 20];
    let mut limit = [0u8; 20];
    let used = decimal(keyboard.len(), &mut digits);
    let most = decimal(N, &mut limit);
    let count_w = font::width_of(used) + font::width_of("/") + font::width_of(most);
    // The right edge of a content line, which on a modal has no scrollbar
    // beside it but stops where one would be.
    let right = layout.content_left + layout.line_width() - 1;
    let status = match (keyboard.mode, keyboard.note) {
        (Mode::Sending, _) => Some("Sending"),
        (_, Some(note)) => Some(note.text()),
        _ if keyboard.shifted() => Some("Shift"),
        _ => None,
    };
    let chars = layout.line_chars();
    let (text_lines, status_y) = if g.lines >= 2 {
        (g.lines - 1, line_y(g.lines - 1))
    } else {
        (0, line_y(0))
    };
    if g.lines >= 2 {
        if let Some(status) = status {
            font::draw(canvas, x, status_y, status, true);
        }
    }
    let count_x = right.saturating_sub(count_w) + font::SPACING;
    let cx = font::draw(canvas, count_x, status_y, used, true);
    let cx = font::draw(canvas, cx, status_y, "/", true);
    font::draw(canvas, cx, status_y, most, true);

    let text = keyboard.text().as_bytes();
    // The cursor is a character of its own, so a full line wraps it.
    let cursor = !keyboard.sending();
    let total = text.len() + usize::from(cursor);
    if text_lines == 0 {
        // One line: the end of the text, beside the count.
        let room = count_x.saturating_sub(x + font::ADVANCE) / font::ADVANCE;
        let from = total.saturating_sub(room);
        draw_run(canvas, x, line_y(0), text, from, total);
        return;
    }
    // The last lines, as many as fit: the end is where the typing is.
    let mut shown = [(0usize, 0usize); MAX_TEXT_LINES];
    let mut count = 0;
    for run in runs(text, total, chars) {
        shown[count % MAX_TEXT_LINES] = run;
        count += 1;
    }
    let keep = text_lines.min(count).min(MAX_TEXT_LINES);
    for n in 0..keep {
        let (from, to) = shown[(count - keep + n) % MAX_TEXT_LINES];
        draw_run(canvas, x, line_y(n), text, from, to);
    }
}

/// The most lines of typed text drawn, whatever the panel.
const MAX_TEXT_LINES: usize = 16;

/// The text and the cursor after it, `total` characters, as runs of at most
/// `width`: broken at a space where there is one, which the break eats, and
/// inside a word only where the word is longer than a line.
fn runs(text: &[u8], total: usize, width: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let at = move |i: usize| text.get(i).copied().unwrap_or(b'_');
    let mut from = 0;
    let mut done = width == 0;
    core::iter::from_fn(move || {
        if done {
            return None;
        }
        if total - from <= width {
            done = true;
            return Some((from, total));
        }
        let space = (from + 1..=from + width).rev().find(|&i| at(i) == b' ');
        let (to, next) = match space {
            Some(i) => (i, i + 1),
            None => (from + width, from + width),
        };
        let run = (from, to);
        from = next;
        Some(run)
    })
}

/// Characters `from..to` of the text, where the one past its end is the
/// cursor.
fn draw_run(canvas: &mut impl Canvas, x: usize, y: usize, text: &[u8], from: usize, to: usize) {
    let mut cx = x;
    for i in from..to {
        let c = text.get(i).copied().unwrap_or(b'_');
        cx = font::draw_char(canvas, cx, y, c, true);
    }
}

/// `text` in lines of at most `width` characters, broken at spaces where
/// it can be and inside a word only where a word is longer than a line.
pub fn wrap(text: &str, width: usize) -> impl Iterator<Item = &str> {
    let mut rest = text.trim_start();
    core::iter::from_fn(move || {
        if rest.is_empty() || width == 0 {
            return None;
        }
        let cut = if rest.len() <= width {
            rest.len()
        } else {
            match rest.as_bytes()[..=width].iter().rposition(|&c| c == b' ') {
                Some(0) | None => width,
                Some(space) => space,
            }
        };
        let (line, tail) = rest.split_at(cut);
        rest = tail.trim_start();
        Some(line.trim_end())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bitmap, Readable};

    type Square = Bitmap<128, 128>;

    /// Short messages people send from a radio, for the counts.
    const SAMPLE: [&str; 10] = [
        "on my way",
        "where are you",
        "at the trailhead in 10",
        "ok see you soon",
        "can you hear me",
        "battery low, back at camp",
        "meet at the car at 5",
        "yes",
        "no signal here",
        "heading north now",
    ];

    /// The presses that type `text` from a fresh keyboard: the shortest way
    /// round to each key, wrapping, *Shift* first where it is needed.
    fn presses(text: &str) -> Vec<Input> {
        let mut out = Vec::new();
        let mut at = START;
        let mut go = |to: (usize, usize), out: &mut Vec<Input>| {
            let down = (to.0 + ROWS - at.0) % ROWS;
            let right = (to.1 + COLS - at.1) % COLS;
            if down <= ROWS - down {
                out.extend(core::iter::repeat_n(Input::Down, down));
            } else {
                out.extend(core::iter::repeat_n(Input::Up, ROWS - down));
            }
            if right <= COLS - right {
                out.extend(core::iter::repeat_n(Input::Right, right));
            } else {
                out.extend(core::iter::repeat_n(Input::Left, COLS - right));
            }
            out.push(Input::Select);
            at = to;
        };
        for c in text.bytes() {
            let (r, k, shift) = find(c).unwrap();
            if shift {
                go((3, COLS - 1), &mut out);
            }
            go((r, k), &mut out);
        }
        out
    }

    fn typed(text: &str) -> Keyboard<64> {
        let mut k = Keyboard::new();
        for input in presses(text) {
            assert_eq!(k.handle(input), Typed::Stay);
        }
        k
    }

    fn lit(canvas: &impl Readable) -> usize {
        (0..canvas.width())
            .flat_map(|x| (0..canvas.height()).map(move |y| (x, y)))
            .filter(|&(x, y)| canvas.pixel(x, y))
            .count()
    }

    /// The number #66 asked for, written down where it cannot drift: on
    /// these messages the grid costs under five presses a character, and a
    /// wheel of the same characters at each position over twice that.
    #[test]
    fn presses_per_character() {
        let chars: usize = SAMPLE.iter().map(|m| m.len()).sum();
        let grid: usize = SAMPLE.iter().map(|m| presses(m).len()).sum();
        let wheel_of = b" abcdefghijklmnopqrstuvwxyz.,?0123456789";
        let wheel: usize = SAMPLE
            .iter()
            .flat_map(|m| m.bytes())
            .map(|c| {
                let i = wheel_of.iter().position(|&w| w == c).unwrap();
                i.min(wheel_of.len() - i) + 1
            })
            .sum();
        let (grid, wheel) = (grid * 100 / chars, wheel * 100 / chars);
        assert!(grid < 500, "{grid} hundredths a character");
        assert!(wheel > 2 * grid, "{wheel} against {grid}");
    }

    #[test]
    fn every_character_is_typed_as_it_is_drawn() {
        for row in KEYS {
            for key in row {
                match key {
                    Key::Char(plain, shifted) => {
                        assert!(font::glyph(plain).is_some(), "{}", plain as char);
                        assert!(font::glyph(shifted).is_some(), "{}", shifted as char);
                        assert!(plain.is_ascii_graphic() && shifted.is_ascii_graphic());
                    }
                    other => {
                        let label = other.label().unwrap();
                        assert!(font::width_of(label) <= font::width_of("Shift"));
                    }
                }
            }
        }
    }

    #[test]
    fn no_character_is_on_two_keys() {
        let mut seen = vec![b' '];
        for key in KEYS.iter().flatten() {
            if let Key::Char(plain, shifted) = *key {
                let halves: &[u8] = if plain == shifted {
                    &[plain]
                } else {
                    &[plain, shifted]
                };
                for &c in halves {
                    assert!(!seen.contains(&c), "{} twice", c as char);
                    seen.push(c);
                }
            }
        }
        // And every one of them is found where it is.
        for &c in &seen {
            assert!(find(c).is_some(), "{}", c as char);
        }
    }

    #[test]
    fn typing_types_and_shift_is_for_one_character() {
        let k = typed("On my way, 10 min!");
        assert_eq!(k.text(), "On my way, 10 min!");
        assert!(!k.shifted());
    }

    #[test]
    fn the_directions_wrap() {
        let mut k = Keyboard::<8>::new();
        for _ in 0..ROWS {
            k.handle(Input::Up);
        }
        assert_eq!(k.at(), START);
        for _ in 0..COLS {
            k.handle(Input::Left);
        }
        assert_eq!(k.at(), START);
        k.handle(Input::Up);
        k.handle(Input::Up);
        k.handle(Input::Up);
        assert_eq!(k.at(), (3, 5), "up from the second row lands on the last");
        for _ in 0..6 {
            k.handle(Input::Right);
        }
        assert_eq!(k.at(), (3, 0));
        assert_eq!(k.key(), letter(b'z'));
    }

    #[test]
    fn back_deletes_and_on_nothing_leaves() {
        let mut k = typed("hi");
        assert_eq!(k.handle(Input::Back), Typed::Stay);
        assert_eq!(k.text(), "h");
        assert_eq!(k.handle(Input::Back), Typed::Stay);
        assert!(k.is_empty());
        assert_eq!(k.handle(Input::Back), Typed::Leave);
    }

    fn to_key(k: &mut Keyboard<64>, to: (usize, usize)) {
        while k.at().0 != to.0 {
            k.handle(Input::Down);
        }
        while k.at().1 != to.1 {
            k.handle(Input::Right);
        }
    }

    #[test]
    fn quit_asks_when_there_is_text() {
        let mut k = typed("hello");
        to_key(&mut k, (0, COLS - 1));
        assert_eq!(k.key(), Key::Quit);
        assert_eq!(k.handle(Input::Select), Typed::Stay);
        assert!(k.quitting());
        // Nothing but the two answers does anything.
        for input in [Input::Up, Input::Down, Input::Left, Input::Right] {
            assert_eq!(k.handle(input), Typed::Stay);
            assert!(k.quitting());
        }
        assert_eq!(k.handle(Input::Back), Typed::Stay);
        assert!(!k.quitting());
        assert_eq!(k.text(), "hello", "kept");
        k.handle(Input::Select);
        assert_eq!(k.handle(Input::Select), Typed::Leave);

        let mut empty = Keyboard::<64>::new();
        to_key(&mut empty, (0, COLS - 1));
        assert_eq!(empty.handle(Input::Select), Typed::Leave, "nothing to lose");
    }

    #[test]
    fn send_waits_for_the_answer() {
        let mut k = typed("yes");
        to_key(&mut k, (1, COLS - 1));
        assert_eq!(k.handle(Input::Select), Typed::Send);
        assert!(k.sending());
        // Presses queued behind it change nothing.
        for input in Input::ALL {
            assert_eq!(k.handle(input), Typed::Stay);
        }
        assert_eq!(k.text(), "yes");
        k.refuse("Not Sent", "Wait for one to go.");
        assert_eq!(k.refused(), Some("Not Sent"));
        // The key that reads on does nothing else.
        assert_eq!(k.handle(Input::Back), Typed::Stay);
        assert_eq!(k.refused(), None);
        assert_eq!(k.text(), "yes");
        assert_eq!(k.handle(Input::Select), Typed::Send, "and again");
    }

    #[test]
    fn send_with_nothing_says_so() {
        let mut k = Keyboard::<64>::new();
        to_key(&mut k, (1, COLS - 1));
        assert_eq!(k.handle(Input::Select), Typed::Stay);
        assert_eq!(k.note, Some(Note::Empty));
        k.handle(Input::Left);
        assert_eq!(k.note, None, "until the next press");
    }

    #[test]
    fn the_limit_is_refused_not_cut() {
        let mut k = Keyboard::<3>::new();
        for _ in 0..3 {
            k.handle(Input::Select);
        }
        assert_eq!(k.text(), "hhh");
        k.handle(Input::Select);
        assert_eq!(k.text(), "hhh");
        assert_eq!(k.note, Some(Note::Full));
        k.handle(Input::Back);
        assert_eq!(k.text(), "hh");
        assert_eq!(k.note, None);
    }

    #[test]
    fn the_grid_fits_both_panels() {
        for layout in [Layout::of(128, 128), Layout::of(128, 64)] {
            let g = Geometry::of(&layout);
            let (x, w) = g.column(COLS - 1);
            assert!(x + w <= layout.width, "{layout:?}");
            assert!(g.cell_w >= font::WIDTH + 2);
            assert!(g.grid_top + ROWS * g.row_h <= layout.content_bottom);
            assert!(g.lines >= 1, "{layout:?}");
            let text_bottom = layout.content_top + g.lines * font::LINE_HEIGHT - 2;
            assert!(text_bottom <= g.rule.unwrap_or(g.grid_top), "{layout:?}");
        }
        let square = Geometry::of(&Layout::of(128, 128));
        assert_eq!(square.lines, 6, "five of text and the status line");
        assert_eq!(square.row_h, 11);
        assert_eq!(Geometry::of(&Layout::of(128, 64)).lines, 1);
    }

    #[test]
    fn what_it_says_fits_a_line() {
        let chars = Layout::of(128, 128).line_chars();
        for line in QUIT_LINES {
            assert!(line.len() <= chars, "{line}");
        }
        for note in [Note::Full, Note::Empty] {
            assert!(note.text().len() + " 287/287".len() <= chars);
        }
    }

    #[test]
    fn typed_text_breaks_at_spaces_and_keeps_its_cursor() {
        let text = b"On my way to the trailhead";
        let got: Vec<_> = runs(text, text.len() + 1, 20)
            .map(|(a, b)| &text[a..b.min(text.len())])
            .collect();
        assert_eq!(got, [&b"On my way to the"[..], &b"trailhead"[..]]);
        // A word longer than a line is cut, and the cursor is the last run.
        let long = [b'h'; 25];
        let got: Vec<_> = runs(&long, 26, 20).collect();
        assert_eq!(got, [(0, 20), (20, 26)]);
        // A space at the end of a full line moves the cursor down.
        let spaced = b"aaaaaaaaaaaaaaaaaaaa ";
        let got: Vec<_> = runs(spaced, spaced.len() + 1, 20).collect();
        assert_eq!(got, [(0, 20), (21, 22)]);
        assert_eq!(runs(b"", 1, 20).collect::<Vec<_>>(), [(0, 1)]);
    }

    #[test]
    fn wrap_breaks_at_spaces() {
        let lines: Vec<_> = wrap("Wait for one to be delivered or to fail.", 20).collect();
        assert_eq!(lines, ["Wait for one to be", "delivered or to", "fail."]);
        let long: Vec<_> = wrap("abcdefghij", 4).collect();
        assert_eq!(long, ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4).count(), 0);
    }

    /// Typing changes the picture, and so does moving, and so does shift:
    /// nothing the keyboard holds is left undrawn.
    #[test]
    fn every_change_is_drawn() {
        fn draw_one<const N: usize>(k: &Keyboard<N>) -> Square {
            let mut b = Square::new();
            draw(&mut b, k);
            b
        }
        let fresh = Keyboard::<64>::new();
        let base = draw_one(&fresh);
        let mut moved = fresh;
        moved.handle(Input::Right);
        let mut typed = fresh;
        typed.handle(Input::Select);
        let mut shifted = fresh;
        shifted.shift = true;
        let mut sending = typed;
        sending.mode = Mode::Sending;
        for (what, k) in [("moved", moved), ("typed", typed), ("shifted", shifted)] {
            assert_ne!(lit(&draw_one(&k)), 0);
            assert!(
                (0..128).any(|x| (0..128).any(|y| draw_one(&k).pixel(x, y) != base.pixel(x, y))),
                "{what}"
            );
        }
        assert!(
            (0..128).any(|x| (0..128)
                .any(|y| draw_one(&sending).pixel(x, y) != draw_one(&typed).pixel(x, y))),
            "sending"
        );
    }

    /// A long message shows its end, with the cursor, and draws nothing
    /// into the grid or past the scrollbar's column on either panel.
    #[test]
    fn a_long_message_shows_its_end_inside_the_text_area() {
        let mut k = Keyboard::<287>::new();
        for _ in 0..200 {
            k.handle(Input::Select);
        }
        fn check<const W: usize, const H: usize>(k: &Keyboard<287>) {
            let mut b = Bitmap::<W, H>::new();
            let layout = Layout::for_canvas(&b);
            let g = Geometry::of(&layout);
            draw(&mut b, k);
            let text_bottom = g.rule.unwrap_or(g.grid_top);
            for y in 0..text_bottom {
                for x in layout.content_left + layout.line_width()..W {
                    assert!(!b.pixel(x, y), "{W}x{H}: drew at ({x}, {y})");
                }
            }
            for y in 0..layout.content_top {
                for x in 0..W {
                    assert!(
                        !b.pixel(x, y),
                        "{W}x{H}: drew in the title bar at ({x}, {y})"
                    );
                }
            }
        }
        check::<128, 128>(&k);
        check::<128, 64>(&k);
    }
}
