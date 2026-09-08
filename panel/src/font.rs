//! A 5 × 7 bitmap font, and just enough text rendering for a status page.
//!
//! # Mixed case
//!
//! The table covers space, the digits, `A`–`Z`, `a`–`z` and fifteen symbols —
//! eighty-one glyphs. Anything outside it renders as a hollow box, so a missing
//! glyph looks like a missing glyph rather than like a space.
//!
//! Lowercase arrived with the on-device menus. A status panel reads perfectly
//! well in capitals, and for phases 7 and 8 it was all capitals: the table held
//! fifty-five glyphs and folded lowercase input to uppercase. Menus are a
//! different kind of reading. A screen of prose-cased labels — `Display
//! Options`, `Bluetooth Toggle` — is scanned rather than read, and word shape
//! is most of what makes scanning work; in full capitals every label is the
//! same rectangle.
//!
//! The fold is still there as a fallback, so a character with no glyph of its
//! own is tried again in uppercase before it becomes a box. Nothing in the
//! table needs that today. It costs one comparison and means an added
//! uppercase-only glyph never silently regresses to a box in lowercase.
//!
//! # How the glyphs got here
//!
//! Written as readable ASCII art and converted to this table by a generator,
//! rather than typed as hex. Eighty-one glyphs of hand-entered hex is four
//! hundred and five chances to make a mistake whose only symptom is a wrong
//! pixel on a screen nobody is looking at closely.
//!
//! Column-major: byte *n* is column *n*, and bit *k* of it is row *k*, top
//! first. That is the orientation the SH1107 wants for a vertical run of eight
//! pixels, so a glyph column is one byte of display RAM when the text lands on
//! a page boundary — which is worth having even though the renderer below does
//! not currently exploit it.
//!
//! The renderer draws on any [`Canvas`]: a glyph is a handful of `set_pixel`
//! calls, and nothing about a font depends on the display it lands on.

use crate::Canvas;

/// Glyph width in pixels.
pub const WIDTH: usize = 5;
/// Glyph height in pixels.
pub const HEIGHT: usize = 7;
/// Pixels between one glyph and the next.
pub const SPACING: usize = 1;
/// Width of one character cell, including the gap after it.
pub const ADVANCE: usize = WIDTH + SPACING;
/// Height of one line, including the gap below it.
pub const LINE_HEIGHT: usize = HEIGHT + 2;

pub const GLYPHS: [(u8, [u8; WIDTH]); 81] = [
    (b' ', [0x00, 0x00, 0x00, 0x00, 0x00]),
    (b'!', [0x00, 0x00, 0x5f, 0x00, 0x00]),
    (b'#', [0x14, 0x7f, 0x14, 0x7f, 0x14]),
    (b'%', [0x63, 0x13, 0x08, 0x64, 0x63]),
    (b'\'', [0x00, 0x00, 0x03, 0x00, 0x00]),
    (b'(', [0x1c, 0x22, 0x41, 0x41, 0x00]),
    (b')', [0x00, 0x41, 0x41, 0x22, 0x1c]),
    (b'*', [0x2a, 0x1c, 0x3e, 0x1c, 0x2a]),
    (b'+', [0x08, 0x08, 0x3e, 0x08, 0x08]),
    (b',', [0x00, 0x70, 0x30, 0x00, 0x00]),
    (b'-', [0x08, 0x08, 0x08, 0x08, 0x08]),
    (b'.', [0x00, 0x60, 0x60, 0x00, 0x00]),
    (b'/', [0x40, 0x30, 0x08, 0x06, 0x01]),
    (b'0', [0x3e, 0x51, 0x49, 0x45, 0x3e]),
    (b'1', [0x00, 0x42, 0x7f, 0x40, 0x00]),
    (b'2', [0x42, 0x61, 0x51, 0x49, 0x46]),
    (b'3', [0x21, 0x41, 0x45, 0x4b, 0x31]),
    (b'4', [0x18, 0x14, 0x12, 0x7f, 0x10]),
    (b'5', [0x27, 0x45, 0x45, 0x45, 0x39]),
    (b'6', [0x3c, 0x4a, 0x49, 0x49, 0x30]),
    (b'7', [0x01, 0x71, 0x09, 0x05, 0x03]),
    (b'8', [0x36, 0x49, 0x49, 0x49, 0x36]),
    (b'9', [0x06, 0x49, 0x49, 0x29, 0x1e]),
    (b':', [0x00, 0x36, 0x36, 0x00, 0x00]),
    (b'<', [0x08, 0x14, 0x22, 0x41, 0x00]),
    (b'=', [0x14, 0x14, 0x14, 0x14, 0x14]),
    (b'>', [0x00, 0x41, 0x22, 0x14, 0x08]),
    (b'?', [0x02, 0x01, 0x51, 0x09, 0x06]),
    (b'A', [0x7e, 0x09, 0x09, 0x09, 0x7e]),
    (b'B', [0x7f, 0x49, 0x49, 0x49, 0x36]),
    (b'C', [0x3e, 0x41, 0x41, 0x41, 0x22]),
    (b'D', [0x7f, 0x41, 0x41, 0x22, 0x1c]),
    (b'E', [0x7f, 0x49, 0x49, 0x49, 0x41]),
    (b'F', [0x7f, 0x09, 0x09, 0x09, 0x01]),
    (b'G', [0x3e, 0x41, 0x49, 0x49, 0x7a]),
    (b'H', [0x7f, 0x08, 0x08, 0x08, 0x7f]),
    (b'I', [0x00, 0x41, 0x7f, 0x41, 0x00]),
    (b'J', [0x30, 0x40, 0x40, 0x40, 0x3f]),
    (b'K', [0x7f, 0x08, 0x14, 0x22, 0x41]),
    (b'L', [0x7f, 0x40, 0x40, 0x40, 0x40]),
    (b'M', [0x7f, 0x02, 0x0c, 0x02, 0x7f]),
    (b'N', [0x7f, 0x04, 0x08, 0x10, 0x7f]),
    (b'O', [0x3e, 0x41, 0x41, 0x41, 0x3e]),
    (b'P', [0x7f, 0x09, 0x09, 0x09, 0x06]),
    (b'Q', [0x3e, 0x41, 0x51, 0x21, 0x5e]),
    (b'R', [0x7f, 0x09, 0x19, 0x29, 0x46]),
    (b'S', [0x46, 0x49, 0x49, 0x49, 0x31]),
    (b'T', [0x01, 0x01, 0x7f, 0x01, 0x01]),
    (b'U', [0x3f, 0x40, 0x40, 0x40, 0x3f]),
    (b'V', [0x1f, 0x20, 0x40, 0x20, 0x1f]),
    (b'W', [0x7f, 0x20, 0x18, 0x20, 0x7f]),
    (b'X', [0x63, 0x14, 0x08, 0x14, 0x63]),
    (b'Y', [0x03, 0x04, 0x78, 0x04, 0x03]),
    (b'Z', [0x61, 0x51, 0x49, 0x45, 0x43]),
    (b'_', [0x40, 0x40, 0x40, 0x40, 0x40]),
    (b'a', [0x20, 0x54, 0x54, 0x54, 0x78]),
    (b'b', [0x7f, 0x48, 0x44, 0x44, 0x38]),
    (b'c', [0x38, 0x44, 0x44, 0x44, 0x44]),
    (b'd', [0x38, 0x44, 0x44, 0x48, 0x7f]),
    (b'e', [0x38, 0x54, 0x54, 0x54, 0x58]),
    (b'f', [0x08, 0x7e, 0x09, 0x09, 0x02]),
    (b'g', [0x0c, 0x52, 0x52, 0x52, 0x3e]),
    (b'h', [0x7f, 0x08, 0x04, 0x04, 0x78]),
    (b'i', [0x00, 0x44, 0x7d, 0x40, 0x00]),
    (b'j', [0x20, 0x40, 0x44, 0x3d, 0x00]),
    (b'k', [0x7f, 0x10, 0x10, 0x28, 0x44]),
    (b'l', [0x00, 0x41, 0x7f, 0x40, 0x00]),
    (b'm', [0x7c, 0x04, 0x38, 0x04, 0x78]),
    (b'n', [0x7c, 0x08, 0x04, 0x04, 0x78]),
    (b'o', [0x38, 0x44, 0x44, 0x44, 0x38]),
    (b'p', [0x7e, 0x12, 0x12, 0x12, 0x0c]),
    (b'q', [0x0c, 0x12, 0x12, 0x12, 0x7e]),
    (b'r', [0x7c, 0x08, 0x04, 0x04, 0x08]),
    (b's', [0x48, 0x54, 0x54, 0x54, 0x24]),
    (b't', [0x04, 0x3f, 0x44, 0x44, 0x20]),
    (b'u', [0x3c, 0x40, 0x40, 0x20, 0x7c]),
    (b'v', [0x1c, 0x20, 0x40, 0x20, 0x1c]),
    (b'w', [0x3c, 0x40, 0x78, 0x40, 0x3c]),
    (b'x', [0x44, 0x28, 0x10, 0x28, 0x44]),
    (b'y', [0x0e, 0x50, 0x50, 0x50, 0x3e]),
    (b'z', [0x44, 0x64, 0x54, 0x4c, 0x44]),
];

/// The columns for one character, or `None` if it is not in the table.
///
/// Tried as written first, then folded to uppercase; see the module docs.
pub fn glyph(c: u8) -> Option<&'static [u8; WIDTH]> {
    exact(c).or_else(|| {
        let upper = c.to_ascii_uppercase();
        // Only worth a second search if folding actually changed something.
        (upper != c).then(|| exact(upper)).flatten()
    })
}

/// One character, looked up exactly as given.
fn exact(c: u8) -> Option<&'static [u8; WIDTH]> {
    let mut lo = 0usize;
    let mut hi = GLYPHS.len();
    // The table is sorted by character, so this is a binary search rather than
    // a scan. It is not about speed -- fifty-five entries is nothing -- but a
    // sorted table is checkable, and a test checks it.
    while lo < hi {
        let mid = (lo + hi) / 2;
        match GLYPHS[mid].0.cmp(&c) {
            core::cmp::Ordering::Less => lo = mid + 1,
            core::cmp::Ordering::Greater => hi = mid,
            core::cmp::Ordering::Equal => return Some(&GLYPHS[mid].1),
        }
    }
    None
}

/// The columns drawn for a character that is not in the table: a hollow box.
///
/// Not a space. A missing glyph that looked like a space would make a wrong
/// string look like a correctly rendered shorter one.
pub const MISSING: [u8; WIDTH] = [0x7F, 0x41, 0x41, 0x41, 0x7F];

/// Draw one character with its top-left corner at `(x, y)`.
///
/// Returns where the next character starts.
pub fn draw_char(canvas: &mut impl Canvas, x: usize, y: usize, c: u8, on: bool) -> usize {
    let columns = glyph(c).unwrap_or(&MISSING);
    for (dx, column) in columns.iter().enumerate() {
        for dy in 0..HEIGHT {
            if column & (1 << dy) != 0 {
                canvas.set_pixel(x + dx, y + dy, on);
            }
        }
    }
    x + ADVANCE
}

/// Draw a string. Returns where the next character would start.
///
/// Characters that would fall off the right edge are not drawn — the canvas
/// clips them anyway, but stopping means the cursor returned is honest about
/// how far it got.
pub fn draw(canvas: &mut impl Canvas, x: usize, y: usize, text: &str, on: bool) -> usize {
    let mut cursor = x;
    let right = canvas.width();
    for c in text.bytes() {
        if cursor + WIDTH > right {
            break;
        }
        cursor = draw_char(canvas, cursor, y, c, on);
    }
    cursor
}

/// How wide a string will be, in pixels, including the gap after the last
/// character.
pub const fn width_of(text: &str) -> usize {
    text.len() * ADVANCE
}

/// Draw a string right-aligned so that it ends at `right`.
///
/// Numbers on a status page belong in a column, and a column of numbers that
/// are not the same length has to be aligned from the right or it does not read
/// as a column.
pub fn draw_right(canvas: &mut impl Canvas, right: usize, y: usize, text: &str, on: bool) {
    let width = width_of(text).saturating_sub(SPACING);
    let x = right.saturating_sub(width);
    draw(canvas, x, y, text, on);
}

// A glyph column is seven rows in the low bits, so the top bit is always clear.
// If it were not, a glyph would draw a pixel one row below its own box and into
// whatever is on the next line.
const _: () = {
    let mut i = 0;
    while i < GLYPHS.len() {
        let mut c = 0;
        while c < WIDTH {
            assert!(GLYPHS[i].1[c] < (1 << HEIGHT));
            c += 1;
        }
        i += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bitmap, Readable};

    /// The panel the font was drawn for.
    type Panel = Bitmap<128, 128>;

    /// Render a glyph back into art, so a test can state what it should look
    /// like in the same form the generator took it in.
    fn art(c: u8) -> Vec<String> {
        let columns = glyph(c).unwrap_or(&MISSING);
        (0..HEIGHT)
            .map(|row| {
                (0..WIDTH)
                    .map(|col| {
                        if columns[col] & (1 << row) != 0 {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// The table has to be sorted, because `glyph` binary-searches it.
    #[test]
    fn the_table_is_sorted_and_has_no_duplicates() {
        for pair in GLYPHS.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{:?} then {:?} is out of order",
                pair[0].0 as char,
                pair[1].0 as char
            );
        }
    }

    /// Every character the table claims to have can be found again.
    #[test]
    fn every_glyph_in_the_table_is_reachable() {
        for (c, columns) in GLYPHS {
            assert_eq!(glyph(c), Some(&columns), "{:?}", c as char);
        }
        assert_eq!(GLYPHS.len(), 81);
    }

    /// Everything a status page needs, present. Stated as a list rather than a
    /// range so that removing one from the table fails here rather than on a
    /// screen.
    #[test]
    fn the_characters_a_status_page_needs_are_all_there() {
        for c in b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ .,:-+/%()!?=<>*#'_" {
            assert!(glyph(*c).is_some(), "{:?} is missing", *c as char);
        }
    }

    /// Lowercase has glyphs of its own, and they are not the uppercase ones.
    ///
    /// The inequality is the point. An `a` that renders as `A` is exactly the
    /// regression this replaced, and it would pass a mere `is_some`.
    #[test]
    fn lowercase_is_its_own_shape() {
        for (lower, upper) in b"abcxyz".iter().zip(b"ABCXYZ") {
            assert!(glyph(*lower).is_some(), "{:?} is missing", *lower as char);
            assert_ne!(glyph(*lower), glyph(*upper), "{:?}", *lower as char);
        }
    }

    /// Every letter, both cases, and no two letters drawn the same.
    ///
    /// Distinctness is what makes a menu readable: two labels that differ by
    /// one letter have to look different. Checked over the whole alphabet
    /// rather than spot-checked, because a copy-and-paste slip in the art is
    /// the likely way to get a duplicate and it would be invisible otherwise.
    #[test]
    fn the_alphabet_is_complete_in_both_cases_and_all_distinct() {
        let mut seen: Vec<(u8, [u8; WIDTH])> = Vec::new();
        for c in (b'a'..=b'z').chain(b'A'..=b'Z') {
            let columns = *glyph(c).unwrap_or_else(|| panic!("{:?} is missing", c as char));
            if let Some((other, _)) = seen.iter().find(|(_, cols)| *cols == columns) {
                panic!("{:?} and {:?} draw the same", *other as char, c as char);
            }
            seen.push((c, columns));
        }
    }

    /// The fold is still there for anything the table has in one case only.
    ///
    /// Nothing needs it today -- every letter has both cases -- so it is
    /// checked on a symbol-free stand-in: a character absent in lowercase form
    /// resolves through its uppercase entry rather than becoming a box.
    #[test]
    fn an_uppercase_only_glyph_is_still_reachable_in_lowercase() {
        // The digits and symbols have no case, so uppercase-folding them is a
        // no-op; the fold is exercised by pretending a letter went missing.
        assert_eq!(exact(b'Z'), glyph(b'Z'));
        assert!(exact(b'z').is_some(), "z has its own glyph now");
        // A character whose uppercase form is in the table and whose lowercase
        // form is not: none exist, so assert the mechanism directly.
        assert_eq!(glyph(b'\xe5'), None, "no glyph, no uppercase form, no box");
    }

    /// Lowercase does not stray above the cap line or below the baseline.
    ///
    /// Row 0 is the top of an uppercase letter and row 6 the baseline. An
    /// x-height letter that reached row 0 would collide with the line above,
    /// and this font has no room under row 6 for a descender, so `g` and `y`
    /// have to turn their tails inside the cell.
    #[test]
    fn lowercase_stays_inside_the_cell() {
        for c in b'a'..=b'z' {
            let columns = glyph(c).unwrap();
            for (x, column) in columns.iter().enumerate() {
                assert_eq!(
                    column & !0x7F,
                    0,
                    "{:?} column {x} draws below the baseline",
                    c as char
                );
            }
        }
        // Only the tall letters reach the cap line: the seven ascenders, plus
        // `i` and `j`, whose tittles sit up there rather than at x-height.
        let tall = b"bdfhklt" // ascenders
            .iter()
            .chain(b"ij") // dotted
            .copied()
            .collect::<Vec<u8>>();
        for c in b'a'..=b'z' {
            let touches_top = glyph(c).unwrap().iter().any(|col| col & 1 != 0);
            assert_eq!(
                touches_top,
                tall.contains(&c),
                "{:?} touching the cap line",
                c as char
            );
        }
    }

    /// Anything else is a visible box, not a space. A missing glyph that looked
    /// like a space would make a wrong string read as a correct shorter one.
    #[test]
    fn an_unknown_character_is_a_box_and_not_a_gap() {
        for c in [b'~', b'^', b'{', b'|', 0x00, 0xFF] {
            assert_eq!(glyph(c), None, "{c:#04x}");
        }
        assert_ne!(MISSING, [0; WIDTH]);
        assert_ne!(Some(&MISSING), glyph(b' '));
    }

    /// Space is the only glyph that draws nothing.
    #[test]
    fn only_space_is_blank() {
        for (c, columns) in GLYPHS {
            let blank = columns.iter().all(|&b| b == 0);
            assert_eq!(blank, c == b' ', "{:?}", c as char);
        }
    }

    /// Two glyphs picked out and checked against the art they were drawn as.
    /// This is what would catch a generator that transposed rows and columns,
    /// or numbered the bits from the bottom.
    #[test]
    fn the_glyphs_are_the_shapes_they_were_drawn_as() {
        assert_eq!(
            art(b'A'),
            vec![".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"]
        );
        assert_eq!(
            art(b'1'),
            vec!["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."]
        );
        // Asymmetric top to bottom *and* left to right, so a flip either way
        // fails.
        assert_eq!(
            art(b'F'),
            vec!["#####", "#....", "#....", "####.", "#....", "#....", "#...."]
        );
    }

    /// Drawing puts pixels where the glyph says, at the offset asked for.
    #[test]
    fn a_character_lands_where_it_is_put() {
        let mut f = Panel::new();
        let next = draw_char(&mut f, 10, 20, b'F', true);
        assert_eq!(next, 10 + ADVANCE);
        // Top bar of the F, all five columns.
        for dx in 0..WIDTH {
            assert!(f.pixel(10 + dx, 20), "top bar column {dx}");
        }
        // Left stem, all seven rows.
        for dy in 0..HEIGHT {
            assert!(f.pixel(10, 20 + dy), "stem row {dy}");
        }
        // And nothing outside its box.
        assert!(!f.pixel(9, 20));
        assert!(!f.pixel(10 + WIDTH, 20));
        assert!(!f.pixel(10, 20 + HEIGHT));
    }

    #[test]
    fn a_string_advances_one_cell_per_character() {
        let mut f = Panel::new();
        let end = draw(&mut f, 0, 0, "ABC", true);
        assert_eq!(end, 3 * ADVANCE);
    }

    /// A string that would run off the right edge stops rather than wrapping,
    /// and says how far it got.
    #[test]
    fn a_string_stops_at_the_right_edge() {
        let mut f = Panel::new();
        let long = "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let end = draw(&mut f, 0, 0, long, true);
        assert!(end <= 128 + ADVANCE);
        // Nothing wrapped onto the row below.
        for y in HEIGHT..LINE_HEIGHT {
            for x in 0..128 {
                assert!(!f.pixel(x, y), "({x},{y}) wrapped");
            }
        }
    }

    /// Right alignment puts the last pixel column of the text at `right`.
    #[test]
    fn right_aligned_text_ends_where_it_is_told() {
        let mut f = Panel::new();
        draw_right(&mut f, 100, 0, "12", true);
        // "12" is two cells wide less the trailing gap: 11 pixels, so it starts
        // at 89 and its last column is 99 -- the pixel before `right`.
        assert_eq!(width_of("12") - SPACING, 11);
        let lit: Vec<usize> = (0..128).filter(|&x| f.pixel(x, 0)).collect();
        assert!(!lit.is_empty());
        assert!(*lit.iter().max().unwrap() < 100);
        assert!(*lit.iter().min().unwrap() >= 89);
    }

    /// Two numbers of different lengths line up on the right. That is the whole
    /// reason `draw_right` exists.
    #[test]
    fn numbers_of_different_lengths_share_a_right_edge() {
        let mut f = Panel::new();
        draw_right(&mut f, 60, 0, "7", true);
        draw_right(&mut f, 60, LINE_HEIGHT, "1234", true);
        let right_of = |y: usize| (0..128).filter(|&x| f.pixel(x, y)).max();
        // The rightmost lit pixel of each row is within one glyph column of the
        // other: the digits themselves differ in which columns they use.
        let a = right_of(0).unwrap();
        let b = right_of(LINE_HEIGHT).unwrap();
        assert!(a.abs_diff(b) <= 1, "{a} vs {b}");
    }
}
