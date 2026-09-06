//! What oxinode knows about the LR1121 that can be decided without a radio.
//!
//! Everything here is pure: timings, bit layouts, and the readings taken from
//! them. It lives in `oxinode-core` so it can be tested on a host, because the
//! firmware crate cannot be tested at all — no std, no probe, no harness — and
//! this board offers no way to observe the radio except through software.
//!
//! * [`reset`] — bringing the chip out of reset, and reading BUSY's answer.
//! * [`version`] — the `GetVersion` reply, and telling a real one from a bus
//!   that is not working.
//! * [`tcxo`] — the oscillator startup delay, and what a temperature reading
//!   has to look like to be believed.
//! * [`rf_switch`] — which DIOs drive the antenna switch, and what each does
//!   in each mode.
//! * [`pa`] — output power limits, and the rules for keying a carrier without
//!   damaging anything.
//! * [`irq`] — which interrupts are routed to the one line that reaches the
//!   MCU.
//! * [`lora`] — modulation parameters, and how long a packet takes to send.

pub mod irq;
pub mod lora;
pub mod pa;
pub mod reset;
pub mod rf_switch;
pub mod tcxo;
pub mod version;

pub use reset::*;
pub use version::*;
