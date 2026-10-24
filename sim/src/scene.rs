//! A navigator and the state a page draws from, rendered the way the board
//! will.
//!
//! [`screens::render`](oxinode_core::screens::render) takes the navigator and
//! a [`State`] by reference, because on the board the caller copies the state
//! out of the modem loop once per redraw. Here the caller is this struct, and
//! the state is a fixture: a board that has just booted with nothing known,
//! one mid-session with every field filled in, or one
//! running on its own with no host attached, which is the one whose settings
//! the panel may change.
//!
//! The scene also plays the caller's part for the actions the navigator hands
//! back. An `Edit` is answered with an editor or a notice by the same rule
//! the firmware applies -- a host on the line owns the radio -- and a
//! confirmed `Set` lands in the scene's own state, so the screen after an
//! edit shows the value that was set. An `Open` opens the conversation on
//! the highlighted line, drawn from the scene's [`Recorded`] conversations,
//! and what a thread shows is marked read in them as the node would. Nothing
//! else is acted on; the actions are collected for the caller to look at.

use monopanel::Canvas;
use oxinode_core::battery;
use oxinode_core::edit::Editor;
use oxinode_core::gps;
use oxinode_core::lr1121::config::{ConfigError, RadioConfig, BENCH_2G4, DEFAULT};
use oxinode_core::rnode::air::Signal;
use oxinode_core::rnode::hosts::Transport;
use oxinode_core::screens::{
    self, Air, BluetoothScreen, ConversationRow, Home, Host, Identity, Mode, NodeHashes, PeerName,
    PeerRow, Peers, Position, Radio, State, System, PEERS,
};
use oxinode_core::sh1107::Frame;
use oxinode_core::status::Bluetooth;
use oxinode_core::ui::{self, Action, Input, Nav, NavExt};

use crate::conversation::{self, Recorded};
use crate::panel::{Panel, Size};

/// Which fixture a scene starts from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fixture {
    /// Nothing known: the screens say so.
    Empty,
    /// A board mid-session, every field filled in, a host on the line.
    Populated,
    /// A board on its own: a TNC with no host, whose settings the panel may
    /// change.
    Standalone,
    /// The standalone board on the other band: 2478 MHz on the u.FL.
    HighFrequency,
    /// The standalone board with the bundled conversations, some unread.
    Conversation,
}

impl Fixture {
    /// The fixture a word names, for the command line.
    pub fn named(word: &str) -> Option<Fixture> {
        match word {
            "empty" => Some(Fixture::Empty),
            "populated" => Some(Fixture::Populated),
            "standalone" => Some(Fixture::Standalone),
            "2g4" => Some(Fixture::HighFrequency),
            "conversation" => Some(Fixture::Conversation),
            _ => None,
        }
    }

    pub fn state(self) -> State {
        match self {
            Fixture::Empty => State::default(),
            Fixture::Populated => populated(),
            Fixture::Standalone => standalone(),
            Fixture::HighFrequency => high_frequency(),
            Fixture::Conversation => with_conversations(&conversation::bench()),
        }
    }
}

/// A board mid-session.
///
/// The numbers are the ones the README reports from the bench -- 915 MHz at
/// SF8, a peer heard at -69 dBm -- so the pictures show values a person would
/// recognise, and none of them is a round number that could be mistaken for a
/// default.
pub fn populated() -> State {
    State {
        name: Some(*b"7F23"),
        home: Home {
            host: Host::Usb,
            mode: Some(Mode::Rnode(Transport::Usb)),
            // Only an image with the node has messages. None unread draws
            // no row; `unread` is the fixture with some.
            unread: cfg!(feature = "node").then_some(0),
            talking: true,
            air: Air::Receiving,
            rx_count: 42,
            tx_count: 7,
            last_rssi_dbm: Some(-69),
            last_snr_quarter_db: Some(45),
            uptime_s: Some(3_723),
            battery: Some(battery::Reading {
                millivolts: 4_020,
                percent: 91,
            }),
            charger: battery::Charger::Charging,
        },
        radio: Radio {
            config: RadioConfig {
                frequency_hz: 915_000_000,
                bandwidth_hz: 125_000,
                spreading_factor: 8,
                coding_rate: 5,
                tx_power_dbm: 17,
                ..DEFAULT
            },
            air: Air::Receiving,
            tnc: true,
        },
        bluetooth: BluetoothScreen {
            link: Bluetooth::Advertising,
            name: Some(*b"RNode 7F23"),
            passkey: None,
            bonded: 2,
        },
        position: Position {
            status: gps::Status::Fix,
            sats_used: Some(7),
            sats_in_view: Some(11),
            fix: Some(gps::Fix {
                lat_udeg: 47_376_887,
                lon_udeg: 8_541_694,
                alt_dm: Some(4_080),
            }),
            fix_age_s: Some(3),
            time: Some(gps::Time {
                hour: 13,
                minute: 47,
                second: 9,
            }),
            date: Some(gps::Date {
                year: 2026,
                month: 9,
                day: 8,
            }),
        },
        system: System {
            version: "0.0.0",
            build: "0000000",
            serial: Some(*b"0123456789ABCDEF"),
            identity: Identity::Signed,
            // The vectors' peer, so the picture holds an address Python
            // computed: `tools/node_vectors.py`.
            node: Some(NodeHashes {
                identity: [
                    0xac, 0xa3, 0x1a, 0xf0, 0x44, 0x1d, 0x81, 0xdb, 0xec, 0x71, 0xe8, 0x2d, 0xa0,
                    0xb4, 0xb5, 0xf5,
                ],
                lxmf: [
                    0xfa, 0xe3, 0x21, 0xc4, 0x42, 0xe3, 0xc9, 0xbd, 0xcd, 0x7a, 0x3e, 0x79, 0xd8,
                    0x50, 0xe0, 0x3c,
                ],
            }),
            // The populated configuration, SF8 at 125 kHz: the board's
            // 183-byte announce is 0.52 s on the air, so 26 s between two.
            announce_gap_s: Some(26),
            free_ram: Some(126_976),
        },
        peers: three_peers(),
        // Only an image with the node has the Messages screen; the bundled
        // conversations, so that opening one shows a thread.
        conversations: conversation::list(&conversation::bench()),
    }
}

/// The bench peer as the board hears it from across the desk: the vectors'
/// name and address, at the populated board's signal.
fn bench_peer() -> PeerRow {
    PeerRow {
        destination: [
            0xfa, 0xe3, 0x21, 0xc4, 0x42, 0xe3, 0xc9, 0xbd, 0xcd, 0x7a, 0x3e, 0x79, 0xd8, 0x50,
            0xe0, 0x3c,
        ],
        name: Some(PeerName::new("Bench peer")),
        age_s: Some(14),
        hops: Some(1),
        signal: Some(Signal {
            rssi_dbm: -69,
            snr_quarter_db: 45,
        }),
    }
}

/// Three peers: the bench peer, a phone heard through another board, and a
/// board that announced with no name.
fn three_peers() -> Peers {
    let mut peers = Peers::default();
    peers.rows[0] = Some(bench_peer());
    peers.rows[1] = Some(PeerRow {
        destination: [0x5d; 16],
        name: Some(PeerName::new("Galaxy S9")),
        age_s: Some(95),
        hops: Some(2),
        signal: None,
    });
    peers.rows[2] = Some(nameless());
    peers
}

fn nameless() -> PeerRow {
    PeerRow {
        destination: [
            0x3b, 0x07, 0x9e, 0x41, 0xd2, 0x88, 0x10, 0x6c, 0x2f, 0xe5, 0x93, 0x5a, 0x0d, 0x71,
            0xc4, 0xb6,
        ],
        name: None,
        age_s: Some(1_260),
        hops: Some(1),
        signal: Some(Signal {
            rssi_dbm: -112,
            snr_quarter_db: -38,
        }),
    }
}

/// One peer: the bench peer.
pub fn one_peer() -> Peers {
    let mut peers = Peers::default();
    peers.rows[0] = Some(bench_peer());
    peers
}

/// As many peers as the node keeps, heard a little longer ago down the list.
pub fn full_of_peers() -> Peers {
    let mut peers = Peers::default();
    for (n, slot) in peers.rows.iter_mut().enumerate() {
        let mut name = String::from("oxinode ");
        name.push_str(&format!("{:04X}", 0x1a2b + n * 0x111));
        *slot = Some(PeerRow {
            destination: [n as u8; 16],
            name: Some(PeerName::new(&name)),
            age_s: Some(8 + 37 * n as u32 * n as u32),
            hops: Some(1 + (n % 3) as u8),
            signal: (n % 3 == 0).then_some(Signal {
                rssi_dbm: -70 - n as i16,
                snr_quarter_db: 30 - 4 * n as i8,
            }),
        });
    }
    debug_assert_eq!(peers.rows.len(), PEERS);
    peers
}

/// One peer that announced without a name.
pub fn nameless_peer() -> Peers {
    let mut peers = Peers::default();
    peers.rows[0] = Some(nameless());
    peers
}

/// One peer with nothing known but its address.
pub fn unknown_peer() -> Peers {
    let mut peers = Peers::default();
    peers.rows[0] = Some(PeerRow {
        name: None,
        age_s: None,
        hops: None,
        signal: None,
        ..nameless()
    });
    peers
}

/// The populated board with its receiver on and still looking: satellites
/// in view, a time, and no fix yet -- the state the screen spends its first
/// minute in after the switch is thrown.
pub fn searching() -> State {
    let mut state = populated();
    state.position = Position {
        status: gps::Status::Searching,
        sats_used: Some(0),
        sats_in_view: Some(5),
        fix: None,
        fix_age_s: None,
        time: Some(gps::Time {
            hour: 13,
            minute: 46,
            second: 2,
        }),
        date: state.position.date,
    };
    state
}

/// The populated board with nobody on the line: a TNC running on its own,
/// which is the case the editors exist for, and in an image with the node a
/// node of its own. Same numbers, so the editors open on values a person
/// would recognise from the other pictures.
pub fn standalone() -> State {
    let mut state = populated();
    state.home.host = Host::None;
    state.home.talking = false;
    state.home.mode = Some(Mode::Node);
    state.radio.tnc = true;
    state
}

/// The standalone board with two messages it has not shown anyone yet.
pub fn unread() -> State {
    let mut state = standalone();
    state.home.unread = Some(2);
    state
}

/// The standalone board with `recorded` as its conversations, and Home
/// counting what in them is unread.
pub fn with_conversations(recorded: &[Recorded]) -> State {
    let mut state = standalone();
    state.conversations = conversation::list(recorded);
    let unread = recorded
        .iter()
        .flat_map(|r| &r.messages)
        .filter(|m| m.unread)
        .count();
    state.home.unread = Some(unread as u16);
    state
}

/// As many conversations as the screen lists, a little older down the list,
/// every other one unread. None of them has a thread to open.
pub fn full_of_conversations() -> State {
    let mut state = standalone();
    for (n, slot) in state.conversations.rows.iter_mut().enumerate() {
        *slot = Some(ConversationRow {
            peer: [n as u8 + 0x40; 16],
            name: Some(PeerName::new(&format!(
                "oxinode {:04X}",
                0x1a2b + n * 0x111
            ))),
            unread: n % 2 == 0,
            age_s: Some(30 + 41 * n as u32 * n as u32),
        });
    }
    state
}

/// The standalone board holding the bench's 2.4 GHz configuration: 2478 MHz,
/// 812.5 kHz, SF8, 11 dBm, uncorrected. What the screens show on the other
/// band, and the editors opening on a frequency with four megahertz digits.
pub fn high_frequency() -> State {
    let mut state = standalone();
    state.radio.config = BENCH_2G4;
    state
}

/// The populated board with a phone on the line instead of USB.
pub fn phone() -> State {
    let mut state = populated();
    state.home.host = Host::Bluetooth;
    state.bluetooth.link = Bluetooth::Connected;
    state
}

/// The populated board, with a phone waiting for its passkey.
pub fn pairing() -> State {
    let mut state = populated();
    state.bluetooth.link = Bluetooth::Connected;
    state.bluetooth.passkey = Some(29_717);
    state
}

/// The populated board, with a configuration the radio refused.
pub fn refused() -> State {
    let mut state = populated();
    state.radio.config.tx_power_dbm = 22;
    state.radio.air = Air::Refused(ConfigError::PowerAboveModuleRating);
    state.home.air = state.radio.air;
    state
}

/// Where the user is, plus what the screens draw from.
#[derive(Clone, Debug)]
pub struct Scene {
    pub nav: Nav,
    pub state: State,
    /// The conversations a thread is drawn from, by its peer: the bundled
    /// ones unless the scene is given others.
    pub recorded: Vec<Recorded>,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    /// Home, at the top, with nothing known.
    pub fn new() -> Self {
        Self::with_state(State::default())
    }

    /// Home, at the top, with the given state.
    pub fn with_state(state: State) -> Self {
        Scene {
            nav: ui::nav(),
            state,
            recorded: conversation::bench(),
        }
    }

    /// Home, at the top, on the standalone board with `recorded` as its
    /// conversations.
    pub fn with_conversations(recorded: Vec<Recorded>) -> Self {
        Scene {
            nav: ui::nav(),
            state: with_conversations(&recorded),
            recorded,
        }
    }

    /// Render the page as it stands, on the board's own frame.
    ///
    /// Takes `&mut self` because the page tells the navigator how tall the
    /// content is; see [`monopanel::page`].
    pub fn frame(&mut self) -> Frame {
        let mut frame = Frame::new();
        self.draw(&mut frame);
        frame
    }

    /// Render the page as it stands, on a panel of any size.
    pub fn panel(&mut self, size: Size) -> Panel {
        let mut panel = Panel::new(size);
        self.draw(&mut panel);
        panel
    }

    /// Render the page as it stands, on whatever is handed in.
    ///
    /// With a conversation open, its thread is the recorded one with that
    /// peer, if there is one, and what it shows is marked read -- in the
    /// recording, and on the Messages screen and Home, as on the board.
    pub fn draw(&mut self, canvas: &mut impl Canvas) {
        let open = self.nav.conversation().map(|c| c.peer());
        let recorded = self.recorded.iter().find(|r| Some(r.peer) == open);
        let entries = recorded.map(Recorded::entries).unwrap_or_default();
        let thread = recorded.map(|r| r.thread(&entries));
        let read = screens::render_with(canvas, &mut self.nav, &self.state, thread.as_ref());
        let Some(read) = read else { return };
        let Some(recorded) = self.recorded.iter_mut().find(|r| r.peer == read.peer) else {
            return;
        };
        recorded.read(read);
        let row = recorded.row();
        for slot in self.state.conversations.rows.iter_mut().flatten() {
            if slot.peer == row.peer {
                slot.unread = row.unread;
            }
        }
        if self.state.home.unread.is_some() {
            let unread = self.recorded.iter().flat_map(|r| &r.messages);
            self.state.home.unread = Some(unread.filter(|m| m.unread).count() as u16);
        }
    }

    /// Press one key. Returns what the board would now have to do, if
    /// anything.
    ///
    /// The page is rendered first, as it would have been on the board before
    /// the key was pressed, so that a `Down` on a fresh screen knows how far
    /// it can go. Then the caller's part is played -- see the module docs --
    /// and the action is returned as the firmware would have received it.
    ///
    /// Rendered on the board's frame: a scene is the board's, and how far a
    /// `Down` goes is a fact about the panel the board has. A caller looking
    /// at another panel size renders with [`Scene::panel`] and presses with
    /// [`Scene::press_on`].
    pub fn press(&mut self, input: Input) -> Option<Action> {
        self.press_on(Size::SQUARE, input)
    }

    /// [`Scene::press`], with the page rendered on a panel of `size` first,
    /// so the scroll is that panel's.
    pub fn press_on(&mut self, size: Size, input: Input) -> Option<Action> {
        if size == Size::SQUARE {
            self.frame();
        } else {
            self.panel(size);
        }
        let action = self.nav.handle(input)?;
        self.answer(action);
        Some(action)
    }

    /// Do what the firmware does with an action, as far as a fixture can.
    fn answer(&mut self, action: Action) {
        let lock = self.state.home.host.lock();
        match (action, lock) {
            // A host has the radio: every action that would change it gets
            // the notice instead, exactly as on the board.
            (action, Some(lock)) if action.changes_the_radio() => self.nav.notice(lock),
            (Action::Edit(field), None) => {
                self.nav.edit(Editor::open(field, self.state.radio.config));
            }
            (Action::Set(setting), None) => self.state.radio.config.apply(setting),
            (Action::Open, _) => {
                let rows = &self.state.conversations.rows;
                if let Some(row) = self.nav.selected().and_then(|n| rows[n]) {
                    self.nav.open_conversation(row.peer);
                }
            }
            _ => {}
        }
    }

    /// Press every key in a script, in order, returning the actions chosen.
    pub fn run(&mut self, inputs: &[Input]) -> Vec<Action> {
        self.run_on(Size::SQUARE, inputs)
    }

    /// [`Scene::run`], on a panel of `size`.
    pub fn run_on(&mut self, size: Size, inputs: &[Input]) -> Vec<Action> {
        inputs
            .iter()
            .filter_map(|&input| self.press_on(size, input))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script;
    use monopanel::Readable;
    use oxinode_core::ui::Screen;

    #[test]
    fn a_scene_starts_at_home_with_the_menu_shut() {
        let scene = Scene::new();
        assert_eq!(scene.nav.current(), Screen::Home);
        assert!(!scene.nav.menu_is_open());
        assert_eq!(scene.state, State::default());
    }

    #[test]
    fn a_script_walks_the_menus_and_collects_the_actions() {
        let mut scene = Scene::new();
        let inputs = script::parse(&format!(
            "right*{} select down select",
            Screen::System.index()
        ))
        .unwrap();
        assert_eq!(scene.run(&inputs), [Action::Reboot]);
        assert_eq!(scene.nav.current(), Screen::System);
        assert!(!scene.nav.menu_is_open());
    }

    /// A press renders first, so scrolling on a fresh screen works: the
    /// populated radio screen is longer than the panel.
    #[test]
    fn scrolling_works_from_the_first_press() {
        let mut scene = Scene::with_state(populated());
        for _ in 0..Screen::Radio.index() {
            scene.press(Input::Right);
        }
        assert_eq!(scene.nav.current(), Screen::Radio);
        scene.press(Input::Down);
        assert_eq!(scene.nav.scroll(), 1);
    }

    /// With nobody on the line, choosing a field opens its editor, and a
    /// confirmed value lands in the state the screens draw from.
    #[test]
    fn an_edit_on_a_standalone_board_changes_the_state() {
        use oxinode_core::lr1121::config::Setting;
        let mut scene = Scene::with_state(standalone());
        let inputs = script::parse(&format!(
            "right*{} select down*6 select",
            Screen::Radio.index()
        ))
        .unwrap();
        assert_eq!(
            scene.run(&inputs),
            [Action::Edit(oxinode_core::edit::Field::TxPower)]
        );
        assert!(scene.nav.is_editing());
        let more = script::parse("up select").unwrap();
        assert_eq!(scene.run(&more), [Action::Set(Setting::TxPower(18))]);
        assert!(!scene.nav.is_editing());
        assert_eq!(scene.state.radio.config.tx_power_dbm, 18);
        assert_eq!(scene.nav.current(), Screen::Radio);
    }

    /// Cancel leaves the state as it was.
    #[test]
    fn a_cancelled_edit_leaves_the_state_alone() {
        let mut scene = Scene::with_state(standalone());
        let before = scene.state;
        let inputs = script::parse(&format!(
            "right*{} select down select up up back",
            Screen::Radio.index()
        ))
        .unwrap();
        let actions = scene.run(&inputs);
        assert_eq!(actions.len(), 1, "only the Edit: {actions:?}");
        assert_eq!(scene.state, before);
        assert!(!scene.nav.is_editing());
    }

    /// With a host on the line, the same presses open the notice instead,
    /// and so does anything else that would change the radio.
    #[test]
    fn a_host_on_the_line_turns_edits_into_the_notice() {
        let mut scene = Scene::with_state(populated());
        scene.run(
            &script::parse(&format!(
                "right*{} select down select",
                Screen::Radio.index()
            ))
            .unwrap(),
        );
        assert_eq!(
            scene.nav.notice_shown(),
            Some(oxinode_core::edit::Lock::Usb)
        );
        assert_eq!(scene.nav.editor(), None);
        scene.press(Input::Back);
        // Radio On/Off is the eighth item.
        scene.run(&script::parse("select down*7 select").unwrap());
        assert!(scene.nav.notice_shown().is_some());
        scene.press(Input::Back);
        // Save Config is not the live radio, and goes through.
        let actions = scene.run(&script::parse("select down*8 select").unwrap());
        assert_eq!(actions, [Action::SaveRadioConfig]);
        assert!(!scene.nav.is_editing());

        let mut scene = Scene::with_state(phone());
        scene.run(
            &script::parse(&format!(
                "right*{} select down select",
                Screen::Radio.index()
            ))
            .unwrap(),
        );
        assert_eq!(
            scene.nav.notice_shown(),
            Some(oxinode_core::edit::Lock::Bluetooth)
        );
    }

    /// Opening the highlighted conversation shows its thread; what the
    /// thread shows is read, on the Messages screen and on Home, as on the
    /// board; back is the list, on the line it was opened from.
    #[cfg(feature = "node")]
    #[test]
    fn a_recorded_conversation_is_opened_read_and_left() {
        let mut scene = Scene::with_state(Fixture::Conversation.state());
        let before = scene.state.home.unread.unwrap();
        let actions = scene.run(&script::parse("right down*3 select").unwrap());
        assert_eq!(actions, [Action::Open]);
        let row = scene.state.conversations.rows[3].unwrap();
        assert_eq!(scene.nav.conversation().map(|c| c.peer()), Some(row.peer));
        assert!(row.unread, "the stranger's question");
        scene.frame();
        assert!(!scene.state.conversations.rows[3].unwrap().unread);
        assert_eq!(scene.state.home.unread, Some(before - 1));
        scene.press(Input::Back);
        assert_eq!(scene.nav.conversation(), None);
        assert_eq!(scene.nav.selected(), Some(3));
        // A screen with no conversations opens nothing.
        let mut empty = Scene::with_state(standalone());
        empty.state.conversations = Default::default();
        assert!(empty
            .run(&script::parse("right select").unwrap())
            .is_empty());
        assert_eq!(empty.nav.conversation(), None);
    }

    /// The frame is the page the core would draw for the same state.
    #[test]
    fn the_frame_is_the_core_page() {
        let mut scene = Scene::with_state(populated());
        scene.press(Input::Right);
        let got = scene.frame();
        let mut nav = ui::nav();
        nav.handle(Input::Right);
        let mut want = Frame::new();
        screens::render(&mut want, &mut nav, &populated());
        assert_eq!(got.as_bytes(), want.as_bytes());
        // And a panel the board's size is the frame, pixel for pixel.
        let panel = scene.panel(Size::SQUARE);
        assert_eq!(panel, Panel::from_frame(&got));
    }

    /// On a shorter panel the same scene shows fewer lines, so a page that
    /// fits the board scrolls there, and the scroll is the panel's.
    #[test]
    fn a_short_panel_scrolls_where_the_square_one_does_not() {
        let mut scene = Scene::with_state(populated());
        // Home has ten lines: fits eleven, not four.
        let mut square = Scene::with_state(populated());
        square.run(&script::parse("down*3").unwrap());
        assert_eq!(square.nav.scroll(), 0);
        scene.run_on(Size::WIDE, &script::parse("down*3").unwrap());
        assert_eq!(scene.nav.scroll(), 3);
        let wide = scene.panel(Size::WIDE);
        assert_eq!((wide.width(), wide.height()), (128, 64));
        let layout = monopanel::Layout::of(128, 64);
        let bar = (layout.content_top..layout.content_bottom).any(|y| wide.pixel(127, y));
        assert!(bar, "a scrollbar on the short panel");
    }

    /// The fixtures are what their names say.
    #[test]
    fn the_fixtures_are_distinct_and_named() {
        assert_eq!(Fixture::named("empty"), Some(Fixture::Empty));
        assert_eq!(Fixture::named("populated"), Some(Fixture::Populated));
        assert_eq!(Fixture::named("standalone"), Some(Fixture::Standalone));
        assert_eq!(Fixture::named("2g4"), Some(Fixture::HighFrequency));
        assert_eq!(Fixture::named("conversation"), Some(Fixture::Conversation));
        let talk = Fixture::Conversation.state();
        assert_eq!(talk.home.host, Host::None);
        assert!(talk.home.unread.unwrap() > 0);
        assert!(talk.conversations.rows[0].is_some());
        assert_eq!(Fixture::named("full"), None);
        assert_eq!(standalone().home.host, Host::None);
        assert!(standalone().radio.tnc);
        assert_eq!(standalone().home.mode, Some(Mode::Node));
        assert_eq!(standalone().radio.config, populated().radio.config);
        // The other band's board is the standalone one on 2478 MHz, and the
        // configuration it holds is one the radio accepts.
        assert_eq!(high_frequency().home.host, Host::None);
        assert_eq!(high_frequency().radio.config.frequency_hz, 2_478_000_000);
        assert!(high_frequency().radio.config.check().is_ok());
        assert_eq!(Fixture::HighFrequency.state(), high_frequency());
        assert_eq!(Fixture::Empty.state(), State::default());
        assert_ne!(Fixture::Populated.state(), State::default());
        assert!(pairing().bluetooth.passkey.is_some());
        assert!(matches!(refused().radio.air, Air::Refused(_)));
        // The populated board is a real one: the numbers are not defaults.
        let p = populated();
        assert_ne!(p.radio.config, DEFAULT);
        assert!(p.home.battery.is_some() && p.home.uptime_s.is_some());
        assert!(p.system.serial.is_some() && p.system.free_ram.is_some());
        // And its radio screen scrolls, which the golden images rely on.
        let mut lines = screens::Lines::new();
        p.lines(Screen::Radio, &mut lines);
        assert!(lines.len() > ui::LAYOUT.visible_lines());
    }
}
