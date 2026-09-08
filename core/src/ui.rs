//! The on-device interface: screens, navigation, and the chrome around them.
//!
//! Phase 7 gave the panel a single status page that the modem redrew and the
//! user could only watch. This is the other half: several pages, a way to move
//! between them, and menus that do something. The shape is deliberately the one
//! the reference firmware uses, because it is a shape people already know --
//! a title bar at the top, a strip of icons at the bottom saying where you are,
//! and a popup menu of actions over the middle.
//!
//! # Everything here is pure
//!
//! No pins, no peripherals, no clock. A [`Nav`] takes an [`Input`] and returns
//! what happened; a render function takes a [`Frame`] and some borrowed state
//! and draws. That is what makes the interface testable without a board, which
//! matters more here than anywhere else in the firmware: a menu that navigates
//! wrongly is invisible in a log and obvious on a screen nobody is watching.
//!
//! The division of labour with the caller is strict. This module never decides
//! *what is true* -- it is handed the modem's state and draws it. It never
//! performs an action either; [`Nav::handle`] returns an [`Action`] and the
//! caller does the work. So a menu item that reboots the board is, in here,
//! only ever the word `Reboot` and a value in an enum.
//!
//! # The navigation model
//!
//! Two levels, and no more, because the panel is 128 pixels square and a deep
//! tree on a small screen is a maze:
//!
//! * **Browsing.** Left and right move between screens, which wrap. Up and down
//!   scroll the current screen's content. This is where the device sits.
//! * **A menu.** Select opens the current screen's action menu over the
//!   content. Up and down move the highlight, select activates it, back closes
//!   it. Left and right are ignored rather than being made to mean something
//!   else, so a stray sideways press cannot change screen out from under a
//!   menu that is open.
//!
//! Back at the browsing level does nothing at all. There is nowhere above the
//! top, and a back button that silently jumps to the first screen is a way to
//! lose your place by leaning on the board.

use crate::font;
use crate::sh1107::{self, Frame};

/// Height of the title bar, including its bottom edge.
pub const TITLE_H: usize = 11;
/// Height of the icon strip at the foot of the screen.
pub const BAR_H: usize = 11;
/// First row of the content area, below the title bar.
pub const CONTENT_TOP: usize = TITLE_H + 2;
/// One past the last row of the content area, above the icon strip.
pub const CONTENT_BOTTOM: usize = sh1107::HEIGHT - BAR_H;
/// Rows available to a screen's own content.
pub const CONTENT_H: usize = CONTENT_BOTTOM - CONTENT_TOP;

/// Width of one icon in the strip.
pub const ICON_W: usize = 7;
/// Height of one icon in the strip.
pub const ICON_H: usize = 7;
/// Width of the cell an icon sits in, including the gap beside it.
pub const ICON_CELL: usize = ICON_W + 4;

/// One icon: column-major, like a glyph. Byte *n* is column *n*, bit *k* row
/// *k*, top first.
pub type Icon = [u8; ICON_W];

/// The icons, in [`Screen::ALL`] order.
///
/// Drawn as ASCII art and converted by a generator, for the same reason the
/// font is: hand-entered hex has no symptom other than a wrong picture.
const ICONS: [Icon; Screen::COUNT] = [
    [0x78, 0x7c, 0x06, 0x77, 0x06, 0x7c, 0x78], // a house
    [0x04, 0x12, 0x09, 0x7d, 0x09, 0x12, 0x04], // an antenna radiating
    [0x00, 0x22, 0x14, 0x7f, 0x36, 0x00, 0x00], // the Bluetooth rune
    [0x1c, 0x36, 0x22, 0x7f, 0x22, 0x36, 0x1c], // a map pin
    [0x1c, 0x3e, 0x77, 0x22, 0x77, 0x3e, 0x1c], // a cog
];

/// The screens, in the order they appear in the strip.
///
/// Ordered by how often they are wanted rather than by how they were built:
/// the status page is where the device sits, the radio is what gets changed,
/// and the system page is the one you visit twice a year. Reordering this
/// reorders the strip and the left/right walk together, which is the point of
/// having one list.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Screen {
    /// What the modem is doing right now.
    Home,
    /// Frequency, bandwidth, spreading factor, coding rate, power.
    Radio,
    /// Advertising, connected, bonded.
    Bluetooth,
    /// Where the board thinks it is.
    Position,
    /// Firmware, identity, and the two ways to restart.
    System,
}

/// What [`Screen::Home`] offers.
const HOME_MENU: [Item; 3] = [
    Item::BACK,
    Item::new("Sleep Screen", Action::SleepScreen),
    Item::new("Redraw", Action::Redraw),
];

/// What [`Screen::Radio`] offers.
const RADIO_MENU: [Item; 3] = [
    Item::BACK,
    Item::new("Radio On/Off", Action::ToggleRadio),
    Item::new("Reset Config", Action::ResetRadioConfig),
];

/// What [`Screen::Bluetooth`] offers.
const BLUETOOTH_MENU: [Item; 2] = [Item::BACK, Item::new("Forget Phones", Action::ForgetBonds)];

/// What [`Screen::Position`] offers. Nothing yet: the GPS is not driven, and a
/// menu of things that do not work is worse than a menu with one way out.
const POSITION_MENU: [Item; 1] = [Item::BACK];

/// What [`Screen::System`] offers.
const SYSTEM_MENU: [Item; 3] = [
    Item::BACK,
    Item::new("Reboot", Action::Reboot),
    Item::new("Bootloader", Action::Bootloader),
];

impl Screen {
    /// Every screen, in strip order.
    pub const ALL: [Screen; 5] = [
        Screen::Home,
        Screen::Radio,
        Screen::Bluetooth,
        Screen::Position,
        Screen::System,
    ];

    /// How many there are.
    pub const COUNT: usize = Self::ALL.len();

    /// Its position in the strip.
    pub const fn index(self) -> usize {
        match self {
            Screen::Home => 0,
            Screen::Radio => 1,
            Screen::Bluetooth => 2,
            Screen::Position => 3,
            Screen::System => 4,
        }
    }

    /// The screen at a position, wrapping.
    pub fn at(index: usize) -> Screen {
        Self::ALL[index % Self::COUNT]
    }

    /// The word in the title bar.
    pub const fn title(self) -> &'static str {
        match self {
            Screen::Home => "Home",
            Screen::Radio => "Radio",
            Screen::Bluetooth => "Bluetooth",
            Screen::Position => "Position",
            Screen::System => "System",
        }
    }

    /// Its icon in the strip.
    pub fn icon(self) -> &'static Icon {
        &ICONS[self.index()]
    }

    /// The heading over its action menu.
    pub const fn menu_title(self) -> &'static str {
        match self {
            Screen::Home => "Home Action",
            Screen::Radio => "Radio Action",
            Screen::Bluetooth => "Bluetooth Action",
            Screen::Position => "Position Action",
            Screen::System => "System Action",
        }
    }

    /// What its action menu offers, `Back` first.
    ///
    /// `Back` is an item as well as a button. The board does have a dedicated
    /// back switch, so this is redundancy rather than necessity -- but a menu
    /// that can only be left by a key the user has not found yet is a trap,
    /// and the way out costs one line.
    pub const fn menu(self) -> &'static [Item] {
        match self {
            Screen::Home => &HOME_MENU,
            Screen::Radio => &RADIO_MENU,
            Screen::Bluetooth => &BLUETOOTH_MENU,
            Screen::Position => &POSITION_MENU,
            Screen::System => &SYSTEM_MENU,
        }
    }
}

/// One line in an action menu.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Item {
    /// What it says on the screen.
    pub label: &'static str,
    /// What the caller should do about it.
    pub action: Action,
}

impl Item {
    /// The item every menu opens with.
    pub const BACK: Item = Item::new("Back", Action::Close);

    pub const fn new(label: &'static str, action: Action) -> Item {
        Item { label, action }
    }
}

/// What the caller should do, once the user has chosen it.
///
/// Deliberately a small closed set of things the firmware can actually do
/// today. A menu item whose action does not exist yet is not listed, because a
/// menu that answers a press with nothing teaches people the board is broken.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Shut the menu. Handled here; never returned to the caller.
    Close,
    /// Blank the panel until the next input.
    SleepScreen,
    /// Force a full repaint.
    Redraw,
    /// Put the radio into or out of receive.
    ToggleRadio,
    /// Forget the host's configuration and go back to the stored one.
    ResetRadioConfig,
    /// Drop every stored pairing.
    ForgetBonds,
    /// Restart into the application.
    Reboot,
    /// Restart into the UF2 bootloader.
    Bootloader,
}

impl Action {
    /// Its name, for a log.
    pub const fn name(self) -> &'static str {
        match self {
            Action::Close => "close",
            Action::SleepScreen => "sleep screen",
            Action::Redraw => "redraw",
            Action::ToggleRadio => "radio on/off",
            Action::ResetRadioConfig => "reset config",
            Action::ForgetBonds => "forget phones",
            Action::Reboot => "reboot",
            Action::Bootloader => "bootloader",
        }
    }
}

/// Which way the user moved.
///
/// Named for the gesture and not the hardware, though on this board the two
/// happen to line up exactly: the Super IO carries a navigation pad, an OK and
/// a back button, which is six switches for six variants. What debounces them
/// is the driver's business.
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

/// Where the user is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Nav {
    screen: usize,
    /// `Some(highlighted item)` while an action menu is open.
    menu: Option<usize>,
    /// First visible content row, in lines, while browsing.
    scroll: usize,
    /// Lines of content the current screen has, set by the renderer.
    lines: usize,
}

impl Default for Nav {
    fn default() -> Self {
        Self::new()
    }
}

impl Nav {
    pub const fn new() -> Self {
        Nav {
            screen: 0,
            menu: None,
            scroll: 0,
            lines: 0,
        }
    }

    /// The screen being shown.
    pub fn screen(&self) -> Screen {
        Screen::at(self.screen)
    }

    /// The highlighted item, while a menu is open.
    pub fn menu_item(&self) -> Option<usize> {
        self.menu
    }

    /// Whether an action menu is covering the content.
    pub fn menu_is_open(&self) -> bool {
        self.menu.is_some()
    }

    /// The first content line visible.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Tell the navigator how tall the current screen's content is.
    ///
    /// Called by whoever renders it, because the renderer is the only thing
    /// that knows. Clamps the scroll, so a screen that shrinks -- a list of
    /// bonded phones losing an entry -- cannot leave the view stranded past
    /// the end of it.
    pub fn set_content_lines(&mut self, lines: usize) {
        self.lines = lines;
        let max = lines.saturating_sub(visible_lines());
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Feed the navigator one gesture.
    ///
    /// Returns an [`Action`] only when the user picked one. Movement returns
    /// `None`, and so does [`Action::Close`], which is this module's own
    /// business rather than the caller's.
    pub fn handle(&mut self, input: Input) -> Option<Action> {
        match self.menu {
            Some(selected) => self.in_menu(selected, input),
            None => {
                self.browsing(input);
                None
            }
        }
    }

    /// Movement between and within screens.
    fn browsing(&mut self, input: Input) {
        match input {
            // Wrapping, so a strip of five screens is never more than two
            // moves away in whichever direction is closer.
            Input::Left => self.screen = (self.screen + Screen::COUNT - 1) % Screen::COUNT,
            Input::Right => self.screen = (self.screen + 1) % Screen::COUNT,
            Input::Up => self.scroll = self.scroll.saturating_sub(1),
            Input::Down => {
                let max = self.lines.saturating_sub(visible_lines());
                self.scroll = (self.scroll + 1).min(max);
            }
            Input::Select => self.menu = Some(0),
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

    /// Movement inside an open action menu.
    fn in_menu(&mut self, selected: usize, input: Input) -> Option<Action> {
        let items = self.screen().menu();
        match input {
            Input::Up => {
                self.menu = Some((selected + items.len() - 1) % items.len());
                None
            }
            Input::Down => {
                self.menu = Some((selected + 1) % items.len());
                None
            }
            Input::Back => {
                self.menu = None;
                None
            }
            Input::Select => match items[selected].action {
                Action::Close => {
                    self.menu = None;
                    None
                }
                // The menu shuts on the way out, so the board is never left
                // showing a menu over the result of what it just did.
                action => {
                    self.menu = None;
                    Some(action)
                }
            },
            // Sideways is ignored while a menu is open; see the module docs.
            Input::Left | Input::Right => None,
        }
    }
}

/// How many lines of text fit in the content area.
pub const fn visible_lines() -> usize {
    CONTENT_H / font::LINE_HEIGHT
}

/// Draw the title bar: a solid strip with the text knocked out of it.
///
/// `left` is the battery or whatever else belongs in the corner, `right` the
/// clock. Both are borrowed rather than formatted here, because formatting
/// needs an allocator or a scratch buffer and the caller has one either way.
pub fn title_bar(frame: &mut Frame, left: &str, title: &str, right: &str) {
    frame.rect(0, 0, sh1107::WIDTH, TITLE_H, true);
    let y = (TITLE_H - font::HEIGHT) / 2;
    font::draw(frame, 2, y, left, false);
    // Centred on the panel rather than between its neighbours, so the title
    // does not shuffle sideways when the battery reading goes from 9% to 100%.
    let x = sh1107::WIDTH.saturating_sub(font::width_of(title)) / 2;
    font::draw(frame, x, y, title, false);
    font::draw_right(frame, sh1107::WIDTH - 2, y, right, false);
}

/// Draw the strip of screen icons across the foot of the panel.
///
/// The current screen's icon is drawn knocked out of a filled cell, which is
/// the only marking that survives being glanced at: a box around an icon and
/// a box beside it look the same at arm's length, an inverted one does not.
pub fn icon_bar(frame: &mut Frame, current: Screen) {
    let top = sh1107::HEIGHT - BAR_H;
    frame.rect(0, top, sh1107::WIDTH, BAR_H, false);
    // A hairline above the strip, so it reads as chrome rather than as the
    // bottom of whatever the screen happens to be showing.
    frame.rect(0, top, sh1107::WIDTH, 1, true);

    let span = Screen::COUNT * ICON_CELL;
    let left = sh1107::WIDTH.saturating_sub(span) / 2;
    for (n, screen) in Screen::ALL.iter().enumerate() {
        let cell_x = left + n * ICON_CELL;
        let on = *screen == current;
        if on {
            frame.rect(cell_x, top + 2, ICON_CELL, BAR_H - 2, true);
        }
        draw_icon(
            frame,
            cell_x + (ICON_CELL - ICON_W) / 2,
            top + 3,
            screen.icon(),
            !on,
        );
    }
}

/// Draw one icon with its top-left corner at `(x, y)`.
pub fn draw_icon(frame: &mut Frame, x: usize, y: usize, icon: &Icon, on: bool) {
    for (dx, column) in icon.iter().enumerate() {
        for dy in 0..ICON_H {
            if column & (1 << dy) != 0 {
                frame.set_pixel(x + dx, y + dy, on);
            }
        }
    }
}

/// Draw the action menu over the content.
///
/// A filled heading, a framed body, and the highlighted item wrapped in `>` and
/// `<` as well as being inverted. Belt and braces on purpose: the panel is
/// viewed at an angle as often as not, and inversion alone is easy to lose.
pub fn menu(frame: &mut Frame, screen: Screen, selected: usize) {
    let items = screen.menu();
    let title = screen.menu_title();

    let rows = items.len();
    let body_h = rows * font::LINE_HEIGHT + 4;
    let head_h = font::HEIGHT + 4;
    let total_h = head_h + body_h;

    let widest = items
        .iter()
        .map(|i| font::width_of(i.label) + 4 * font::ADVANCE)
        .fold(font::width_of(title) + 8, usize::max);
    let w = widest.min(sh1107::WIDTH - 8);
    let x = (sh1107::WIDTH - w) / 2;
    // Centred in the content area rather than on the panel, so it never sits
    // over the title bar or the icon strip.
    let y = CONTENT_TOP + CONTENT_H.saturating_sub(total_h) / 2;

    frame.rect(x, y, w, head_h, true);
    let tx = x + w.saturating_sub(font::width_of(title)) / 2;
    font::draw(frame, tx, y + 2, title, false);

    frame.rect(x, y + head_h, w, body_h, false);
    frame.frame_rect(x, y + head_h, w, body_h, true);

    for (n, item) in items.iter().enumerate() {
        let row_y = y + head_h + 2 + n * font::LINE_HEIGHT;
        let chosen = n == selected;
        if chosen {
            frame.rect(x + 1, row_y - 1, w - 2, font::LINE_HEIGHT, true);
        }
        let label_w = font::width_of(item.label) + if chosen { 4 * font::ADVANCE } else { 0 };
        let mut lx = x + w.saturating_sub(label_w) / 2;
        if chosen {
            // One advance of gap inside each bracket, which is what the
            // width above budgets for. The first version drew the label hard
            // against `>` and left the gap only before `<`; it looked right
            // as a pixel count and wrong as a picture, and was the first
            // thing the simulator caught.
            lx = font::draw(frame, lx, row_y, ">", false) + font::ADVANCE;
            lx = font::draw(frame, lx, row_y, item.label, false);
            font::draw(frame, lx + font::ADVANCE, row_y, "<", false);
        } else {
            font::draw(frame, lx, row_y, item.label, true);
        }
    }
}

/// Draw a scrollbar down the right edge of the content area.
///
/// Drawn only when there is something to scroll. A full-height bar on a screen
/// that fits says "there is more" as loudly as a short one does.
pub fn scrollbar(frame: &mut Frame, first: usize, lines: usize) {
    let visible = visible_lines();
    if lines <= visible {
        return;
    }
    let x = sh1107::WIDTH - 2;
    let track = CONTENT_H;
    let thumb = (track * visible / lines).max(4);
    let span = track - thumb;
    let top = CONTENT_TOP + span * first / (lines - visible);
    frame.rect(x, CONTENT_TOP, 1, track, false);
    frame.rect(x, top, 2, thumb, true);
}

/// Left edge of a screen's own content, in from the panel edge.
pub const CONTENT_LEFT: usize = 4;

/// Draw a whole page: chrome, content and, if one is open, the menu.
///
/// This is the one composition the firmware and the simulator share, so that
/// what is looked at on the host is what is drawn on the board. `lines` is the
/// current screen's content, one string per line, already formatted -- the
/// caller owns the scratch buffers, for the reason [`title_bar`] gives. The
/// navigator is told how tall the content is here, because this is the first
/// place that knows, and it is told before the scroll is read so that a screen
/// shorter than the last one cannot be shown scrolled past its end.
pub fn page(frame: &mut Frame, nav: &mut Nav, left: &str, right: &str, lines: &[&str]) {
    frame.fill(false);
    nav.set_content_lines(lines.len());
    let screen = nav.screen();
    title_bar(frame, left, screen.title(), right);

    let first = nav.scroll();
    for (n, line) in lines.iter().skip(first).take(visible_lines()).enumerate() {
        font::draw(
            frame,
            CONTENT_LEFT,
            CONTENT_TOP + n * font::LINE_HEIGHT,
            line,
            true,
        );
    }
    scrollbar(frame, first, lines.len());

    icon_bar(frame, screen);
    if let Some(selected) = nav.menu_item() {
        menu(frame, screen, selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_input_has_its_own_name() {
        let all = [
            Input::Left,
            Input::Right,
            Input::Up,
            Input::Down,
            Input::Select,
            Input::Back,
        ];
        for (i, a) in all.iter().enumerate() {
            assert_eq!(a.name(), a.name().to_lowercase());
            for b in &all[i + 1..] {
                assert_ne!(a.name(), b.name());
            }
        }
    }

    /// Every menu item's action has a name of its own.
    #[test]
    fn every_action_has_its_own_name() {
        let mut names = Vec::new();
        for screen in Screen::ALL {
            for item in screen.menu() {
                let name = item.action.name();
                assert!(!name.is_empty());
                if item.action != Action::Close {
                    assert!(!names.contains(&name), "{name} repeats");
                    names.push(name);
                }
            }
        }
        assert_eq!(Action::Close.name(), "close");
    }

    /// Walk the whole strip and come back to where you started.
    #[test]
    fn right_wraps_all_the_way_round() {
        let mut nav = Nav::new();
        assert_eq!(nav.screen(), Screen::Home);
        for _ in 0..Screen::COUNT {
            nav.handle(Input::Right);
        }
        assert_eq!(nav.screen(), Screen::Home);
    }

    /// Left from the first screen lands on the last, rather than sticking.
    #[test]
    fn left_from_the_first_screen_wraps_to_the_last() {
        let mut nav = Nav::new();
        nav.handle(Input::Left);
        assert_eq!(nav.screen(), Screen::System);
        assert_eq!(nav.screen().index(), Screen::COUNT - 1);
    }

    /// Every screen is reachable by walking, and each one only once per lap.
    #[test]
    fn one_lap_visits_every_screen_exactly_once() {
        let mut nav = Nav::new();
        let mut seen = Vec::new();
        for _ in 0..Screen::COUNT {
            seen.push(nav.screen());
            nav.handle(Input::Right);
        }
        let mut sorted: Vec<usize> = seen.iter().map(|s| s.index()).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), Screen::COUNT, "{seen:?}");
    }

    /// Select opens the menu on the item that closes it again.
    ///
    /// Opening on `Back` rather than on the first real action is what makes a
    /// double press harmless: OK, OK is a no-op, not a reboot.
    #[test]
    fn a_menu_opens_on_back() {
        let mut nav = Nav::new();
        nav.handle(Input::Right); // Radio
        assert!(!nav.menu_is_open());
        assert_eq!(nav.handle(Input::Select), None);
        assert!(nav.menu_is_open());
        assert_eq!(nav.menu_item(), Some(0));
        assert_eq!(nav.screen().menu()[0], Item::BACK);
        assert_eq!(nav.handle(Input::Select), None, "Back is not an action");
        assert!(!nav.menu_is_open());
    }

    /// Choosing an action shuts the menu and hands it over exactly once.
    #[test]
    fn choosing_an_action_returns_it_and_closes_the_menu() {
        let mut nav = Nav::new();
        for _ in 0..Screen::System.index() {
            nav.handle(Input::Right);
        }
        assert_eq!(nav.screen(), Screen::System);
        nav.handle(Input::Select);
        nav.handle(Input::Down); // off Back, onto Reboot
        assert_eq!(nav.handle(Input::Select), Some(Action::Reboot));
        assert!(!nav.menu_is_open());
        // And it is not delivered a second time.
        assert_eq!(nav.handle(Input::Up), None);
    }

    /// Back closes an open menu without choosing anything.
    #[test]
    fn back_closes_a_menu_without_acting() {
        let mut nav = Nav::new();
        nav.handle(Input::Select);
        nav.handle(Input::Down);
        assert!(nav.menu_is_open());
        assert_eq!(nav.handle(Input::Back), None);
        assert!(!nav.menu_is_open());
    }

    /// Back at the top level does nothing, rather than jumping home.
    #[test]
    fn back_while_browsing_does_not_move() {
        let mut nav = Nav::new();
        nav.handle(Input::Right);
        nav.handle(Input::Right);
        let before = nav;
        assert_eq!(nav.handle(Input::Back), None);
        assert_eq!(nav, before);
    }

    /// Sideways cannot change screen out from under an open menu.
    #[test]
    fn sideways_is_ignored_while_a_menu_is_open() {
        let mut nav = Nav::new();
        nav.handle(Input::Select);
        let screen = nav.screen();
        assert_eq!(nav.handle(Input::Left), None);
        assert_eq!(nav.handle(Input::Right), None);
        assert_eq!(nav.screen(), screen);
        assert!(nav.menu_is_open());
    }

    /// The highlight wraps within a menu, both ways.
    #[test]
    fn the_menu_highlight_wraps() {
        let mut nav = Nav::new();
        for _ in 0..Screen::System.index() {
            nav.handle(Input::Right);
        }
        let count = nav.screen().menu().len();
        nav.handle(Input::Select);
        nav.handle(Input::Up);
        assert_eq!(nav.menu_item(), Some(count - 1), "up from the top");
        nav.handle(Input::Down);
        assert_eq!(nav.menu_item(), Some(0), "down from the bottom");
    }

    /// Every menu starts with Back, so every menu can be left.
    #[test]
    fn every_menu_can_be_left() {
        for screen in Screen::ALL {
            let items = screen.menu();
            assert_eq!(items[0], Item::BACK, "{screen:?}");
            assert_eq!(
                items.iter().filter(|i| i.action == Action::Close).count(),
                1,
                "{screen:?} has more than one way out"
            );
        }
    }

    /// Scrolling stops at both ends rather than running off.
    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut nav = Nav::new();
        nav.set_content_lines(visible_lines() + 3);
        for _ in 0..20 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 3, "cannot scroll past the last line");
        for _ in 0..20 {
            nav.handle(Input::Up);
        }
        assert_eq!(nav.scroll(), 0, "cannot scroll above the first");
    }

    /// A screen that fits does not scroll at all.
    #[test]
    fn a_screen_that_fits_does_not_scroll() {
        let mut nav = Nav::new();
        nav.set_content_lines(2);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 0);
    }

    /// Content that shrinks under a scrolled view pulls the view back.
    #[test]
    fn a_shrinking_screen_does_not_stay_scrolled_past_its_end() {
        let mut nav = Nav::new();
        nav.set_content_lines(visible_lines() + 5);
        for _ in 0..5 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 5);
        nav.set_content_lines(visible_lines() + 1);
        assert_eq!(nav.scroll(), 1, "clamped to what is left");
    }

    /// Changing screen starts at the top of the new one.
    #[test]
    fn a_new_screen_starts_at_its_top() {
        let mut nav = Nav::new();
        nav.set_content_lines(visible_lines() + 5);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2);
        nav.handle(Input::Right);
        assert_eq!(nav.scroll(), 0);
    }

    /// The chrome leaves the content area alone.
    ///
    /// The whole layout rests on these three not overlapping, and the numbers
    /// are easy to nudge apart by one while editing a constant.
    #[test]
    fn the_chrome_does_not_overlap_the_content() {
        let mut frame = Frame::new();
        frame.fill(false);
        title_bar(&mut frame, "99%", "Home", "12:45p");
        icon_bar(&mut frame, Screen::Home);
        for y in CONTENT_TOP..CONTENT_BOTTOM {
            for x in 0..sh1107::WIDTH {
                assert!(!frame.pixel(x, y), "chrome drew at ({x}, {y})");
            }
        }
    }

    /// The title bar is a solid strip with dark text in it.
    #[test]
    fn the_title_bar_is_knocked_out_rather_than_drawn() {
        let mut frame = Frame::new();
        frame.fill(false);
        title_bar(&mut frame, "99%", "Home", "12:45p");
        let top = (0..sh1107::WIDTH).filter(|&x| frame.pixel(x, 0)).count();
        assert_eq!(top, sh1107::WIDTH, "the strip should be solid");
        let middle = (0..sh1107::WIDTH).filter(|&x| frame.pixel(x, 3)).count();
        assert!(middle < sh1107::WIDTH, "the text should be knocked out");
    }

    /// Exactly one icon is highlighted, and it is the current screen's.
    #[test]
    fn the_strip_highlights_only_where_you_are() {
        for current in Screen::ALL {
            let mut frame = Frame::new();
            frame.fill(false);
            icon_bar(&mut frame, current);
            let top = sh1107::HEIGHT - BAR_H;
            let span = Screen::COUNT * ICON_CELL;
            let left = (sh1107::WIDTH - span) / 2;
            for (n, _) in Screen::ALL.iter().enumerate() {
                let cell_x = left + n * ICON_CELL;
                // The row under the icon is background in an unfilled cell and
                // solid in the filled one.
                let lit = (cell_x..cell_x + ICON_CELL)
                    .filter(|&x| frame.pixel(x, top + BAR_H - 1))
                    .count();
                let filled = lit == ICON_CELL;
                assert_eq!(
                    filled,
                    n == current.index(),
                    "{current:?}: cell {n} filled={filled}"
                );
            }
        }
    }

    /// Every menu fits inside the content area, on every screen.
    #[test]
    fn no_menu_draws_outside_the_content_area() {
        for screen in Screen::ALL {
            for selected in 0..screen.menu().len() {
                let mut frame = Frame::new();
                frame.fill(false);
                menu(&mut frame, screen, selected);
                for x in 0..sh1107::WIDTH {
                    for y in (0..CONTENT_TOP).chain(CONTENT_BOTTOM..sh1107::HEIGHT) {
                        assert!(!frame.pixel(x, y), "{screen:?} drew at ({x}, {y})");
                    }
                }
            }
        }
    }

    /// A menu actually draws something, and something different per selection.
    ///
    /// Guards against a layout slip that renders an empty box: every assertion
    /// above is about where it does *not* draw.
    #[test]
    fn a_menu_draws_and_the_highlight_moves() {
        let screen = Screen::System;
        let mut shots = Vec::new();
        for selected in 0..screen.menu().len() {
            let mut frame = Frame::new();
            frame.fill(false);
            menu(&mut frame, screen, selected);
            let lit = (0..sh1107::WIDTH)
                .flat_map(|x| (0..sh1107::HEIGHT).map(move |y| (x, y)))
                .filter(|&(x, y)| frame.pixel(x, y))
                .count();
            assert!(lit > 100, "selection {selected} drew almost nothing");
            shots.push(frame);
        }
        for (a, b) in shots.iter().zip(shots.iter().skip(1)) {
            let differs = (0..sh1107::WIDTH)
                .flat_map(|x| (0..sh1107::HEIGHT).map(move |y| (x, y)))
                .any(|(x, y)| a.pixel(x, y) != b.pixel(x, y));
            assert!(differs, "the highlight did not move");
        }
    }

    /// The scrollbar appears only when there is more than one screenful.
    #[test]
    fn the_scrollbar_appears_only_when_it_is_needed() {
        let lit = |lines, first| {
            let mut frame = Frame::new();
            frame.fill(false);
            scrollbar(&mut frame, first, lines);
            (0..sh1107::WIDTH)
                .flat_map(|x| (0..sh1107::HEIGHT).map(move |y| (x, y)))
                .filter(|&(x, y)| frame.pixel(x, y))
                .count()
        };
        assert_eq!(lit(visible_lines(), 0), 0, "a screen that fits");
        assert!(lit(visible_lines() * 3, 0) > 0, "a screen that does not");
    }

    /// The thumb moves down as the view does, and stays on the track.
    #[test]
    fn the_scrollbar_thumb_tracks_the_view() {
        let lines = visible_lines() * 3;
        let top_of = |first| {
            let mut frame = Frame::new();
            frame.fill(false);
            scrollbar(&mut frame, first, lines);
            (0..sh1107::HEIGHT).find(|&y| frame.pixel(sh1107::WIDTH - 1, y))
        };
        let first = top_of(0).expect("a thumb at the top");
        let last = top_of(lines - visible_lines()).expect("a thumb at the bottom");
        assert!(first < last, "the thumb should move down");
        assert!(first >= CONTENT_TOP, "above the track");
        assert!(last < CONTENT_BOTTOM, "below the track");
    }

    /// The content area is tall enough to be worth having.
    #[test]
    fn the_layout_leaves_room_to_draw_in() {
        const _: () = assert!(CONTENT_TOP < CONTENT_BOTTOM);
        assert!(
            visible_lines() >= 8,
            "only {} lines fit; the chrome is too tall",
            visible_lines()
        );
    }

    /// No two screens share an icon or a title.
    #[test]
    fn every_screen_is_distinguishable() {
        let mut icons = Vec::new();
        let mut titles = Vec::new();
        for screen in Screen::ALL {
            assert!(!icons.contains(&screen.icon()), "{screen:?} icon repeats");
            assert!(
                !titles.contains(&screen.title()),
                "{screen:?} title repeats"
            );
            icons.push(screen.icon());
            titles.push(screen.title());
        }
    }

    /// `at` and `index` are inverses, including past the end.
    #[test]
    fn a_screen_and_its_position_agree() {
        for (n, screen) in Screen::ALL.iter().enumerate() {
            assert_eq!(screen.index(), n);
            assert_eq!(Screen::at(n), *screen);
            assert_eq!(Screen::at(n + Screen::COUNT), *screen, "wrapping");
        }
    }

    /// A composed page carries the chrome of the screen the navigator is on.
    #[test]
    fn a_page_shows_the_current_screen() {
        let mut nav = Nav::new();
        nav.handle(Input::Right);
        let mut frame = Frame::new();
        page(&mut frame, &mut nav, "99%", "12:45p", &[]);

        let mut chrome = Frame::new();
        chrome.fill(false);
        title_bar(&mut chrome, "99%", Screen::Radio.title(), "12:45p");
        icon_bar(&mut chrome, Screen::Radio);
        assert_eq!(frame.as_bytes(), chrome.as_bytes(), "chrome only, on Radio");
    }

    /// Content lines land in the content area, starting at the scroll.
    #[test]
    fn a_page_draws_its_lines_from_the_scroll() {
        let lines: Vec<String> = (0..visible_lines() + 4)
            .map(|n| format!("line {n}"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut nav = Nav::new();
        let mut top = Frame::new();
        page(&mut top, &mut nav, "", "", &refs);
        assert_eq!(nav.scroll(), 0);

        nav.handle(Input::Down);
        let mut down = Frame::new();
        page(&mut down, &mut nav, "", "", &refs);
        assert_eq!(nav.scroll(), 1, "the page told the navigator its height");

        // Row 2 of the scrolled page is row 1 of the unscrolled one, shifted
        // up by one line, in the text columns.
        for x in CONTENT_LEFT..sh1107::WIDTH - 4 {
            for dy in 0..font::HEIGHT {
                assert_eq!(
                    down.pixel(x, CONTENT_TOP + dy),
                    top.pixel(x, CONTENT_TOP + font::LINE_HEIGHT + dy),
                    "at ({x}, {dy})"
                );
            }
        }
        // And it did not draw in the chrome.
        for x in 0..sh1107::WIDTH {
            for y in (0..CONTENT_TOP).chain(CONTENT_BOTTOM..sh1107::HEIGHT) {
                assert_eq!(
                    top.pixel(x, y),
                    down.pixel(x, y),
                    "chrome changed at ({x}, {y})"
                );
            }
        }
    }

    /// Fewer lines than fit means no scrollbar; more means one.
    #[test]
    fn a_page_scrolls_only_when_it_must() {
        let bar_lit = |count: usize| {
            let lines: Vec<String> = (0..count).map(|n| n.to_string()).collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let mut nav = Nav::new();
            let mut frame = Frame::new();
            page(&mut frame, &mut nav, "", "", &refs);
            (CONTENT_TOP..CONTENT_BOTTOM).any(|y| frame.pixel(sh1107::WIDTH - 1, y))
        };
        assert!(!bar_lit(visible_lines()));
        assert!(bar_lit(visible_lines() + 1));
    }

    /// An open menu is drawn over the content, and a closed one is not.
    #[test]
    fn a_page_overlays_the_menu_when_one_is_open() {
        let mut nav = Nav::new();
        let mut closed = Frame::new();
        page(&mut closed, &mut nav, "", "", &[]);
        nav.handle(Input::Select);
        let mut open = Frame::new();
        page(&mut open, &mut nav, "", "", &[]);

        let mut expected = closed.clone();
        menu(&mut expected, Screen::Home, 0);
        assert_eq!(open.as_bytes(), expected.as_bytes());
        assert_ne!(open.as_bytes(), closed.as_bytes());
    }

    /// A page that shrinks between renders pulls a scrolled view back.
    #[test]
    fn a_page_that_shrinks_is_not_left_scrolled_past_its_end() {
        let long: Vec<String> = (0..visible_lines() + 5).map(|n| n.to_string()).collect();
        let long_refs: Vec<&str> = long.iter().map(String::as_str).collect();
        let mut nav = Nav::new();
        let mut frame = Frame::new();
        page(&mut frame, &mut nav, "", "", &long_refs);
        for _ in 0..5 {
            nav.handle(Input::Down);
        }
        assert_eq!(nav.scroll(), 5);
        page(&mut frame, &mut nav, "", "", &["only one"]);
        assert_eq!(nav.scroll(), 0);
    }

    /// The highlight's brackets sit the same distance from the label on
    /// both sides.
    ///
    /// Found by looking at a golden image rather than by any assertion: the
    /// label was drawn flush against `>` and a full advance away from `<`.
    #[test]
    fn the_highlighted_label_is_bracketed_symmetrically() {
        let screen = Screen::System;
        let selected = 1;
        let mut frame = Frame::new();
        frame.fill(false);
        menu(&mut frame, screen, selected);
        // The box's left edge is the leftmost lit column; the highlighted row
        // is the first row below the filled heading where the column just
        // inside that edge is lit again.
        let box_x = (0..sh1107::WIDTH)
            .find(|&x| (CONTENT_TOP..CONTENT_BOTTOM).any(|y| frame.pixel(x, y)))
            .expect("a menu box");
        let inside = |y: usize| frame.pixel(box_x + 1, y);
        let box_top = (CONTENT_TOP..CONTENT_BOTTOM)
            .find(|&y| inside(y))
            .expect("a heading");
        let body = (box_top..CONTENT_BOTTOM)
            .find(|&y| !inside(y))
            .expect("a body below the heading");
        let row_y = (body..CONTENT_BOTTOM)
            .find(|&y| inside(y))
            .expect("an inverted row");
        // In an inverted row, text is dark on light. Walk the columns and
        // record which are entirely lit across the glyph height: those are
        // the gaps.
        let is_gap = |x: usize| (0..font::HEIGHT).all(|dy| frame.pixel(x, row_y + 1 + dy));
        // Only inside the box: outside it every column is dark.
        let box_right = (0..sh1107::WIDTH)
            .rev()
            .find(|&x| frame.pixel(x, row_y))
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
}
