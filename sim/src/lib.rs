//! A panel simulator, so the interface can be built without a board.
//!
//! The on-device interface is pure code: a
//! [`Nav`](oxinode_core::ui::Nav) that takes an input and says what happened,
//! and render functions that take a canvas and draw. The interface itself is
//! its own crate, [`monopanel`], and oxinode's screens are what it draws. All
//! of it is testable by assertions about pixels, and none of it can be
//! *looked at* that way. This crate is the looking.
//!
//! Five things, each in its own module:
//!
//! * [`panel`] is a canvas of any size, so the same screens can be rendered
//!   on the board's 128 x 128 and on a 128 x 64 -- the second size the
//!   interface crate is held to.
//! * [`image`] turns a canvas into a PNG at 4x with a visible pixel grid,
//!   which at panel size reads better than the panel does, and compares two
//!   of them.
//! * [`script`] parses `right right select down select` into inputs, so a path
//!   through the menus is a fixture rather than a hand-written call sequence.
//! * [`scene`] is a navigator plus the borrowed state a page needs, and renders
//!   whole pages the way the firmware will.
//! * [`golden`] renders a fixed set of states and checks them against committed
//!   PNGs, writing a diff image on mismatch. That is the regression net the
//!   pixel assertions cannot be: they say where nothing is drawn, and a golden
//!   image says what it looks like. The set is drawn at both panel sizes.
//!
//! [`text`] and [`tty`] are the terminal mode: block-character rendering and
//! arrow keys, for exploring rather than asserting.
//!
//! # What it does not cover
//!
//! Button electrical behaviour, debounce and auto-repeat timing; I²C timing
//! and the panel's own refresh; and anything about real modem state, which is
//! supplied by the caller here exactly as it is on the board. Those need
//! hardware, and the simulator should not be trusted past its evidence.

pub mod golden;
pub mod image;
pub mod panel;
pub mod scene;
pub mod script;
pub mod text;
pub mod tty;
