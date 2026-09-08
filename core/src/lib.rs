//! Logic that has no business touching hardware.
//!
//! The firmware crate cannot be unit tested: it depends on `embassy-nrf`, which
//! only builds for `thumbv7em-none-eabihf`, and there is no debug probe to run a
//! test harness on the board anyway. So anything that is *decidable without a
//! peripheral* lives here instead, where `cargo test` can reach it on the host.
//!
//! Today that is a small pile -- phases 0-2 are mostly register pokes, which are
//! not meaningfully testable off-target. The seam matters more than the current
//! contents: the RNode/KISS layer in phase 5 is almost entirely pure byte
//! manipulation, and that is where the bugs will be.
//!
//! Nothing in here may depend on `embassy-*`, `cortex-m`, or any specific chip.
//!
//! The on-device interface is not here either. It is [`monopanel`], a crate
//! with nothing of oxinode in it; what is here is what oxinode supplies to it
//! -- its screens and their content, its actions, its editor -- and the
//! SH1107 framebuffer it draws on. See `docs/phase-13-interface-crate.md`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod battery;
pub mod ble;
pub mod edit;
pub mod gpio;
pub mod hash;
pub mod linker_script;
pub mod logbuf;
pub mod lr1121;
pub mod meshtastic;
pub mod pad;
pub mod rnode;
pub mod screens;
pub mod serial;
pub mod sh1107;
pub mod status;
pub mod ui;
pub mod usb;
