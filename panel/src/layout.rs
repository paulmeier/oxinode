//! Where the chrome goes, derived from the size of the canvas.
//!
//! A title bar across the top, an icon strip across the foot, and the
//! content between them. The two bars are as tall as what they hold -- the
//! font and the icons -- and the content gets whatever is left, so a shorter
//! panel loses content lines and nothing else. Nothing here is a fixed
//! number: every constant the first version of this interface hard-coded for
//! a 128 × 128 panel is a field computed from the width and the height, and
//! the same page drawn on a 128 × 64 panel has the same bars with four lines
//! between them instead of eleven.

use crate::font;
use crate::nav::{ICON_CELL, ICON_H};

/// The layout of one page on a canvas of a given size.
///
/// `const`, so a firmware can hold the one for its panel as a constant and
/// size its buffers from it; a simulator computes one per canvas.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Canvas width, in pixels.
    pub width: usize,
    /// Canvas height, in pixels.
    pub height: usize,
    /// Height of the title bar, including its bottom edge.
    pub title_h: usize,
    /// Height of the icon strip at the foot of the screen.
    pub bar_h: usize,
    /// First row of the content area, below the title bar.
    pub content_top: usize,
    /// One past the last row of the content area, above the icon strip.
    pub content_bottom: usize,
    /// Left edge of a screen's own content, in from the canvas edge.
    pub content_left: usize,
}

impl Layout {
    /// The layout for a canvas `width` by `height`.
    ///
    /// A canvas too short to hold both bars gets a content area of zero
    /// rows rather than a negative one; nothing is drawn in it, and the
    /// bars overlap. That is the honest rendering of a panel the interface
    /// does not fit on.
    pub const fn of(width: usize, height: usize) -> Layout {
        let title_h = font::HEIGHT + 4;
        let bar_h = ICON_H + 4;
        let content_top = title_h + 2;
        let content_bottom = height.saturating_sub(bar_h);
        Layout {
            width,
            height,
            title_h,
            bar_h,
            content_top,
            content_bottom: if content_bottom < content_top {
                content_top
            } else {
                content_bottom
            },
            content_left: 4,
        }
    }

    /// The layout for a canvas.
    pub fn for_canvas(canvas: &impl crate::Canvas) -> Layout {
        Layout::of(canvas.width(), canvas.height())
    }

    /// Rows available to a screen's own content.
    pub const fn content_h(&self) -> usize {
        self.content_bottom - self.content_top
    }

    /// How many lines of text fit in the content area.
    pub const fn visible_lines(&self) -> usize {
        self.content_h() / font::LINE_HEIGHT
    }

    /// Characters that fit on one content line.
    ///
    /// The content starts at [`Layout::content_left`] and must stop short of
    /// the scrollbar, which is the last two columns, with a pixel between.
    pub const fn line_chars(&self) -> usize {
        self.width.saturating_sub(self.content_left + 2 + 1) / font::ADVANCE
    }

    /// Pixels a content line may be wide before it runs into the scrollbar.
    pub const fn line_width(&self) -> usize {
        self.width.saturating_sub(self.content_left + 2)
    }

    /// How wide the icon strip's cells are, for `count` screens.
    pub const fn strip_span(count: usize) -> usize {
        count * ICON_CELL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers the interface was first drawn with, so the derivation
    /// cannot drift from the pictures that were approved against them.
    #[test]
    fn the_square_panel_lays_out_as_it_always_did() {
        let l = Layout::of(128, 128);
        assert_eq!(l.title_h, 11);
        assert_eq!(l.bar_h, 11);
        assert_eq!(l.content_top, 13);
        assert_eq!(l.content_bottom, 117);
        assert_eq!(l.content_h(), 104);
        assert_eq!(l.content_left, 4);
        assert_eq!(l.visible_lines(), 11);
        assert_eq!(l.line_chars(), 20);
        assert_eq!(l.line_width(), 122);
    }

    /// Half the height loses content lines and nothing else.
    #[test]
    fn a_short_panel_keeps_its_bars_and_loses_lines() {
        let tall = Layout::of(128, 128);
        let short = Layout::of(128, 64);
        assert_eq!(short.title_h, tall.title_h);
        assert_eq!(short.bar_h, tall.bar_h);
        assert_eq!(short.content_top, tall.content_top);
        assert_eq!(short.content_bottom, 53);
        assert_eq!(short.visible_lines(), 4);
        assert_eq!(short.line_chars(), tall.line_chars(), "same width");
    }

    /// The content area is what the layout says it is, and the width is
    /// what sets the line length.
    #[test]
    fn width_sets_the_line_and_height_sets_the_lines() {
        assert_eq!(Layout::of(64, 128).line_chars(), 9);
        assert_eq!(Layout::of(64, 128).visible_lines(), 11);
        assert_eq!(Layout::of(256, 40).line_chars(), 41);
        assert_eq!(Layout::of(256, 40).visible_lines(), 1);
        // Thirty-two rows is two bars and eight pixels: not a line.
        assert_eq!(Layout::of(256, 32).visible_lines(), 0);
    }

    /// A panel too short for the chrome gets no content rows, not a
    /// negative number of them.
    #[test]
    fn a_panel_too_short_for_the_chrome_has_no_content() {
        let tiny = Layout::of(32, 16);
        assert_eq!(tiny.content_h(), 0);
        assert_eq!(tiny.visible_lines(), 0);
        assert_eq!(tiny.content_bottom, tiny.content_top);
        let none = Layout::of(8, 8);
        assert_eq!(none.line_chars(), 0);
        assert_eq!(none.line_width(), 2);
    }

    /// The layout of a canvas is the layout of its size.
    #[test]
    fn for_canvas_is_of_its_size() {
        let b = crate::Bitmap::<100, 40>::new();
        assert_eq!(Layout::for_canvas(&b), Layout::of(100, 40));
    }
}
