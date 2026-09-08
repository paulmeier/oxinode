//! The `embedded-graphics` adapter, both ways round.
//!
//! [`Target`] makes any [`Canvas`] a `DrawTarget`, so the ecosystem's
//! primitives, text and images can be drawn onto a framebuffer this
//! interface owns. [`Display`] makes any monochrome `DrawTarget` a
//! [`Canvas`], so this interface can be drawn onto any of the very large
//! number of display drivers that speak `embedded-graphics`.
//!
//! Both are thin: a pixel in one convention is a pixel in the other. What the
//! adapter has to decide is what to do with an error, because a [`Canvas`]
//! cannot fail and a `DrawTarget` can. [`Display`] keeps the last one for its
//! owner to look at, rather than dropping it on the floor or turning every
//! `set_pixel` in the interface into a `Result`.

use core::convert::Infallible;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

use crate::Canvas;

/// A [`Canvas`] as an `embedded-graphics` draw target.
///
/// Borrowed rather than owned, so drawing something from the ecosystem is a
/// step in the middle of drawing a page: `Target(&mut frame)` for the
/// duration of the call.
pub struct Target<'a, C: Canvas>(pub &'a mut C);

impl<C: Canvas> OriginDimensions for Target<'_, C> {
    fn size(&self) -> Size {
        Size::new(self.0.width() as u32, self.0.height() as u32)
    }
}

impl<C: Canvas> DrawTarget for Target<'_, C> {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, colour) in pixels {
            // `embedded-graphics` coordinates are signed and may fall off any
            // edge; the canvas clips the far edges itself.
            if let (Ok(x), Ok(y)) = (usize::try_from(point.x), usize::try_from(point.y)) {
                self.0.set_pixel(x, y, colour.is_on());
            }
        }
        Ok(())
    }

    fn clear(&mut self, colour: Self::Color) -> Result<(), Self::Error> {
        self.0.fill(colour.is_on());
        Ok(())
    }
}

/// A monochrome `embedded-graphics` display as a [`Canvas`].
///
/// A display that cannot fail -- a framebuffer -- is simply drawn on. One
/// that can -- a driver that talks to a bus on every pixel -- has its errors
/// kept, one at a time, for [`Display::error`]; the interface goes on
/// drawing, because there is nothing it could do about a bus that is down
/// except finish the page.
pub struct Display<D: DrawTarget<Color = BinaryColor>> {
    inner: D,
    error: Option<D::Error>,
}

impl<D: DrawTarget<Color = BinaryColor>> Display<D> {
    /// Wrap a display.
    pub fn new(inner: D) -> Self {
        Display { inner, error: None }
    }

    /// The display itself.
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// The display itself, to draw on or flush.
    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    /// Unwrap it.
    pub fn into_inner(self) -> D {
        self.inner
    }

    /// The most recent error the display reported while being drawn on,
    /// taking it. `None` when every draw since the last call succeeded.
    pub fn error(&mut self) -> Option<D::Error> {
        self.error.take()
    }

    fn note(&mut self, result: Result<(), D::Error>) {
        if let Err(e) = result {
            self.error = Some(e);
        }
    }
}

impl<D: DrawTarget<Color = BinaryColor> + OriginDimensions> Canvas for Display<D> {
    fn width(&self) -> usize {
        self.inner.size().width as usize
    }

    fn height(&self) -> usize {
        self.inner.size().height as usize
    }

    fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        // The canvas contract clips; a draw target may not, or may error on
        // a point outside itself, so the check is made here.
        if x >= self.width() || y >= self.height() {
            return;
        }
        let (Ok(px), Ok(py)) = (i32::try_from(x), i32::try_from(y)) else {
            return;
        };
        let result = self
            .inner
            .draw_iter([Pixel(Point::new(px, py), BinaryColor::from(on))]);
        self.note(result);
    }

    fn fill(&mut self, on: bool) {
        let result = self.inner.clear(BinaryColor::from(on));
        self.note(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::fixture::*;
    use crate::{page, Bitmap, Input, Readable};
    use embedded_graphics::mock_display::MockDisplay;
    use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle};

    /// A primitive drawn through the target lands on the canvas, clipped
    /// at every edge including the negative ones.
    #[test]
    fn a_primitive_draws_onto_the_canvas() {
        let mut b = Bitmap::<32, 32>::new();
        Rectangle::new(Point::new(2, 3), Size::new(4, 5))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(&mut Target(&mut b))
            .unwrap();
        for y in 0..32 {
            for x in 0..32 {
                let inside = (2..6).contains(&x) && (3..8).contains(&y);
                assert_eq!(b.pixel(x, y), inside, "({x}, {y})");
            }
        }
        // Off the top-left: only the on-canvas quarter lands.
        let mut b = Bitmap::<32, 32>::new();
        Rectangle::new(Point::new(-2, -2), Size::new(4, 4))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(&mut Target(&mut b))
            .unwrap();
        assert_eq!(b.lit(), 4);
        assert!(b.pixel(0, 0) && b.pixel(1, 1));
        // Off the bottom-right: clipped by the canvas.
        Rectangle::new(Point::new(30, 30), Size::new(10, 10))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(&mut Target(&mut b))
            .unwrap();
        assert_eq!(b.lit(), 8);
        assert_eq!(Target(&mut b).size(), Size::new(32, 32));
    }

    /// `Off` clears, and `clear` fills.
    #[test]
    fn colours_map_to_on_and_off() {
        let mut b = Bitmap::<8, 8>::new();
        Target(&mut b).clear(BinaryColor::On).unwrap();
        assert_eq!(b.lit(), 64);
        Rectangle::new(Point::new(0, 0), Size::new(8, 4))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(&mut Target(&mut b))
            .unwrap();
        assert_eq!(b.lit(), 32);
        assert!(!b.pixel(0, 0) && b.pixel(0, 4));
        Target(&mut b).clear(BinaryColor::Off).unwrap();
        assert_eq!(b.lit(), 0);
    }

    /// A page drawn on an `embedded-graphics` display is the page drawn on
    /// a bitmap of the same size, pixel for pixel.
    #[test]
    fn a_page_on_a_display_is_the_page_on_a_bitmap() {
        let mut mock = MockDisplay::<BinaryColor>::new();
        mock.set_allow_overdraw(true);
        let mut display = Display::new(mock);
        assert_eq!((display.width(), display.height()), (64, 64));
        let mut on_display = nav();
        on_display.handle(Input::Right);
        on_display.handle(Input::Select);
        page(
            &mut display,
            &mut on_display,
            "9%",
            "BT",
            &["one", "two", "three"],
        );
        assert!(display.error().is_none());

        let mut on_bitmap = nav();
        on_bitmap.handle(Input::Right);
        on_bitmap.handle(Input::Select);
        let mut bitmap = Bitmap::<64, 64>::new();
        page(
            &mut bitmap,
            &mut on_bitmap,
            "9%",
            "BT",
            &["one", "two", "three"],
        );
        assert!(bitmap.lit() > 200, "a page was drawn");

        let mock = display.into_inner();
        for y in 0..64 {
            for x in 0..64 {
                let on = mock.get_pixel(Point::new(x as i32, y as i32)) == Some(BinaryColor::On);
                assert_eq!(on, bitmap.pixel(x, y), "({x}, {y})");
            }
        }
    }

    /// Off-display pixels are clipped before they reach a display that
    /// would object to them.
    #[test]
    fn a_display_is_clipped_at_its_edges() {
        let mut mock = MockDisplay::<BinaryColor>::new();
        mock.set_allow_overdraw(true);
        // The mock panics on out-of-bounds drawing unless told otherwise, so
        // a pixel that reached it would fail this test loudly.
        let mut display = Display::new(mock);
        display.set_pixel(64, 0, true);
        display.set_pixel(0, 64, true);
        display.set_pixel(usize::MAX, usize::MAX, true);
        display.rect(60, 60, 10, 10, true);
        assert!(display.error().is_none());
        let mock = display.into_inner();
        assert_eq!(mock.affected_area().size, Size::new(4, 4));
        assert_eq!(mock.get_pixel(Point::new(63, 63)), Some(BinaryColor::On));
    }

    /// A display that fails keeps its error for its owner, and the drawing
    /// carries on.
    #[test]
    fn a_failing_display_keeps_the_error() {
        #[derive(Debug, PartialEq)]
        struct Bus(u32);
        struct Flaky {
            calls: u32,
            drawn: u32,
        }
        impl OriginDimensions for Flaky {
            fn size(&self) -> Size {
                Size::new(16, 16)
            }
        }
        impl DrawTarget for Flaky {
            type Color = BinaryColor;
            type Error = Bus;
            fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Bus>
            where
                I: IntoIterator<Item = Pixel<BinaryColor>>,
            {
                self.calls += 1;
                self.drawn += pixels.into_iter().count() as u32;
                if self.calls % 3 == 0 {
                    Err(Bus(self.calls))
                } else {
                    Ok(())
                }
            }
        }
        let mut display = Display::new(Flaky { calls: 0, drawn: 0 });
        assert_eq!(display.error(), None);
        display.rect(0, 0, 2, 2, true);
        assert_eq!(display.inner().drawn, 4, "every pixel was attempted");
        assert_eq!(display.error(), Some(Bus(3)), "the failure was kept");
        assert_eq!(display.error(), None, "and taken");
        display.set_pixel(0, 0, true);
        display.set_pixel(0, 0, true);
        assert_eq!(display.inner_mut().calls, 6);
        assert_eq!(display.error(), Some(Bus(6)));
    }

    /// Something round, because a menu box is all straight lines and a
    /// circle is what the ecosystem draws that this crate does not.
    #[test]
    fn the_ecosystem_can_draw_what_this_crate_cannot() {
        let mut b = Bitmap::<32, 32>::new();
        Circle::new(Point::new(4, 4), 24)
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(&mut Target(&mut b))
            .unwrap();
        assert!(b.lit() > 40);
        assert!(!b.pixel(16, 16), "hollow");
        assert!(!b.pixel(4, 4), "round, not square");
    }
}
