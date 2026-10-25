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
//! Two levels and two exceptions, because a small panel with a deep tree on
//! it is a maze:
//!
//! * **Browsing.** Left and right move between screens, which wrap. Up and
//!   down scroll the current screen's content. This is where the device
//!   sits.
//!
//!   A screen can instead be a **list** ([`Screen::open`]): each of its
//!   lines is one entry, up and down move a highlight over them (the view
//!   follows it), and select hands back the screen's `open` action for the
//!   caller to act on the highlighted entry, [`Nav::selected`]. A list
//!   with no entries yet is an ordinary screen until it has some. A list has
//!   no menu, because select is taken.
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
//!   is finished, and that the screen's scroll is kept under it. A modal
//!   does not scroll: what it says has to fit.
//! * **A detail**, the other exception: something the application opens
//!   over a screen, usually from a list, that is browsed like a screen and
//!   is longer than one -- a conversation, a log. It has its own title, its
//!   own scroll and its own menu, which select opens; up and down scroll it
//!   by line, left and right move it by whatever the application says its
//!   parts are ([`Detail::step`]), and back closes it, leaving the screen
//!   under it where it was: the same scroll, the same highlight. The strip
//!   still marks the screen it was opened from. Opened with
//!   [`Nav::open_detail`]; a menu or a modal can be opened over it.
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
    /// `Some` makes the screen a list: select hands this back for the
    /// highlighted entry, [`Nav::selected`], rather than opening a menu, so
    /// a list's menu must be empty. `None` for an ordinary screen.
    pub open: Option<A>,
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
    /// The caller has this to do, and the modal stays open until it has:
    /// what to do is in the modal ([`Nav::modal`]), and the caller closes
    /// it ([`Nav::close_modal`]) or tells it why not ([`Nav::modal_mut`]).
    Keep(A),
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

/// Something the application opens over a screen that is browsed like one:
/// see the module docs.
///
/// The navigator owns the value while it is open and keeps its scroll; the
/// application draws its lines, as it does a screen's. The value can be
/// changed while it is open ([`Nav::detail_mut`]), which is how one learns
/// where its parts begin once its lines have been laid out.
pub trait Detail<A: 'static> {
    /// The word in the title bar while it is open.
    fn title(&self) -> &'static str;

    /// The heading over its menu.
    fn menu_title(&self) -> &'static str;

    /// What its menu offers, [`Item::BACK`] first; empty for none, and then
    /// select does nothing.
    fn menu(&self) -> &'static [Item<A>];

    /// The line left or right moves the view to, from `top`, the first line
    /// in view: the start of the part before it or the next one, by the
    /// detail's own idea of a part. The navigator clamps what comes back, so
    /// "the end" can be said as `usize::MAX`.
    fn step(&self, top: usize, forward: bool) -> usize;
}

/// The detail for an application that has none. Like [`NoModal`], it can
/// never be opened.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NoDetail {}

impl<A: 'static> Detail<A> for NoDetail {
    fn title(&self) -> &'static str {
        match *self {}
    }

    fn menu_title(&self) -> &'static str {
        match *self {}
    }

    fn menu(&self) -> &'static [Item<A>] {
        match *self {}
    }

    fn step(&self, _top: usize, _forward: bool) -> usize {
        match *self {}
    }
}

/// What is over the content, if anything.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Overlay<M> {
    None,
    /// An action menu, with the highlighted item: the detail's while one is
    /// open, else the screen's.
    Menu(usize),
    /// Something of the application's.
    Modal(M),
}

/// Where the view of a screen is: what a detail keeps of the screen under it.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
struct Place {
    /// First visible content row, in lines.
    scroll: usize,
    /// Lines of content, set by the renderer.
    lines: usize,
    /// The highlighted entry, on a list.
    selected: usize,
    /// Entries the list has, set by the renderer.
    entries: usize,
}

/// A detail, and the place of the screen under it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Opened<D> {
    detail: D,
    under: Place,
}

/// Where the user is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Nav<A: 'static, M = NoModal, D = NoDetail> {
    screens: &'static [Screen<A>],
    screen: usize,
    overlay: Overlay<M>,
    /// Where the view is, on the screen or on the detail over it.
    place: Place,
    /// Lines of content that fit, set by the renderer, which is the only
    /// thing that knows how tall the canvas is.
    visible: usize,
    detail: Option<Opened<D>>,
}

impl<A: 'static, M, D> Nav<A, M, D> {
    /// On the first screen, at its top, with nothing open.
    ///
    /// `screens` must not be empty: there has to be somewhere to be. A list
    /// must have no menu, since select opens its entries.
    pub const fn new(screens: &'static [Screen<A>]) -> Self {
        assert!(!screens.is_empty(), "a navigator needs a screen to be on");
        let mut n = 0;
        while n < screens.len() {
            assert!(
                screens[n].open.is_none() || screens[n].menu.is_empty(),
                "a list's select opens its entries, so it can have no menu"
            );
            n += 1;
        }
        Nav {
            screens,
            screen: 0,
            overlay: Overlay::None,
            place: Place {
                scroll: 0,
                lines: 0,
                selected: 0,
                entries: 0,
            },
            visible: 0,
            detail: None,
        }
    }

    /// The screens, in strip order.
    pub const fn screens(&self) -> &'static [Screen<A>] {
        self.screens
    }

    /// The position of the screen being shown, in the strip. While a detail
    /// is open, the screen it was opened from.
    pub const fn screen_index(&self) -> usize {
        self.screen
    }

    /// The screen being shown, or the one under the detail.
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

    /// The modal, to change it while it is open.
    pub fn modal_mut(&mut self) -> Option<&mut M> {
        match &mut self.overlay {
            Overlay::Modal(modal) => Some(modal),
            _ => None,
        }
    }

    /// Close the modal, if one is open: the caller's answer to
    /// [`Outcome::Keep`] once it has done what the modal asked.
    pub fn close_modal(&mut self) {
        if self.modal_is_open() {
            self.overlay = Overlay::None;
        }
    }

    /// Open `modal` over the current screen, replacing whatever overlay
    /// there was.
    pub fn open(&mut self, modal: M) {
        self.overlay = Overlay::Modal(modal);
    }

    /// The detail, while one is open.
    pub fn detail(&self) -> Option<&D> {
        self.detail.as_ref().map(|o| &o.detail)
    }

    /// The detail, to change while it is open.
    pub fn detail_mut(&mut self) -> Option<&mut D> {
        self.detail.as_mut().map(|o| &mut o.detail)
    }

    /// Whether a detail is open.
    pub const fn detail_is_open(&self) -> bool {
        self.detail.is_some()
    }

    /// Open `detail` over the current screen, at its top, with nothing over
    /// it. The screen's scroll and highlight are kept for when it closes;
    /// opening one over another replaces it and keeps the screen's.
    pub fn open_detail(&mut self, detail: D) {
        let under = match self.detail.take() {
            Some(open) => open.under,
            None => self.place,
        };
        self.detail = Some(Opened { detail, under });
        self.place = Place::default();
        self.overlay = Overlay::None;
    }

    /// Close the detail, if one is open, and whatever is over it: the
    /// screen under it is where it was.
    pub fn close_detail(&mut self) {
        if let Some(open) = self.detail.take() {
            self.place = open.under;
            self.overlay = Overlay::None;
        }
    }

    /// The first content line visible.
    pub const fn scroll(&self) -> usize {
        self.place.scroll
    }

    /// Move the view so `line` is at its top, as far as the content goes.
    /// For an application placing a detail it has just laid out: at the
    /// first thing not yet seen, say.
    pub fn set_scroll(&mut self, line: usize) {
        self.place.scroll = line.min(self.max_scroll());
    }

    /// The highlighted entry, on a list that has entries and with no detail
    /// over it.
    pub fn selected(&self) -> Option<usize> {
        (self.is_list() && self.place.entries > 0).then_some(self.place.selected)
    }

    /// Whether what is being browsed is a list: a list screen, with no
    /// detail over it.
    fn is_list(&self) -> bool {
        self.detail.is_none() && self.screen().open.is_some()
    }

    fn max_scroll(&self) -> usize {
        self.place.lines.saturating_sub(self.visible)
    }

    /// Tell the navigator how tall the current screen's content is, and how
    /// much of it fits.
    ///
    /// Called by whoever renders it, because the renderer is the only thing
    /// that knows either. Clamps the scroll, so a screen that shrinks -- a
    /// list losing an entry -- cannot leave the view stranded past the end
    /// of it.
    pub fn set_content(&mut self, lines: usize, visible: usize) {
        self.place.lines = lines;
        self.visible = visible;
        let max = self.max_scroll();
        if self.place.scroll > max {
            self.place.scroll = max;
        }
    }

    /// Tell the navigator how many of a list's lines are entries: the first
    /// `entries`, one a line. Called by the renderer after
    /// [`Nav::set_content`]. Clamps the highlight to the list as it now is
    /// and brings it into view. Zero, for a list with nothing in it yet,
    /// makes the screen an ordinary one until there is.
    /// On anything but a list it does nothing: there is no highlight to
    /// keep.
    pub fn set_entries(&mut self, entries: usize) {
        if !self.is_list() {
            return;
        }
        self.place.entries = entries;
        self.place.selected = self.place.selected.min(entries.saturating_sub(1));
        if entries > 0 {
            self.follow();
        }
    }

    /// Scroll just far enough that the highlight is in view.
    fn follow(&mut self) {
        let selected = self.place.selected;
        if selected < self.place.scroll {
            self.place.scroll = selected;
        } else if self.visible > 0 && selected >= self.place.scroll + self.visible {
            self.place.scroll = selected + 1 - self.visible;
        }
        self.place.scroll = self.place.scroll.min(self.max_scroll());
    }
}

impl<A: Copy + 'static, M: Modal<A>, D: Detail<A>> Nav<A, M, D> {
    /// The word in the title bar: the modal's while one is open, else the
    /// detail's, else the screen's.
    pub fn title(&self) -> &'static str {
        match (&self.overlay, self.detail()) {
            (Overlay::Modal(modal), _) => modal.title(),
            (_, Some(detail)) => detail.title(),
            _ => self.screen().title,
        }
    }

    /// The heading and items of the menu select opens: the detail's while
    /// one is open, else the screen's.
    pub fn menu(&self) -> (&'static str, &'static [Item<A>]) {
        match self.detail() {
            Some(detail) => (detail.menu_title(), detail.menu()),
            None => (self.screen().menu_title, self.screen().menu),
        }
    }

    /// Feed the navigator one gesture.
    ///
    /// Returns an action only when the user picked one -- from a menu, out
    /// of a modal, or an entry of a list. Movement returns `None`, and so
    /// does an item that only closes the menu, which is this module's own
    /// business rather than the caller's.
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
                Outcome::Keep(action) => Some(action),
            },
            Overlay::None => self.browsing(input),
        }
    }

    /// Movement between and within screens, and within a detail.
    fn browsing(&mut self, input: Input) -> Option<A> {
        if let Some(open) = &self.detail {
            let top = self.place.scroll;
            match input {
                Input::Left | Input::Right => {
                    let to = open.detail.step(top, input == Input::Right);
                    self.place.scroll = to.min(self.max_scroll());
                }
                Input::Up => self.place.scroll = top.saturating_sub(1),
                Input::Down => self.place.scroll = (top + 1).min(self.max_scroll()),
                Input::Select => {
                    if !open.detail.menu().is_empty() {
                        self.overlay = Overlay::Menu(0);
                    }
                }
                Input::Back => self.close_detail(),
            }
            return None;
        }
        let list = self.is_list() && self.place.entries > 0;
        let count = self.screens.len();
        match input {
            // Wrapping, so a strip of screens is never more than half a lap
            // away in whichever direction is closer.
            Input::Left => self.screen = (self.screen + count - 1) % count,
            Input::Right => self.screen = (self.screen + 1) % count,
            // A list moves its highlight, which stops at the ends rather
            // than wrapping: the top of a list is a place, as the top of a
            // page is.
            Input::Up if list => {
                self.place.selected = self.place.selected.saturating_sub(1);
                self.follow();
            }
            Input::Down if list => {
                self.place.selected = (self.place.selected + 1).min(self.place.entries - 1);
                self.follow();
            }
            Input::Up => self.place.scroll = self.place.scroll.saturating_sub(1),
            Input::Down => self.place.scroll = (self.place.scroll + 1).min(self.max_scroll()),
            Input::Select if list => return self.screen().open,
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
            self.place = Place::default();
        }
        None
    }

    /// Movement inside an open action menu.
    fn in_menu(&mut self, selected: usize, input: Input) -> Option<A> {
        let (_, items) = self.menu();
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
        /// Open the highlighted letter of the inbox.
        Read,
        Reply,
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

    pub const SCREENS: [Screen<Act>; 4] = [
        Screen {
            title: "Home",
            menu_title: "Home Action",
            icon: [0x78, 0x7c, 0x06, 0x77, 0x06, 0x7c, 0x78],
            menu: &HOME_MENU,
            open: None,
        },
        Screen {
            title: "Settings",
            menu_title: "Settings Action",
            icon: [0x1c, 0x3e, 0x77, 0x22, 0x77, 0x3e, 0x1c],
            menu: &LONG_MENU,
            open: None,
        },
        Screen {
            title: "Inbox",
            menu_title: "",
            icon: [0x7f, 0x43, 0x45, 0x49, 0x45, 0x43, 0x7f],
            menu: &[],
            open: Some(Act::Read),
        },
        Screen {
            title: "About",
            menu_title: "About Action",
            icon: [0x00, 0x22, 0x14, 0x7f, 0x36, 0x00, 0x00],
            menu: &[],
            open: None,
        },
    ];

    /// Where the inbox is in the strip.
    pub const INBOX: usize = 2;

    /// A letter: a detail whose parts are paragraphs of five lines.
    #[derive(Copy, Clone, PartialEq, Eq, Debug)]
    pub struct Letter(pub usize);

    pub const LETTER_MENU: [Item<Act>; 2] = [Item::BACK, Item::new("Reply", Act::Reply)];

    impl Detail<Act> for Letter {
        fn title(&self) -> &'static str {
            "Letter"
        }

        fn menu_title(&self) -> &'static str {
            "Letter Action"
        }

        fn menu(&self) -> &'static [Item<Act>] {
            &LETTER_MENU
        }

        fn step(&self, top: usize, forward: bool) -> usize {
            if forward {
                (top / 5 + 1) * 5
            } else if top % 5 == 0 {
                top.saturating_sub(5)
            } else {
                top / 5 * 5
            }
        }
    }

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

    pub fn nav() -> Nav<Act, Counter, Letter> {
        Nav::new(&SCREENS)
    }

    /// A navigator on the inbox, rendered once with `entries` letters on a
    /// panel of eleven lines.
    pub fn on_inbox(entries: usize) -> Nav<Act, Counter, Letter> {
        let mut nav = nav();
        for _ in 0..INBOX {
            nav.handle(Input::Right);
        }
        nav.set_content(entries, 11);
        nav.set_entries(entries);
        nav
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

    // ---- the list ------------------------------------------------------

    /// Up and down move the highlight over a list's entries, stopping at
    /// both ends, and the view follows it.
    #[test]
    fn a_list_moves_its_highlight_and_the_view_follows() {
        let mut nav = on_inbox(20);
        assert_eq!(nav.selected(), Some(0));
        nav.handle(Input::Up);
        assert_eq!(nav.selected(), Some(0), "stops at the top");
        for _ in 0..10 {
            nav.handle(Input::Down);
        }
        assert_eq!((nav.selected(), nav.scroll()), (Some(10), 0), "in view");
        nav.handle(Input::Down);
        assert_eq!((nav.selected(), nav.scroll()), (Some(11), 1), "followed");
        for _ in 0..30 {
            nav.handle(Input::Down);
        }
        assert_eq!((nav.selected(), nav.scroll()), (Some(19), 9), "stops");
        for _ in 0..12 {
            nav.handle(Input::Up);
        }
        assert_eq!((nav.selected(), nav.scroll()), (Some(7), 7), "back up");
    }

    /// Select on a list hands back its `open` action, once, and opens no
    /// menu: which entry is the navigator's to say.
    #[test]
    fn select_on_a_list_hands_back_open() {
        let mut nav = on_inbox(3);
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(Act::Read));
        assert_eq!(nav.selected(), Some(1));
        assert!(!nav.menu_is_open());
        assert_eq!(nav.handle(Input::Back), None, "nothing to close");
        assert_eq!(nav.selected(), Some(1));
    }

    /// A list with nothing in it is an ordinary screen with no menu: select
    /// does nothing, and nothing is highlighted.
    #[test]
    fn an_empty_list_has_nothing_to_open() {
        let mut nav = on_inbox(0);
        assert_eq!(nav.selected(), None);
        let before = nav;
        assert_eq!(nav.handle(Input::Select), None);
        assert_eq!(nav, before);
        // Entries arriving make it a list.
        nav.set_content(1, 11);
        nav.set_entries(1);
        assert_eq!(nav.handle(Input::Select), Some(Act::Read));
    }

    /// A list that loses entries pulls its highlight back onto what is left,
    /// and keeps it in view.
    #[test]
    fn a_shrinking_list_keeps_its_highlight_on_it() {
        let mut nav = on_inbox(20);
        for _ in 0..19 {
            nav.handle(Input::Down);
        }
        nav.set_content(4, 11);
        nav.set_entries(4);
        assert_eq!((nav.selected(), nav.scroll()), (Some(3), 0));
    }

    /// Leaving a list and coming back starts it at its top, as any screen.
    #[test]
    fn a_list_left_and_returned_to_starts_at_the_top() {
        let mut nav = on_inbox(5);
        nav.handle(Input::Down);
        nav.handle(Input::Right);
        assert_eq!(nav.selected(), None, "not a list");
        nav.handle(Input::Left);
        nav.set_content(5, 11);
        nav.set_entries(5);
        assert_eq!(nav.selected(), Some(0));
    }

    /// Only a list has a highlight; an ordinary screen ignores being told
    /// about entries, and keeps its scroll.
    #[test]
    fn an_ordinary_screen_has_no_highlight() {
        let mut nav = nav();
        nav.set_content(20, 11);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        nav.set_entries(20);
        assert_eq!(nav.selected(), None);
        assert_eq!(nav.scroll(), 2);
    }

    /// A list cannot have a menu: select is taken.
    #[test]
    #[should_panic(expected = "no menu")]
    fn a_list_with_a_menu_is_refused() {
        static BAD: [Screen<Act>; 1] = [Screen {
            title: "Bad",
            menu_title: "Bad Action",
            icon: [0; ICON_W],
            menu: &HOME_MENU,
            open: Some(Act::Read),
        }];
        let _ = Nav::<Act>::new(&BAD);
    }

    // ---- the detail ----------------------------------------------------

    /// The inbox's thirteenth letter open as a detail, the inbox under it
    /// scrolled to keep the highlight in view.
    fn reading() -> Nav<Act, Counter, Letter> {
        let mut nav = on_inbox(20);
        for _ in 0..12 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.handle(Input::Select), Some(Act::Read));
        nav.open_detail(Letter(12));
        nav.set_content(23, 11);
        nav
    }

    /// A detail has its own title and scroll, keeps the strip on the screen
    /// it came from, and back puts the screen under it exactly where it was.
    #[test]
    fn a_detail_keeps_the_screen_under_it() {
        let mut nav = reading();
        assert!(nav.detail_is_open());
        assert_eq!(nav.detail(), Some(&Letter(12)));
        assert_eq!(nav.title(), "Letter");
        assert_eq!(nav.screen_index(), INBOX);
        assert_eq!(nav.selected(), None, "no highlight in a detail");
        assert_eq!(nav.scroll(), 0, "opened at its top");
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2);
        for _ in 0..20 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 12, "stops at the end");
        assert_eq!(nav.handle(Input::Back), None);
        assert!(!nav.detail_is_open());
        assert_eq!(nav.title(), "Inbox");
        assert_eq!((nav.selected(), nav.scroll()), (Some(12), 2));
        assert_eq!(nav.handle(Input::Back), None, "and back stops there");
        assert_eq!(nav.screen_index(), INBOX);
    }

    /// Sideways in a detail moves it by part, as the detail says, clamped to
    /// the content; it never changes screen.
    #[test]
    fn sideways_in_a_detail_moves_by_part() {
        let mut nav = reading();
        let mut tops = Vec::new();
        for _ in 0..4 {
            nav.handle(Input::Right);
            tops.push(nav.scroll());
        }
        assert_eq!(tops, [5, 10, 12, 12], "the last part is clamped");
        nav.handle(Input::Left);
        assert_eq!(nav.scroll(), 10);
        nav.handle(Input::Up);
        nav.handle(Input::Left);
        assert_eq!(nav.scroll(), 5, "from inside a part, to its start");
        for _ in 0..3 {
            nav.handle(Input::Left);
        }
        assert_eq!(nav.scroll(), 0);
        assert_eq!(nav.screen_index(), INBOX);
    }

    /// Select opens the detail's own menu, whose action comes back once;
    /// back from the menu leaves the detail open.
    #[test]
    fn a_detail_has_a_menu_of_its_own() {
        let mut nav = reading();
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), None);
        assert_eq!(nav.menu(), ("Letter Action", &LETTER_MENU[..]));
        assert_eq!(nav.title(), "Letter");
        nav.handle(Input::Left);
        assert_eq!(nav.menu_item(), Some(0), "sideways ignored in the menu");
        assert_eq!(nav.scroll(), 1);
        nav.handle(Input::Back);
        assert!(nav.detail_is_open() && !nav.menu_is_open());
        nav.handle(Input::Select);
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(Act::Reply));
        assert!(nav.detail_is_open(), "still reading");
        assert_eq!(nav.scroll(), 1);
    }

    /// A modal opened over a detail takes the keys and closes back onto it.
    #[test]
    fn a_modal_over_a_detail_closes_back_onto_it() {
        let mut nav = reading();
        nav.handle(Input::Down);
        nav.open(Counter(0));
        assert_eq!(nav.title(), "Count");
        nav.handle(Input::Down);
        nav.handle(Input::Right);
        assert_eq!(nav.scroll(), 1, "the keys went to the modal");
        nav.handle(Input::Back);
        assert!(nav.detail_is_open());
        assert_eq!(nav.title(), "Letter");
    }

    /// A detail opened over a detail replaces it, and the screen under both
    /// is still the one kept; closing shuts whatever was over it too.
    #[test]
    fn a_detail_over_a_detail_keeps_the_screen() {
        let mut nav = reading();
        nav.handle(Input::Down);
        nav.open_detail(Letter(3));
        assert_eq!(nav.detail(), Some(&Letter(3)));
        assert_eq!(nav.scroll(), 0);
        nav.set_content(23, 11);
        nav.handle(Input::Select);
        assert!(nav.menu_is_open());
        nav.close_detail();
        assert!(!nav.menu_is_open());
        assert_eq!((nav.selected(), nav.scroll()), (Some(12), 2));
        nav.close_detail();
        assert_eq!(nav.selected(), Some(12), "closing nothing changes nothing");
    }

    /// The application can place a detail's view, as far as it goes.
    #[test]
    fn set_scroll_is_clamped_to_the_content() {
        let mut nav = reading();
        nav.set_scroll(7);
        assert_eq!(nav.scroll(), 7);
        nav.set_scroll(usize::MAX);
        assert_eq!(nav.scroll(), 12);
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
