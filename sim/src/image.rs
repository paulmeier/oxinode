//! A frame as a picture: 4x, with a pixel grid, in and out of PNG.
//!
//! The scale and the grid are not decoration. At 1:1 a 128-pixel frame is a
//! postage stamp, and at 4x without a grid a one-pixel misalignment -- which is
//! the only kind of layout bug this interface has -- is a faint smudge. With
//! the grid every panel pixel is a countable cell.

use oxinode_core::sh1107::{self, Frame};

/// Panel pixels are rendered as cells this many image pixels square.
pub const SCALE: usize = 4;
/// Width and height of a rendered frame, in image pixels.
pub const SIZE: usize = sh1107::WIDTH * SCALE;

/// One RGB colour.
pub type Rgb = [u8; 3];

/// An unlit panel pixel: not quite black, so the grid can be darker than it
/// without disappearing.
pub const OFF: Rgb = [0x14, 0x14, 0x14];
/// A lit panel pixel: the OLED's white, which is a little grey.
pub const ON: Rgb = [0xEC, 0xEC, 0xEC];
/// The grid between cells.
pub const GRID: Rgb = [0x2A, 0x2A, 0x2A];
/// Where a golden and an actual image disagree.
pub const DIFF: Rgb = [0xFF, 0x20, 0x20];

/// A rendered image: `SIZE` x `SIZE`, RGB, row-major.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Image {
    pixels: Vec<Rgb>,
}

impl Image {
    /// Render a frame.
    ///
    /// Each panel pixel becomes a `SCALE` x `SCALE` cell whose last row and
    /// column are grid, so the picture is the panel with a fine dark mesh
    /// over it.
    pub fn render(frame: &Frame) -> Image {
        let mut pixels = vec![OFF; SIZE * SIZE];
        for y in 0..sh1107::HEIGHT {
            for x in 0..sh1107::WIDTH {
                let colour = if frame.pixel(x, y) { ON } else { OFF };
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        let grid = dx == SCALE - 1 || dy == SCALE - 1;
                        pixels[(y * SCALE + dy) * SIZE + x * SCALE + dx] =
                            if grid { GRID } else { colour };
                    }
                }
            }
        }
        Image { pixels }
    }

    /// The colour at an image coordinate.
    pub fn get(&self, x: usize, y: usize) -> Rgb {
        self.pixels[y * SIZE + x]
    }

    /// Encode as a PNG.
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, SIZE as u32, SIZE as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("a PNG header");
            writer
                .write_image_data(self.pixels.as_flattened())
                .expect("PNG image data");
        }
        out
    }

    /// Decode a PNG written by [`Image::to_png`].
    ///
    /// Anything that is not an 8-bit RGB image of the right size is refused
    /// rather than coerced, because the only way such a file ends up in the
    /// golden directory is by mistake, and a comparison against a coerced
    /// image reports a difference in the wrong place.
    pub fn from_png(bytes: &[u8]) -> Result<Image, String> {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        let info = reader.info();
        if (info.width as usize, info.height as usize) != (SIZE, SIZE) {
            return Err(format!(
                "expected {SIZE}x{SIZE}, got {}x{}",
                info.width, info.height
            ));
        }
        if info.color_type != png::ColorType::Rgb || info.bit_depth != png::BitDepth::Eight {
            return Err(format!(
                "expected 8-bit RGB, got {:?} {:?}",
                info.color_type, info.bit_depth
            ));
        }
        let mut buf = vec![0; reader.output_buffer_size().ok_or("no buffer size")?];
        let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let pixels = buf[..frame.buffer_size()]
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        Ok(Image { pixels })
    }

    /// Where two images differ, as a picture: the expected image dimmed, with
    /// every differing pixel painted [`DIFF`]. Returns `None` when they are
    /// identical.
    pub fn diff(expected: &Image, actual: &Image) -> Option<Image> {
        if expected == actual {
            return None;
        }
        let pixels = expected
            .pixels
            .iter()
            .zip(&actual.pixels)
            .map(|(e, a)| {
                if e == a {
                    [e[0] / 3, e[1] / 3, e[2] / 3]
                } else {
                    DIFF
                }
            })
            .collect();
        Some(Image { pixels })
    }

    /// How many pixels differ between two images.
    pub fn differing_pixels(a: &Image, b: &Image) -> usize {
        a.pixels
            .iter()
            .zip(&b.pixels)
            .filter(|(a, b)| a != b)
            .count()
    }
}

/// Rebuild a frame from a dump of the controller's RAM, as the firmware's
/// `CMD_DISP_READ` or a logged framebuffer would hand it over.
///
/// The byte layout is the controller's own, so this goes through the same
/// address arithmetic the firmware uses to draw, rather than a second copy
/// of it.
pub fn frame_from_bytes(bytes: &[u8; sh1107::BUFFER_LEN]) -> Frame {
    let mut frame = Frame::new();
    for y in 0..sh1107::HEIGHT {
        for x in 0..sh1107::WIDTH {
            if let Some((index, bit)) = sh1107::ram_position(x, y) {
                frame.set_pixel(x, y, bytes[index] & (1 << bit) != 0);
            }
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_pixel(x: usize, y: usize) -> Frame {
        let mut frame = Frame::new();
        frame.set_pixel(x, y, true);
        frame
    }

    /// A lit panel pixel is a lit cell with grid on two edges.
    #[test]
    fn a_pixel_becomes_a_cell_with_a_grid_edge() {
        let image = Image::render(&one_pixel(3, 5));
        for dy in 0..SCALE {
            for dx in 0..SCALE {
                let got = image.get(3 * SCALE + dx, 5 * SCALE + dy);
                let want = if dx == SCALE - 1 || dy == SCALE - 1 {
                    GRID
                } else {
                    ON
                };
                assert_eq!(got, want, "at ({dx}, {dy}) in the cell");
            }
        }
        // And its neighbour is unlit, with the same grid.
        assert_eq!(image.get(4 * SCALE, 5 * SCALE), OFF);
        assert_eq!(image.get(4 * SCALE + SCALE - 1, 5 * SCALE), GRID);
    }

    /// The grid is drawn whether or not the pixel is lit.
    #[test]
    fn the_grid_covers_the_whole_image() {
        let mut frame = Frame::new();
        frame.fill(true);
        let image = Image::render(&frame);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let grid = x % SCALE == SCALE - 1 || y % SCALE == SCALE - 1;
                assert_eq!(image.get(x, y) == GRID, grid, "at ({x}, {y})");
            }
        }
    }

    /// Encoding and decoding is lossless.
    #[test]
    fn png_round_trips() {
        let mut frame = Frame::new();
        frame.rect(10, 20, 30, 40, true);
        frame.set_pixel(127, 127, true);
        let image = Image::render(&frame);
        let png = image.to_png();
        assert_eq!(Image::from_png(&png).unwrap(), image);
        // And it is worth the dependency: a raw dump is 3/4 MB.
        assert!(png.len() < 20_000, "{} bytes", png.len());
    }

    /// A PNG of the wrong shape is refused, not resized.
    #[test]
    fn the_wrong_kind_of_png_is_refused() {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 2, 2);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 1, 2, 3]).unwrap();
        }
        let err = Image::from_png(&out).unwrap_err();
        assert!(err.contains("expected"), "{err}");
        assert!(Image::from_png(b"not a png").is_err());
    }

    /// Identical images have no diff; different ones have a diff that marks
    /// exactly the differing cells.
    #[test]
    fn a_diff_marks_where_and_only_where_they_differ() {
        let a = Image::render(&one_pixel(1, 1));
        assert!(Image::diff(&a, &a).is_none());
        let b = Image::render(&one_pixel(2, 1));
        let diff = Image::diff(&a, &b).expect("a difference");
        // Two cells' worth of non-grid pixels differ.
        let marked = (0..SIZE * SIZE).filter(|&i| diff.pixels[i] == DIFF).count();
        assert_eq!(marked, 2 * (SCALE - 1) * (SCALE - 1));
        assert_eq!(Image::differing_pixels(&a, &b), marked);
        assert_eq!(diff.get(SCALE, SCALE), DIFF);
        assert_eq!(diff.get(2 * SCALE, SCALE), DIFF);
        // Everything else is dimmed, not black and not the original.
        let untouched = diff.get(50 * SCALE, 50 * SCALE);
        assert_ne!(untouched, OFF);
        assert_ne!(untouched, DIFF);
    }

    /// A controller-layout dump comes back as the frame that produced it.
    #[test]
    fn a_ram_dump_round_trips() {
        let mut frame = Frame::new();
        frame.rect(0, 0, 128, 11, true);
        frame.set_pixel(77, 99, true);
        let rebuilt = frame_from_bytes(frame.as_bytes());
        assert_eq!(rebuilt.as_bytes(), frame.as_bytes());
    }
}
