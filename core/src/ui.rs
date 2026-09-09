//! oxinode's interface: its screens, its menus, and what a menu item means.
//!
//! The interface itself -- the navigation model, the title bar and the icon
//! strip, the menus and the scrolling, the font -- is [`monopanel`], a crate
//! with nothing of oxinode in it. This module is what oxinode supplies to
//! it, and it is exactly the four things the crate's docs say an application
//! supplies: the screens as a slice of descriptors, an action type for the
//! menu items to carry, a modal for the third level, and a canvas -- which is
//! [`Frame`], over in [`crate::sh1107`], where the SH1107's layout is.
//!
//! The division of labour is enforced with a crate boundary. Nothing here
//! decides *what is true*: the screens' content is [`crate::screens`]'s,
//! formatted from a `State` the caller copies out of the modem loop. Nothing
//! here performs an action either; [`Nav::handle`] returns an [`Action`] and
//! the caller does the work. So a menu item that reboots the board is, in
//! here, only ever the word `Reboot` and a value in an enum.
//!
//! # The third level
//!
//! The one exception -- one value, opened from a menu item -- is the
//! crate's [`Modal`]. oxinode's is [`Overlay`]: an [`Editor`] for one radio
//! parameter, or a [`Lock`] notice saying why it cannot be edited right now.
//! An editor is opened by the caller with [`NavExt::edit`] rather than by
//! the menu itself, because opening one needs the configuration to edit and
//! the navigator is not handed that -- and because the caller is the one
//! that knows whether a host has the radio, in which case it opens a
//! [`NavExt::notice`] instead.

use monopanel::{Icon, Item, Layout, Modal, Outcome, Screen as Spec};

use crate::edit::{Confirm, Editor, Field, Lock};
use crate::lr1121::config::Setting;
use crate::sh1107;

pub use monopanel::Input;

/// The layout of a page on this board's panel.
///
/// A constant rather than something computed per render, so that buffers
/// can be sized from it: [`crate::screens::LINE_CHARS`] is its line length.
pub const LAYOUT: Layout = Layout::of(sh1107::WIDTH, sh1107::HEIGHT);

/// The screens, in the order they appear in the strip.
///
/// Ordered by how often they are wanted rather than by how they were built:
/// the status page is where the device sits, the radio is what gets changed,
/// and the system page is the one you visit twice a year. Reordering
/// [`Screen::ALL`] reorders the strip and the left/right walk together, which
/// is the point of having one list.
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
const HOME_MENU: [Item<Action>; 3] = [
    Item::BACK,
    Item::new("Sleep Screen", Action::SleepScreen),
    Item::new("Redraw", Action::Redraw),
];

/// What [`Screen::Radio`] offers: an editor for each of the five parameters
/// a host sets, then the three things that act on the configuration whole.
const RADIO_MENU: [Item<Action>; 9] = [
    Item::BACK,
    Item::new(Field::Frequency.label(), Action::Edit(Field::Frequency)),
    Item::new(Field::Bandwidth.label(), Action::Edit(Field::Bandwidth)),
    Item::new(
        Field::SpreadingFactor.label(),
        Action::Edit(Field::SpreadingFactor),
    ),
    Item::new(Field::CodingRate.label(), Action::Edit(Field::CodingRate)),
    Item::new(Field::TxPower.label(), Action::Edit(Field::TxPower)),
    Item::new("Radio On/Off", Action::ToggleRadio),
    Item::new("Save Config", Action::SaveRadioConfig),
    Item::new("Reset Config", Action::ResetRadioConfig),
];

/// What [`Screen::Bluetooth`] offers.
const BLUETOOTH_MENU: [Item<Action>; 2] =
    [Item::BACK, Item::new("Forget Phones", Action::ForgetBonds)];

/// What [`Screen::Position`] offers: the receiver's power, which is the one
/// thing about it a person would want to change from here.
const POSITION_MENU: [Item<Action>; 2] = [Item::BACK, Item::new("GPS On/Off", Action::ToggleGps)];

/// What [`Screen::System`] offers.
const SYSTEM_MENU: [Item<Action>; 3] = [
    Item::BACK,
    Item::new("Reboot", Action::Reboot),
    Item::new("Bootloader", Action::Bootloader),
];

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

/// The screens as the interface crate sees them: what [`Nav`] is built over.
///
/// Every menu opens with `Back`. `Back` is an item as well as a button. The
/// board does have a dedicated back switch, so this is redundancy rather
/// than necessity -- but a menu that can only be left by a key the user has
/// not found yet is a trap, and the way out costs one line.
pub const SCREENS: [Spec<Action>; Screen::COUNT] = [
    Spec {
        title: "Home",
        menu_title: "Home Action",
        icon: ICONS[0],
        menu: &HOME_MENU,
    },
    Spec {
        title: "Radio",
        menu_title: "Radio Action",
        icon: ICONS[1],
        menu: &RADIO_MENU,
    },
    Spec {
        title: "Bluetooth",
        menu_title: "Bluetooth Action",
        icon: ICONS[2],
        menu: &BLUETOOTH_MENU,
    },
    Spec {
        title: "Position",
        menu_title: "Position Action",
        icon: ICONS[3],
        menu: &POSITION_MENU,
    },
    Spec {
        title: "System",
        menu_title: "System Action",
        icon: ICONS[4],
        menu: &SYSTEM_MENU,
    },
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

    /// Its descriptor: what the interface crate draws it from.
    pub const fn spec(self) -> &'static Spec<Action> {
        &SCREENS[self.index()]
    }

    /// The word in the title bar.
    pub const fn title(self) -> &'static str {
        self.spec().title
    }

    /// Its icon in the strip.
    pub const fn icon(self) -> &'static Icon {
        &self.spec().icon
    }

    /// The heading over its action menu.
    pub const fn menu_title(self) -> &'static str {
        self.spec().menu_title
    }

    /// What its action menu offers, `Back` first.
    pub const fn menu(self) -> &'static [Item<Action>] {
        self.spec().menu
    }
}

/// What the caller should do, once the user has chosen it.
///
/// Deliberately a small closed set of things the firmware can actually do
/// today. A menu item whose action does not exist yet is not listed, because a
/// menu that answers a press with nothing teaches people the board is broken.
/// Closing a menu is not an action: that is the interface's own business,
/// and an item that only closes carries `None`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Blank the panel until the next input.
    SleepScreen,
    /// Force a full repaint.
    Redraw,
    /// Put the radio into or out of receive.
    ToggleRadio,
    /// Forget the host's configuration and go back to the stored one, or to
    /// the default if nothing is stored.
    ResetRadioConfig,
    /// Store the live configuration as the one the board boots with.
    SaveRadioConfig,
    /// Open an editor for one radio parameter. The caller answers with
    /// [`NavExt::edit`] or [`NavExt::notice`]; see the module docs.
    Edit(Field),
    /// An editor confirmed a value that passed validation. Apply it.
    Set(Setting),
    /// Drop every stored pairing.
    ForgetBonds,
    /// Power the GPS receiver up or down. The rule: the switch decides
    /// when it moves, and this decides in between.
    ToggleGps,
    /// Restart into the application.
    Reboot,
    /// Restart into the UF2 bootloader.
    Bootloader,
}

impl Action {
    /// Its name, for a log.
    pub const fn name(self) -> &'static str {
        match self {
            Action::SleepScreen => "sleep screen",
            Action::Redraw => "redraw",
            Action::ToggleRadio => "radio on/off",
            Action::ResetRadioConfig => "reset config",
            Action::SaveRadioConfig => "save config",
            Action::Edit(field) => field.name(),
            Action::Set(setting) => setting.name(),
            Action::ForgetBonds => "forget phones",
            Action::ToggleGps => "gps on/off",
            Action::Reboot => "reboot",
            Action::Bootloader => "bootloader",
        }
    }

    /// Whether carrying this out changes the live radio configuration or the
    /// radio's state -- the things a host that has the line believes it owns.
    ///
    /// This is the host-ownership rule in one place. A caller with a host on
    /// the line refuses these and does the rest; see
    /// `docs/architecture/interface.md`.
    pub const fn changes_the_radio(self) -> bool {
        matches!(
            self,
            Action::ToggleRadio | Action::ResetRadioConfig | Action::Edit(_) | Action::Set(_)
        )
    }
}

/// What can replace a screen's content: the third navigation level.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Overlay {
    /// An editor for one value.
    Editor(Editor),
    /// A notice saying why the value cannot be edited. Any key closes it.
    Notice(Lock),
}

impl Modal<Action> for Overlay {
    /// The field while editing, `Locked` for a notice.
    fn title(&self) -> &'static str {
        match self {
            Overlay::Editor(editor) => editor.field().title(),
            Overlay::Notice(_) => "Locked",
        }
    }

    /// Keys inside an editor: up and down change the value, left and right
    /// move the cursor where there is one, select confirms and back
    /// cancels. A confirmed, validated value comes back as [`Action::Set`];
    /// a refused one keeps the editor open with the refusal in it; cancel
    /// hands back nothing, which is the whole of what cancel does.
    ///
    /// Any key acknowledges a notice. Left and right included: the notice
    /// is over one screen and a sideways press should not change it
    /// underneath.
    fn handle(&mut self, input: Input) -> Outcome<Action> {
        let Overlay::Editor(editor) = self else {
            return Outcome::Close;
        };
        match input {
            Input::Up => editor.up(),
            Input::Down => editor.down(),
            Input::Left => editor.left(),
            Input::Right => editor.right(),
            Input::Select => {
                return match editor.confirm() {
                    Confirm::Apply(setting) => Outcome::Emit(Action::Set(setting)),
                    Confirm::Refused(_) => Outcome::Stay,
                }
            }
            Input::Back => return Outcome::Close,
        }
        Outcome::Stay
    }
}

/// Where the user is: the interface crate's navigator, over oxinode's
/// screens, actions and overlay.
pub type Nav = monopanel::Nav<Action, Overlay>;

/// A navigator on the Home screen, at its top, with nothing open.
pub const fn nav() -> Nav {
    Nav::new(&SCREENS)
}

/// What oxinode asks of the navigator beyond what the crate provides: the
/// current screen as the enum, and the two overlays by name.
pub trait NavExt {
    /// The screen being shown.
    fn current(&self) -> Screen;
    /// The editor, while one is open.
    fn editor(&self) -> Option<&Editor>;
    /// The notice, while one is showing.
    fn notice_shown(&self) -> Option<Lock>;
    /// Whether an editor or a notice has replaced the screen's content.
    fn is_editing(&self) -> bool;
    /// Open an editor. The caller's answer to [`Action::Edit`] when nobody
    /// else has the radio. Replaces whatever overlay there was.
    fn edit(&mut self, editor: Editor);
    /// Show why the radio cannot be edited from here right now. The
    /// caller's answer to [`Action::Edit`] -- or to any action for which
    /// [`Action::changes_the_radio`] holds -- while a host has the line.
    fn notice(&mut self, lock: Lock);
}

impl NavExt for Nav {
    fn current(&self) -> Screen {
        Screen::at(self.screen_index())
    }

    fn editor(&self) -> Option<&Editor> {
        match self.modal() {
            Some(Overlay::Editor(editor)) => Some(editor),
            _ => None,
        }
    }

    fn notice_shown(&self) -> Option<Lock> {
        match self.modal() {
            Some(Overlay::Notice(lock)) => Some(*lock),
            _ => None,
        }
    }

    fn is_editing(&self) -> bool {
        self.modal_is_open()
    }

    fn edit(&mut self, editor: Editor) {
        self.open(Overlay::Editor(editor));
    }

    fn notice(&mut self, lock: Lock) {
        self.open(Overlay::Notice(lock));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::DEFAULT;
    use crate::sh1107::Frame;
    use monopanel::page;

    /// The layout is the one every golden image was approved against.
    #[test]
    fn the_layout_is_the_panels() {
        assert_eq!(LAYOUT, Layout::of(128, 128));
        assert!(
            LAYOUT.visible_lines() >= 8,
            "only {} lines fit; the chrome is too tall",
            LAYOUT.visible_lines()
        );
    }

    /// Every menu item's action has a name of its own.
    #[test]
    fn every_action_has_its_own_name() {
        let mut names = Vec::new();
        for screen in Screen::ALL {
            for item in screen.menu() {
                if let Some(action) = item.action {
                    let name = action.name();
                    assert!(!name.is_empty());
                    assert!(!names.contains(&name), "{name} repeats");
                    names.push(name);
                }
            }
        }
    }

    /// The enum and the descriptor table agree, entry for entry.
    #[test]
    fn the_enum_and_the_table_agree() {
        assert_eq!(SCREENS.len(), Screen::COUNT);
        for (n, screen) in Screen::ALL.iter().enumerate() {
            assert_eq!(screen.index(), n);
            assert_eq!(Screen::at(n), *screen);
            assert_eq!(Screen::at(n + Screen::COUNT), *screen, "wrapping");
            assert_eq!(screen.spec(), &SCREENS[n]);
            assert_eq!(screen.title(), SCREENS[n].title);
            assert_eq!(screen.icon(), &SCREENS[n].icon);
            assert_eq!(screen.menu(), SCREENS[n].menu);
            assert_eq!(screen.menu_title(), SCREENS[n].menu_title);
        }
        let mut nav = nav();
        for screen in Screen::ALL {
            assert_eq!(nav.current(), screen);
            assert_eq!(nav.screen(), screen.spec());
            nav.handle(Input::Right);
        }
        assert_eq!(nav.current(), Screen::Home, "wrapped");
    }

    /// Every menu starts with Back, so every menu can be left.
    #[test]
    fn every_menu_can_be_left() {
        for screen in Screen::ALL {
            let items = screen.menu();
            assert_eq!(items[0], Item::BACK, "{screen:?}");
            assert_eq!(
                items.iter().filter(|i| i.action.is_none()).count(),
                1,
                "{screen:?} has more than one way out"
            );
        }
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

    /// Choosing an action from oxinode's own menus hands it back once, with
    /// the menu shut: the crate's model, on the real screen set.
    #[test]
    fn choosing_an_action_returns_it_and_closes_the_menu() {
        let mut nav = nav();
        for _ in 0..Screen::System.index() {
            nav.handle(Input::Right);
        }
        assert_eq!(nav.current(), Screen::System);
        nav.handle(Input::Select);
        assert_eq!(nav.menu_item(), Some(0));
        assert_eq!(nav.handle(Input::Select), None, "Back is not an action");
        nav.handle(Input::Select);
        nav.handle(Input::Down); // off Back, onto Reboot
        assert_eq!(nav.handle(Input::Select), Some(Action::Reboot));
        assert!(!nav.menu_is_open());
        assert_eq!(nav.handle(Input::Up), None, "not delivered twice");
    }

    /// Every one of oxinode's menus fits inside the content area of its
    /// own panel, whichever item is highlighted.
    #[test]
    fn no_menu_draws_outside_the_content_area() {
        for screen in Screen::ALL {
            for selected in 0..screen.menu().len() {
                let mut frame = Frame::new();
                monopanel::menu(&mut frame, screen.spec(), selected);
                for x in 0..sh1107::WIDTH {
                    for y in (0..LAYOUT.content_top).chain(LAYOUT.content_bottom..sh1107::HEIGHT) {
                        assert!(!frame.pixel(x, y), "{screen:?} drew at ({x}, {y})");
                    }
                }
                // And nothing was windowed: the square panel holds it all.
                assert_eq!(
                    monopanel::menu_window(&LAYOUT, screen.menu().len(), selected),
                    0..screen.menu().len()
                );
            }
        }
    }

    // ---- the editor level ------------------------------------------------

    fn on_radio_menu(item: usize) -> Nav {
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.handle(Input::Select);
        for _ in 0..item {
            nav.handle(Input::Down);
        }
        nav
    }

    /// The menu hands back `Edit(field)` and opens nothing itself: opening
    /// needs the configuration, which the caller has and the navigator does
    /// not.
    #[test]
    fn an_editor_is_asked_for_not_opened_by_the_menu() {
        for (n, field) in Field::ALL.iter().enumerate() {
            let mut nav = on_radio_menu(n + 1);
            assert_eq!(nav.handle(Input::Select), Some(Action::Edit(*field)));
            assert!(!nav.menu_is_open());
            assert!(!nav.is_editing(), "{field:?}");
            assert_eq!(nav.title(), "Radio");
        }
    }

    /// Confirming a legal value returns it once and closes the editor.
    #[test]
    fn confirming_returns_the_setting_and_closes() {
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.edit(Editor::open(Field::TxPower, DEFAULT));
        assert!(nav.is_editing());
        assert_eq!(nav.title(), "TX Power");
        assert_eq!(nav.handle(Input::Up), None);
        assert_eq!(
            nav.handle(Input::Select),
            Some(Action::Set(Setting::TxPower(DEFAULT.tx_power_dbm + 1)))
        );
        assert!(!nav.is_editing());
        assert_eq!(
            nav.current(),
            Screen::Radio,
            "back where the editor was opened"
        );
        assert_eq!(nav.handle(Input::Up), None, "not delivered twice");
    }

    /// **Cancel leaves the previous value in place.** Back closes the editor
    /// and hands back nothing, so there is no value for the caller to apply.
    #[test]
    fn cancel_returns_nothing_and_closes() {
        for field in Field::ALL {
            let mut nav = nav();
            nav.handle(Input::Right);
            nav.edit(Editor::open(field, DEFAULT));
            nav.handle(Input::Up);
            nav.handle(Input::Right);
            nav.handle(Input::Up);
            assert!(nav.editor().unwrap().changed(), "{field:?}");
            assert_eq!(nav.handle(Input::Back), None, "{field:?}");
            assert!(!nav.is_editing(), "{field:?}");
            assert_eq!(nav.editor(), None);
        }
    }

    /// A refused value keeps the editor open, with the refusal in it, and
    /// hands nothing back.
    #[test]
    fn a_refused_value_keeps_the_editor_open() {
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.edit(Editor::open(Field::TxPower, DEFAULT));
        for _ in 0..8 {
            nav.handle(Input::Up); // 14 -> 22
        }
        assert_eq!(nav.handle(Input::Select), None);
        assert!(nav.is_editing());
        let editor = nav.editor().unwrap();
        assert_eq!(editor.candidate(), 22);
        assert!(editor.refused().is_some());
        // Down clears it, and then the value goes through.
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(
            nav.handle(Input::Select),
            Some(Action::Set(Setting::TxPower(20)))
        );
    }

    /// Sideways inside an editor moves the cursor, not the screen.
    #[test]
    fn sideways_in_an_editor_does_not_change_screen() {
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.edit(Editor::open(Field::Frequency, DEFAULT));
        nav.handle(Input::Right);
        nav.handle(Input::Right);
        assert_eq!(nav.current(), Screen::Radio);
        assert_eq!(nav.editor().unwrap().cursor(), 2);
        nav.handle(Input::Left);
        assert_eq!(nav.editor().unwrap().cursor(), 1);
    }

    /// A notice is closed by any key, with nothing chosen, and the screen
    /// under it does not move.
    #[test]
    fn a_notice_closes_on_any_key() {
        for input in Input::ALL {
            let mut nav = nav();
            nav.handle(Input::Right);
            nav.notice(Lock::Usb);
            assert!(nav.is_editing());
            assert_eq!(nav.notice_shown(), Some(Lock::Usb));
            assert_eq!(nav.editor(), None);
            assert_eq!(nav.title(), "Locked");
            assert_eq!(nav.handle(input), None, "{input:?}");
            assert!(!nav.is_editing());
            assert_eq!(nav.current(), Screen::Radio, "{input:?}");
        }
    }

    /// The screen's scroll survives an editor being opened over it.
    #[test]
    fn editing_keeps_the_screens_scroll() {
        let visible = LAYOUT.visible_lines();
        let long: Vec<String> = (0..visible + 5).map(|n| n.to_string()).collect();
        let long_refs: Vec<&str> = long.iter().map(String::as_str).collect();
        let mut nav = nav();
        let mut frame = Frame::new();
        page(&mut frame, &mut nav, "", "", &long_refs);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.scroll(), 2);
        nav.edit(Editor::open(Field::TxPower, DEFAULT));
        page(&mut frame, &mut nav, "", "", &["one", "two"]);
        assert_eq!(nav.scroll(), 2, "the editor's short page did not clamp it");
        nav.handle(Input::Back);
        page(&mut frame, &mut nav, "", "", &long_refs);
        assert_eq!(nav.scroll(), 2);
    }

    /// An editor page has no scrollbar and carries the field's title.
    #[test]
    fn an_editor_page_is_titled_and_unscrolled() {
        let many: Vec<String> = (0..LAYOUT.visible_lines() + 5)
            .map(|n| n.to_string())
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut nav = nav();
        nav.handle(Input::Right);
        nav.edit(Editor::open(Field::Bandwidth, DEFAULT));
        let mut frame = Frame::new();
        page(&mut frame, &mut nav, "", "", &refs);
        let bar =
            (LAYOUT.content_top..LAYOUT.content_bottom).any(|y| frame.pixel(sh1107::WIDTH - 1, y));
        assert!(!bar, "an editor does not scroll");
        let mut chrome = Frame::new();
        chrome.fill(false);
        monopanel::title_bar(&mut chrome, "", "Bandwidth", "");
        let row = |f: &Frame| {
            (0..sh1107::WIDTH)
                .map(|x| f.pixel(x, 4))
                .collect::<Vec<_>>()
        };
        assert_eq!(row(&frame), row(&chrome));
    }

    /// The actions that a host on the line owns are exactly the ones that
    /// change the live radio; the rest are the panel's whatever is attached.
    #[test]
    fn the_host_owns_exactly_the_radio_actions() {
        let mut owned = Vec::new();
        let mut free = Vec::new();
        for screen in Screen::ALL {
            for item in screen.menu() {
                match item.action {
                    Some(action) if action.changes_the_radio() => owned.push(item.label),
                    Some(_) => free.push(item.label),
                    None => {}
                }
            }
        }
        assert_eq!(
            owned,
            [
                "Frequency",
                "Bandwidth",
                "Spread Factor",
                "Coding Rate",
                "TX Power",
                "Radio On/Off",
                "Reset Config",
            ]
        );
        assert!(
            free.contains(&"Save Config"),
            "storing is not the live radio"
        );
        assert!(free.contains(&"Forget Phones"));
        assert!(free.contains(&"Reboot"));
        assert!(
            free.contains(&"GPS On/Off"),
            "the receiver is not the radio, whoever has the line"
        );
        assert!(Action::Set(Setting::TxPower(1)).changes_the_radio());
    }
}
