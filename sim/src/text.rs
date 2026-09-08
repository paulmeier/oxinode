//! A frame as characters, for a terminal.
//!
//! Two renderings, because terminals come in two sizes. Half blocks put two
//! panel rows in one character row and need a 64-line terminal to show the
//! whole panel, which most are not. Braille puts a 2 x 4 cell in one
//! character and fits in 32 lines, at the cost of looking like braille.

use monopanel::Readable;

/// Which character set to draw with.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Cells {
    /// One character per 2 x 4 pixels: 64 columns by 32 rows.
    Braille,
    /// One character per 1 x 2 pixels: 128 columns by 64 rows.
    HalfBlocks,
}

impl Cells {
    /// The terminal size a whole panel of `width` by `height` needs, as
    /// (columns, rows), rounding a partial cell up.
    pub const fn size(self, width: usize, height: usize) -> (usize, usize) {
        match self {
            Cells::Braille => (width.div_ceil(2), height.div_ceil(4)),
            Cells::HalfBlocks => (width, height.div_ceil(2)),
        }
    }

    /// The largest rendering of a panel of `width` by `height` that fits a
    /// terminal of the given size.
    pub fn fitting(width: usize, height: usize, columns: usize, rows: usize) -> Cells {
        let (w, h) = Cells::HalfBlocks.size(width, height);
        // A line or two for the caption.
        if columns >= w && rows >= h + 2 {
            Cells::HalfBlocks
        } else {
            Cells::Braille
        }
    }
}

/// Render a canvas as lines of text, without trailing newlines.
pub fn render(canvas: &impl Readable, cells: Cells) -> Vec<String> {
    match cells {
        Cells::Braille => braille(canvas),
        Cells::HalfBlocks => half_blocks(canvas),
    }
}

/// Dot weights in a braille cell, by (column, row). Unicode numbers the dots
/// down the left column then down the right, with the fourth row's two dots
/// added afterwards -- so the order is not the obvious one.
const BRAILLE_DOTS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

fn braille(frame: &impl Readable) -> Vec<String> {
    let (columns, rows) = Cells::Braille.size(frame.width(), frame.height());
    (0..rows)
        .map(|row| {
            (0..columns)
                .map(|column| {
                    let mut dots = 0;
                    for (dx, weights) in BRAILLE_DOTS.iter().enumerate() {
                        for (dy, weight) in weights.iter().enumerate() {
                            if frame.pixel(column * 2 + dx, row * 4 + dy) {
                                dots |= weight;
                            }
                        }
                    }
                    char::from_u32(0x2800 + dots).expect("a braille pattern")
                })
                .collect()
        })
        .collect()
}

fn half_blocks(frame: &impl Readable) -> Vec<String> {
    let (columns, rows) = Cells::HalfBlocks.size(frame.width(), frame.height());
    (0..rows)
        .map(|row| {
            (0..columns)
                .map(|x| {
                    match (frame.pixel(x, row * 2), frame.pixel(x, row * 2 + 1)) {
                        (true, true) => '\u{2588}',  // full block
                        (true, false) => '\u{2580}', // upper half
                        (false, true) => '\u{2584}', // lower half
                        (false, false) => ' ',
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{Panel, Size};
    use monopanel::Canvas;
    use oxinode_core::sh1107::Frame;

    fn one_pixel(x: usize, y: usize) -> Frame {
        let mut frame = Frame::new();
        frame.set_pixel(x, y, true);
        frame
    }

    /// Both renderings are exactly the size they claim.
    #[test]
    fn a_rendering_has_the_size_it_claims() {
        for cells in [Cells::Braille, Cells::HalfBlocks] {
            let lines = render(&Frame::new(), cells);
            let (w, h) = cells.size(128, 128);
            assert_eq!(lines.len(), h, "{cells:?}");
            for line in &lines {
                assert_eq!(line.chars().count(), w, "{cells:?}");
            }
            // A shorter panel is fewer rows, and one that is not a whole
            // number of cells is rounded up rather than cut.
            let wide = render(&Panel::new(Size::WIDE), cells);
            assert_eq!(wide.len(), cells.size(128, 64).1);
            let odd = Panel::new(Size {
                width: 7,
                height: 5,
            });
            assert_eq!(
                cells.size(7, 5),
                match cells {
                    Cells::Braille => (4, 2),
                    Cells::HalfBlocks => (7, 3),
                }
            );
            assert_eq!(render(&odd, cells).len(), cells.size(7, 5).1);
        }
        // A pixel in the last, partial cell is still shown.
        let mut odd = Panel::new(Size {
            width: 7,
            height: 5,
        });
        odd.set_pixel(6, 4, true);
        assert_ne!(
            render(&odd, Cells::Braille)[1].chars().nth(3).unwrap(),
            '\u{2800}'
        );
    }

    /// Each braille dot is the pixel Unicode says it is.
    #[test]
    fn braille_dots_land_where_unicode_puts_them() {
        // Dot 1 is top-left; dot 8 (0x80) is bottom-right; dot 7 (0x40) is
        // bottom-left, which is the one an obvious column-major mapping gets
        // wrong.
        let cases = [
            ((0, 0), 0x01),
            ((1, 3), 0x80),
            ((0, 3), 0x40),
            ((1, 0), 0x08),
        ];
        for ((dx, dy), weight) in cases {
            let lines = render(&one_pixel(10 + dx, 20 + dy), Cells::Braille);
            let cell = lines[5].chars().nth(5).unwrap();
            assert_eq!(cell as u32, 0x2800 + weight, "dot at ({dx}, {dy})");
        }
        let blank = render(&Frame::new(), Cells::Braille);
        assert!(blank.iter().all(|l| l.chars().all(|c| c == '\u{2800}')));
    }

    /// Half blocks pick the right half.
    #[test]
    fn half_blocks_pick_the_right_half() {
        let at = |frame: &Frame| render(frame, Cells::HalfBlocks)[3].chars().nth(7).unwrap();
        assert_eq!(at(&one_pixel(7, 6)), '\u{2580}');
        assert_eq!(at(&one_pixel(7, 7)), '\u{2584}');
        let mut both = one_pixel(7, 6);
        both.set_pixel(7, 7, true);
        assert_eq!(at(&both), '\u{2588}');
        assert_eq!(at(&Frame::new()), ' ');
    }

    /// The bigger rendering is chosen only when it fits, caption included.
    #[test]
    fn the_rendering_fits_the_terminal() {
        assert_eq!(Cells::fitting(128, 128, 80, 24), Cells::Braille);
        assert_eq!(Cells::fitting(128, 128, 200, 60), Cells::Braille);
        assert_eq!(Cells::fitting(128, 128, 127, 80), Cells::Braille);
        assert_eq!(Cells::fitting(128, 128, 128, 66), Cells::HalfBlocks);
        assert_eq!(Cells::fitting(128, 64, 128, 34), Cells::HalfBlocks);
        assert_eq!(Cells::fitting(128, 64, 128, 33), Cells::Braille);
    }
}
