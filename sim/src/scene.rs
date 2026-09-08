//! A navigator and the state a page draws from, rendered the way the board
//! will.
//!
//! [`screens::render`](oxinode_core::screens::render) takes the navigator and
//! a [`State`] by reference, because on the board the caller copies the state
//! out of the modem loop once per redraw. Here the caller is this struct, and
//! the state is a fixture: a board that has just booted with nothing known,
//! one mid-session with every field filled in, or -- since phase 12 -- one
//! running on its own with no host attached, which is the one whose settings
//! the panel may change.
//!
//! The scene also plays the caller's part for the actions the navigator hands
//! back. An `Edit` is answered with an editor or a notice by the same rule
//! the firmware applies -- a host on the line owns the radio -- and a
//! confirmed `Set` lands in the scene's own state, so the screen after an
//! edit shows the value that was set. Nothing else is acted on; the actions
//! are collected for the caller to look at.

use oxinode_core::battery;
use oxinode_core::edit::Editor;
use oxinode_core::lr1121::config::{ConfigError, RadioConfig, DEFAULT};
use oxinode_core::screens::{
    self, Air, BluetoothScreen, Home, Host, Identity, Position, Radio, State, System,
};
use oxinode_core::sh1107::Frame;
use oxinode_core::status::Bluetooth;
use oxinode_core::ui::{Action, Input, Nav};

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
}

impl Fixture {
    /// The fixture a word names, for the command line.
    pub fn named(word: &str) -> Option<Fixture> {
        match word {
            "empty" => Some(Fixture::Empty),
            "populated" => Some(Fixture::Populated),
            "standalone" => Some(Fixture::Standalone),
            _ => None,
        }
    }

    pub fn state(self) -> State {
        match self {
            Fixture::Empty => State::default(),
            Fixture::Populated => populated(),
            Fixture::Standalone => standalone(),
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
        home: Home {
            host: Host::Usb,
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
            charging: true,
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
        position: Position,
        system: System {
            version: "0.0.0",
            serial: Some(*b"0123456789ABCDEF"),
            identity: Identity::Signed,
            free_ram: Some(126_976),
        },
    }
}

/// The populated board with nobody on the line: a TNC running on its own,
/// which is the case phase 12 exists for. Same numbers, so the editors open
/// on values a person would recognise from the other pictures.
pub fn standalone() -> State {
    let mut state = populated();
    state.home.host = Host::None;
    state.home.talking = false;
    state.radio.tnc = true;
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
            nav: Nav::new(),
            state,
        }
    }

    /// Render the page as it stands.
    ///
    /// Takes `&mut self` because the page tells the navigator how tall the
    /// content is; see [`oxinode_core::ui::page`].
    pub fn frame(&mut self) -> Frame {
        let mut frame = Frame::new();
        screens::render(&mut frame, &mut self.nav, &self.state);
        frame
    }

    /// Press one key. Returns what the board would now have to do, if
    /// anything.
    ///
    /// The page is rendered first, as it would have been on the board before
    /// the key was pressed, so that a `Down` on a fresh screen knows how far
    /// it can go. Then the caller's part is played -- see the module docs --
    /// and the action is returned as the firmware would have received it.
    pub fn press(&mut self, input: Input) -> Option<Action> {
        self.frame();
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
            _ => {}
        }
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
    use oxinode_core::ui::{self, Screen};

    #[test]
    fn a_scene_starts_at_home_with_the_menu_shut() {
        let scene = Scene::new();
        assert_eq!(scene.nav.screen(), Screen::Home);
        assert!(!scene.nav.menu_is_open());
        assert_eq!(scene.state, State::default());
    }

    #[test]
    fn a_script_walks_the_menus_and_collects_the_actions() {
        let mut scene = Scene::new();
        let inputs = script::parse("right*4 select down select").unwrap();
        assert_eq!(scene.run(&inputs), [Action::Reboot]);
        assert_eq!(scene.nav.screen(), Screen::System);
        assert!(!scene.nav.menu_is_open());
    }

    /// A press renders first, so scrolling on a fresh screen works: the
    /// populated radio screen is longer than the panel.
    #[test]
    fn scrolling_works_from_the_first_press() {
        let mut scene = Scene::with_state(populated());
        scene.press(Input::Right);
        assert_eq!(scene.nav.screen(), Screen::Radio);
        scene.press(Input::Down);
        assert_eq!(scene.nav.scroll(), 1);
    }

    /// With nobody on the line, choosing a field opens its editor, and a
    /// confirmed value lands in the state the screens draw from.
    #[test]
    fn an_edit_on_a_standalone_board_changes_the_state() {
        use oxinode_core::lr1121::config::Setting;
        let mut scene = Scene::with_state(standalone());
        let inputs = script::parse("right select down*5 select").unwrap();
        assert_eq!(
            scene.run(&inputs),
            [Action::Edit(oxinode_core::edit::Field::TxPower)]
        );
        assert!(scene.nav.is_editing());
        let more = script::parse("up select").unwrap();
        assert_eq!(scene.run(&more), [Action::Set(Setting::TxPower(18))]);
        assert!(!scene.nav.is_editing());
        assert_eq!(scene.state.radio.config.tx_power_dbm, 18);
        assert_eq!(scene.nav.screen(), Screen::Radio);
    }

    /// Cancel leaves the state as it was.
    #[test]
    fn a_cancelled_edit_leaves_the_state_alone() {
        let mut scene = Scene::with_state(standalone());
        let before = scene.state;
        let inputs = script::parse("right select down select up up back").unwrap();
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
        scene.run(&script::parse("right select down select").unwrap());
        assert_eq!(
            scene.nav.notice_shown(),
            Some(oxinode_core::edit::Lock::Usb)
        );
        assert_eq!(scene.nav.editor(), None);
        scene.press(Input::Back);
        // Radio On/Off is the seventh item.
        scene.run(&script::parse("select down*6 select").unwrap());
        assert!(scene.nav.notice_shown().is_some());
        scene.press(Input::Back);
        // Save Config is not the live radio, and goes through.
        let actions = scene.run(&script::parse("select down*7 select").unwrap());
        assert_eq!(actions, [Action::SaveRadioConfig]);
        assert!(!scene.nav.is_editing());

        let mut scene = Scene::with_state(phone());
        scene.run(&script::parse("right select down select").unwrap());
        assert_eq!(
            scene.nav.notice_shown(),
            Some(oxinode_core::edit::Lock::Bluetooth)
        );
    }

    /// The frame is the page the core would draw for the same state.
    #[test]
    fn the_frame_is_the_core_page() {
        let mut scene = Scene::with_state(populated());
        scene.press(Input::Right);
        let got = scene.frame();
        let mut nav = Nav::new();
        nav.handle(Input::Right);
        let mut want = Frame::new();
        screens::render(&mut want, &mut nav, &populated());
        assert_eq!(got.as_bytes(), want.as_bytes());
    }

    /// The fixtures are what their names say.
    #[test]
    fn the_fixtures_are_distinct_and_named() {
        assert_eq!(Fixture::named("empty"), Some(Fixture::Empty));
        assert_eq!(Fixture::named("populated"), Some(Fixture::Populated));
        assert_eq!(Fixture::named("standalone"), Some(Fixture::Standalone));
        assert_eq!(Fixture::named("full"), None);
        assert_eq!(standalone().home.host, Host::None);
        assert!(standalone().radio.tnc);
        assert_eq!(standalone().radio.config, populated().radio.config);
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
        assert!(lines.len() > ui::visible_lines());
    }
}
