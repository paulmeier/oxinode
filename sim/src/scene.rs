//! A navigator and the state a page borrows, rendered the way the board will.
//!
//! [`ui::page`](oxinode_core::ui::page) takes the navigator and everything it
//! draws by reference, because on the board the caller owns the scratch
//! buffers. Here the caller is this struct, and it owns `String`s instead.

use oxinode_core::sh1107::Frame;
use oxinode_core::ui::{self, Action, Input, Nav};

/// Where the user is, plus what the page shows around them.
#[derive(Clone, Debug)]
pub struct Scene {
    pub nav: Nav,
    /// The title bar's left corner: the battery, on the board.
    pub left: String,
    /// The title bar's right corner: the clock.
    pub right: String,
    /// The current screen's content, one entry per line.
    pub lines: Vec<String>,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    /// Home, at the top, with the corner readings phase 9's tests use.
    pub fn new() -> Self {
        Scene {
            nav: Nav::new(),
            left: "99%".to_string(),
            right: "12:45p".to_string(),
            lines: Vec::new(),
        }
    }

    /// Sample content, long enough to scroll.
    ///
    /// The screens do not draw their own content yet -- that is the next
    /// phase's job, and it needs the modem's state -- so a golden image of a
    /// scrolled page has to be of *something*. Numbered lines make the scroll
    /// position legible in the picture, which real content would not.
    pub fn with_sample_lines(mut self, count: usize) -> Self {
        self.lines = (1..=count).map(|n| format!("Sample line {n}")).collect();
        self
    }

    /// Render the page as it stands.
    ///
    /// Takes `&mut self` because the page tells the navigator how tall the
    /// content is; see [`ui::page`].
    pub fn frame(&mut self) -> Frame {
        let lines: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        let mut frame = Frame::new();
        ui::page(&mut frame, &mut self.nav, &self.left, &self.right, &lines);
        frame
    }

    /// Press one key. Returns what the board would now have to do, if
    /// anything.
    ///
    /// The page is rendered first, as it would have been on the board before
    /// the key was pressed, so that a `Down` on a fresh screen knows how far
    /// it can go.
    pub fn press(&mut self, input: Input) -> Option<Action> {
        self.frame();
        self.nav.handle(input)
    }

    /// Press every key in a script, in order, returning the actions chosen.
    pub fn run(&mut self, inputs: &[Input]) -> Vec<Action> {
        inputs
            .iter()
            .filter_map(|&input| self.press(input))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script;
    use oxinode_core::ui::Screen;

    #[test]
    fn a_scene_starts_at_home_with_the_menu_shut() {
        let scene = Scene::new();
        assert_eq!(scene.nav.screen(), Screen::Home);
        assert!(!scene.nav.menu_is_open());
    }

    #[test]
    fn a_script_walks_the_menus_and_collects_the_actions() {
        let mut scene = Scene::new();
        let inputs = script::parse("right*4 select down select").unwrap();
        assert_eq!(scene.run(&inputs), [Action::Reboot]);
        assert_eq!(scene.nav.screen(), Screen::System);
        assert!(!scene.nav.menu_is_open());
    }

    /// A press renders first, so scrolling on a fresh screen works.
    #[test]
    fn scrolling_works_from_the_first_press() {
        let mut scene = Scene::new().with_sample_lines(ui::visible_lines() + 3);
        scene.press(Input::Down);
        assert_eq!(scene.nav.scroll(), 1);
    }

    /// The frame is the page the core would draw for the same state.
    #[test]
    fn the_frame_is_the_core_page() {
        let mut scene = Scene::new().with_sample_lines(2);
        scene.press(Input::Right);
        let got = scene.frame();
        let mut nav = Nav::new();
        nav.handle(Input::Right);
        let mut want = Frame::new();
        ui::page(
            &mut want,
            &mut nav,
            "99%",
            "12:45p",
            &["Sample line 1", "Sample line 2"],
        );
        assert_eq!(got.as_bytes(), want.as_bytes());
    }
}
