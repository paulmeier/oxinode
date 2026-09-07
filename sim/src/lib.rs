//! A panel simulator, so the interface can be built without a board.
//!
//! Phase 9 built the on-device interface as pure code in `oxinode-core`: a
//! [`Nav`](oxinode_core::ui::Nav) that takes an input and says what happened,
//! and render functions that take a [`Frame`](oxinode_core::sh1107::Frame) and
//! draw. All of it is testable by assertions about pixels, and none of it can
//! be *looked at* that way. This crate is the looking.
//!
//! Four things, each in its own module:
//!
//! * [`image`] turns a frame into a PNG at 4x with a visible pixel grid, which
//!   at 128 x 128 reads better than the panel does, and compares two of them.
//! * [`script`] parses `right right select down select` into inputs, so a path
//!   through the menus is a fixture rather than a hand-written call sequence.
//! * [`scene`] is a navigator plus the borrowed state a page needs, and renders
//!   whole pages the way the firmware will.
//! * [`golden`] renders a fixed set of states and checks them against committed
//!   PNGs, writing a diff image on mismatch. That is the regression net the
//!   pixel assertions cannot be: they say where nothing is drawn, and a golden
//!   image says what it looks like.
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
pub mod scene;
pub mod script;
pub mod text;
pub mod tty;
