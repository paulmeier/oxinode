//! A navigator and the state a page draws from, rendered the way the board
//! will.
//!
//! [`screens::render`](oxinode_core::screens::render) takes the navigator and
//! a [`State`] by reference, because on the board the caller copies the state
//! out of the modem loop once per redraw. Here the caller is this struct, and
//! the state is a fixture: either a board that has just booted with nothing
//! known, or one mid-session with every field filled in.

use oxinode_core::battery;
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
    /// A board mid-session, every field filled in.
    Populated,
}

impl Fixture {
    /// The fixture a word names, for the command line.
    pub fn named(word: &str) -> Option<Fixture> {
        match word {
            "empty" => Some(Fixture::Empty),
            "populated" => Some(Fixture::Populated),
            _ => None,
        }
    }

    pub fn state(self) -> State {
        match self {
            Fixture::Empty => State::default(),
            Fixture::Populated => populated(),
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
        assert_eq!(Fixture::named("full"), None);
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
