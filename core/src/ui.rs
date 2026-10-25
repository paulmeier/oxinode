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
//! parameter, or a [`Lock`] notice saying why it cannot be edited right now,
//! or the crate's [`Keyboard`], typing a message in a conversation
//! ([`NavExt::compose`]). The keyboard's *Send* is [`Action::SendTyped`],
//! handed back with the keyboard still open: the caller reads the text from
//! it ([`NavExt::typing`]), and either closes it because the message was
//! written ([`NavExt::written`]) or puts the refusal in it
//! ([`NavExt::refuse`]), where the text is kept for another go.
//!
//! The other is the crate's [`Detail`]: oxinode's is a [`Conversation`],
//! the thread with one peer, opened from the Messages screen's list or the
//! Peers screen's -- from the second, before anything has been said. Like an
//! editor it is opened by the caller ([`NavExt::open_conversation`]), in
//! answer to [`Action::Open`], because which peer the highlighted line is
//! is the caller's to know.
//! An editor is opened by the caller with [`NavExt::edit`] rather than by
//! the menu itself, because opening one needs the configuration to edit and
//! the navigator is not handed that -- and because the caller is the one
//! that knows whether a host has the radio, in which case it opens a
//! [`NavExt::notice`] instead.

use monopanel::{Detail, Icon, Item, Keyboard, Layout, Modal, Outcome, Screen as Spec, Typed};

use crate::edit::{Confirm, Editor, Field, Lock};
use crate::lr1121::config::Setting;
use crate::screens::{Thread, THREAD};
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
    /// The conversations, one a line, each opening the thread with its
    /// peer. Only in an image with the node.
    #[cfg(feature = "node")]
    Messages,
    /// Who else is around: the peers the node has heard. Only in an image
    /// with the node; an RNode has no peers of its own.
    #[cfg(feature = "node")]
    Peers,
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
#[cfg(not(feature = "node"))]
const HOME_MENU: [Item<Action>; 3] = [
    Item::BACK,
    Item::new("Sleep Screen", Action::SleepScreen),
    Item::new("Redraw", Action::Redraw),
];

/// With the node, also *Announce Now*. Only there: an RNode has nothing of
/// its own to announce, and a menu item that answers with nothing teaches
/// people the board is broken.
#[cfg(feature = "node")]
const HOME_MENU: [Item<Action>; 4] = [
    Item::BACK,
    Item::new("Sleep Screen", Action::SleepScreen),
    Item::new("Redraw", Action::Redraw),
    Item::new("Announce Now", Action::Announce),
];

/// The lines a conversation's menu sends as they are: short, and the ones
/// a person answers with most, before there is a keyboard. Fixed in the
/// firmware; each fits a menu line and a packet many times over.
pub const REPLIES: [&str; 6] = [
    "OK",
    "Yes",
    "No",
    "On my way",
    "Can't talk now",
    "Where are you?",
];

/// The longest message the keyboard types: what fits in one packet, which
/// is `node::lxmf::MAX_CONTENT`, asserted equal there. A number here because
/// the interface is built without the node too.
pub const MESSAGE_MAX: usize = 287;

/// A message being typed: the crate's keyboard, held to [`MESSAGE_MAX`].
pub type Typing = Keyboard<MESSAGE_MAX>;

/// What a conversation offers: a message typed on the keyboard; the
/// replies, sent at once to the peer whose conversation it is; the board's
/// position, written when it is sent; and another go for whatever in the
/// conversation failed.
const REPLY_MENU: [Item<Action>; 10] = [
    Item::BACK,
    Item::new("Type a Message", Action::Compose),
    Item::new(REPLIES[0], Action::Reply(0)),
    Item::new(REPLIES[1], Action::Reply(1)),
    Item::new(REPLIES[2], Action::Reply(2)),
    Item::new(REPLIES[3], Action::Reply(3)),
    Item::new(REPLIES[4], Action::Reply(4)),
    Item::new(REPLIES[5], Action::Reply(5)),
    Item::new("Send Position", Action::SendPosition),
    Item::new("Retry Failed", Action::RetryFailed),
];

/// What [`Screen::Radio`] offers: the band, an editor for each of the five
/// parameters a host sets, then the three things that act on the
/// configuration whole. The band first, as it is on the screen.
const RADIO_MENU: [Item<Action>; 10] = [
    Item::BACK,
    Item::new(Field::Band.label(), Action::Edit(Field::Band)),
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

/// The icons, one per screen.
///
/// Drawn as ASCII art and converted by a generator, for the same reason the
/// font is: hand-entered hex has no symptom other than a wrong picture.
const HOUSE: Icon = [0x78, 0x7c, 0x06, 0x77, 0x06, 0x7c, 0x78];
#[cfg(feature = "node")]
const BUBBLE: Icon = [0x4e, 0x31, 0x15, 0x11, 0x15, 0x11, 0x0e]; // a speech bubble
#[cfg(feature = "node")]
const PEOPLE: Icon = [0x72, 0x77, 0x72, 0x00, 0x72, 0x77, 0x72]; // two people
const ANTENNA: Icon = [0x04, 0x12, 0x09, 0x7d, 0x09, 0x12, 0x04]; // radiating
const RUNE: Icon = [0x00, 0x22, 0x14, 0x7f, 0x36, 0x00, 0x00]; // Bluetooth's
const PIN: Icon = [0x1c, 0x36, 0x22, 0x7f, 0x22, 0x36, 0x1c]; // a map pin
const COG: Icon = [0x1c, 0x3e, 0x77, 0x22, 0x77, 0x3e, 0x1c];

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
        icon: HOUSE,
        menu: &HOME_MENU,
        open: None,
    },
    #[cfg(feature = "node")]
    Spec {
        title: "Messages",
        menu_title: "",
        icon: BUBBLE,
        menu: &[],
        open: Some(Action::Open),
    },
    #[cfg(feature = "node")]
    Spec {
        title: "Peers",
        menu_title: "",
        icon: PEOPLE,
        menu: &[],
        open: Some(Action::Open),
    },
    Spec {
        title: "Radio",
        menu_title: "Radio Action",
        icon: ANTENNA,
        menu: &RADIO_MENU,
        open: None,
    },
    Spec {
        title: "Bluetooth",
        menu_title: "Bluetooth Action",
        icon: RUNE,
        menu: &BLUETOOTH_MENU,
        open: None,
    },
    Spec {
        title: "Position",
        menu_title: "Position Action",
        icon: PIN,
        menu: &POSITION_MENU,
        open: None,
    },
    Spec {
        title: "System",
        menu_title: "System Action",
        icon: COG,
        menu: &SYSTEM_MENU,
        open: None,
    },
];

/// Two with the node, for the Messages and Peers screens; none without.
const NODE: usize = 2 * cfg!(feature = "node") as usize;

impl Screen {
    /// Every screen, in strip order. Messages and Peers sit next to Home:
    /// they are what a node is looked at for.
    pub const ALL: [Screen; 5 + NODE] = [
        Screen::Home,
        #[cfg(feature = "node")]
        Screen::Messages,
        #[cfg(feature = "node")]
        Screen::Peers,
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
            #[cfg(feature = "node")]
            Screen::Messages => 1,
            #[cfg(feature = "node")]
            Screen::Peers => 2,
            Screen::Radio => 1 + NODE,
            Screen::Bluetooth => 2 + NODE,
            Screen::Position => 3 + NODE,
            Screen::System => 4 + NODE,
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
    /// Announce the board now, as soon as the announce budget allows. Only
    /// offered in an image with the node.
    Announce,
    /// Open the conversation on the highlighted line of the Messages
    /// screen, or with the highlighted peer of the Peers screen, whether or
    /// not anything has been said. The caller answers with
    /// [`NavExt::open_conversation`]; see the module docs.
    Open,
    /// Open the keyboard over the open conversation. The caller answers
    /// with [`NavExt::compose`], as it does [`Action::Edit`].
    Compose,
    /// The keyboard's *Send*: write what it holds to the peer whose
    /// conversation is open, then [`NavExt::written`] or
    /// [`NavExt::refuse`]. See the module docs.
    SendTyped,
    /// Send [`REPLIES`]`[n]` to the peer whose conversation is open.
    Reply(u8),
    /// Send the board's position, as [`crate::screens::position_text`]
    /// writes it, to the peer whose conversation is open; or say why not.
    SendPosition,
    /// Queue again the messages that failed in the open conversation.
    RetryFailed,
}

impl Action {
    /// The text a canned reply sends; `None` for every other action.
    pub fn reply(self) -> Option<&'static str> {
        match self {
            Action::Reply(n) => REPLIES.get(usize::from(n)).copied(),
            _ => None,
        }
    }

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
            Action::Announce => "announce now",
            Action::Open => "open conversation",
            Action::Compose => "type a message",
            Action::SendTyped => "send typed",
            Action::Reply(n) if (n as usize) < REPLIES.len() => REPLIES[n as usize],
            Action::Reply(_) => "reply",
            Action::SendPosition => "send position",
            Action::RetryFailed => "retry failed",
        }
    }

    /// Whether carrying this out changes the live radio configuration or the
    /// radio's state, or puts something of the board's own on the air -- the
    /// things a host that has the line believes it owns.
    ///
    /// This is the host-ownership rule in one place. A caller with a host on
    /// the line refuses these and does the rest; see
    /// `docs/architecture/interface.md`. A message is not one of them: it is
    /// written under a host and held until the host has gone, and the
    /// thread says so.
    pub const fn changes_the_radio(self) -> bool {
        matches!(
            self,
            Action::ToggleRadio
                | Action::ResetRadioConfig
                | Action::Edit(_)
                | Action::Set(_)
                | Action::Announce
        )
    }
}

/// What can replace a screen's content: the third navigation level.
///
/// The keyboard makes it a few hundred bytes, the text being typed, where
/// the rest are a few. Not boxed: the RNode image has no allocator, and
/// there is one overlay, in the one navigator, for the board's life.
#[allow(clippy::large_enum_variant)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Overlay {
    /// An editor for one value.
    Editor(Editor),
    /// A notice saying why the value cannot be edited. Any key closes it.
    Notice(Lock),
    /// A notice saying why a message was not sent. Any key closes it.
    Refused(Refusal),
    /// A message being typed in the open conversation.
    Compose(Typing),
}

/// Why a message was not written, from a conversation's menu.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// *Send Position* with no fix: no position is sent rather than a wrong
    /// one.
    NoFix,
    /// Every message the board keeps is still on its way.
    Full,
    /// It does not fit in one packet. Never a canned line or the position,
    /// which are a few dozen bytes, or a typed message, which is held to
    /// [`MESSAGE_MAX`] as it is typed.
    TooLong,
    /// *Retry Failed* in a conversation where nothing has failed.
    NothingFailed,
}

impl Refusal {
    /// The word over the notice.
    pub const fn title(self) -> &'static str {
        match self {
            Refusal::NoFix | Refusal::Full | Refusal::TooLong => "Not Sent",
            Refusal::NothingFailed => "Retry",
        }
    }

    /// The first line of the notice.
    pub const fn headline(self) -> &'static str {
        match self {
            Refusal::NoFix => "No position to send.",
            Refusal::Full => "Too many on the way.",
            Refusal::TooLong => "Too long to send.",
            Refusal::NothingFailed => "Nothing has failed.",
        }
    }

    /// What it means, and what to do about it.
    pub const fn reason(self) -> &'static str {
        match self {
            Refusal::NoFix => "The GPS has no fix yet. See the Position screen.",
            Refusal::Full => "Wait for one to be delivered or to fail.",
            Refusal::TooLong => "A message goes in one packet, which this would not fit.",
            Refusal::NothingFailed => "Every message here is delivered or on its way.",
        }
    }
}

impl Modal<Action> for Overlay {
    /// The field while editing, `Locked` for a notice.
    fn title(&self) -> &'static str {
        match self {
            Overlay::Editor(editor) => editor.field().title(),
            Overlay::Notice(_) => "Locked",
            Overlay::Refused(refusal) => refusal.title(),
            Overlay::Compose(_) => "Message",
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
    ///
    /// The keyboard's keys are the keyboard's; its *Send* is
    /// [`Action::SendTyped`], with the keyboard left open for the answer.
    fn handle(&mut self, input: Input) -> Outcome<Action> {
        if let Overlay::Compose(typing) = self {
            return match typing.handle(input) {
                Typed::Stay => Outcome::Stay,
                Typed::Leave => Outcome::Close,
                Typed::Send => Outcome::Keep(Action::SendTyped),
            };
        }
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

/// The thread with one peer, open over the Messages screen: the crate's
/// detail level.
///
/// It holds which peer, and what the last drawing of the thread said about
/// it: where each message starts, for left and right to stop at, and
/// whether it has been placed yet -- at its first unread message, the first
/// time it is drawn. The messages themselves are the node's, and are handed
/// to [`crate::screens::render_with`] each time.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Conversation {
    peer: [u8; 16],
    stops: [u16; THREAD],
    count: u8,
    placed: bool,
    /// Go to the end when next drawn: something was just written there.
    to_end: bool,
}

impl Conversation {
    /// The conversation with `peer`, not yet drawn.
    pub const fn new(peer: [u8; 16]) -> Self {
        Conversation {
            peer,
            stops: [0; THREAD],
            count: 0,
            placed: false,
            to_end: false,
        }
    }

    /// Whose it is.
    pub const fn peer(&self) -> [u8; 16] {
        self.peer
    }

    /// Whether it has been drawn, and so placed, once.
    pub const fn placed(&self) -> bool {
        self.placed
    }

    /// Whether it is to be drawn at its end, as [`Conversation::show_end`]
    /// asked.
    pub const fn wants_end(&self) -> bool {
        self.to_end
    }

    /// Draw it at its end next time: the caller has just written a reply,
    /// which is there, and which the thread as last drawn did not have.
    pub fn show_end(&mut self) {
        self.to_end = true;
    }

    /// Learn where the thread's messages start, from the thread as it is
    /// drawn now, and that it has been placed.
    pub fn lay_out(&mut self, thread: &Thread<'_>) {
        self.count = thread.stops(&mut self.stops) as u8;
        self.placed = true;
        self.to_end = false;
    }

    fn stops(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.stops[..usize::from(self.count)]
            .iter()
            .map(|&s| usize::from(s))
    }
}

impl Detail<Action> for Conversation {
    fn title(&self) -> &'static str {
        "Thread"
    }

    fn menu_title(&self) -> &'static str {
        "Reply"
    }

    fn menu(&self) -> &'static [Item<Action>] {
        &REPLY_MENU
    }

    /// By message: right to the start of the next one, or the end if there
    /// is no next; left to the start of the one the view is in, or of the
    /// one before if the view starts on one.
    fn step(&self, top: usize, forward: bool) -> usize {
        if forward {
            self.stops().find(|&s| s > top).unwrap_or(usize::MAX)
        } else {
            self.stops().rev().find(|&s| s < top).unwrap_or(0)
        }
    }
}

/// Where the user is: the interface crate's navigator, over oxinode's
/// screens, actions, overlay and conversation.
pub type Nav = monopanel::Nav<Action, Overlay, Conversation>;

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
    /// Why a message was not sent, while that is showing.
    fn refusal_shown(&self) -> Option<Refusal>;
    /// Say why a message was not sent. The caller's answer to a reply it
    /// cannot write. Over a typed one, the keyboard says it and keeps the
    /// text.
    fn refuse(&mut self, refusal: Refusal);
    /// Open the keyboard, empty, over the open conversation: the caller's
    /// answer to [`Action::Compose`].
    fn compose(&mut self);
    /// The keyboard, while one is open.
    fn typing(&self) -> Option<&Typing>;
    /// A message from the conversation's menu or its keyboard was written:
    /// close the keyboard if it was typed there, and show the thread's end,
    /// where the message now is.
    fn written(&mut self);
    /// Whether an editor or a notice has replaced the screen's content.
    fn is_editing(&self) -> bool;
    /// Open an editor. The caller's answer to [`Action::Edit`] when nobody
    /// else has the radio. Replaces whatever overlay there was.
    fn edit(&mut self, editor: Editor);
    /// Show why the radio cannot be edited from here right now. The
    /// caller's answer to [`Action::Edit`] -- or to any action for which
    /// [`Action::changes_the_radio`] holds -- while a host has the line.
    fn notice(&mut self, lock: Lock);
    /// The conversation, while one is open.
    fn conversation(&self) -> Option<&Conversation>;
    /// The conversation, to tell it what was drawn.
    fn conversation_mut(&mut self) -> Option<&mut Conversation>;
    /// Open the conversation with `peer` over the Messages or the Peers
    /// screen: the caller's answer to [`Action::Open`].
    fn open_conversation(&mut self, peer: [u8; 16]);
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

    fn refusal_shown(&self) -> Option<Refusal> {
        match self.modal() {
            Some(Overlay::Refused(refusal)) => Some(*refusal),
            _ => None,
        }
    }

    fn refuse(&mut self, refusal: Refusal) {
        if let Some(Overlay::Compose(typing)) = self.modal_mut() {
            typing.refuse(refusal.headline(), refusal.reason());
        } else {
            self.open(Overlay::Refused(refusal));
        }
    }

    fn compose(&mut self) {
        self.open(Overlay::Compose(Typing::new()));
    }

    fn typing(&self) -> Option<&Typing> {
        match self.modal() {
            Some(Overlay::Compose(typing)) => Some(typing),
            _ => None,
        }
    }

    fn written(&mut self) {
        if self.typing().is_some() {
            self.close_modal();
        }
        if let Some(open) = self.conversation_mut() {
            open.show_end();
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

    fn conversation(&self) -> Option<&Conversation> {
        self.detail()
    }

    fn conversation_mut(&mut self) -> Option<&mut Conversation> {
        self.detail_mut()
    }

    fn open_conversation(&mut self, peer: [u8; 16]) {
        self.open_detail(Conversation::new(peer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lr1121::config::DEFAULT;
    use crate::sh1107::Frame;
    use monopanel::page;

    /// A navigator on the Radio screen, however many screens are before it.
    fn on_radio() -> Nav {
        let mut nav = nav();
        while nav.current() != Screen::Radio {
            nav.handle(Input::Right);
        }
        nav
    }

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

    /// Every menu starts with Back, so every menu can be left. A list has
    /// none: select opens what is highlighted.
    #[test]
    fn every_menu_can_be_left() {
        for screen in Screen::ALL {
            let items = screen.menu();
            if screen.spec().open.is_some() {
                assert!(items.is_empty(), "{screen:?} is a list with a menu");
                continue;
            }
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
                // Every menu but the Radio screen's is whole on the square
                // panel. The Radio menu's ten items are one more than it
                // holds, so its window slides by one, and whichever item is
                // highlighted is in it.
                let len = screen.menu().len();
                let window = monopanel::menu_window(&LAYOUT, len, selected);
                assert!(window.contains(&selected), "{screen:?} {selected}");
                if screen == Screen::Radio {
                    assert_eq!(window.len(), len - 1, "{screen:?} {selected}");
                } else {
                    assert_eq!(window, 0..len, "{screen:?} {selected}");
                }
            }
        }
    }

    // ---- conversations -------------------------------------------------

    /// The Messages screen sits next to Home, is a list, and hands back
    /// `Open` for its highlighted line; the conversation the caller opens
    /// over it is titled, has the reply menu, and keeps the strip on it.
    #[cfg(feature = "node")]
    #[test]
    fn messages_is_a_list_that_opens_a_conversation() {
        assert_eq!(Screen::Messages.index(), 1);
        assert_eq!(Screen::Messages.spec().open, Some(Action::Open));
        assert!(!Action::Open.changes_the_radio(), "reading is the panel's");
        let mut nav = nav();
        nav.handle(Input::Right);
        assert_eq!(nav.current(), Screen::Messages);
        nav.set_content(2, LAYOUT.visible_lines());
        nav.set_entries(2);
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(Action::Open));
        assert_eq!(nav.selected(), Some(1));
        nav.open_conversation([7; 16]);
        assert_eq!(nav.conversation().map(Conversation::peer), Some([7; 16]));
        assert_eq!(nav.title(), "Thread");
        assert_eq!(nav.current(), Screen::Messages);
        nav.handle(Input::Select);
        assert_eq!(nav.menu(), ("Reply", &REPLY_MENU[..]));
        assert_eq!(nav.handle(Input::Select), None, "Back is not an action");
        nav.handle(Input::Back);
        assert_eq!(nav.conversation(), None);
        assert_eq!(nav.selected(), Some(1), "back on the line it opened");
    }

    /// The Peers screen is a list too, of two lines a peer, and hands back
    /// `Open` for its highlighted peer: the conversation the caller opens
    /// over it has the reply menu, and back is the peer it was opened on.
    #[cfg(feature = "node")]
    #[test]
    fn peers_is_a_list_that_opens_a_conversation() {
        assert_eq!(Screen::Peers.spec().open, Some(Action::Open));
        assert!(Screen::Peers.menu().is_empty(), "select is taken");
        let mut nav = nav();
        while nav.current() != Screen::Peers {
            nav.handle(Input::Right);
        }
        nav.set_content(6, LAYOUT.visible_lines());
        nav.set_entries_of(3, crate::screens::PEER_LINES);
        nav.handle(Input::Down);
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(Action::Open));
        assert_eq!(nav.selected(), Some(2));
        nav.open_conversation([9; 16]);
        assert_eq!(nav.title(), "Thread");
        assert_eq!(nav.current(), Screen::Peers);
        nav.handle(Input::Select);
        assert_eq!(nav.menu(), ("Reply", &REPLY_MENU[..]));
        nav.handle(Input::Back);
        nav.handle(Input::Back);
        assert_eq!(nav.conversation(), None);
        assert_eq!(nav.selected(), Some(2), "back on the peer it opened");
    }

    /// The reply menu: Back, the keyboard, every canned line in order, the
    /// position and the retry. Each line is sent as it reads, and fits the
    /// menu.
    #[test]
    fn the_reply_menu_sends_each_line_as_it_reads() {
        assert_eq!(REPLY_MENU[0], Item::BACK);
        assert_eq!(REPLY_MENU[1].action, Some(Action::Compose));
        for (n, reply) in REPLIES.iter().enumerate() {
            let item = REPLY_MENU[2 + n];
            assert_eq!(item.label, *reply);
            assert_eq!(item.action, Some(Action::Reply(n as u8)));
            assert_eq!(item.action.unwrap().reply(), Some(*reply));
            assert!(reply.len() <= crate::screens::LINE_CHARS - 2, "{reply}");
        }
        assert_eq!(REPLY_MENU[8].action, Some(Action::SendPosition));
        assert_eq!(REPLY_MENU[9].action, Some(Action::RetryFailed));
        assert_eq!(Action::Reply(REPLIES.len() as u8).reply(), None);
        assert_eq!(Action::SendPosition.reply(), None);
        // Writing a message is not taking the radio from a host: it waits.
        for item in &REPLY_MENU[1..] {
            assert!(!item.action.unwrap().changes_the_radio(), "{}", item.label);
        }
        // And every action has a name of its own, the screens' included.
        let mut names: Vec<&str> = Screen::ALL
            .iter()
            .flat_map(|s| s.menu())
            .chain(&REPLY_MENU)
            .filter_map(|i| i.action.map(Action::name))
            .chain([Action::SendTyped.name()])
            .collect();
        let all = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all);
    }

    /// The reply menu's nine items fit the panel with whichever is
    /// highlighted in view, as the Radio menu's ten do.
    #[test]
    fn the_reply_menu_draws_inside_the_content_area() {
        for selected in 0..REPLY_MENU.len() {
            let mut frame = Frame::new();
            monopanel::menu_of(&mut frame, "Reply", &REPLY_MENU, selected);
            for x in 0..sh1107::WIDTH {
                for y in (0..LAYOUT.content_top).chain(LAYOUT.content_bottom..sh1107::HEIGHT) {
                    assert!(!frame.pixel(x, y), "item {selected} drew at ({x}, {y})");
                }
            }
            let window = monopanel::menu_window(&LAYOUT, REPLY_MENU.len(), selected);
            assert!(window.contains(&selected), "{selected}");
        }
    }

    /// A refusal is a notice: titled for what was refused, closed by any
    /// key, with nothing done.
    #[test]
    fn a_refusal_is_shown_and_any_key_closes_it() {
        for input in [Input::Up, Input::Left, Input::Select, Input::Back] {
            let mut nav = nav();
            nav.refuse(Refusal::NoFix);
            assert_eq!(nav.refusal_shown(), Some(Refusal::NoFix));
            assert_eq!(nav.title(), "Not Sent");
            assert!(nav.is_editing() && nav.notice_shown().is_none());
            assert_eq!(nav.handle(input), None);
            assert_eq!(nav.refusal_shown(), None, "{input:?}");
        }
        assert_eq!(Overlay::Refused(Refusal::NothingFailed).title(), "Retry");
    }

    /// Every title clears both corners of the title bar with a cell to
    /// spare, at their widest: a full battery and a board's name.
    #[test]
    fn every_title_clears_the_corners() {
        use monopanel::font::{width_of, ADVANCE};
        let width = LAYOUT.width;
        let left = 2 + width_of("100%");
        let right = width - 2 - width_of("7F23");
        let titles = Screen::ALL
            .iter()
            .map(|s| s.title())
            .chain([Conversation::new([0; 16]).title()]);
        for title in titles {
            let x = (width - width_of(title)) / 2;
            assert!(x >= left + ADVANCE, "{title} meets the battery");
            assert!(
                x + width_of(title) + ADVANCE <= right,
                "{title} meets the name"
            );
        }
    }

    /// Left and right move a conversation by message: to the next start,
    /// or the end; to the start of the one in view, or the one before.
    #[test]
    fn a_conversation_steps_by_message() {
        let mut c = Conversation::new([0; 16]);
        c.stops[..3].copy_from_slice(&[0, 4, 7]);
        c.count = 3;
        assert_eq!(c.step(0, true), 4);
        assert_eq!(c.step(5, true), 7);
        assert_eq!(c.step(7, true), usize::MAX, "the end");
        assert_eq!(c.step(7, false), 4);
        assert_eq!(c.step(6, false), 4, "the start of the one in view");
        assert_eq!(c.step(4, false), 0);
        assert_eq!(c.step(0, false), 0);
        let empty = Conversation::new([0; 16]);
        assert_eq!(empty.step(3, true), usize::MAX);
        assert_eq!(empty.step(3, false), 0);
        assert!(!empty.placed());
    }

    /// The speech bubble, drawn back as art: an icon typed as hex has no
    /// symptom but a wrong picture.
    #[cfg(feature = "node")]
    #[test]
    fn the_bubble_is_a_bubble() {
        let art: Vec<String> = (0..7)
            .map(|row| {
                BUBBLE
                    .iter()
                    .map(|col| if col >> row & 1 == 1 { '#' } else { '.' })
                    .collect()
            })
            .collect();
        assert_eq!(
            art,
            [".#####.", "#.....#", "#.#.#.#", "#.....#", ".#####.", ".#.....", "#......",]
        );
    }

    // ---- the editor level ------------------------------------------------

    fn on_radio_menu(item: usize) -> Nav {
        let mut nav = on_radio();
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
        let mut nav = on_radio();
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
            let mut nav = on_radio();
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
        let mut nav = on_radio();
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
        let mut nav = on_radio();
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
            let mut nav = on_radio();
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
        let mut nav = on_radio();
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
        // With the node, announcing puts the board's own packet on the air,
        // which is the host's while there is one.
        #[cfg(feature = "node")]
        assert_eq!(owned.first(), Some(&"Announce Now"));
        #[cfg(feature = "node")]
        owned.remove(0);
        assert_eq!(
            owned,
            [
                "Band",
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
        assert!(
            Action::Set(Setting::Band(crate::lr1121::pa::Band::HighFrequency)).changes_the_radio()
        );
    }

    /// A navigator with the keyboard open over the conversation with
    /// `[7; 16]`, as the menu's *Type a Message* and the caller's answer
    /// leave it.
    fn typing_to_seven() -> Nav {
        let mut nav = nav();
        nav.open_conversation([7; 16]);
        nav.handle(Input::Select);
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(Action::Compose));
        nav.compose();
        nav
    }

    /// Typing, then *Send*: the action comes back with the keyboard still
    /// open and holding the text, over the conversation, and closes when
    /// the caller says the message was written.
    #[test]
    fn send_hands_the_text_back_and_written_closes_it() {
        let mut nav = typing_to_seven();
        assert_eq!(nav.title(), "Message");
        assert!(!Action::Compose.changes_the_radio());
        assert!(!Action::SendTyped.changes_the_radio());
        nav.handle(Input::Select); // h
        nav.handle(Input::Up);
        nav.handle(Input::Right);
        nav.handle(Input::Right);
        nav.handle(Input::Select); // i
                                   // Down to Send: from row one, column seven, to column ten.
        for _ in 0..3 {
            nav.handle(Input::Right);
        }
        assert_eq!(nav.typing().unwrap().key(), monopanel::keyboard::Key::Send);
        assert_eq!(nav.handle(Input::Select), Some(Action::SendTyped));
        assert_eq!(nav.typing().map(Typing::text), Some("hi"));
        assert_eq!(nav.conversation().map(Conversation::peer), Some([7; 16]));
        nav.written();
        assert!(nav.typing().is_none());
        assert!(nav.conversation().unwrap().wants_end());
        assert_eq!(nav.title(), "Thread");
    }

    /// A refusal over the keyboard is said in it, and the text is kept;
    /// the refusal notice is for the menu's lines.
    #[test]
    fn a_refused_typed_message_keeps_its_text() {
        let mut nav = typing_to_seven();
        nav.handle(Input::Select);
        nav.handle(Input::Up);
        for _ in 0..5 {
            nav.handle(Input::Right);
        }
        assert_eq!(nav.handle(Input::Select), Some(Action::SendTyped));
        nav.refuse(Refusal::Full);
        assert_eq!(nav.refusal_shown(), None);
        let typing = nav.typing().unwrap();
        assert_eq!(typing.refused(), Some(Refusal::Full.headline()));
        assert_eq!(typing.text(), "h");
        nav.handle(Input::Back);
        assert_eq!(nav.typing().map(Typing::text), Some("h"), "Back read on");
    }

    /// Back on nothing typed leaves the keyboard for the thread under it.
    #[test]
    fn back_on_nothing_goes_back_to_the_thread() {
        let mut nav = typing_to_seven();
        assert_eq!(nav.handle(Input::Back), None);
        assert!(nav.typing().is_none());
        assert_eq!(nav.conversation().map(Conversation::peer), Some([7; 16]));
    }
}
