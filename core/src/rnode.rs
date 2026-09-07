//! The RNode protocol: what a Reticulum host says down the serial port, and
//! what it expects back.
//!
//! * [`kiss`] — the framing.
//! * [`command`] — the command set, and which frames the host un-escapes.
//! * [`protocol`] — the conversation: commands in, responses and actions out.
//!
//! # Provenance
//!
//! This is a clean-room implementation. RNode_Firmware_CE is GPLv3 and none of
//! it has been read, ported or transliterated; oxinode is MIT/Apache-2.0.
//!
//! The protocol is taken from the **counterpart** instead — Reticulum's own
//! `RNS/Interfaces/RNodeInterface.py`, version 1.5.0 — which is the
//! implementation oxinode has to satisfy and therefore the specification in the
//! only sense that matters. What is taken from it is interface data: command
//! bytes, field widths, byte order, and which frames the host un-escapes.
//!
//! Every constant here names the host behaviour that pins it, so that a
//! disagreement later can be checked against the thing that decided it rather
//! than re-derived.

pub mod command;
pub mod display;
pub mod eeprom;
pub mod kiss;
pub mod protocol;
pub mod store;
