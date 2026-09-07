//! The navigation pad, as a state machine: debounce and auto-repeat.
//!
//! The Super IO carries six switches -- a four-way pad, OK and back -- and the
//! interface models six gestures. Between them sits this module: it takes
//! switch levels stamped with a time, and gives back gestures. Everything
//! electrical (which pins, pull-ups, which interrupt) belongs to the firmware's
//! driver; everything *temporal* is here, where it can be tested with a clock
//! that is a number.
//!
//! # Why this is a pad driver and not a trackball driver
//!
//! Meshtastic reuses its trackball driver for this pad, and the difference is
//! not cosmetic. A trackball emits a burst of edges as the ball rolls and is
//! read by counting them; a pad emits one edge and then a *level* that lasts as
//! long as a finger does. Count edges from a pad and you get one step per press
//! with no key repeat; watch levels on a ball and you get a runaway cursor. So
//! this module is about levels and time, never about edges: an edge is only the
//! moment the firmware learns that a level changed.
//!
//! # The model
//!
//! Each switch is a small machine with two clocks:
//!
//! * **Debounce.** A change of level starts a timer; the change is *committed*
//!   only once the level has held for [`DEBOUNCE_MS`]. Every further change
//!   before then restarts the timer, so it does not matter how many times a
//!   contact bounces, only how long it takes to stop. A bounce that ends back
//!   where it began commits nothing.
//! * **Repeat.** Once a direction is committed as pressed, it emits again after
//!   [`REPEAT_DELAY_MS`] and then every [`REPEAT_PERIOD_MS`] until it is
//!   committed as released. OK and back never repeat: a held OK is one OK.
//!
//! There is no chording, no long-press and no double-press. Six switches, six
//! gestures, one meaning each. Two keys held at once produce their own two
//! gestures independently, and nothing that either would not have produced
//! alone.
//!
//! # Driving it
//!
//! The firmware calls [`Pad::level`] whenever it learns a switch's level -- from
//! an edge interrupt, or by re-reading every pin on every wake, which is what
//! it actually does -- and then [`Pad::poll`] with the current time, which
//! returns the events that are now due. [`Pad::deadline`] says when `poll` next
//! needs calling, and is `None` when nothing is pending: that is what lets the
//! driver sleep until the next edge and be *quiet* when nothing is pressed.
//!
//! A late poll emits at most one repeat per key, never a catch-up burst. If the
//! render loop stalled for a second, the user gets one step, not eight.
//!
//! # The numbers
//!
//! [`DEBOUNCE_MS`] is a starting figure, not a measurement. Tactile dome
//! switches settle in 1-10 ms when new and get worse with wear, and 20 ms
//! covers that with margin while adding less latency than one frame of the
//! render loop. What settles it is [`Event::settle_ms`] and
//! [`Event::bounces`]: every committed press reports how long the contact took
//! to stop bouncing and how many edges it produced, the firmware logs them, and
//! the constant is to be revised against what the real switches do.

use crate::ui::Input;

/// A time in milliseconds on the caller's clock. Only differences matter.
pub type Millis = u64;

/// How long a level must hold before it is believed.
pub const DEBOUNCE_MS: Millis = 20;

/// How long a direction is held before it starts repeating.
///
/// Long enough that a deliberate single press never repeats by accident --
/// a press is typically 100-200 ms -- and short enough that a hold feels like
/// it is doing something.
pub const REPEAT_DELAY_MS: Millis = 400;

/// The interval between repeats of a held direction: eight steps a second.
///
/// The content rows on the panel are one text line each, so this is eight
/// lines a second -- fast enough to cross a screen, slow enough to stop where
/// you meant to.
pub const REPEAT_PERIOD_MS: Millis = 125;

// A repeat that could land before the press it repeats, or faster than a
// contact can settle, is a driver that repeats bounces. Ordered, at compile
// time.
const _: () = assert!(DEBOUNCE_MS < REPEAT_PERIOD_MS && REPEAT_PERIOD_MS < REPEAT_DELAY_MS);

/// One of the six switches on the Super IO.
///
/// Named for the switch, not the gesture. The two line up one to one on this
/// board, but the pad does not know what [`Input::Select`] means and the
/// interface does not know what a debounce is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
}

impl Key {
    /// Every switch, in the order events are reported in.
    pub const ALL: [Key; 6] = [
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Ok,
        Key::Back,
    ];

    /// The gesture this switch means.
    pub const fn gesture(self) -> Input {
        match self {
            Key::Up => Input::Up,
            Key::Down => Input::Down,
            Key::Left => Input::Left,
            Key::Right => Input::Right,
            Key::Ok => Input::Select,
            Key::Back => Input::Back,
        }
    }

    /// Whether holding this switch repeats its gesture.
    ///
    /// The four directions do, because a held direction is how you scroll.
    /// OK and back do not: an action chosen once is chosen once, and a menu
    /// that is left is left.
    pub const fn repeats(self) -> bool {
        matches!(self, Key::Up | Key::Down | Key::Left | Key::Right)
    }

    /// Its name, for the log.
    pub const fn name(self) -> &'static str {
        match self {
            Key::Up => "up",
            Key::Down => "down",
            Key::Left => "left",
            Key::Right => "right",
            Key::Ok => "ok",
            Key::Back => "back",
        }
    }

    const fn index(self) -> usize {
        match self {
            Key::Up => 0,
            Key::Down => 1,
            Key::Left => 2,
            Key::Right => 3,
            Key::Ok => 4,
            Key::Back => 5,
        }
    }
}

/// Why a gesture was emitted.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The switch was pressed.
    Press,
    /// The switch is still held, and it is a direction.
    Repeat,
}

/// A gesture the pad produced, with what the contact did on the way.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Event {
    pub key: Key,
    pub kind: Kind,
    /// Edges seen after the first one and before the level settled: zero for a
    /// clean contact. Meaningful on a [`Kind::Press`]; zero on a repeat.
    pub bounces: u8,
    /// Milliseconds from the first edge to the last, on a press. Together with
    /// `bounces` this is the measurement that [`DEBOUNCE_MS`] is to be settled
    /// against. Zero on a repeat.
    pub settle_ms: u16,
}

impl Event {
    /// The gesture, for the interface.
    pub const fn input(&self) -> Input {
        self.key.gesture()
    }
}

/// The events one [`Pad::poll`] produced, in [`Key::ALL`] order.
///
/// A fixed-capacity list rather than an allocation: a poll can produce at most
/// one event per key.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Events {
    items: [Option<Event>; 6],
    len: usize,
}

impl Events {
    const fn empty() -> Self {
        Events {
            items: [None; 6],
            len: 0,
        }
    }

    fn push(&mut self, event: Event) {
        self.items[self.len] = Some(event);
        self.len += 1;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
        self.items[..self.len].iter().map(|e| e.expect("pushed"))
    }
}

impl IntoIterator for Events {
    type Item = Event;
    type IntoIter = core::iter::Flatten<core::array::IntoIter<Option<Event>, 6>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter().flatten()
    }
}

/// One switch's state.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Switch {
    /// The level the pad believes: pressed or not.
    settled: bool,
    /// The level most recently reported.
    raw: bool,
    /// When `raw` first departed from `settled`, while it still does.
    unstable_since: Option<Millis>,
    /// When `raw` last changed, while unstable.
    last_edge: Millis,
    /// Edges since `unstable_since`, not counting the first.
    bounces: u8,
    /// When the next repeat is due, while settled pressed and repeating.
    next_repeat: Option<Millis>,
}

impl Switch {
    const RELEASED: Switch = Switch {
        settled: false,
        raw: false,
        unstable_since: None,
        last_edge: 0,
        bounces: 0,
        next_repeat: None,
    };

    fn level(&mut self, pressed: bool, now: Millis) {
        if pressed == self.raw {
            // A re-read that agrees. Not an edge, so not a bounce.
            return;
        }
        self.raw = pressed;
        match self.unstable_since {
            // Bouncing already: another edge, and the clock restarts.
            Some(_) => self.bounces = self.bounces.saturating_add(1),
            None => {
                self.unstable_since = Some(now);
                self.bounces = 0;
            }
        }
        self.last_edge = now;
    }

    fn poll(&mut self, key: Key, now: Millis) -> Option<Event> {
        let mut event = None;

        if let Some(since) = self.unstable_since {
            if now.saturating_sub(self.last_edge) >= DEBOUNCE_MS {
                self.unstable_since = None;
                if self.raw != self.settled {
                    self.settled = self.raw;
                    if self.settled {
                        event = Some(Event {
                            key,
                            kind: Kind::Press,
                            bounces: self.bounces,
                            settle_ms: (self.last_edge - since).min(u16::MAX as Millis) as u16,
                        });
                        self.next_repeat = key.repeats().then_some(now + REPEAT_DELAY_MS);
                    } else {
                        self.next_repeat = None;
                    }
                }
                // Else: it bounced and came back. Nothing happened.
            }
        }

        // `next_repeat` is `Some` only between a committed press of a
        // direction and its committed release, so it is the whole of the
        // condition. A press committed above scheduled it a full delay from
        // now, so a press and a repeat never land in the same poll.
        if let Some(due) = self.next_repeat {
            if now >= due {
                event = Some(Event {
                    key,
                    kind: Kind::Repeat,
                    bounces: 0,
                    settle_ms: 0,
                });
                // From now, not from `due`: a late poll gets one repeat,
                // not a burst.
                self.next_repeat = Some(now + REPEAT_PERIOD_MS);
            }
        }

        event
    }

    fn deadline(&self) -> Option<Millis> {
        let debounce = self.unstable_since.map(|_| self.last_edge + DEBOUNCE_MS);
        match (debounce, self.next_repeat) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// All six switches.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Pad {
    switches: [Switch; 6],
}

impl Default for Pad {
    fn default() -> Self {
        Self::new()
    }
}

impl Pad {
    /// Every switch released.
    pub const fn new() -> Self {
        Pad {
            switches: [Switch::RELEASED; 6],
        }
    }

    /// Report a switch's level. `pressed` is the electrical fact already
    /// translated -- the caller knows the switches are active low, this does
    /// not.
    ///
    /// Idempotent: reporting the level the pad last heard is not an edge.
    pub fn level(&mut self, key: Key, pressed: bool, now: Millis) {
        self.switches[key.index()].level(pressed, now);
    }

    /// Advance the clock and collect what is now due.
    pub fn poll(&mut self, now: Millis) -> Events {
        let mut events = Events::empty();
        for key in Key::ALL {
            if let Some(event) = self.switches[key.index()].poll(key, now) {
                events.push(event);
            }
        }
        events
    }

    /// When [`poll`](Self::poll) next has something to do, or `None` if nothing
    /// is pending and the next thing that can happen is an edge.
    pub fn deadline(&self) -> Option<Millis> {
        self.switches.iter().filter_map(Switch::deadline).min()
    }

    /// Whether the pad believes this switch is down.
    pub fn is_pressed(&self, key: Key) -> bool {
        self.switches[key.index()].settled
    }
}

/// The three-position mode switch, decoded from its two lines.
///
/// Meshtastic names P1.09 `SWITCH_MODE1` ("Top Position") and P0.12
/// `SWITCH_MODE2` ("Middle Position"), reads the second with no pull and
/// treats it low as "GPS off". That is the whole of what the sources say.
/// There is no Super IO schematic, so which level each position drives is a
/// hypothesis until it is read on the board with the switch in all three
/// positions: this decoder assumes each line is pulled to its active level in
/// its own position and the bottom position drives neither.
///
/// The raw levels are what the firmware logs; this is only a reading of them.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Top,
    Middle,
    Bottom,
    /// Both lines active at once, which no position of a three-way switch
    /// should produce.
    Invalid,
}

impl Mode {
    /// Decode from the two lines, each `true` when active.
    pub const fn decode(mode1_active: bool, mode2_active: bool) -> Mode {
        match (mode1_active, mode2_active) {
            (true, false) => Mode::Top,
            (false, true) => Mode::Middle,
            (false, false) => Mode::Bottom,
            (true, true) => Mode::Invalid,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Mode::Top => "top",
            Mode::Middle => "middle",
            Mode::Bottom => "bottom",
            Mode::Invalid => "invalid",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Press a key cleanly at `at` and poll once it has settled.
    fn press(pad: &mut Pad, key: Key, at: Millis) -> Events {
        pad.level(key, true, at);
        pad.poll(at + DEBOUNCE_MS)
    }

    fn release(pad: &mut Pad, key: Key, at: Millis) -> Events {
        pad.level(key, false, at);
        pad.poll(at + DEBOUNCE_MS)
    }

    fn inputs(events: Events) -> Vec<Input> {
        events.into_iter().map(|e| e.input()).collect()
    }

    fn kinds(events: Events) -> Vec<Kind> {
        events.into_iter().map(|e| e.kind).collect()
    }

    #[test]
    fn every_switch_has_its_own_gesture() {
        let gestures: Vec<Input> = Key::ALL.iter().map(|k| k.gesture()).collect();
        assert_eq!(
            gestures,
            [
                Input::Up,
                Input::Down,
                Input::Left,
                Input::Right,
                Input::Select,
                Input::Back
            ]
        );
        // Six switches, six gestures: no two switches mean the same thing.
        for (i, a) in gestures.iter().enumerate() {
            for b in &gestures[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn only_the_directions_repeat() {
        assert!(Key::Up.repeats());
        assert!(Key::Down.repeats());
        assert!(Key::Left.repeats());
        assert!(Key::Right.repeats());
        assert!(!Key::Ok.repeats());
        assert!(!Key::Back.repeats());
    }

    #[test]
    fn names_are_distinct_and_lowercase() {
        let names: Vec<&str> = Key::ALL.iter().map(|k| k.name()).collect();
        for (i, a) in names.iter().enumerate() {
            assert_eq!(*a, a.to_lowercase());
            assert!(!names[i + 1..].contains(a));
        }
    }

    #[test]
    fn the_numbers_are_what_the_docs_say() {
        // Pinned so a change is a deliberate edit here and in the README.
        assert_eq!(DEBOUNCE_MS, 20);
        assert_eq!(REPEAT_DELAY_MS, 400);
        assert_eq!(REPEAT_PERIOD_MS, 125);
    }

    #[test]
    fn a_fresh_pad_is_idle() {
        let pad = Pad::new();
        assert_eq!(pad.deadline(), None);
        for key in Key::ALL {
            assert!(!pad.is_pressed(key));
        }
        assert!(Pad::default().poll(1000).is_empty());
    }

    #[test]
    fn a_press_is_reported_once_it_has_held_for_the_debounce() {
        let mut pad = Pad::new();
        pad.level(Key::Ok, true, 100);
        // Not yet.
        assert!(pad.poll(100).is_empty());
        assert!(pad.poll(100 + DEBOUNCE_MS - 1).is_empty());
        assert!(!pad.is_pressed(Key::Ok));
        // Exactly at the boundary, yes.
        let events = pad.poll(100 + DEBOUNCE_MS);
        assert_eq!(inputs(events), [Input::Select]);
        assert_eq!(kinds(events), [Kind::Press]);
        assert!(pad.is_pressed(Key::Ok));
        // And only once.
        assert!(pad.poll(100 + DEBOUNCE_MS + 1).is_empty());
    }

    #[test]
    fn a_clean_press_reports_no_bounces() {
        let mut pad = Pad::new();
        let events = press(&mut pad, Key::Up, 50);
        let event = events.iter().next().unwrap();
        assert_eq!(event.bounces, 0);
        assert_eq!(event.settle_ms, 0);
    }

    #[test]
    fn bouncing_restarts_the_clock_and_is_counted() {
        let mut pad = Pad::new();
        // Down, up, down, up, down: five edges over 7 ms.
        pad.level(Key::Down, true, 100);
        pad.level(Key::Down, false, 102);
        pad.level(Key::Down, true, 103);
        pad.level(Key::Down, false, 105);
        pad.level(Key::Down, true, 107);
        // Measured from the last edge, not the first.
        assert!(pad.poll(100 + DEBOUNCE_MS).is_empty());
        assert!(pad.poll(107 + DEBOUNCE_MS - 1).is_empty());
        let events = pad.poll(107 + DEBOUNCE_MS);
        assert_eq!(events.len(), 1);
        let event = events.iter().next().unwrap();
        assert_eq!(event.input(), Input::Down);
        assert_eq!(event.kind, Kind::Press);
        assert_eq!(event.bounces, 4, "edges after the first");
        assert_eq!(event.settle_ms, 7, "first edge to last");
    }

    #[test]
    fn a_bounce_that_returns_to_where_it_started_is_nothing() {
        let mut pad = Pad::new();
        pad.level(Key::Ok, true, 100);
        pad.level(Key::Ok, false, 105);
        assert!(pad.poll(105 + DEBOUNCE_MS).is_empty());
        assert!(!pad.is_pressed(Key::Ok));
        // And the pad is idle again, not stuck waiting on anything.
        assert_eq!(pad.deadline(), None);
        assert!(pad.poll(10_000).is_empty());
    }

    #[test]
    fn a_glitch_shorter_than_the_debounce_is_not_a_press() {
        let mut pad = Pad::new();
        pad.level(Key::Back, true, 100);
        pad.level(Key::Back, false, 100 + DEBOUNCE_MS - 1);
        assert!(pad.poll(100 + DEBOUNCE_MS).is_empty());
        assert!(pad.poll(200).is_empty());
    }

    #[test]
    fn repeating_the_same_level_is_not_an_edge() {
        let mut pad = Pad::new();
        pad.level(Key::Left, true, 100);
        // The driver re-reads every pin on every wake; agreeing re-reads must
        // neither restart the debounce nor count as bounces.
        pad.level(Key::Left, true, 110);
        pad.level(Key::Left, true, 115);
        let events = pad.poll(100 + DEBOUNCE_MS);
        assert_eq!(events.len(), 1);
        let event = events.iter().next().unwrap();
        assert_eq!(event.bounces, 0);
        assert_eq!(event.settle_ms, 0);
    }

    #[test]
    fn a_release_emits_nothing() {
        let mut pad = Pad::new();
        press(&mut pad, Key::Right, 0);
        assert!(release(&mut pad, Key::Right, 100).is_empty());
        assert!(!pad.is_pressed(Key::Right));
        assert_eq!(pad.deadline(), None);
    }

    #[test]
    fn a_held_direction_repeats_after_the_delay_and_then_every_period() {
        let mut pad = Pad::new();
        let t0 = 1000;
        assert_eq!(inputs(press(&mut pad, Key::Down, t0)), [Input::Down]);
        let pressed_at = t0 + DEBOUNCE_MS;
        // Quiet until the delay.
        assert!(pad.poll(pressed_at + REPEAT_DELAY_MS - 1).is_empty());
        let first = pad.poll(pressed_at + REPEAT_DELAY_MS);
        assert_eq!(kinds(first), [Kind::Repeat]);
        assert_eq!(inputs(first), [Input::Down]);
        // Then every period.
        let mut now = pressed_at + REPEAT_DELAY_MS;
        for _ in 0..5 {
            assert!(pad.poll(now + REPEAT_PERIOD_MS - 1).is_empty());
            now += REPEAT_PERIOD_MS;
            assert_eq!(kinds(pad.poll(now)), [Kind::Repeat]);
        }
        // Until it is let go.
        assert!(release(&mut pad, Key::Down, now + 10).is_empty());
        assert!(pad.poll(now + 10_000).is_empty());
    }

    #[test]
    fn a_late_poll_gets_one_repeat_not_a_burst() {
        let mut pad = Pad::new();
        press(&mut pad, Key::Up, 0);
        // The render loop stalled for a second.
        let late = DEBOUNCE_MS + REPEAT_DELAY_MS + 1000;
        assert_eq!(pad.poll(late).len(), 1);
        // And the next one is a full period after the late poll, not the next
        // slot of the original schedule.
        assert!(pad.poll(late + REPEAT_PERIOD_MS - 1).is_empty());
        assert_eq!(pad.poll(late + REPEAT_PERIOD_MS).len(), 1);
    }

    #[test]
    fn a_held_ok_is_one_ok() {
        let mut pad = Pad::new();
        assert_eq!(inputs(press(&mut pad, Key::Ok, 0)), [Input::Select]);
        assert_eq!(pad.deadline(), None, "nothing scheduled for a held OK");
        for t in (DEBOUNCE_MS..60_000).step_by(37) {
            assert!(pad.poll(t).is_empty(), "OK repeated at {t} ms");
        }
        assert!(pad.is_pressed(Key::Ok));
    }

    #[test]
    fn a_held_back_is_one_back() {
        let mut pad = Pad::new();
        assert_eq!(inputs(press(&mut pad, Key::Back, 0)), [Input::Back]);
        assert_eq!(pad.deadline(), None);
        assert!(pad.poll(5_000).is_empty());
    }

    #[test]
    fn a_repeat_event_carries_no_bounce_statistics() {
        let mut pad = Pad::new();
        pad.level(Key::Left, true, 0);
        pad.level(Key::Left, false, 3);
        pad.level(Key::Left, true, 5);
        pad.poll(5 + DEBOUNCE_MS);
        let repeat = pad
            .poll(5 + DEBOUNCE_MS + REPEAT_DELAY_MS)
            .iter()
            .next()
            .unwrap();
        assert_eq!(repeat.kind, Kind::Repeat);
        assert_eq!(repeat.bounces, 0);
        assert_eq!(repeat.settle_ms, 0);
    }

    #[test]
    fn two_keys_held_are_two_keys_and_nothing_more() {
        let mut pad = Pad::new();
        pad.level(Key::Up, true, 0);
        pad.level(Key::Ok, true, 5);
        // Reported in Key::ALL order regardless of press order.
        let events = pad.poll(5 + DEBOUNCE_MS);
        assert_eq!(inputs(events), [Input::Up, Input::Select]);
        // The direction repeats on its own schedule; OK stays silent.
        let later = pad.poll(5 + DEBOUNCE_MS + REPEAT_DELAY_MS);
        assert_eq!(inputs(later), [Input::Up]);
        assert_eq!(kinds(later), [Kind::Repeat]);
    }

    #[test]
    fn events_come_out_in_switch_order() {
        let mut pad = Pad::new();
        for key in Key::ALL.iter().rev() {
            pad.level(*key, true, 0);
        }
        let events = pad.poll(DEBOUNCE_MS);
        let keys: Vec<Key> = events.into_iter().map(|e| e.key).collect();
        assert_eq!(keys, Key::ALL);
        assert_eq!(events.len(), 6);
        assert!(!events.is_empty());
    }

    #[test]
    fn a_press_while_bouncing_on_release_still_ends_pressed() {
        let mut pad = Pad::new();
        press(&mut pad, Key::Right, 0);
        // Release starts, bounces, and the finger comes back down before it
        // settles: the pad never saw a release.
        pad.level(Key::Right, false, 200);
        pad.level(Key::Right, true, 210);
        assert!(pad.poll(210 + DEBOUNCE_MS).is_empty());
        assert!(pad.is_pressed(Key::Right));
    }

    #[test]
    fn deadline_is_the_debounce_while_bouncing() {
        let mut pad = Pad::new();
        pad.level(Key::Up, true, 100);
        assert_eq!(pad.deadline(), Some(100 + DEBOUNCE_MS));
        pad.level(Key::Up, false, 105);
        assert_eq!(
            pad.deadline(),
            Some(105 + DEBOUNCE_MS),
            "moves with the last edge"
        );
    }

    #[test]
    fn deadline_is_the_repeat_while_a_direction_is_held() {
        let mut pad = Pad::new();
        press(&mut pad, Key::Left, 0);
        assert_eq!(pad.deadline(), Some(DEBOUNCE_MS + REPEAT_DELAY_MS));
        pad.poll(DEBOUNCE_MS + REPEAT_DELAY_MS);
        assert_eq!(
            pad.deadline(),
            Some(DEBOUNCE_MS + REPEAT_DELAY_MS + REPEAT_PERIOD_MS)
        );
    }

    #[test]
    fn deadline_is_the_earliest_of_several() {
        let mut pad = Pad::new();
        press(&mut pad, Key::Down, 0); // repeat due at DEBOUNCE + DELAY
        pad.level(Key::Ok, true, 30); // debounce due at 50
        assert_eq!(pad.deadline(), Some(30 + DEBOUNCE_MS));
    }

    #[test]
    fn a_release_in_progress_does_not_keep_the_repeat_alive() {
        let mut pad = Pad::new();
        press(&mut pad, Key::Down, 0);
        pad.level(Key::Down, false, 100);
        // Still settled pressed, so the repeat is still on the calendar, but
        // the debounce lands first.
        assert_eq!(pad.deadline(), Some(100 + DEBOUNCE_MS));
        assert!(pad.poll(100 + DEBOUNCE_MS).is_empty());
        assert_eq!(pad.deadline(), None);
    }

    #[test]
    fn a_press_arriving_late_to_the_poll_is_still_measured_from_its_edge() {
        let mut pad = Pad::new();
        pad.level(Key::Ok, true, 100);
        // Polled long after the debounce would have expired; it still commits
        // exactly once and reports a clean contact.
        let events = pad.poll(5000);
        assert_eq!(events.len(), 1);
        assert_eq!(events.iter().next().unwrap().settle_ms, 0);
        assert!(pad.poll(5001).is_empty());
    }

    #[test]
    fn settle_time_saturates_rather_than_wrapping() {
        let mut pad = Pad::new();
        pad.level(Key::Ok, true, 0);
        pad.level(Key::Ok, false, 1);
        pad.level(Key::Ok, true, 100_000);
        let event = pad.poll(100_000 + DEBOUNCE_MS).iter().next().unwrap();
        assert_eq!(event.settle_ms, u16::MAX);
    }

    #[test]
    fn a_full_session_on_a_scrolling_list() {
        // Press down, hold it for a second, let go, then press OK: the
        // interface should see 1 press + 5 repeats of Down, then one Select.
        //
        // Driven the way the firmware drives it: levels arrive when their
        // edge happens, and the clock advances to whichever of the next edge
        // and the pad's own deadline comes first.
        let mut pad = Pad::new();
        let mut seen = Vec::new();
        let script: &[(Millis, Key, bool)] = &[
            (0, Key::Down, true),
            (1000, Key::Down, false),
            (1100, Key::Ok, true),
        ];
        let mut now = 0;
        let mut next = 0;
        let mut rounds = 0;
        while now <= 2000 {
            // A machine whose deadline never advances would loop here
            // forever; that is a failure, not a hang.
            rounds += 1;
            assert!(rounds < 100, "the clock stopped advancing at {now} ms");
            while next < script.len() && script[next].0 <= now {
                let (at, key, pressed) = script[next];
                pad.level(key, pressed, at);
                next += 1;
            }
            seen.extend(pad.poll(now));
            now = match (pad.deadline(), script.get(next)) {
                (Some(d), Some(s)) => d.min(s.0),
                (Some(d), None) => d,
                (None, Some(s)) => s.0,
                (None, None) => break,
            };
        }
        let downs = seen.iter().filter(|e| e.key == Key::Down).count();
        // Held from 20 ms (settled) to 1020 ms (release settled): the press,
        // then repeats at 420, 545, 670, 795, 920 -- and not at 1045.
        assert_eq!(downs, 6);
        let oks: Vec<&Event> = seen.iter().filter(|e| e.key == Key::Ok).collect();
        assert_eq!(oks.len(), 1);
        assert_eq!(oks[0].kind, Kind::Press);
        // Order is preserved: every Down before the Select.
        let last_down = seen.iter().rposition(|e| e.key == Key::Down).unwrap();
        let the_ok = seen.iter().position(|e| e.key == Key::Ok).unwrap();
        assert!(last_down < the_ok);
    }

    #[test]
    fn mode_switch_decodes_one_line_per_position() {
        assert_eq!(Mode::decode(true, false), Mode::Top);
        assert_eq!(Mode::decode(false, true), Mode::Middle);
        assert_eq!(Mode::decode(false, false), Mode::Bottom);
        assert_eq!(Mode::decode(true, true), Mode::Invalid);
    }

    #[test]
    fn mode_names_are_distinct() {
        let all = [Mode::Top, Mode::Middle, Mode::Bottom, Mode::Invalid];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.name(), b.name());
            }
        }
    }
}
