//! Arrow keys against a block-character panel, for exploring.
//!
//! The terminal is put into raw mode with `stty`, not a crate, because the
//! whole of what is needed is "one byte at a time, no echo" and the fix-up
//! afterwards, and every host this runs on has `stty`. The key decoding is
//! the only part with any logic in it, and it is pure.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use oxinode_core::ui::{Action, Input};

use crate::scene::Scene;
use crate::script;
use crate::text::{self, Cells};

/// What a key press meant.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Key {
    Press(Input),
    Quit,
    /// Bytes that mean nothing here.
    Unknown,
}

/// Decode what one read from the terminal delivered.
///
/// Arrow keys arrive as `ESC [ A`..`D` in one read; a bare escape is `Back`.
/// The letter keys are for keyboards without a comfortable arrow cluster and
/// for muscle memory: vi's, a gamer's, and the script's own words.
pub fn decode(bytes: &[u8]) -> Key {
    match bytes {
        b"\x1b[A" | b"\x1bOA" => Key::Press(Input::Up),
        b"\x1b[B" | b"\x1bOB" => Key::Press(Input::Down),
        b"\x1b[C" | b"\x1bOC" => Key::Press(Input::Right),
        b"\x1b[D" | b"\x1bOD" => Key::Press(Input::Left),
        b"\x1b" | b"\x7f" | b"\x08" => Key::Press(Input::Back),
        b"\r" | b"\n" | b" " => Key::Press(Input::Select),
        b"q" | b"Q" | b"\x03" | b"\x04" => Key::Quit,
        [letter] => match letter.to_ascii_lowercase() {
            b'k' | b'w' => Key::Press(Input::Up),
            b'j' => Key::Press(Input::Down),
            b'h' | b'a' => Key::Press(Input::Left),
            // `s` is the script's word for select, so WASD's down is `x`.
            b'x' => Key::Press(Input::Down),
            c => match script::key_named(std::str::from_utf8(&[c]).unwrap_or("")) {
                Some(input) => Key::Press(input),
                None => Key::Unknown,
            },
        },
        _ => Key::Unknown,
    }
}

/// The text of the whole display: caption, panel, and a status line.
pub fn screen(scene: &mut Scene, cells: Cells, last: Option<Action>) -> String {
    let mut out = String::new();
    out.push_str("oxinode panel  arrows move, Enter selects, Esc/Backspace back, q quits\n");
    let frame = scene.frame();
    for line in text::render(&frame, cells) {
        out.push_str(&line);
        out.push('\n');
    }
    let nav = &scene.nav;
    out.push_str(&format!(
        "{:?}  menu: {}  scroll: {}",
        nav.screen(),
        match nav.menu_item() {
            Some(n) => nav.screen().menu()[n].label,
            None => "closed",
        },
        nav.scroll()
    ));
    if let Some(action) = last {
        out.push_str(&format!("  action: {action:?}"));
    }
    out.push('\n');
    out
}

/// Ask `stty` for the terminal size, as (columns, rows).
pub fn terminal_size() -> Option<(usize, usize)> {
    let out = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let rows = parts.next()?.parse().ok()?;
    let columns = parts.next()?.parse().ok()?;
    Some((columns, rows))
}

/// Raw mode for as long as this lives.
struct RawMode {
    saved: String,
}

impl RawMode {
    fn enter() -> io::Result<RawMode> {
        let out = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other("stty -g failed; is stdin a terminal?"));
        }
        let saved = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let status = Command::new("stty")
            .args(["raw", "-echo"])
            .stdin(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(io::Error::other("stty raw failed"));
        }
        Ok(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = Command::new("stty")
            .arg(&self.saved)
            .stdin(Stdio::inherit())
            .status();
    }
}

/// Run the interactive session until the user quits.
pub fn run(mut scene: Scene, cells: Option<Cells>) -> io::Result<()> {
    let cells = cells.unwrap_or_else(|| {
        let (columns, rows) = terminal_size().unwrap_or((80, 24));
        Cells::fitting(columns, rows)
    });
    let _raw = RawMode::enter()?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut last = None;
    loop {
        // Clear, home, and draw. Raw mode turns off output post-processing,
        // so every newline needs its carriage return put back.
        let text = screen(&mut scene, cells, last).replace('\n', "\r\n");
        write!(stdout, "\x1b[2J\x1b[H{text}")?;
        stdout.flush()?;

        let mut buf = [0u8; 16];
        let n = stdin.read(&mut buf)?;
        if n == 0 {
            break;
        }
        match decode(&buf[..n]) {
            Key::Press(input) => last = scene.press(input),
            Key::Quit => break,
            Key::Unknown => {}
        }
    }
    write!(stdout, "\r\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxinode_core::ui::Screen;

    #[test]
    fn arrows_enter_and_escape_are_the_pad() {
        assert_eq!(decode(b"\x1b[A"), Key::Press(Input::Up));
        assert_eq!(decode(b"\x1b[B"), Key::Press(Input::Down));
        assert_eq!(decode(b"\x1b[C"), Key::Press(Input::Right));
        assert_eq!(decode(b"\x1b[D"), Key::Press(Input::Left));
        assert_eq!(
            decode(b"\x1bOD"),
            Key::Press(Input::Left),
            "application mode"
        );
        assert_eq!(decode(b"\r"), Key::Press(Input::Select));
        assert_eq!(decode(b"\x1b"), Key::Press(Input::Back));
        assert_eq!(decode(b"\x7f"), Key::Press(Input::Back));
    }

    #[test]
    fn letters_and_quitting() {
        assert_eq!(decode(b"h"), Key::Press(Input::Left));
        assert_eq!(decode(b"J"), Key::Press(Input::Down));
        assert_eq!(decode(b"s"), Key::Press(Input::Select));
        assert_eq!(decode(b"x"), Key::Press(Input::Down));
        assert_eq!(decode(b"q"), Key::Quit);
        assert_eq!(decode(b"\x03"), Key::Quit, "ctrl-c in raw mode");
        assert_eq!(decode(b"z"), Key::Unknown);
        assert_eq!(decode(b"\x1b[Z"), Key::Unknown);
        assert_eq!(decode(b""), Key::Unknown);
    }

    /// The screen text is the caption, the panel, and where you are.
    #[test]
    fn the_screen_says_where_you_are() {
        let mut scene = Scene::new();
        scene.press(Input::Right);
        scene.press(Input::Select);
        let text = screen(&mut scene, Cells::Braille, Some(Action::Redraw));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1 + Cells::Braille.size().1 + 1);
        assert!(
            lines.last().unwrap().contains("Radio"),
            "{}",
            lines.last().unwrap()
        );
        assert!(lines.last().unwrap().contains("menu: Back"));
        assert!(lines.last().unwrap().contains("action: Redraw"));
        assert_eq!(scene.nav.screen(), Screen::Radio);
    }
}
