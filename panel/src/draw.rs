//! The chrome and the widgets: a title bar, an icon strip, a menu, a
//! scrollbar, and the page that composes them.
//!
//! The shape is deliberately the one the Meshtastic firmware uses, because
//! it is a shape people already know -- a title bar at the top, a strip of
//! icons at the bottom saying where you are, and a popup menu of actions
//! over the middle. Every position comes from a [`Layout`] derived from the
//! canvas, so the same code draws the same page on any panel the bars fit
//! on; what changes with the panel is how many lines fit between them, and
//! how much of a long menu shows at once.

use crate::font;
use crate::layout::Layout;
use crate::nav::{Icon, Modal, Nav, Screen, ICON_CELL, ICON_H, ICON_W};
use crate::Canvas;

/// Draw the title bar: a solid strip with the text knocked out of it.
///
/// `left` is the battery or whatever else belongs in the corner, `right` the
/// clock. Both are borrowed rather than formatted here, because formatting
/// needs an allocator or a scratch buffer and the caller has one either way.
pub fn title_bar(canvas: &mut impl Canvas, left: &str, title: &str, right: &str) {
    let layout = Layout::for_canvas(canvas);
    canvas.rect(0, 0, layout.width, layout.title_h, true);
    let y = (layout.title_h - font::HEIGHT) / 2;
    font::draw(canvas, 2, y, left, false);
    // Centred on the panel rather than between its neighbours, so the title
    // does not shuffle sideways when the battery reading goes from 9% to 100%.
    let x = layout.width.saturating_sub(font::width_of(title)) / 2;
    font::draw(canvas, x, y, title, false);
    font::draw_right(canvas, layout.width.saturating_sub(2), y, right, false);
}

/// Draw the strip of screen icons across the foot of the panel.
///
/// The current screen's icon is drawn knocked out of a filled cell, which is
/// the only marking that survives being glanced at: a box around an icon and
/// a box beside it look the same at arm's length, an inverted one does not.
pub fn icon_bar<A>(canvas: &mut impl Canvas, screens: &[Screen<A>], current: usize) {
    let layout = Layout::for_canvas(canvas);
    let top = layout.height.saturating_sub(layout.bar_h);
    canvas.rect(0, top, layout.width, layout.bar_h, false);
    // A hairline above the strip, so it reads as chrome rather than as the
    // bottom of whatever the screen happens to be showing.
    canvas.rect(0, top, layout.width, 1, true);

    let span = Layout::strip_span(screens.len());
    let left = layout.width.saturating_sub(span) / 2;
    for (n, screen) in screens.iter().enumerate() {
        let cell_x = left + n * ICON_CELL;
        let on = n == current;
        if on {
            canvas.rect(cell_x, top + 2, ICON_CELL, layout.bar_h - 2, true);
        }
        draw_icon(
            canvas,
            cell_x + (ICON_CELL - ICON_W) / 2,
            top + 3,
            &screen.icon,
            !on,
        );
    }
}

/// Draw one icon with its top-left corner at `(x, y)`.
pub fn draw_icon(canvas: &mut impl Canvas, x: usize, y: usize, icon: &Icon, on: bool) {
    for (dx, column) in icon.iter().enumerate() {
        for dy in 0..ICON_H {
            if column & (1 << dy) != 0 {
                canvas.set_pixel(x + dx, y + dy, on);
            }
        }
    }
}

/// Which items of a menu are shown, on a panel that may not hold them all.
///
/// A menu taller than the content area shows a window of its items with the
/// highlighted one inside it -- near the middle where it can be, at an end
/// where it must -- so the highlight is always on the panel and moving it
/// moves the window. On a panel that holds the whole menu the window is the
/// whole menu, and the picture is what it always was.
pub fn menu_window(layout: &Layout, count: usize, selected: usize) -> core::ops::Range<usize> {
    let head_h = font::HEIGHT + 4;
    let fits = layout
        .content_h()
        .saturating_sub(head_h + 4)
        .checked_div(font::LINE_HEIGHT)
        .unwrap_or(0)
        .max(1)
        .min(count);
    if count == 0 {
        return 0..0;
    }
    let first = selected.saturating_sub(fits / 2).min(count - fits);
    first..first + fits
}

/// Draw the action menu over the content.
///
/// A filled heading, a framed body, and the highlighted item wrapped in `>` and
/// `<` as well as being inverted. Belt and braces on purpose: the panel is
/// viewed at an angle as often as not, and inversion alone is easy to lose.
pub fn menu<A>(canvas: &mut impl Canvas, screen: &Screen<A>, selected: usize) {
    let layout = Layout::for_canvas(canvas);
    let items = screen.menu;
    let title = screen.menu_title;
    if items.is_empty() {
        return;
    }
    let window = menu_window(&layout, items.len(), selected);

    let rows = window.len();
    let body_h = rows * font::LINE_HEIGHT + 4;
    let head_h = font::HEIGHT + 4;
    let total_h = head_h + body_h;

    // Sized for the whole menu, not the window, so the box does not change
    // width as the highlight moves through it.
    let widest = items
        .iter()
        .map(|i| font::width_of(i.label) + 4 * font::ADVANCE)
        .fold(font::width_of(title) + 8, usize::max);
    let w = widest.min(layout.width.saturating_sub(8));
    let x = layout.width.saturating_sub(w) / 2;
    // Centred in the content area rather than on the panel, so it never sits
    // over the title bar or the icon strip.
    let y = layout.content_top + layout.content_h().saturating_sub(total_h) / 2;

    canvas.rect(x, y, w, head_h, true);
    let tx = x + w.saturating_sub(font::width_of(title)) / 2;
    font::draw(canvas, tx, y + 2, title, false);

    canvas.rect(x, y + head_h, w, body_h, false);
    canvas.frame_rect(x, y + head_h, w, body_h, true);

    for (n, item) in items[window.clone()].iter().enumerate() {
        let row_y = y + head_h + 2 + n * font::LINE_HEIGHT;
        let chosen = window.start + n == selected;
        if chosen {
            canvas.rect(
                x + 1,
                row_y - 1,
                w.saturating_sub(2),
                font::LINE_HEIGHT,
                true,
            );
        }
        let label_w = font::width_of(item.label) + if chosen { 4 * font::ADVANCE } else { 0 };
        let mut lx = x + w.saturating_sub(label_w) / 2;
        if chosen {
            // One advance of gap inside each bracket, which is what the
            // width above budgets for. The first version drew the label hard
            // against `>` and left the gap only before `<`; it looked right
            // as a pixel count and wrong as a picture, and was the first
            // thing a golden image caught.
            lx = font::draw(canvas, lx, row_y, ">", false) + font::ADVANCE;
            lx = font::draw(canvas, lx, row_y, item.label, false);
            font::draw(canvas, lx + font::ADVANCE, row_y, "<", false);
        } else {
            font::draw(canvas, lx, row_y, item.label, true);
        }
    }
}

/// Draw a scrollbar down the right edge of the content area.
///
/// Drawn only when there is something to scroll. A full-height bar on a screen
/// that fits says "there is more" as loudly as a short one does.
pub fn scrollbar(canvas: &mut impl Canvas, first: usize, lines: usize) {
    let layout = Layout::for_canvas(canvas);
    let visible = layout.visible_lines();
    if lines <= visible || layout.width < 2 {
        return;
    }
    let x = layout.width - 2;
    let track = layout.content_h();
    let thumb = (track * visible / lines).max(4).min(track);
    let span = track - thumb;
    let top = layout.content_top + span * first / (lines - visible);
    canvas.rect(x, layout.content_top, 1, track, false);
    canvas.rect(x, top, 2, thumb, true);
}

/// Draw a whole page: chrome, content and, if one is open, the menu.
///
/// This is the one composition a firmware and a simulator share, so that
/// what is looked at on the host is what is drawn on the board. `lines` is
/// the current screen's content, one string per line, already formatted --
/// the caller owns the scratch buffers, for the reason [`title_bar`] gives.
/// The navigator is told how tall the content is and how much fits here,
/// because this is the first place that knows either, and it is told before
/// the scroll is read so that a screen shorter than the last one cannot be
/// shown scrolled past its end.
///
/// A modal's lines replace the screen's and are never scrolled, so the
/// screen's own scroll is left where it was for when the modal closes.
pub fn page<A: Copy + 'static, M: Modal<A>>(
    canvas: &mut impl Canvas,
    nav: &mut Nav<A, M>,
    left: &str,
    right: &str,
    lines: &[&str],
) {
    let layout = Layout::for_canvas(canvas);
    canvas.fill(false);
    let first = if nav.modal_is_open() {
        0
    } else {
        nav.set_content(lines.len(), layout.visible_lines());
        nav.scroll()
    };
    title_bar(canvas, left, nav.title(), right);

    for (n, line) in lines
        .iter()
        .skip(first)
        .take(layout.visible_lines())
        .enumerate()
    {
        font::draw(
            canvas,
            layout.content_left,
            layout.content_top + n * font::LINE_HEIGHT,
            line,
            true,
        );
    }
    if !nav.modal_is_open() {
        scrollbar(canvas, first, lines.len());
    }

    icon_bar(canvas, nav.screens(), nav.screen_index());
    if let Some(selected) = nav.menu_item() {
        menu(canvas, nav.screen(), selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::fixture::*;
    use crate::nav::Input;
    use crate::{Bitmap, Readable};

    /// The panel the interface was first drawn on, and a shorter one.
    type Square = Bitmap<128, 128>;
    type Wide = Bitmap<128, 64>;

    fn lit(canvas: &impl Readable) -> usize {
        (0..canvas.width())
            .flat_map(|x| (0..canvas.height()).map(move |y| (x, y)))
            .filter(|&(x, y)| canvas.pixel(x, y))
            .count()
    }

    fn differs(a: &impl Readable, b: &impl Readable) -> bool {
        (0..a.width())
            .flat_map(|x| (0..a.height()).map(move |y| (x, y)))
            .any(|(x, y)| a.pixel(x, y) != b.pixel(x, y))
    }

    /// The chrome leaves the content area alone, on both panels.
    ///
    /// The whole layout rests on these three not overlapping, and the numbers
    /// are easy to nudge apart by one while editing a constant.
    #[test]
    fn the_chrome_does_not_overlap_the_content() {
        fn check<const W: usize, const H: usize>() {
            let mut b = Bitmap::<W, H>::new();
            let layout = Layout::for_canvas(&b);
            title_bar(&mut b, "99%", "Home", "12:45p");
            icon_bar(&mut b, &SCREENS, 0);
            for y in layout.content_top..layout.content_bottom {
                for x in 0..W {
                    assert!(!b.pixel(x, y), "{W}x{H}: chrome drew at ({x}, {y})");
                }
            }
        }
        check::<128, 128>();
        check::<128, 64>();
        check::<64, 48>();
    }

    /// The title bar is a solid strip with dark text in it.
    #[test]
    fn the_title_bar_is_knocked_out_rather_than_drawn() {
        let mut b = Square::new();
        title_bar(&mut b, "99%", "Home", "12:45p");
        let top = (0..128).filter(|&x| b.pixel(x, 0)).count();
        assert_eq!(top, 128, "the strip should be solid");
        let middle = (0..128).filter(|&x| b.pixel(x, 3)).count();
        assert!(middle < 128, "the text should be knocked out");
    }

    /// The title is centred on the panel, whatever the panel's width.
    #[test]
    fn the_title_is_centred_on_the_panel() {
        fn centre<const W: usize>() -> usize {
            let mut b = Bitmap::<W, 32>::new();
            title_bar(&mut b, "", "Home", "");
            let dark: Vec<usize> = (0..W).filter(|&x| !b.pixel(x, 5)).collect();
            (dark.first().unwrap() + dark.last().unwrap()) / 2
        }
        assert!(centre::<128>().abs_diff(64) <= 3);
        assert!(centre::<64>().abs_diff(32) <= 3);
    }

    /// Exactly one icon is highlighted, and it is the current screen's.
    #[test]
    fn the_strip_highlights_only_where_you_are() {
        for current in 0..SCREENS.len() {
            let mut b = Wide::new();
            let layout = Layout::for_canvas(&b);
            icon_bar(&mut b, &SCREENS, current);
            let top = layout.height - layout.bar_h;
            let span = Layout::strip_span(SCREENS.len());
            let left = (layout.width - span) / 2;
            for n in 0..SCREENS.len() {
                let cell_x = left + n * ICON_CELL;
                // The row under the icon is background in an unfilled cell and
                // solid in the filled one.
                let filled = (cell_x..cell_x + ICON_CELL)
                    .filter(|&x| b.pixel(x, top + layout.bar_h - 1))
                    .count()
                    == ICON_CELL;
                assert_eq!(filled, n == current, "{current}: cell {n} filled={filled}");
            }
        }
    }

    /// An icon lands where it is put, the right way up.
    #[test]
    fn an_icon_lands_where_it_is_put() {
        let mut b = Bitmap::<16, 16>::new();
        // A single column with its top bit set.
        let icon: Icon = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40];
        draw_icon(&mut b, 3, 4, &icon, true);
        assert!(b.pixel(3, 4), "column 0 row 0");
        assert!(b.pixel(9, 10), "column 6 row 6");
        assert_eq!(b.lit(), 2);
    }

    /// Every menu fits inside the content area, on every screen, on a
    /// panel that holds it and on one that does not.
    #[test]
    fn no_menu_draws_outside_the_content_area() {
        fn check<const W: usize, const H: usize>() {
            for screen in &SCREENS {
                for selected in 0..screen.menu.len() {
                    let mut b = Bitmap::<W, H>::new();
                    let layout = Layout::for_canvas(&b);
                    menu(&mut b, screen, selected);
                    for x in 0..W {
                        for y in (0..layout.content_top).chain(layout.content_bottom..H) {
                            assert!(
                                !b.pixel(x, y),
                                "{W}x{H} {} drew at ({x}, {y})",
                                screen.title
                            );
                        }
                    }
                }
            }
        }
        check::<128, 128>();
        check::<128, 64>();
    }

    /// A menu actually draws something, and something different per selection.
    ///
    /// Guards against a layout slip that renders an empty box: every assertion
    /// above is about where it does *not* draw.
    #[test]
    fn a_menu_draws_and_the_highlight_moves() {
        let screen = &SCREENS[1];
        let mut shots = Vec::new();
        for selected in 0..screen.menu.len() {
            let mut b = Square::new();
            menu(&mut b, screen, selected);
            assert!(lit(&b) > 100, "selection {selected} drew almost nothing");
            shots.push(b);
        }
        for (a, b) in shots.iter().zip(shots.iter().skip(1)) {
            assert!(differs(a, b), "the highlight did not move");
        }
    }

    /// An empty menu draws nothing at all.
    #[test]
    fn an_empty_menu_draws_nothing() {
        let mut b = Square::new();
        menu(&mut b, &SCREENS[2], 0);
        assert_eq!(lit(&b), 0);
        assert_eq!(menu_window(&Layout::of(128, 128), 0, 0), 0..0);
    }

    /// On a panel that holds the whole menu the window is the whole menu;
    /// on one that does not, the window always holds the selection and
    /// slides with it.
    #[test]
    fn the_menu_window_holds_the_selection() {
        let square = Layout::of(128, 128);
        for selected in 0..9 {
            assert_eq!(menu_window(&square, 9, selected), 0..9);
        }
        let wide = Layout::of(128, 64);
        let fits = menu_window(&wide, 9, 0).len();
        assert!(fits < 9, "the long menu should not fit a 128 x 64");
        assert!(fits >= 1);
        for selected in 0..9 {
            let window = menu_window(&wide, 9, selected);
            assert!(window.contains(&selected), "{selected}: {window:?}");
            assert_eq!(window.len(), fits, "{selected}");
            assert!(window.end <= 9);
        }
        assert_eq!(menu_window(&wide, 9, 0).start, 0);
        assert_eq!(menu_window(&wide, 9, 8).end, 9);
        // A menu that fits exactly is not windowed.
        assert_eq!(menu_window(&wide, fits, fits - 1), 0..fits);
        // Even a panel with no content rows shows one item, rather than a
        // heading with nothing under it.
        assert_eq!(menu_window(&Layout::of(64, 16), 3, 2).len(), 1);
    }

    /// On the short panel, the long menu's picture changes as the highlight
    /// walks through it, and the highlighted label is always visible.
    #[test]
    fn a_long_menu_scrolls_on_a_short_panel() {
        let screen = &SCREENS[1];
        let mut shots = Vec::new();
        for selected in 0..screen.menu.len() {
            let mut b = Wide::new();
            menu(&mut b, screen, selected);
            assert!(lit(&b) > 100, "selection {selected} drew almost nothing");
            shots.push(b);
        }
        for (n, (a, b)) in shots.iter().zip(shots.iter().skip(1)).enumerate() {
            assert!(
                differs(a, b),
                "{n} -> {}: the picture did not change",
                n + 1
            );
        }
    }

    /// The scrollbar appears only when there is more than one screenful.
    #[test]
    fn the_scrollbar_appears_only_when_it_is_needed() {
        let lit_after = |lines, first| {
            let mut b = Square::new();
            scrollbar(&mut b, first, lines);
            lit(&b)
        };
        let visible = Layout::of(128, 128).visible_lines();
        assert_eq!(lit_after(visible, 0), 0, "a screen that fits");
        assert!(lit_after(visible * 3, 0) > 0, "a screen that does not");
    }

    /// The thumb moves down as the view does, and stays on the track.
    #[test]
    fn the_scrollbar_thumb_tracks_the_view() {
        fn check<const W: usize, const H: usize>() {
            let layout = Layout::of(W, H);
            let lines = layout.visible_lines() * 3;
            let top_of = |first| {
                let mut b = Bitmap::<W, H>::new();
                scrollbar(&mut b, first, lines);
                (0..H).find(|&y| b.pixel(W - 1, y))
            };
            let first = top_of(0).expect("a thumb at the top");
            let last = top_of(lines - layout.visible_lines()).expect("a thumb at the bottom");
            assert!(first < last, "{W}x{H}: the thumb should move down");
            assert!(first >= layout.content_top, "{W}x{H}: above the track");
            assert!(last < layout.content_bottom, "{W}x{H}: below the track");
        }
        check::<128, 128>();
        check::<128, 64>();
    }

    /// The content area is tall enough to be worth having.
    #[test]
    fn the_layout_leaves_room_to_draw_in() {
        assert!(Layout::of(128, 128).visible_lines() >= 8);
        assert!(Layout::of(128, 64).visible_lines() >= 3);
    }

    /// A composed page carries the chrome of the screen the navigator is on.
    #[test]
    fn a_page_shows_the_current_screen() {
        let mut nav = nav();
        nav.handle(Input::Right);
        let mut b = Square::new();
        page(&mut b, &mut nav, "99%", "12:45p", &[]);

        let mut chrome = Square::new();
        title_bar(&mut chrome, "99%", SCREENS[1].title, "12:45p");
        icon_bar(&mut chrome, &SCREENS, 1);
        assert_eq!(b, chrome, "chrome only, on the second screen");
    }

    /// Content lines land in the content area, starting at the scroll.
    #[test]
    fn a_page_draws_its_lines_from_the_scroll() {
        fn check<const W: usize, const H: usize>() {
            let layout = Layout::of(W, H);
            let visible = layout.visible_lines();
            let lines: Vec<String> = (0..visible + 4).map(|n| format!("line {n}")).collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let mut nav = nav();
            let mut top = Bitmap::<W, H>::new();
            page(&mut top, &mut nav, "", "", &refs);
            assert_eq!(nav.scroll(), 0);

            nav.handle(Input::Down);
            let mut down = Bitmap::<W, H>::new();
            page(&mut down, &mut nav, "", "", &refs);
            assert_eq!(nav.scroll(), 1, "the page told the navigator its height");

            // Row 2 of the scrolled page is row 1 of the unscrolled one,
            // shifted up by one line, in the text columns.
            for x in layout.content_left..W - 4 {
                for dy in 0..font::HEIGHT {
                    assert_eq!(
                        down.pixel(x, layout.content_top + dy),
                        top.pixel(x, layout.content_top + font::LINE_HEIGHT + dy),
                        "{W}x{H} at ({x}, {dy})"
                    );
                }
            }
            // And it did not draw in the chrome.
            for x in 0..W {
                for y in (0..layout.content_top).chain(layout.content_bottom..H) {
                    assert_eq!(
                        top.pixel(x, y),
                        down.pixel(x, y),
                        "chrome changed at ({x}, {y})"
                    );
                }
            }
        }
        check::<128, 128>();
        check::<128, 64>();
    }

    /// Fewer lines than fit means no scrollbar; more means one -- and how
    /// many fit depends on the panel.
    #[test]
    fn a_page_scrolls_only_when_it_must() {
        fn bar_lit<const W: usize, const H: usize>(count: usize) -> bool {
            let lines: Vec<String> = (0..count).map(|n| n.to_string()).collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let mut nav = nav();
            let mut b = Bitmap::<W, H>::new();
            page(&mut b, &mut nav, "", "", &refs);
            let layout = Layout::of(W, H);
            (layout.content_top..layout.content_bottom).any(|y| b.pixel(W - 1, y))
        }
        assert!(!bar_lit::<128, 128>(11));
        assert!(bar_lit::<128, 128>(12));
        assert!(!bar_lit::<128, 64>(4));
        assert!(bar_lit::<128, 64>(5), "five lines do not fit a 128 x 64");
    }

    /// An open menu is drawn over the content, and a closed one is not.
    #[test]
    fn a_page_overlays_the_menu_when_one_is_open() {
        let mut nav = nav();
        let mut closed = Square::new();
        page(&mut closed, &mut nav, "", "", &[]);
        nav.handle(Input::Select);
        let mut open = Square::new();
        page(&mut open, &mut nav, "", "", &[]);

        let mut expected = closed.clone();
        menu(&mut expected, &SCREENS[0], 0);
        assert_eq!(open, expected);
        assert!(differs(&open, &closed));
    }

    /// A page that shrinks between renders pulls a scrolled view back.
    #[test]
    fn a_page_that_shrinks_is_not_left_scrolled_past_its_end() {
        let long: Vec<String> = (0..11 + 5).map(|n| n.to_string()).collect();
        let long_refs: Vec<&str> = long.iter().map(String::as_str).collect();
        let mut nav = nav();
        let mut b = Square::new();
        page(&mut b, &mut nav, "", "", &long_refs);
        for _ in 0..5 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 5);
        page(&mut b, &mut nav, "", "", &["only one"]);
        assert_eq!(nav.scroll(), 0);
    }

    /// A modal page has no scrollbar, carries the modal's title, and leaves
    /// the screen's scroll alone.
    #[test]
    fn a_modal_page_is_titled_and_unscrolled() {
        let many: Vec<String> = (0..11 + 5).map(|n| n.to_string()).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut nav = nav();
        let mut b = Square::new();
        page(&mut b, &mut nav, "", "", &refs);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2);
        nav.open(Counter(0));
        page(&mut b, &mut nav, "", "", &refs);
        let layout = Layout::for_canvas(&b);
        let bar = (layout.content_top..layout.content_bottom).any(|y| b.pixel(127, y));
        assert!(!bar, "a modal does not scroll");
        assert_eq!(nav.scroll(), 2, "the modal's page did not clamp it");
        let mut chrome = Square::new();
        title_bar(&mut chrome, "", "Count", "");
        let row = |c: &Square| (0..128).map(|x| c.pixel(x, 4)).collect::<Vec<_>>();
        assert_eq!(row(&b), row(&chrome));
        nav.handle(Input::Back);
        page(&mut b, &mut nav, "", "", &refs);
        assert_eq!(nav.scroll(), 2);
    }

    /// The highlight's brackets sit the same distance from the label on
    /// both sides.
    ///
    /// Found by looking at a golden image rather than by any assertion: the
    /// label was drawn flush against `>` and a full advance away from `<`.
    #[test]
    fn the_highlighted_label_is_bracketed_symmetrically() {
        let screen = &SCREENS[1];
        let selected = 1;
        let mut b = Square::new();
        let layout = Layout::for_canvas(&b);
        menu(&mut b, screen, selected);
        let (top, bottom) = (layout.content_top, layout.content_bottom);
        // The box's left edge is the leftmost lit column; the highlighted row
        // is the first row below the filled heading where the column just
        // inside that edge is lit again.
        let box_x = (0..128)
            .find(|&x| (top..bottom).any(|y| b.pixel(x, y)))
            .expect("a menu box");
        let inside = |y: usize| b.pixel(box_x + 1, y);
        let box_top = (top..bottom).find(|&y| inside(y)).expect("a heading");
        let body = (box_top..bottom)
            .find(|&y| !inside(y))
            .expect("a body below the heading");
        let row_y = (body..bottom)
            .find(|&y| inside(y))
            .expect("an inverted row");
        // In an inverted row, text is dark on light. Walk the columns and
        // record which are entirely lit across the glyph height: those are
        // the gaps.
        let is_gap = |x: usize| (0..font::HEIGHT).all(|dy| b.pixel(x, row_y + 1 + dy));
        let box_right = (0..128)
            .rev()
            .find(|&x| b.pixel(x, row_y))
            .expect("a right edge");
        let dark: Vec<usize> = (box_x + 1..box_right).filter(|&x| !is_gap(x)).collect();
        let (first, last) = (*dark.first().unwrap(), *dark.last().unwrap());
        // `>` is the first dark run, `<` the last; the label is in between.
        let after_open = (first..).find(|&x| is_gap(x)).unwrap();
        let label_start = (after_open..).find(|&x| !is_gap(x)).unwrap();
        let before_close = (0..=last).rev().find(|&x| is_gap(x)).unwrap();
        let label_end = (0..=before_close).rev().find(|&x| !is_gap(x)).unwrap();
        assert_eq!(
            label_start - after_open,
            before_close - label_end,
            "gap after > is {}, gap before < is {}",
            label_start - after_open,
            before_close - label_end
        );
        assert!(
            label_start - after_open > 1,
            "the brackets should stand off"
        );
    }

    /// Nothing panics on a canvas too small for any of it.
    #[test]
    fn a_tiny_canvas_does_not_panic() {
        let mut nav = nav();
        for (w, h) in [(1, 1), (8, 8), (32, 16), (128, 20), (2, 128)] {
            // A canvas of runtime size, to reach the odd ones.
            struct Any {
                w: usize,
                h: usize,
                lit: usize,
            }
            impl Canvas for Any {
                fn width(&self) -> usize {
                    self.w
                }
                fn height(&self) -> usize {
                    self.h
                }
                fn set_pixel(&mut self, _x: usize, _y: usize, on: bool) {
                    if on {
                        self.lit += 1;
                    }
                }
            }
            let mut c = Any { w, h, lit: 0 };
            page(&mut c, &mut nav, "9%", "BT", &["one", "two"]);
            nav.handle(Input::Select);
            page(&mut c, &mut nav, "9%", "BT", &["one", "two"]);
            nav.handle(Input::Back);
            nav.handle(Input::Right);
            nav.handle(Input::Select);
            page(&mut c, &mut nav, "9%", "BT", &["one", "two"]);
            nav.handle(Input::Back);
            nav.handle(Input::Left);
        }
    }
}
