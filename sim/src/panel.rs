//! A panel of any size, for rendering the interface where there is no
//! framebuffer type for it.
//!
//! The board's frame is 128 × 128 and lives in the controller's own layout;
//! the interface crate draws on anything that can set a pixel. This is the
//! anything: width, height, and a `Vec` of booleans, so the simulator can
//! show what the same screens look like on a 128 × 64 panel -- or any other
//! -- without a framebuffer type per size.

use monopanel::{Canvas, Readable};
use oxinode_core::sh1107;

/// A panel size, as the command line names one: `128x64`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Size {
    pub width: usize,
    pub height: usize,
}

impl Size {
    /// The board's own panel.
    pub const SQUARE: Size = Size {
        width: sh1107::WIDTH,
        height: sh1107::HEIGHT,
    };

    /// The other common OLED, and the one the RNode protocol assumes.
    pub const WIDE: Size = Size {
        width: 128,
        height: 64,
    };

    /// Parse `WxH`.
    pub fn parse(word: &str) -> Option<Size> {
        let (w, h) = word.split_once(['x', 'X', '*'])?;
        let size = Size {
            width: w.trim().parse().ok()?,
            height: h.trim().parse().ok()?,
        };
        (size.width > 0 && size.height > 0).then_some(size)
    }

    /// `WxH`, for a file name or a message.
    pub fn name(self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

/// A panel in memory, of any size.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Panel {
    size: Size,
    pixels: Vec<bool>,
}

impl Panel {
    /// An all-dark panel.
    pub fn new(size: Size) -> Panel {
        Panel {
            size,
            pixels: vec![false; size.width * size.height],
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// A copy of the board's frame, pixel for pixel.
    pub fn from_frame(frame: &sh1107::Frame) -> Panel {
        let mut panel = Panel::new(Size::SQUARE);
        for y in 0..sh1107::HEIGHT {
            for x in 0..sh1107::WIDTH {
                panel.set_pixel(x, y, frame.pixel(x, y));
            }
        }
        panel
    }
}

impl Canvas for Panel {
    fn width(&self) -> usize {
        self.size.width
    }

    fn height(&self) -> usize {
        self.size.height
    }

    fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        if x < self.size.width && y < self.size.height {
            self.pixels[y * self.size.width + x] = on;
        }
    }

    fn fill(&mut self, on: bool) {
        self.pixels.fill(on);
    }
}

impl Readable for Panel {
    fn pixel(&self, x: usize, y: usize) -> bool {
        x < self.size.width && y < self.size.height && self.pixels[y * self.size.width + x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_as_written_and_refuse_nonsense() {
        assert_eq!(Size::parse("128x64"), Some(Size::WIDE));
        assert_eq!(Size::parse("128X128"), Some(Size::SQUARE));
        assert_eq!(
            Size::parse(" 64 x 48 "),
            Some(Size {
                width: 64,
                height: 48
            })
        );
        assert_eq!(Size::parse("128"), None);
        assert_eq!(Size::parse("0x64"), None);
        assert_eq!(Size::parse("wide"), None);
        assert_eq!(Size::WIDE.name(), "128x64");
        assert_eq!(Size::parse(&Size::SQUARE.name()), Some(Size::SQUARE));
    }

    #[test]
    fn a_panel_is_its_size_and_clips() {
        let mut p = Panel::new(Size::WIDE);
        assert_eq!((p.width(), p.height()), (128, 64));
        p.set_pixel(127, 63, true);
        p.set_pixel(128, 0, true);
        p.set_pixel(0, 64, true);
        assert!(p.pixel(127, 63));
        assert!(!p.pixel(128, 0));
        assert_eq!(p.pixels.iter().filter(|&&b| b).count(), 1);
        p.fill(true);
        assert_eq!(p.pixels.iter().filter(|&&b| b).count(), 128 * 64);
    }

    /// A panel made from the board's frame reads the same.
    #[test]
    fn a_frame_copies_across_exactly() {
        let mut frame = sh1107::Frame::new();
        frame.rect(3, 5, 20, 7, true);
        frame.set_pixel(127, 127, true);
        let panel = Panel::from_frame(&frame);
        assert_eq!(panel.size(), Size::SQUARE);
        for y in 0..128 {
            for x in 0..128 {
                assert_eq!(panel.pixel(x, y), frame.pixel(x, y), "({x}, {y})");
            }
        }
    }
}
