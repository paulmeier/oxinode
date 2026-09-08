//! The surface the interface draws on, and the least a display has to offer.
//!
//! Three methods: how wide, how tall, and set one pixel. Everything else in
//! the crate -- rectangles, glyphs, the title bar, a menu -- is built on
//! those three, which is what lets the same interface land on a 128 × 128
//! OLED, a 128 × 64 one, a framebuffer in a simulator, or an
//! `embedded-graphics` display through the optional adapter.
//!
//! A canvas is not asked to read a pixel back. Plenty of displays cannot,
//! and none of the drawing needs it. Reading is a separate, smaller trait,
//! [`Readable`], for the things that compare pictures: tests, a simulator, a
//! screenshot over a wire.

/// Somewhere to draw.
///
/// Coordinates are pixels from the top-left corner, `x` across and `y` down.
/// A coordinate off the canvas is ignored, never wrapped: the whole layout
/// assumes it can draw a rectangle that runs past the edge and lose the part
/// that does, and a canvas that wrapped would turn a one-pixel overrun into a
/// stripe down the far side.
pub trait Canvas {
    /// Width in pixels.
    fn width(&self) -> usize;

    /// Height in pixels.
    fn height(&self) -> usize;

    /// Set or clear one pixel. Off-canvas coordinates do nothing.
    fn set_pixel(&mut self, x: usize, y: usize, on: bool);

    /// Set every pixel to `on`.
    ///
    /// A pixel at a time by default; a canvas with a faster way -- a byte
    /// fill -- overrides it.
    fn fill(&mut self, on: bool) {
        for y in 0..self.height() {
            for x in 0..self.width() {
                self.set_pixel(x, y, on);
            }
        }
    }

    /// Fill a rectangle, clipped to the canvas.
    ///
    /// Clipped rather than rejected: drawing is a place where being strict
    /// costs callers a bounds check at every site and buys nothing, because
    /// there is no correct behaviour other than "do not draw off the screen".
    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, on: bool) {
        for py in y..y.saturating_add(h).min(self.height()) {
            for px in x..x.saturating_add(w).min(self.width()) {
                self.set_pixel(px, py, on);
            }
        }
    }

    /// Draw the outline of a rectangle, one pixel thick.
    fn frame_rect(&mut self, x: usize, y: usize, w: usize, h: usize, on: bool) {
        if w == 0 || h == 0 {
            return;
        }
        self.rect(x, y, w, 1, on);
        self.rect(x, y + h - 1, w, 1, on);
        self.rect(x, y, 1, h, on);
        self.rect(x + w - 1, y, 1, h, on);
    }
}

/// A canvas whose pixels can be read back.
///
/// What a test or a simulator needs and a display does not. Kept apart from
/// [`Canvas`] so that a write-only display is still a canvas.
pub trait Readable: Canvas {
    /// Whether a pixel is lit. Off-canvas coordinates read as dark.
    fn pixel(&self, x: usize, y: usize) -> bool;
}

/// A canvas in memory, one byte per pixel, of any size.
///
/// The crate's own tests draw on this, and a simulator can use it for a
/// panel size it has no framebuffer type for. It is deliberately the simplest
/// thing that works rather than the densest: a device driver has its own
/// framebuffer in its controller's own layout, and that is where the bit
/// packing belongs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bitmap<const W: usize, const H: usize> {
    pixels: [[bool; W]; H],
}

impl<const W: usize, const H: usize> Default for Bitmap<W, H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize, const H: usize> Bitmap<W, H> {
    /// An all-dark bitmap.
    pub const fn new() -> Self {
        Bitmap {
            pixels: [[false; W]; H],
        }
    }

    /// How many pixels are lit.
    pub fn lit(&self) -> usize {
        self.pixels
            .iter()
            .map(|row| row.iter().filter(|&&p| p).count())
            .sum()
    }
}

impl<const W: usize, const H: usize> Canvas for Bitmap<W, H> {
    fn width(&self) -> usize {
        W
    }

    fn height(&self) -> usize {
        H
    }

    fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        if x < W && y < H {
            self.pixels[y][x] = on;
        }
    }

    fn fill(&mut self, on: bool) {
        self.pixels = [[on; W]; H];
    }
}

impl<const W: usize, const H: usize> Readable for Bitmap<W, H> {
    fn pixel(&self, x: usize, y: usize) -> bool {
        x < W && y < H && self.pixels[y][x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bitmap_is_the_size_it_says_and_starts_dark() {
        let b = Bitmap::<20, 10>::new();
        assert_eq!((b.width(), b.height()), (20, 10));
        assert_eq!(b.lit(), 0);
        for y in 0..10 {
            for x in 0..20 {
                assert!(!b.pixel(x, y));
            }
        }
    }

    #[test]
    fn a_pixel_round_trips_and_the_edge_is_ignored() {
        let mut b = Bitmap::<20, 10>::new();
        b.set_pixel(19, 9, true);
        assert!(b.pixel(19, 9));
        b.set_pixel(20, 0, true);
        b.set_pixel(0, 10, true);
        b.set_pixel(usize::MAX, usize::MAX, true);
        assert_eq!(b.lit(), 1, "nothing off the edge was drawn");
        assert!(!b.pixel(20, 0), "off the edge reads as dark");
        b.set_pixel(19, 9, false);
        assert_eq!(b.lit(), 0);
    }

    /// The provided drawing methods, checked on the simplest canvas so that
    /// every other canvas inherits a known-good version of them.
    #[test]
    fn a_rectangle_covers_exactly_its_own_area() {
        let mut b = Bitmap::<30, 30>::new();
        b.rect(10, 20, 5, 7, true);
        for y in 0..30 {
            for x in 0..30 {
                let inside = (10..15).contains(&x) && (20..27).contains(&y);
                assert_eq!(b.pixel(x, y), inside, "({x},{y})");
            }
        }
    }

    /// Clipped, not wrapped. A rectangle running off the right edge must not
    /// reappear on the left.
    #[test]
    fn a_rectangle_past_the_edge_is_clipped() {
        let mut b = Bitmap::<16, 16>::new();
        b.rect(14, 14, 10, 10, true);
        assert!(b.pixel(15, 15));
        assert!(!b.pixel(0, 0));
        assert_eq!(b.lit(), 4, "only the 2x2 corner is on the canvas");
        // And a rectangle that would overflow `usize` is clipped too.
        b.rect(1, 1, usize::MAX, usize::MAX, true);
        assert_eq!(b.lit(), 15 * 15);
    }

    #[test]
    fn an_outline_is_hollow_and_a_degenerate_one_is_nothing() {
        let mut b = Bitmap::<16, 16>::new();
        b.frame_rect(0, 0, 16, 16, true);
        assert!(b.pixel(0, 0) && b.pixel(15, 0) && b.pixel(0, 15) && b.pixel(15, 15));
        assert!(b.pixel(8, 0) && b.pixel(8, 15) && b.pixel(0, 8) && b.pixel(15, 8));
        assert!(!b.pixel(8, 8), "the middle should be empty");
        assert_eq!(b.lit(), 4 * 16 - 4);
        let mut none = Bitmap::<16, 16>::new();
        none.frame_rect(3, 3, 0, 5, true);
        none.frame_rect(3, 3, 5, 0, true);
        assert_eq!(none.lit(), 0);
    }

    #[test]
    fn filling_sets_or_clears_everything() {
        let mut b = Bitmap::<8, 4>::new();
        b.fill(true);
        assert_eq!(b.lit(), 32);
        b.fill(false);
        assert_eq!(b.lit(), 0);
    }

    /// The default `fill` -- the one a canvas gets if it does not override
    /// it -- agrees with the byte fill.
    #[test]
    fn the_default_fill_matches_the_fast_one() {
        struct Slow(Bitmap<8, 4>);
        impl Canvas for Slow {
            fn width(&self) -> usize {
                8
            }
            fn height(&self) -> usize {
                4
            }
            fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
                self.0.set_pixel(x, y, on);
            }
        }
        let mut slow = Slow(Bitmap::new());
        slow.fill(true);
        let mut fast = Bitmap::<8, 4>::new();
        fast.fill(true);
        assert_eq!(slow.0, fast);
    }
}
