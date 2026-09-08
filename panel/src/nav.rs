//! The navigation model: where the user is, and what a key does there.
//!
//! Everything here is pure. A [`Nav`] takes an [`Input`] and returns what
//! happened; it holds no pins, no clock and no state of the device it is on.
//! That is what makes the interface testable without a board, which matters
//! more here than anywhere else in a firmware: a menu that navigates wrongly
//! is invisible in a log and obvious on a screen nobody is watching.
//!
//! # What the application supplies
//!
//! The screens. A [`Nav`] is built over a slice of [`Screen`] descriptors --
//! a title, an icon, a menu -- and each menu item carries an action of the
//! application's own type `A`. The navigator never interprets an action: it
//! hands one back from [`Nav::handle`] when the user picks it and the caller
//! does the work. So a menu item that reboots a board is, in here, only ever
//! a label and a value.
//!
//! # The navigation model
//!
//! Two levels and one exception, because a small panel with a deep tree on
//! it is a maze:
//!
//! * **Browsing.** Left and right move between screens, which wrap. Up and
//!   down scroll the current screen's content. This is where the device
//!   sits.
//! * **A menu.** Select opens the current screen's action menu over the
//!   content. Up and down move the highlight, select activates it, back
//!   closes it. Left and right are ignored rather than being made to mean
//!   something else, so a stray sideways press cannot change screen out from
//!   under a menu that is open.
//! * **A modal**, the exception: something the application opens over a
//!   screen and takes the keys until it is done -- an editor for one value, a
//!   notice to acknowledge. What it is and what each key does inside it are
//!   the application's, through the [`Modal`] trait; what is here is that it
//!   is opened with [`Nav::open`], that it gets every key until it says it
//!   is finished, and that the screen's scroll is kept under it.
//!
//! Back at the browsing level does nothing at all. There is nowhere above
//! the top, and a back button that silently jumps to the first screen is a
//! way to lose your place by leaning on the board.

/// Width of one icon in the strip.
pub const ICON_W: usize = 7;
/// Height of one icon in the strip.
pub const ICON_H: usize = 7;
/// Width of the cell an icon sits in, including the gap beside it.
pub const ICON_CELL: usize = ICON_W + 4;

/// One icon: column-major, like a glyph. Byte *n* is column *n*, bit *k* row
/// *k*, top first.
pub type Icon = [u8; ICON_W];

/// One screen, as the application describes it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Screen<A: 'static> {
    /// The word in the title bar.
    pub title: &'static str,
    /// The heading over its action menu.
    pub menu_title: &'static str,
    /// Its icon in the strip.
    pub icon: Icon,
    /// What its action menu offers. Should open with [`Item::BACK`]: a menu
    /// that can only be left by a key the user has not found yet is a trap,
    /// and the way out costs one line.
    pub menu: &'static [Item<A>],
}

/// One line in an action menu.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Item<A> {
    /// What it says on the screen.
    pub label: &'static str,
    /// What the caller should do about it, or `None` to close the menu and
    /// do nothing -- which is the navigator's own business and never
    /// handed out.
    pub action: Option<A>,
}

impl<A> Item<A> {
    /// The item every menu opens with.
    pub const BACK: Item<A> = Item {
        label: "Back",
        action: None,
    };

    /// An item that hands `action` to the caller when chosen.
    pub const fn new(label: &'static str, action: A) -> Item<A> {
        Item {
            label,
            action: Some(action),
        }
    }

    /// An item that only closes the menu.
    pub const fn closing(label: &'static str) -> Item<A> {
        Item {
            label,
            action: None,
        }
    }
}

/// Which way the user moved.
///
/// Named for the gesture and not the hardware. A navigation pad with an OK
/// and a back button is six switches for six variants; a rotary encoder with
/// a push is three gestures that a driver has to spread over these. What
/// debounces them is the driver's business.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Input {
    Left,
    Right,
    Up,
    Down,
    Select,
    Back,
}

impl Input {
    /// Every input, for a test or a script that walks them.
    pub const ALL: [Input; 6] = [
        Input::Left,
        Input::Right,
        Input::Up,
        Input::Down,
        Input::Select,
        Input::Back,
    ];

    /// Its name, for a log or a script.
    pub const fn name(self) -> &'static str {
        match self {
            Input::Left => "left",
            Input::Right => "right",
            Input::Up => "up",
            Input::Down => "down",
            Input::Select => "select",
            Input::Back => "back",
        }
    }
}

/// What a key did inside a modal.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome<A> {
    /// The modal stays open, possibly changed.
    Stay,
    /// The modal is finished with nothing to hand back: cancelled, or
    /// acknowledged.
    Close,
    /// The modal is finished and the caller has this to do.
    Emit(A),
}

/// Something the application opens over a screen that takes the keys until
/// it is done.
///
/// The navigator owns the value while it is open, gives it every key, and
/// closes it when it says so. A modal's lines replace the screen's and are
/// never scrolled -- see [`crate::page`] -- so a modal is expected to fit.
pub trait Modal<A> {
    /// The word in the title bar while it is open.
    fn title(&self) -> &'static str;

    /// One key.
    fn handle(&mut self, input: Input) -> Outcome<A>;
}

/// The modal for an application that has none.
///
/// It can never be opened, because there is no value of it to open; the
/// navigator is then exactly the two-level one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NoModal {}

impl<A> Modal<A> for NoModal {
    fn title(&self) -> &'static str {
        match *self {}
    }

    fn handle(&mut self, _input: Input) -> Outcome<A> {
        match *self {}
    }
}

/// What is over the content, if anything.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Overlay<M> {
    None,
    /// An action menu, with the highlighted item.
    Menu(usize),
    /// Something of the application's.
    Modal(M),
}

/// Where the user is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Nav<A: 'static, M = NoModal> {
    screens: &'static [Screen<A>],
    screen: usize,
    overlay: Overlay<M>,
    /// First visible content row, in lines, while browsing.
    scroll: usize,
    /// Lines of content the current screen has, set by the renderer.
    lines: usize,
    /// Lines of content that fit, set by the renderer, which is the only
    /// thing that knows how tall the canvas is.
    visible: usize,
}

impl<A: 'static, M> Nav<A, M> {
    /// On the first screen, at its top, with nothing open.
    ///
    /// `screens` must not be empty: there has to be somewhere to be.
    pub const fn new(screens: &'static [Screen<A>]) -> Self {
        assert!(!screens.is_empty(), "a navigator needs a screen to be on");
        Nav {
            screens,
            screen: 0,
            overlay: Overlay::None,
            scroll: 0,
            lines: 0,
            visible: 0,
        }
    }

    /// The screens, in strip order.
    pub const fn screens(&self) -> &'static [Screen<A>] {
        self.screens
    }

    /// The position of the screen being shown, in the strip.
    pub const fn screen_index(&self) -> usize {
        self.screen
    }

    /// The screen being shown.
    pub fn screen(&self) -> &'static Screen<A> {
        &self.screens[self.screen]
    }

    /// The highlighted item, while a menu is open.
    pub const fn menu_item(&self) -> Option<usize> {
        match self.overlay {
            Overlay::Menu(selected) => Some(selected),
            _ => None,
        }
    }

    /// Whether an action menu is covering the content.
    pub const fn menu_is_open(&self) -> bool {
        matches!(self.overlay, Overlay::Menu(_))
    }

    /// The modal, while one is open.
    pub const fn modal(&self) -> Option<&M> {
        match &self.overlay {
            Overlay::Modal(modal) => Some(modal),
            _ => None,
        }
    }

    /// Whether a modal has replaced the screen's content.
    pub const fn modal_is_open(&self) -> bool {
        matches!(self.overlay, Overlay::Modal(_))
    }

    /// Open `modal` over the current screen, replacing whatever overlay
    /// there was.
    pub fn open(&mut self, modal: M) {
        self.overlay = Overlay::Modal(modal);
    }

    /// The first content line visible.
    pub const fn scroll(&self) -> usize {
        self.scroll
    }

    /// Tell the navigator how tall the current screen's content is, and how
    /// much of it fits.
    ///
    /// Called by whoever renders it, because the renderer is the only thing
    /// that knows either. Clamps the scroll, so a screen that shrinks -- a
    /// list losing an entry -- cannot leave the view stranded past the end
    /// of it.
    pub fn set_content(&mut self, lines: usize, visible: usize) {
        self.lines = lines;
        self.visible = visible;
        let max = lines.saturating_sub(visible);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Movement between and within screens.
    fn browsing(&mut self, input: Input) {
        let count = self.screens.len();
        match input {
            // Wrapping, so a strip of screens is never more than half a lap
            // away in whichever direction is closer.
            Input::Left => self.screen = (self.screen + count - 1) % count,
            Input::Right => self.screen = (self.screen + 1) % count,
            Input::Up => self.scroll = self.scroll.saturating_sub(1),
            Input::Down => {
                let max = self.lines.saturating_sub(self.visible);
                self.scroll = (self.scroll + 1).min(max);
            }
            // A screen with no menu has nothing to open, and a press that
            // opened an empty box would be a press that did nothing visible.
            Input::Select => {
                if !self.screen().menu.is_empty() {
                    self.overlay = Overlay::Menu(0);
                }
            }
            // Nothing above the top level; see the module docs.
            Input::Back => {}
        }
        if matches!(input, Input::Left | Input::Right) {
            // A new screen starts at its top, and its length is unknown until
            // it has been rendered once.
            self.scroll = 0;
            self.lines = 0;
        }
    }
}

impl<A: Copy + 'static, M: Modal<A>> Nav<A, M> {
    /// The word in the title bar: the modal's while one is open, else the
    /// screen's.
    pub fn title(&self) -> &'static str {
        match &self.overlay {
            Overlay::Modal(modal) => modal.title(),
            Overlay::None | Overlay::Menu(_) => self.screen().title,
        }
    }

    /// Feed the navigator one gesture.
    ///
    /// Returns an action only when the user picked one -- from a menu, or
    /// out of a modal. Movement returns `None`, and so does an item that
    /// only closes the menu, which is this module's own business rather
    /// than the caller's.
    pub fn handle(&mut self, input: Input) -> Option<A> {
        match &mut self.overlay {
            Overlay::Menu(selected) => {
                let selected = *selected;
                self.in_menu(selected, input)
            }
            Overlay::Modal(modal) => match modal.handle(input) {
                Outcome::Stay => None,
                Outcome::Close => {
                    self.overlay = Overlay::None;
                    None
                }
                Outcome::Emit(action) => {
                    self.overlay = Overlay::None;
                    Some(action)
                }
            },
            Overlay::None => {
                self.browsing(input);
                None
            }
        }
    }

    /// Movement inside an open action menu.
    fn in_menu(&mut self, selected: usize, input: Input) -> Option<A> {
        let items = self.screen().menu;
        match input {
            Input::Up => {
                self.overlay = Overlay::Menu((selected + items.len() - 1) % items.len());
                None
            }
            Input::Down => {
                self.overlay = Overlay::Menu((selected + 1) % items.len());
                None
            }
            Input::Back => {
                self.overlay = Overlay::None;
                None
            }
            // The menu shuts on the way out either way, so the board is never
            // left showing a menu over the result of what it just did.
            Input::Select => {
                self.overlay = Overlay::None;
                items[selected].action
            }
            // Sideways is ignored while a menu is open; see the module docs.
            Input::Left | Input::Right => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A screen set for the crate's own tests: three screens, one of them
    //! with a long menu and one with none.

    use super::*;

    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub enum Act {
        Beep,
        Boop,
        Set(i32),
        Reboot,
    }

    pub const HOME_MENU: [Item<Act>; 3] = [
        Item::BACK,
        Item::new("Beep", Act::Beep),
        Item::new("Boop", Act::Boop),
    ];

    /// Longer than a 128 × 64 panel can show at once.
    pub const LONG_MENU: [Item<Act>; 9] = [
        Item::BACK,
        Item::new("One", Act::Set(1)),
        Item::new("Two", Act::Set(2)),
        Item::new("Three", Act::Set(3)),
        Item::new("Four", Act::Set(4)),
        Item::new("Five", Act::Set(5)),
        Item::new("Six", Act::Set(6)),
        Item::new("Seven", Act::Set(7)),
        Item::new("Reboot", Act::Reboot),
    ];

    pub const SCREENS: [Screen<Act>; 3] = [
        Screen {
            title: "Home",
            menu_title: "Home Action",
            icon: [0x78, 0x7c, 0x06, 0x77, 0x06, 0x7c, 0x78],
            menu: &HOME_MENU,
        },
        Screen {
            title: "Settings",
            menu_title: "Settings Action",
            icon: [0x1c, 0x3e, 0x77, 0x22, 0x77, 0x3e, 0x1c],
            menu: &LONG_MENU,
        },
        Screen {
            title: "About",
            menu_title: "About Action",
            icon: [0x00, 0x22, 0x14, 0x7f, 0x36, 0x00, 0x00],
            menu: &[],
        },
    ];

    /// A modal that counts presses, closes on back, and emits on select.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct Counter(pub i32);

    impl Modal<Act> for Counter {
        fn title(&self) -> &'static str {
            "Count"
        }

        fn handle(&mut self, input: Input) -> Outcome<Act> {
            match input {
                Input::Up => {
                    self.0 += 1;
                    Outcome::Stay
                }
                Input::Down => {
                    self.0 -= 1;
                    Outcome::Stay
                }
                Input::Left | Input::Right => Outcome::Stay,
                Input::Select => Outcome::Emit(Act::Set(self.0)),
                Input::Back => Outcome::Close,
            }
        }
    }

    pub fn nav() -> Nav<Act, Counter> {
        Nav::new(&SCREENS)
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn every_input_has_its_own_name() {
        for (i, a) in Input::ALL.iter().enumerate() {
            assert_eq!(a.name(), a.name().to_lowercase());
            for b in &Input::ALL[i + 1..] {
                assert_ne!(a.name(), b.name());
            }
        }
    }

    /// Walk the whole strip and come back to where you started.
    #[test]
    fn right_wraps_all_the_way_round() {
        let mut nav = nav();
        assert_eq!(nav.screen_index(), 0);
        assert_eq!(nav.screen().title, "Home");
        for _ in 0..SCREENS.len() {
            nav.handle(Input::Right);
        }
        assert_eq!(nav.screen_index(), 0);
    }

    /// Left from the first screen lands on the last, rather than sticking.
    #[test]
    fn left_from_the_first_screen_wraps_to_the_last() {
        let mut nav = nav();
        nav.handle(Input::Left);
        assert_eq!(nav.screen_index(), SCREENS.len() - 1);
        assert_eq!(nav.screen().title, "About");
    }

    /// Every screen is reachable by walking, and each one only once per lap.
    #[test]
    fn one_lap_visits_every_screen_exactly_once() {
        let mut nav = nav();
        let mut seen = Vec::new();
        for _ in 0..SCREENS.len() {
            seen.push(nav.screen_index());
            nav.handle(Input::Right);
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), SCREENS.len(), "{seen:?}");
    }

    /// Select opens the menu on the item that closes it again.
    ///
    /// Opening on `Back` rather than on the first real action is what makes a
    /// double press harmless: OK, OK is a no-op, not a reboot.
    #[test]
    fn a_menu_opens_on_back() {
        let mut nav = nav();
        assert!(!nav.menu_is_open());
        assert_eq!(nav.handle(Input::Select), None);
        assert!(nav.menu_is_open());
        assert_eq!(nav.menu_item(), Some(0));
        assert_eq!(nav.screen().menu[0], Item::BACK);
        assert_eq!(nav.handle(Input::Select), None, "Back is not an action");
        assert!(!nav.menu_is_open());
    }

    /// Choosing an action shuts the menu and hands it over exactly once.
    #[test]
    fn choosing_an_action_returns_it_and_closes_the_menu() {
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.handle(Input::Select);
        nav.handle(Input::Up); // off Back, onto the last item
        assert_eq!(nav.handle(Input::Select), Some(Act::Reboot));
        assert!(!nav.menu_is_open());
        // And it is not delivered a second time.
        assert_eq!(nav.handle(Input::Up), None);
    }

    /// Back closes an open menu without choosing anything.
    #[test]
    fn back_closes_a_menu_without_acting() {
        let mut nav = nav();
        nav.handle(Input::Select);
        nav.handle(Input::Down);
        assert!(nav.menu_is_open());
        assert_eq!(nav.handle(Input::Back), None);
        assert!(!nav.menu_is_open());
    }

    /// Back at the top level does nothing, rather than jumping home.
    #[test]
    fn back_while_browsing_does_not_move() {
        let mut nav = nav();
        nav.handle(Input::Right);
        let before = nav;
        assert_eq!(nav.handle(Input::Back), None);
        assert_eq!(nav, before);
    }

    /// Sideways cannot change screen out from under an open menu.
    #[test]
    fn sideways_is_ignored_while_a_menu_is_open() {
        let mut nav = nav();
        nav.handle(Input::Select);
        assert_eq!(nav.handle(Input::Left), None);
        assert_eq!(nav.handle(Input::Right), None);
        assert_eq!(nav.screen_index(), 0);
        assert!(nav.menu_is_open());
    }

    /// The highlight wraps within a menu, both ways.
    #[test]
    fn the_menu_highlight_wraps() {
        let mut nav = nav();
        nav.handle(Input::Right);
        let count = nav.screen().menu.len();
        nav.handle(Input::Select);
        nav.handle(Input::Up);
        assert_eq!(nav.menu_item(), Some(count - 1), "up from the top");
        nav.handle(Input::Down);
        assert_eq!(nav.menu_item(), Some(0), "down from the bottom");
    }

    /// A screen with no menu cannot be left showing an empty one.
    #[test]
    fn a_screen_with_no_menu_opens_nothing() {
        let mut nav = nav();
        nav.handle(Input::Left); // About
        assert!(nav.screen().menu.is_empty());
        let before = nav;
        assert_eq!(nav.handle(Input::Select), None);
        assert!(!nav.menu_is_open());
        assert_eq!(nav, before, "the press did nothing at all");
    }

    /// Scrolling stops at both ends rather than running off.
    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut nav = nav();
        nav.set_content(11 + 3, 11);
        for _ in 0..20 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 3, "cannot scroll past the last line");
        for _ in 0..20 {
            nav.handle(Input::Up);
        }
        assert_eq!(nav.scroll(), 0, "cannot scroll above the first");
    }

    /// A screen that fits does not scroll at all, and neither does one
    /// that has not been rendered yet.
    #[test]
    fn a_screen_that_fits_does_not_scroll() {
        let mut nav = nav();
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 0, "unrendered");
        nav.set_content(2, 11);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 0);
    }

    /// Content that shrinks under a scrolled view pulls the view back.
    #[test]
    fn a_shrinking_screen_does_not_stay_scrolled_past_its_end() {
        let mut nav = nav();
        nav.set_content(11 + 5, 11);
        for _ in 0..5 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 5);
        nav.set_content(11 + 1, 11);
        assert_eq!(nav.scroll(), 1, "clamped to what is left");
        // And a canvas that shows less pulls it too: the same content on a
        // panel with fewer lines cannot be scrolled further than it can
        // hold, and is clamped the other way when it can hold more.
        nav.set_content(11 + 1, 4);
        assert_eq!(nav.scroll(), 1);
        nav.set_content(11 + 1, 12);
        assert_eq!(nav.scroll(), 0);
    }

    /// Changing screen starts at the top of the new one.
    #[test]
    fn a_new_screen_starts_at_its_top() {
        let mut nav = nav();
        nav.set_content(11 + 5, 11);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2);
        nav.handle(Input::Right);
        assert_eq!(nav.scroll(), 0);
    }

    // ---- the modal level ----------------------------------------------

    /// A modal is opened by the caller, not by a menu, and carries its own
    /// title.
    #[test]
    fn a_modal_is_opened_by_the_caller_and_titled() {
        let mut nav = nav();
        assert_eq!(nav.title(), "Home");
        assert!(!nav.modal_is_open());
        nav.open(Counter(3));
        assert!(nav.modal_is_open());
        assert_eq!(nav.modal(), Some(&Counter(3)));
        assert_eq!(nav.title(), "Count");
        assert!(!nav.menu_is_open());
    }

    /// Every key goes to the modal while it is open; a `Stay` keeps it and
    /// keeps its changed value.
    #[test]
    fn keys_go_to_the_modal_and_stay_keeps_it() {
        let mut nav = nav();
        nav.open(Counter(0));
        assert_eq!(nav.handle(Input::Up), None);
        assert_eq!(nav.handle(Input::Up), None);
        assert_eq!(nav.handle(Input::Down), None);
        assert_eq!(nav.modal(), Some(&Counter(1)));
        // Sideways inside a modal does not change screen.
        nav.handle(Input::Left);
        nav.handle(Input::Right);
        assert_eq!(nav.screen_index(), 0);
        assert!(nav.modal_is_open());
    }

    /// `Emit` hands the value back once and closes.
    #[test]
    fn emit_returns_once_and_closes() {
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.open(Counter(4));
        assert_eq!(nav.handle(Input::Select), Some(Act::Set(4)));
        assert!(!nav.modal_is_open());
        assert_eq!(nav.modal(), None);
        assert_eq!(nav.title(), "Settings", "back where it was opened");
        assert_eq!(nav.handle(Input::Up), None, "not delivered twice");
    }

    /// `Close` hands back nothing, which is the whole of what cancel does.
    #[test]
    fn close_returns_nothing_and_closes() {
        let mut nav = nav();
        nav.open(Counter(4));
        nav.handle(Input::Up);
        assert_eq!(nav.handle(Input::Back), None);
        assert!(!nav.modal_is_open());
        assert_eq!(nav.screen_index(), 0);
    }

    /// Opening a modal over an open menu replaces the menu, and opening one
    /// over another replaces that.
    #[test]
    fn opening_replaces_whatever_overlay_there_was() {
        let mut nav = nav();
        nav.handle(Input::Select);
        assert!(nav.menu_is_open());
        nav.open(Counter(1));
        assert!(!nav.menu_is_open());
        nav.open(Counter(2));
        assert_eq!(nav.modal(), Some(&Counter(2)));
    }

    /// The screen's scroll survives a modal being opened over it.
    #[test]
    fn a_modal_keeps_the_screens_scroll() {
        let mut nav = nav();
        nav.set_content(11 + 5, 11);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2);
        nav.open(Counter(0));
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2, "down went to the modal");
        nav.handle(Input::Back);
        assert_eq!(nav.scroll(), 2);
    }

    /// A navigator with no modal type is the two-level one, and still
    /// carries a title.
    #[test]
    fn a_navigator_without_a_modal_still_navigates() {
        let mut nav: Nav<Act> = Nav::new(&SCREENS);
        assert_eq!(nav.title(), "Home");
        nav.handle(Input::Right);
        nav.handle(Input::Select);
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(Act::Set(1)));
        assert_eq!(nav.modal(), None);
        assert!(!nav.modal_is_open());
    }

    /// Items say what they are.
    #[test]
    fn items_carry_their_action_or_none() {
        let back: Item<Act> = Item::BACK;
        assert_eq!(back.label, "Back");
        assert_eq!(back.action, None);
        assert_eq!(Item::<Act>::closing("Done").action, None);
        assert_eq!(Item::new("Beep", Act::Beep).action, Some(Act::Beep));
    }
}
